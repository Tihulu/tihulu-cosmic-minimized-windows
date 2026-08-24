#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
set -Eeuo pipefail

APP_FILTER="${1:-brave}"
CAPTURES="${2:-64}"
PROBE="${HOME}/.local/bin/tihulu-preview-probe"

if [ ! -x "$PROBE" ]; then
  echo "tihulu-preview-probe is not installed. Run scripts/install-enhanced.sh first." >&2
  exit 1
fi

COMP_PID="$(pgrep -x cosmic-comp | head -1 || true)"
if [ -z "$COMP_PID" ]; then
  echo "cosmic-comp is not running." >&2
  exit 1
fi

printf 'Before: cosmic-comp PID=%s FD=%s\n' \
  "$COMP_PID" "$(find "/proc/$COMP_PID/fd" -mindepth 1 -maxdepth 1 2>/dev/null | wc -l)"
printf 'Minimize a matching %s window, then leave it minimized during the probe.\n\n' "$APP_FILTER"

exec "$PROBE" --app "$APP_FILTER" --captures "$CAPTURES"
