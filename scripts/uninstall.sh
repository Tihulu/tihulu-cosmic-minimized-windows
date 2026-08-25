#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
set -Eeuo pipefail
PREFIX="${PREFIX:-/usr}"
APP_ID="io.github.tihulu.MinimizedWindows"
sudo rm -f "$PREFIX/bin/tihulu-cosmic-minimized-windows"
sudo rm -f "$PREFIX/share/applications/$APP_ID.desktop"
sudo rm -f "$PREFIX/share/icons/hicolor/scalable/apps/$APP_ID.svg"
sudo rm -f "$PREFIX/share/icons/hicolor/symbolic/apps/$APP_ID-symbolic.svg"
if command -v update-desktop-database >/dev/null 2>&1; then
  sudo update-desktop-database "$PREFIX/share/applications" >/dev/null 2>&1 || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  sudo gtk-update-icon-cache -f -t "$PREFIX/share/icons/hicolor" >/dev/null 2>&1 || true
fi
printf 'Tihulu Minimized Windows removed.\n'
