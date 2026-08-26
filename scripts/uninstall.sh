#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
set -Eeuo pipefail

PREFIX="${PREFIX:-/usr}"
APP_ID="io.github.tihulu.MinimizedWindows"
PREVIEW_SERVICE="tihulu-previewd.service"

if command -v systemctl >/dev/null 2>&1; then
  systemctl --user disable --now "$PREVIEW_SERVICE" >/dev/null 2>&1 || true
fi

sudo rm -f "$PREFIX/bin/tihulu-cosmic-minimized-windows"
sudo rm -f "$PREFIX/bin/tihulu-previewd"
sudo rm -f "$PREFIX/share/applications/$APP_ID.desktop"
sudo rm -f "$PREFIX/share/icons/hicolor/scalable/apps/$APP_ID.svg"
sudo rm -f "$PREFIX/share/icons/hicolor/symbolic/apps/$APP_ID-symbolic.svg"
sudo rm -f "$PREFIX/lib/systemd/user/$PREVIEW_SERVICE"

if command -v systemctl >/dev/null 2>&1; then
  systemctl --user daemon-reload >/dev/null 2>&1 || true
  systemctl --user reset-failed "$PREVIEW_SERVICE" >/dev/null 2>&1 || true
fi

if [ -n "${XDG_RUNTIME_DIR:-}" ]; then
  rm -rf -- "$XDG_RUNTIME_DIR/tihulu-cosmic-minimized-windows"
fi

if command -v update-desktop-database >/dev/null 2>&1; then
  sudo update-desktop-database "$PREFIX/share/applications" >/dev/null 2>&1 || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  sudo gtk-update-icon-cache -f -t "$PREFIX/share/icons/hicolor" >/dev/null 2>&1 || true
fi

printf 'Tihulu Minimized Windows and tihulu-previewd removed.\n'
