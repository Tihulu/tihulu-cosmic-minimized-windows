#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
set -Eeuo pipefail

REPO="${REPO:-Tihulu/tihulu-cosmic-minimized-windows}"
REF="${REF:-stable}"
PREFIX="${PREFIX:-/usr}"
BIN="tihulu-cosmic-minimized-windows"
PREVIEW_BIN="tihulu-previewd"
PREVIEW_SERVICE="tihulu-previewd.service"
MEDIA_BIN="tihulu-mediad"
MEDIA_SERVICE="tihulu-mediad.service"
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

enable_daemons() {
  if ! need systemctl; then
    warn "systemctl not found; preview/media daemons were installed but user services were not enabled."
    return
  fi

  local env_names=(XDG_RUNTIME_DIR DBUS_SESSION_BUS_ADDRESS)
  if [ -n "${WAYLAND_DISPLAY:-}" ]; then
    env_names+=(WAYLAND_DISPLAY)
  fi
  if [ -n "${DISPLAY:-}" ]; then
    env_names+=(DISPLAY)
  fi

  log "Enabling preview/media daemon user services"
  systemctl --user import-environment "${env_names[@]}" >/dev/null 2>&1 || true
  systemctl --user daemon-reload

  if ! systemctl --user enable --now "$PREVIEW_SERVICE"; then
    warn "$PREVIEW_SERVICE could not be started. Preview will stay unavailable until previewd is healthy."
  fi
  if ! systemctl --user enable --now "$MEDIA_SERVICE"; then
    warn "$MEDIA_SERVICE could not be started. Media controls will stay unavailable until mediad is healthy."
  fi

  if systemctl --user is-active --quiet "$PREVIEW_SERVICE"; then
    printf 'previewd user service is active.\n'
  else
    warn "$PREVIEW_SERVICE is not active."
  fi
  if systemctl --user is-active --quiet "$MEDIA_SERVICE"; then
    printf 'mediad user service is active.\n'
  else
    warn "$MEDIA_SERVICE is not active."
  fi
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
  cargo check --all-targets

  log "Building release binaries"
  cargo build --release --bin "$BIN" --bin "$PREVIEW_BIN" --bin "$MEDIA_BIN"

  log "Installing"
  sudo install -Dm0755 "target/release/$BIN" "$PREFIX/bin/$BIN"
  sudo install -Dm0755 "target/release/$PREVIEW_BIN" "$PREFIX/bin/$PREVIEW_BIN"
  sudo install -Dm0755 "target/release/$MEDIA_BIN" "$PREFIX/bin/$MEDIA_BIN"
  sudo install -Dm0644 "resources/$APP_ID.desktop" \
    "$PREFIX/share/applications/$APP_ID.desktop"
  sudo install -Dm0644 "resources/icons/$APP_ID.svg" \
    "$PREFIX/share/icons/hicolor/scalable/apps/$APP_ID.svg"
  sudo install -Dm0644 "resources/icons/$APP_ID-symbolic.svg" \
    "$PREFIX/share/icons/hicolor/symbolic/apps/$APP_ID-symbolic.svg"
  sudo install -Dm0644 "resources/systemd/$PREVIEW_SERVICE" \
    "$PREFIX/lib/systemd/user/$PREVIEW_SERVICE"
  sudo install -Dm0644 "resources/systemd/$MEDIA_SERVICE" \
    "$PREFIX/lib/systemd/user/$MEDIA_SERVICE"

  if need update-desktop-database; then
    sudo update-desktop-database "$PREFIX/share/applications" >/dev/null 2>&1 || true
  fi
  if need gtk-update-icon-cache; then
    sudo gtk-update-icon-cache -f -t "$PREFIX/share/icons/hicolor" >/dev/null 2>&1 || true
  fi

  enable_daemons

  printf '\nTihulu Minimized Windows installed.\n'
  printf 'In COSMIC Settings → Desktop → Dock/Panel, remove the old applet instance and add “Tihulu Minimized Windows” again.\n'
  printf 'Preview activates when Safe Core is off, Preview is on, and tihulu-previewd is healthy.\n'
  printf 'Media controls activate when Safe Core is off, Media is on, and tihulu-mediad finds an MPRIS player.\n'
}

main "$@"
