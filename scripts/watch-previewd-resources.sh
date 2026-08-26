#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
set -u

INTERVAL="${INTERVAL:-2}"
ONCE="${ONCE:-0}"
SERVICE="tihulu-previewd.service"
RUNTIME_ROOT="${XDG_RUNTIME_DIR:-}/tihulu-cosmic-minimized-windows"

proc_row() {
  local label="$1" pid="$2" status="/proc/$2/status"
  [ -r "$status" ] || return 0

  local fd rss shmem memfd=0 capture_memfd=0 target entry
  fd="$(ls -1 "/proc/$pid/fd" 2>/dev/null | wc -l)"
  rss="$(awk '$1 == "VmRSS:" { print $2; exit }' "$status" 2>/dev/null)"
  shmem="$(awk '$1 == "RssShmem:" { print $2; exit }' "$status" 2>/dev/null)"
  rss="${rss:-0}"
  shmem="${shmem:-0}"

  for entry in "/proc/$pid/fd"/*; do
    target="$(readlink "$entry" 2>/dev/null || true)"
    case "$target" in
      /memfd:*|memfd:*)
        memfd=$((memfd + 1))
        case "$target" in
          *capture*|*screencopy*|*minimize-applet*)
            capture_memfd=$((capture_memfd + 1))
            ;;
        esac
        ;;
    esac
  done

  printf '%-12s %-8s %-6s %-10s %-10s %-7s %-12s\n' \
    "$label" "$pid" "$fd" "$rss" "$shmem" "$memfd" "$capture_memfd"
}

show_snapshot() {
  if [ -t 1 ]; then
    printf '\033[H\033[2J'
  fi

  printf 'Tihulu preview runtime resources — %s\n' "$(date '+%F %T %Z')"
  if command -v systemctl >/dev/null 2>&1; then
    printf 'previewd service: %s\n' "$(systemctl --user is-active "$SERVICE" 2>/dev/null || true)"
  fi
  printf '\n%-12s %-8s %-6s %-10s %-10s %-7s %-12s\n' \
    'PROCESS' 'PID' 'FD' 'RSS_KB' 'SHMEM_KB' 'MEMFD' 'CAPTURE_MEMFD'

  local pid found=0
  while read -r pid; do
    [ -n "$pid" ] || continue
    proc_row 'applet' "$pid"
    found=1
  done < <(pgrep -f '(^|/)tihulu-cosmic-minimized-windows([[:space:]]|$)' 2>/dev/null || true)

  while read -r pid; do
    [ -n "$pid" ] || continue
    proc_row 'previewd' "$pid"
    found=1
  done < <(pgrep -x tihulu-previewd 2>/dev/null || true)

  while read -r pid; do
    [ -n "$pid" ] || continue
    proc_row 'panel' "$pid"
    found=1
  done < <(pgrep -x cosmic-panel 2>/dev/null || true)

  while read -r pid; do
    [ -n "$pid" ] || continue
    proc_row 'comp' "$pid"
    found=1
  done < <(pgrep -x cosmic-comp 2>/dev/null || true)

  if [ "$found" -eq 0 ]; then
    printf '(no matching processes found)\n'
  fi

  if [ -n "${XDG_RUNTIME_DIR:-}" ] && [ -d "$RUNTIME_ROOT/previews" ]; then
    local cache_count cache_bytes
    cache_count="$(find "$RUNTIME_ROOT/previews" -maxdepth 1 -type f -name '*.rgba' 2>/dev/null | wc -l)"
    cache_bytes="$(du -sb "$RUNTIME_ROOT/previews" 2>/dev/null | awk '{print $1}')"
    printf '\npreview cache: files=%s bytes=%s (hard limits: 16 files / 8 MiB)\n' \
      "$cache_count" "${cache_bytes:-0}"
  fi

  printf '\nWatch for bounded oscillation. Monotonic FD/memfd growth is a FAIL.\n'
}

while :; do
  show_snapshot
  [ "$ONCE" = "1" ] && break
  sleep "$INTERVAL"
done
