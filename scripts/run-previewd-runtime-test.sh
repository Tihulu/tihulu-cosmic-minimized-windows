#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
set -Eeuo pipefail

SERVICE="tihulu-previewd.service"
INTERVAL="${INTERVAL:-2}"
MODE="${MODE:-interactive}"
RESULTS_ROOT="${RESULTS_ROOT:-$HOME/tihulu-previewd-runtime-results}"
RUN_ID="$(date '+%Y%m%d-%H%M%S')"
RUN_DIR="$RESULTS_ROOT/$RUN_ID"
CSV="$RUN_DIR/resources.csv"
MANUAL="$RUN_DIR/manual-checks.tsv"
PHASE_FILE="$RUN_DIR/phase"
MONITOR_PID=""
MANUAL_FAIL=0
AUTO_FAIL=0
SERVICE_STOPPED_BY_TEST=0

log() { printf '\n==> %s\n' "$*"; }
warn() { printf '\nWARN: %s\n' "$*" >&2; }
fail() { printf '\nFAIL: %s\n' "$*" >&2; exit 2; }
need() { command -v "$1" >/dev/null 2>&1 || fail "Required command not found: $1"; }

cleanup() {
  if [ -n "$MONITOR_PID" ]; then
    kill "$MONITOR_PID" 2>/dev/null || true
    wait "$MONITOR_PID" 2>/dev/null || true
  fi
  if [ "$SERVICE_STOPPED_BY_TEST" -eq 1 ]; then
    systemctl --user start "$SERVICE" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT INT TERM

for cmd in systemctl journalctl pgrep awk find du readlink wc date sleep; do
  need "$cmd"
done

mkdir -p "$RUN_DIR"
printf 'check\tresult\n' > "$MANUAL"
printf '%s\n' 'preflight' > "$PHASE_FILE"

set_phase() {
  printf '%s\n' "$1" > "$PHASE_FILE"
  log "Phase: $1"
}

confirm() {
  local key="$1" prompt="$2" answer
  printf '\n%s\n' "$prompt"
  printf 'Pass this check? [y/N]: '
  IFS= read -r answer || answer=""
  case "$answer" in
    y|Y|yes|YES|Yes)
      printf '%s\tPASS\n' "$key" >> "$MANUAL"
      ;;
    *)
      printf '%s\tFAIL\n' "$key" >> "$MANUAL"
      MANUAL_FAIL=1
      ;;
  esac
}

proc_status_value() {
  local pid="$1" field="$2"
  [ -r "/proc/$pid/status" ] || { printf '0'; return; }
  awk -v field="$field" '$1 == field ":" { print $2; found=1; exit } END { if (!found) print 0 }' "/proc/$pid/status"
}

fd_count() {
  local pid="$1"
  [ -d "/proc/$pid/fd" ] || { printf '0'; return; }
  find "/proc/$pid/fd" -mindepth 1 -maxdepth 1 2>/dev/null | wc -l
}

