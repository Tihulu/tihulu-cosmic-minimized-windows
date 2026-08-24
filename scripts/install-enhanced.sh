#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
set -Eeuo pipefail

REPO="${REPO:-Tihulu/tihulu-cosmic-minimized-windows}"
REF="${REF:-v0.5-safe-switch-daemons}"
BUILD_DIR=""
LOCAL_BIN="${HOME}/.local/bin"
USER_SYSTEMD="${HOME}/.config/systemd/user"

log() { printf '\n==> %s\n' "$*"; }
need() { command -v "$1" >/dev/null 2>&1; }

cleanup() {
  if [ -n "$BUILD_DIR" ]; then
    rm -rf -- "$BUILD_DIR"
  fi
}
trap cleanup EXIT

if [ -f "$HOME/.cargo/env" ]; then
  # shellcheck disable=SC1090
  source "$HOME/.cargo/env"
fi

if need apt-get; then
  log "Installing runtime/build dependencies"
  sudo apt-get update
  sudo apt-get install -y \
    build-essential cmake curl git playerctl pulseaudio-utils \
    libegl1-mesa-dev libexpat1-dev libfontconfig-dev libfreetype-dev \
    libwayland-dev libxkbcommon-dev pkgconf
fi

if ! need cargo; then
  log "Installing Rust with rustup"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
  # shellcheck disable=SC1090
  source "$HOME/.cargo/env"
fi

BUILD_DIR="$(mktemp -d -t tihulu-minimized-enhanced.XXXXXX)"
log "Downloading source: $REF"
curl -fsSL "https://github.com/${REPO}/archive/${REF}.tar.gz" \
  | tar -xz -C "$BUILD_DIR" --strip-components=1
cd "$BUILD_DIR"

log "Building isolated helpers"
cargo build --release --bin tihulu-mediad --bin tihulu-preview-probe

install -d "$LOCAL_BIN" "$USER_SYSTEMD"
install -m0755 target/release/tihulu-mediad "$LOCAL_BIN/tihulu-mediad"
install -m0755 target/release/tihulu-preview-probe "$LOCAL_BIN/tihulu-preview-probe"
install -m0644 resources/systemd/user/tihulu-mediad.service \
  "$USER_SYSTEMD/tihulu-mediad.service"

log "Enabling media helper"
systemctl --user daemon-reload
systemctl --user enable --now tihulu-mediad.service

cat <<'EOF'

Enhanced helpers are installed, but Safe Mode remains the applet default.

- Media helper: tihulu-mediad.service
- Preview safety probe: ~/.local/bin/tihulu-preview-probe

Do NOT enable live previews yet. First run the bounded probe while a Brave window is minimized:

  ~/.local/bin/tihulu-preview-probe --app brave --captures 64

The probe stops early if cosmic-comp FD/RSS growth looks unsafe and writes:
  $XDG_RUNTIME_DIR/tihulu-minimized-windows/preview-probe.csv

Switch to Enhanced mode only from the applet after the helper tests are satisfactory.
EOF
