#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
set -Eeuo pipefail

REPO="${REPO:-Tihulu/tihulu-cosmic-minimized-windows}"
REF="${REF:-stable}"
PREFIX="${PREFIX:-/usr}"
BIN="tihulu-cosmic-minimized-windows"
APP_ID="io.github.tihulu.MinimizedWindows"
BUILD_DIR=""

if [ -f "$HOME/.cargo/env" ]; then
  # shellcheck disable=SC1090
  source "$HOME/.cargo/env"
fi

log() { printf '\n==> %s\n' "$*"; }
warn() { printf '\nWARN: %s\n' "$*" >&2; }
need() { command -v "$1" >/dev/null 2>&1; }

cleanup() {
  if [ -n "$BUILD_DIR" ]; then
    rm -rf -- "$BUILD_DIR"
  fi
}
trap cleanup EXIT

install_deps() {
  if ! need apt-get; then
    warn "apt-get not found. Install the COSMIC/libcosmic build dependencies manually."
    return
  fi
  log "Installing build dependencies"
  sudo apt-get update
  sudo apt-get install -y \
    build-essential cmake curl git \
    libegl1-mesa-dev libexpat1-dev libfontconfig-dev libfreetype-dev \
    libwayland-dev libxkbcommon-dev pkgconf
}

rust_is_new_enough() {
  need rustc || return 1
  local v major minor
  v="$(rustc --version | awk '{print $2}')"
  major="${v%%.*}"
  v="${v#*.}"
  minor="${v%%.*}"
  [ "$major" -gt 1 ] || { [ "$major" -eq 1 ] && [ "$minor" -ge 93 ]; }
}

ensure_rust() {
  if need cargo && rust_is_new_enough; then
    return
  fi
  if need rustup; then
    log "Updating Rust stable toolchain"
    rustup toolchain install stable
    rustup default stable
  else
    log "Installing Rust with rustup"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
  fi
  # shellcheck disable=SC1090
  source "$HOME/.cargo/env"
}

main() {
  install_deps
  ensure_rust

  BUILD_DIR="$(mktemp -d -t tihulu-minimized.XXXXXX)"
  log "Downloading verified source: $REF"
  curl -fsSL "https://github.com/${REPO}/archive/${REF}.tar.gz" \
    | tar -xz -C "$BUILD_DIR" --strip-components=1
  cd "$BUILD_DIR"

  log "Checking source"
  cargo check

  log "Building release binary"
  cargo build --release

  log "Installing"
  sudo install -Dm0755 "target/release/$BIN" "$PREFIX/bin/$BIN"
  sudo install -Dm0644 "resources/$APP_ID.desktop" \
    "$PREFIX/share/applications/$APP_ID.desktop"

  if need update-desktop-database; then
    sudo update-desktop-database "$PREFIX/share/applications" >/dev/null 2>&1 || true
  fi

  printf '\nTihulu Minimized Windows installed.\n'
  printf 'In COSMIC Settings → Desktop → Dock/Panel, remove the stock “Minimized Windows” and add “Tihulu Minimized Windows”.\n'
}

main "$@"