memfd_counts() {
  local pid="$1" total=0 capture=0 entry target
  if [ ! -d "/proc/$pid/fd" ]; then
    printf '0 0'
    return
  fi
  for entry in "/proc/$pid/fd"/*; do
    [ -e "$entry" ] || continue
    target="$(readlink "$entry" 2>/dev/null || true)"
    case "$target" in
      /memfd:*|memfd:*)
        total=$((total + 1))
        case "$target" in
          *capture*|*screencopy*|*minimize-applet*) capture=$((capture + 1)) ;;
        esac
        ;;
    esac
  done
  printf '%s %s' "$total" "$capture"
}

first_pid() {
  case "$1" in
    previewd) pgrep -x tihulu-previewd 2>/dev/null | head -1 || true ;;
    comp) pgrep -x cosmic-comp 2>/dev/null | head -1 || true ;;
    applet) pgrep -f '(^|/)tihulu-cosmic-minimized-windows([[:space:]]|$)' 2>/dev/null | head -1 || true ;;
    panel) pgrep -x cosmic-panel 2>/dev/null | head -1 || true ;;
  esac
}

sample_row() {
  local phase pp cp pfd prss pmem pcap cfd crss cmem ccap cache_files=0 cache_bytes=0 counts
  phase="$(cat "$PHASE_FILE" 2>/dev/null || printf unknown)"
  pp="$(first_pid previewd)"
  cp="$(first_pid comp)"

  if [ -n "$pp" ]; then
    pfd="$(fd_count "$pp")"
    prss="$(proc_status_value "$pp" VmRSS)"
    counts="$(memfd_counts "$pp")"
    pmem="${counts%% *}"
    pcap="${counts##* }"
  else
    pp=0; pfd=0; prss=0; pmem=0; pcap=0
  fi

  if [ -n "$cp" ]; then
    cfd="$(fd_count "$cp")"
    crss="$(proc_status_value "$cp" VmRSS)"
    counts="$(memfd_counts "$cp")"
    cmem="${counts%% *}"
    ccap="${counts##* }"
  else
    cp=0; cfd=0; crss=0; cmem=0; ccap=0
  fi

  if [ -n "${XDG_RUNTIME_DIR:-}" ] && [ -d "$XDG_RUNTIME_DIR/tihulu-cosmic-minimized-windows/previews" ]; then
    cache_files="$(find "$XDG_RUNTIME_DIR/tihulu-cosmic-minimized-windows/previews" -maxdepth 1 -type f -name '*.rgba' 2>/dev/null | wc -l)"
    cache_bytes="$(du -sb "$XDG_RUNTIME_DIR/tihulu-cosmic-minimized-windows/previews" 2>/dev/null | awk '{print $1}')"
    cache_bytes="${cache_bytes:-0}"
  fi

  printf '%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s\n' \
    "$(date +%s)" "$phase" "$pp" "$pfd" "$prss" "$pmem" "$pcap" \
    "$cp" "$cfd" "$crss" "$cmem" "$ccap" "$cache_files" "$cache_bytes" >> "$CSV"
}

monitor_loop() {
  printf 'timestamp,phase,previewd_pid,previewd_fd,previewd_rss_kb,previewd_memfd,previewd_capture_memfd,cosmic_comp_pid,cosmic_comp_fd,cosmic_comp_rss_kb,cosmic_comp_memfd,cosmic_comp_capture_memfd,cache_files,cache_bytes\n' > "$CSV"
  while :; do
    sample_row
    sleep "$INTERVAL"
  done
}

resource_summary() {
  log "Resource summary"
  awk -F, '
    NR == 1 { next }
    {
      if (NR == 2) {
        pfd0=$4; cfd0=$9; pcap0=$7; ccap0=$12
      }
      pfd=$4; cfd=$9; pcap=$7; ccap=$12
      if ($4 > pfdmax) pfdmax=$4
      if ($9 > cfdmax) cfdmax=$9
      if ($5 > prssmax) prssmax=$5
      if ($10 > crssmax) crssmax=$10
      if ($13 > cachemax) cachemax=$13
      if ($14 > cachebytesmax) cachebytesmax=$14
    }
    END {
      printf "previewd FD        start=%s end=%s max=%s\n", pfd0, pfd, pfdmax
      printf "cosmic-comp FD     start=%s end=%s max=%s\n", cfd0, cfd, cfdmax
      printf "previewd cap-memfd start=%s end=%s\n", pcap0, pcap
      printf "comp cap-memfd     start=%s end=%s\n", ccap0, ccap
      printf "previewd RSS max   %s KiB\n", prssmax
      printf "cosmic-comp RSS max %s KiB\n", crssmax
      printf "cache max          %s files / %s bytes\n", cachemax, cachebytesmax
    }
  ' "$CSV"
}

check_nonmonotonic_growth() {
  local column="$1" label="$2"
  if awk -F, -v col="$column" '
      NR == 1 { next }
      $2 ~ /^(safe-core|extended|hover|churn)$/ && $col ~ /^[0-9]+$/ {
        v=$col+0
        if (!seen) { first=v; prev=v; seen=1; count=1; next }
        count++
        if (v < prev) nondec=0
        prev=v; last=v
      }
      END {
        if (!seen || count < 6) exit 0
        if (nondec != 0 && (last-first) >= 4) exit 1
        exit 0
      }
    ' "$CSV"; then
    printf 'bounded: %s\n' "$label"
  else
    printf 'FAIL monotonic growth: %s\n' "$label"
    AUTO_FAIL=1
  fi
}

automated_checks() {
  log "Automated safety checks"
  local cache_files_max cache_bytes_max previewd_fd_max
  cache_files_max="$(awk -F, 'NR>1 && $13>m {m=$13} END {print m+0}' "$CSV")"
  cache_bytes_max="$(awk -F, 'NR>1 && $14>m {m=$14} END {print m+0}' "$CSV")"
  previewd_fd_max="$(awk -F, 'NR>1 && $4>m {m=$4} END {print m+0}' "$CSV")"

  if [ "$cache_files_max" -gt 16 ]; then
    printf 'FAIL cache file cap: %s > 16\n' "$cache_files_max"
    AUTO_FAIL=1
  else
    printf 'PASS cache file cap: max=%s\n' "$cache_files_max"
  fi
  if [ "$cache_bytes_max" -gt $((8 * 1024 * 1024)) ]; then
    printf 'FAIL cache byte cap: %s > 8 MiB\n' "$cache_bytes_max"
    AUTO_FAIL=1
  else
    printf 'PASS cache byte cap: max=%s bytes\n' "$cache_bytes_max"
  fi
  if [ "$previewd_fd_max" -ge 256 ]; then
    printf 'FAIL previewd FD reached service limit: max=%s\n' "$previewd_fd_max"
    AUTO_FAIL=1
  else
    printf 'PASS previewd FD below LimitNOFILE: max=%s\n' "$previewd_fd_max"
  fi

  check_nonmonotonic_growth 4 'previewd FD'
  check_nonmonotonic_growth 7 'previewd capture memfd'
  check_nonmonotonic_growth 9 'cosmic-comp FD'
  check_nonmonotonic_growth 12 'cosmic-comp capture memfd'
}

collect_bundle() {
  sample_row || true
  systemctl --user status "$SERVICE" --no-pager > "$RUN_DIR/previewd-status.txt" 2>&1 || true
  journalctl --user -u "$SERVICE" --since '30 minutes ago' --no-pager > "$RUN_DIR/previewd-journal.txt" 2>&1 || true
  pgrep -af tihulu-cosmic-minimized-windows > "$RUN_DIR/applet-processes.txt" 2>&1 || true
  pgrep -af tihulu-previewd > "$RUN_DIR/previewd-processes.txt" 2>&1 || true
  pgrep -af cosmic-comp > "$RUN_DIR/cosmic-comp-processes.txt" 2>&1 || true
}

preflight() {
  log "Preflight"
  [ -n "${XDG_RUNTIME_DIR:-}" ] || fail "XDG_RUNTIME_DIR is not set; run this inside the COSMIC graphical session."
  [ -n "${WAYLAND_DISPLAY:-}" ] || fail "WAYLAND_DISPLAY is not set; run this inside the COSMIC Wayland session."
  systemctl --user is-active --quiet "$SERVICE" || {
    systemctl --user status "$SERVICE" --no-pager || true
    journalctl --user -u "$SERVICE" -n 50 --no-pager || true
    fail "$SERVICE is not active."
  }
  local limit
  limit="$(systemctl --user show "$SERVICE" -p LimitNOFILE --value 2>/dev/null || true)"
  printf 'previewd service active; LimitNOFILE=%s\n' "${limit:-unknown}"
  if [ -n "$limit" ] && [ "$limit" != "256" ]; then
    warn "Expected LimitNOFILE=256, got $limit. Reinstall the current candidate before acceptance."
    AUTO_FAIL=1
  fi
  if ! journalctl --user -u "$SERVICE" -n 100 --no-pager 2>/dev/null | grep -q 'tihulu-previewd ready:'; then
    warn "No recent 'tihulu-previewd ready:' line found in the journal."
  fi
}

post_login_mode() {
  preflight
  set_phase post-login
  sample_row
  confirm post-login "After logout/login: the applet is present, grouping/restore/close work, persisted mode is correct, and previews recover when Extended is persisted and previewd is healthy."
  collect_bundle
  if [ "$MANUAL_FAIL" -eq 0 ] && [ "$AUTO_FAIL" -eq 0 ]; then
    printf '\nVERDICT: POST-LOGIN PASS\n'
    printf 'Bundle: %s\n' "$RUN_DIR"
    exit 0
  fi
  printf '\nVERDICT: POST-LOGIN FAIL\nBundle: %s\n' "$RUN_DIR"
  exit 2
}

if [ "$MODE" = "post-login" ]; then
  post_login_mode
fi

preflight
monitor_loop &
MONITOR_PID=$!
sleep 2

set_phase safe-core
printf '\nKeep Safe Core ON. Minimize two Brave windows, then test repeated hover, pin/open, exact restore, minimize again, and exact close.\n'
confirm safe-core "Safe Core shows title/icon rows only; grouping, restore, close and popup lifetime all behave correctly."

set_phase extended
printf '\nHave at least two minimized windows. Open the group popup and turn Safe Core mode OFF. Wait for thumbnails.\n'
confirm extended "Thumbnails appear for the correct windows and the popup says: Window previews are provided by tihulu-previewd. Media controls are not enabled yet."

set_phase hover
printf '\nPerform 100+ icon-to-popup hover cycles. Exercise Brave with 2, then 3, 4 and 5 minimized windows, and rapidly switch to another minimized app group.\n'
confirm hover "Hover stays cache-only from the UI perspective: no blank/mismatched previews, wrong group, stale close, popup jump or crash."

set_phase churn
printf '\nPerform at least 20 restore/minimize/close/new-window cycles while previews are enabled.\n'
confirm churn "Exact restore/close remains correct; newly minimized windows get previews and removed windows disappear from the UI/cache."

set_phase fallback
log "Stopping previewd for fallback test"
systemctl --user stop "$SERVICE"
SERVICE_STOPPED_BY_TEST=1
sleep 18
confirm fallback "With previewd stopped for >15 s, the applet remains usable with title/icon Safe Core rows and reports Extended requested but Safe Core fallback active."

set_phase recovery
log "Starting previewd for recovery test"
systemctl --user start "$SERVICE"
SERVICE_STOPPED_BY_TEST=0
sleep 18
systemctl --user is-active --quiet "$SERVICE" || { warn "previewd did not restart"; AUTO_FAIL=1; }
confirm recovery "Without restarting cosmic-panel, existing minimized windows are re-captured sequentially and thumbnails return."

set_phase restart-on-failure
log "Testing systemd Restart=on-failure"
systemctl --user kill -s SIGKILL "$SERVICE" || true
sleep 4
if systemctl --user is-active --quiet "$SERVICE"; then
  printf 'PASS systemd restart-on-failure\n'
else
  printf 'FAIL systemd restart-on-failure\n'
  AUTO_FAIL=1
fi
sleep 16
confirm restart-on-failure "After SIGKILL and automatic service restart, previews recover and the applet remains responsive."

set_phase multi-monitor
printf '\nIf two monitors are available, test windows from both monitors, group switching and exact restore. If not available, answer y to mark this environment as single-monitor and record that coverage remains pending.\n'
confirm multi-monitor "Two-monitor behavior passed, OR this machine currently has only one monitor and you accept that two-monitor coverage remains pending."

set_phase final
sleep 2
kill "$MONITOR_PID" 2>/dev/null || true
wait "$MONITOR_PID" 2>/dev/null || true
MONITOR_PID=""
collect_bundle
resource_summary
automated_checks

printf '\nManual checks:\n'
column -t -s $'\t' "$MANUAL" 2>/dev/null || cat "$MANUAL"

if [ "$MANUAL_FAIL" -eq 0 ] && [ "$AUTO_FAIL" -eq 0 ]; then
  printf '\nVERDICT: PASS CANDIDATE — live preview/runtime checks passed.\n'
  printf 'Logout/login coverage is still required before merge. After logging back in, run:\n'
  printf '  MODE=post-login bash <(curl -fsSL https://raw.githubusercontent.com/Tihulu/tihulu-cosmic-minimized-windows/feature/previewd/scripts/run-previewd-runtime-test.sh)\n'
  printf 'Bundle: %s\n' "$RUN_DIR"
  exit 0
fi

printf '\nVERDICT: FAIL — do not merge previewd.\n'
printf 'Failure bundle: %s\n' "$RUN_DIR"
exit 2
