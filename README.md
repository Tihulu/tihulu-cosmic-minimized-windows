# Tihulu Minimized Windows

<p align="center">
  A stability-first minimized-window applet for the <strong>COSMIC desktop</strong>.
</p>

<p align="center">
  <strong>Pop!_OS 24.04 · COSMIC · Wayland</strong>
</p>

<p align="center">
  <img src="docs/screenshots/media-popup.webp" alt="Tihulu Minimized Windows rich media popup" width="720">
</p>

<p align="center">
  Lightweight minimized-window handling with optional live previews and media controls through isolated helper daemons.
</p>

<p align="center">
  <a href="#install">Install</a> ·
  <a href="#features">Features</a> ·
  <a href="#architecture">Architecture</a> ·
  <a href="#stability-policy">Stability</a>
</p>

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/Tihulu/tihulu-cosmic-minimized-windows/stable/scripts/quick-install.sh | bash
```

Then open **COSMIC Settings → Desktop → Dock/Panel**, remove the stock **Minimized Windows** applet if necessary, and add **Tihulu Minimized Windows**.

## Features

- grouped minimized windows with one panel/dock icon per application
- exact-window restore and close
- live minimized-window previews in the click popup
- media metadata, artwork, playback controls and draggable seek bar
- per-player volume controls
- browser volume support through the PipeWire-Pulse app stream when MPRIS volume is ineffective
- Safe Core compact fallback mode
- horizontal and vertical COSMIC panel/dock support
- preview and media work kept outside the `cosmic-panel` process

### Media controls

<p align="center">
  <img src="docs/screenshots/media-popup.webp" alt="Media controls with artwork, playback, seek and volume" width="700">
</p>

Media integration runs in `tihulu-mediad`, keeping MPRIS/D-Bus and browser audio-stream handling outside the panel process.

### Live window preview

<p align="center">
  <img src="docs/screenshots/preview-popup.webp" alt="Live minimized-window preview popup" width="700">
</p>

The click popup can show a captured preview of the exact minimized window before you restore it. Preview capture runs in `tihulu-previewd`; if capture is unavailable for one window, that window falls back compactly without breaking the rest of the popup.

### Settings and backend status

<p align="center">
  <img src="docs/screenshots/settings.webp" alt="Tihulu Minimizer settings" width="520">
</p>

The settings popup exposes Safe Core, Media and Preview controls together with backend health information.

Clean installs use:

- **Safe Core:** OFF
- **Media:** ON
- **Preview:** ON

The normal interaction model is click/right-click with a pinned popup; there is no hover-preview path.

### Safe Core

<p align="center">
  <img src="docs/screenshots/safe-core.webp" alt="Safe Core compact minimized-window popup" width="680">
</p>

Safe Core keeps the popup compact when rich preview/media features are disabled or unavailable, while normal window restore and close remain available.

## Architecture

The panel applet is intentionally kept small. Rich features run in separate processes:

```text
tihulu-cosmic-minimized-windows
    lightweight COSMIC panel UI
            |
            +-- IPC --> tihulu-previewd
            |
            +-- IPC --> tihulu-mediad
```

`tihulu-previewd` owns preview capture work. `tihulu-mediad` owns MPRIS/D-Bus media integration and browser audio-stream volume handling. The panel process itself does not perform MPRIS or PipeWire/Pulse audio-control work.

If either optional backend is unavailable, the affected rich feature falls back independently instead of breaking the normal minimized-window popup.

## Requirements

Supported target environment:

- Pop!_OS 24.04
- COSMIC desktop
- Wayland session
- systemd user services for the optional preview/media daemons

The project is built against pinned COSMIC/libcosmic protocol revisions. Other desktop environments and older Pop!_OS releases are not currently supported.

## Stability policy

The project prioritizes panel stability over rich features. Behavior-changing releases are not promoted to `stable` on CI alone: they are tested in a real COSMIC session and must keep `cosmic-panel` stable with bounded resource usage.

In particular:

- no popup auto-reopen/watchdog churn
- preview failure falls back only for the affected window
- media-daemon failure hides media controls without breaking the window popup
- preview/media helpers use bounded IPC/resource behavior

## Development

```bash
cargo fmt --all -- --check
cargo check --all-targets
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
cargo build --release
```

## License

AGPL-3.0-only. See `LICENSE`.

This is a separate implementation for COSMIC interoperability and does not include System76's `cosmic-applet-minimize` source files.
