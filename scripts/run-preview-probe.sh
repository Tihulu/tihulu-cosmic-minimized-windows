#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
set -euo pipefail

REPO_URL="https://github.com/Tihulu/tihulu-cosmic-minimized-windows.git"
BRANCH="experiment/ext-image-copy-probe"
MATCH_TERM="${MATCH_TERM:-brave}"
CAPTURES="${CAPTURES:-500}"
SAMPLE_EVERY="${SAMPLE_EVERY:-10}"
RESULT_DIR="${RESULT_DIR:-$HOME/tihulu-preview-probe-results}"

fail() {
    printf 'FAIL: %s\n' "$*" >&2
    exit 1
}

command -v git >/dev/null 2>&1 || fail "git is required"
command -v cargo >/dev/null 2>&1 || fail "cargo is required"
command -v pgrep >/dev/null 2>&1 || fail "pgrep is required"
command -v python3 >/dev/null 2>&1 || fail "python3 is required"

[[ "${XDG_SESSION_TYPE:-}" == "wayland" || -n "${WAYLAND_DISPLAY:-}" ]] || \
    fail "this must run inside your COSMIC Wayland session"
[[ -n "${XDG_RUNTIME_DIR:-}" ]] || fail "XDG_RUNTIME_DIR is not set"
pgrep -x cosmic-comp >/dev/null 2>&1 || fail "cosmic-comp is not running"

mkdir -p "$RESULT_DIR"
WORKDIR="$(mktemp -d -t tihulu-preview-probe.XXXXXX)"
trap 'rm -rf "$WORKDIR"' EXIT

printf '==> Cloning %s (%s) into a temporary directory\n' "$REPO_URL" "$BRANCH"
git clone --quiet --depth 1 --branch "$BRANCH" "$REPO_URL" "$WORKDIR/repo"
cd "$WORKDIR/repo"

printf '==> Building release probe\n'
cargo build --release --bin tihulu-preview-probe
BIN="$WORKDIR/repo/target/release/tihulu-preview-probe"

printf '\n==> Available toplevels\n'
LIST_OUTPUT="$($BIN --list)"
printf '%s\n' "$LIST_OUTPUT"

if ! grep -qi -- "$MATCH_TERM" <<<"$LIST_OUTPUT"; then
    fail "no toplevel matches '$MATCH_TERM'; set MATCH_TERM to a distinctive title/app-id"
fi

printf '\nIMPORTANT: keep the intended %s window minimized for the entire capture run.\n' "$MATCH_TERM"
printf 'The standard ext-foreign-toplevel protocol cannot verify minimized state itself.\n\n'

STAMP="$(date +%Y%m%d-%H%M%S)"
CSV="$RESULT_DIR/${MATCH_TERM//[^[:alnum:]._-]/_}-${CAPTURES}-${STAMP}.csv"

printf '==> Running %s captures, sampling every %s\n' "$CAPTURES" "$SAMPLE_EVERY"
set +e
"$BIN" \
    --match "$MATCH_TERM" \
    --captures "$CAPTURES" \
    --sample-every "$SAMPLE_EVERY" \
    --output "$CSV"
PROBE_RC=$?
set -e

[[ -s "$CSV" ]] || fail "probe produced no CSV output"

printf '\n==> Resource summary\n'
set +e
python3 - "$CSV" "$PROBE_RC" "$CAPTURES" <<'PY'
import csv
import sys
from pathlib import Path

path = Path(sys.argv[1])
probe_rc = int(sys.argv[2])
expected_captures = int(sys.argv[3])
with path.open(newline="") as handle:
    rows = list(csv.DictReader(handle))

if not rows:
    print("VERDICT: FAIL — CSV has no samples.")
    raise SystemExit(2)

numeric = [
    "probe_fd",
    "probe_rss_kb",
    "probe_shmem_kb",
    "probe_memfd",
    "cosmic_comp_fd",
    "cosmic_comp_rss_kb",
    "cosmic_comp_shmem_kb",
    "cosmic_comp_memfd",
    "cosmic_comp_capture_memfd",
]

def value(row, key):
    raw = (row.get(key) or "").strip()
    try:
        return int(raw)
    except ValueError:
        return 0

first = rows[0]
last = rows[-1]
for key in numeric:
    values = [value(row, key) for row in rows]
    print(
        f"{key:28s} start={values[0]:8d} end={values[-1]:8d} "
        f"max={max(values):8d} delta={values[-1]-values[0]:+8d}"
    )

capture_rows = [row for row in rows if row.get("phase") == "capture"]
failed_samples = sum(
    (row.get("capture_ok") or "").strip().lower() not in {"true", "1"}
    for row in capture_rows
)
final_iteration = value(last, "iteration")
print(f"CSV sample rows               {len(rows)}")
print(f"failed capture samples        {failed_samples}")
print(f"final iteration               {final_iteration}/{expected_captures}")
print(f"probe exit code               {probe_rc}")

# The Rust circuit breaker is authoritative for monotonic FD/capture-memfd growth.
if probe_rc == 2:
    print("VERDICT: FAIL — circuit breaker tripped; preview integration is NOT approved.")
    raise SystemExit(2)
if probe_rc != 0 or final_iteration != expected_captures:
    print("VERDICT: FAIL — probe did not complete all requested captures.")
    raise SystemExit(2)

# Additional conservative end-to-start checks. Small bounded oscillations are allowed.
fd_keys = [
    "probe_fd",
    "cosmic_comp_fd",
    "cosmic_comp_capture_memfd",
    "cosmic_comp_memfd",
]
fd_growth = {key: value(last, key) - value(first, key) for key in fd_keys}
if any(delta >= 4 for delta in fd_growth.values()):
    print("VERDICT: FAIL — persistent FD/memfd growth remains at the end of the run.")
    raise SystemExit(2)

if failed_samples:
    print("VERDICT: REVIEW — at least one capture failed; inspect the CSV before approval.")
    raise SystemExit(3)

# RSS is noisy, so it never hard-fails automatically. A meaningful retained increase is REVIEW.
rss_keys = ["probe_rss_kb", "cosmic_comp_rss_kb"]
for key in rss_keys:
    values = [value(row, key) for row in rows]
    retained = values[-1] - values[0]
    if retained >= 32 * 1024:
        print(
            f"VERDICT: REVIEW — {key} retained >=32 MiB above baseline; "
            "inspect the CSV trend before approval."
        )
        raise SystemExit(3)

print("VERDICT: PASS — 500-capture safety checks show bounded FD/memfd behavior.")
print("CSV retained for audit. previewd work may begin only from a real minimized-window run.")
PY
ANALYZE_RC=$?
set -e

printf '\nCSV: %s\n' "$CSV"
exit "$ANALYZE_RC"
