#!/usr/bin/env bash
set -Eeuo pipefail
PREFIX="${PREFIX:-/usr}"
sudo rm -f "$PREFIX/bin/tihulu-cosmic-minimized-windows"
sudo rm -f "$PREFIX/share/applications/io.github.tihulu.MinimizedWindows.desktop"
printf 'Tihulu Minimized Windows removed.\n'
