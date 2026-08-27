# Tihulu Minimized Windows

A stability-first minimized-window applet for the **COSMIC desktop**.

It keeps the normal minimized-window workflow lightweight while adding optional live previews and media controls through isolated helper daemons.

**Target environment:** Pop!_OS 24.04 · COSMIC · Wayland

<p align="center">
  <img src="docs/screenshots/media-popup.webp" alt="Tihulu Minimized Windows rich media popup" width="420">
</p>

## Highlights

- grouped minimized windows with one panel/dock icon per application
- exact-window restore and close
- live minimized-window previews in the click popup
- media metadata, artwork, playback controls and draggable seek bar
- per-player volume controls
- browser volume support through the PipeWire-Pulse app stream when MPRIS volume is ineffective
- Safe Core fallback mode
- manual **Reload backends** recovery
- horizontal and vertical COSMIC panel/dock support
- rich preview/media work kept outside the `cosmic-panel` process

## Screenshots

<table>
<tr>
<td width="50%" align="center">
  <img src="docs/screenshots/preview-popup.webp" alt="Live minimized-window preview popup" width="360"><br>
  <strong>Live window preview</strong><br>
  A captured preview of the exact minimized window, shown only in the click/pinned popup.
</td>
<td width="50%" align="center">
  <img src="docs/screenshots/safe-core.webp" alt="Safe Core compact minimized-window popup" width="420"><br>
  <strong>Safe Core</strong><br>
  A compact icon/title popup when rich preview and media features are disabled or unavailable.
</td>
</tr>
</table>

### Settings and backend status

<p align="center">
  <img src="docs/screenshots/settings.webp" alt="Tihulu Minimizer settings" width="380">
</p>

The settings popup exposes Safe Core, Media, Preview and the experimental Hover option, plus backend health and manual reload status.

## Default settings

Clean installs use:

- **Safe Core:** OFF
- **Media:** ON
- **Preview:** ON
- **Hover:** OFF

Hover remains experimental and disabled by default. The normal interaction model is click/right-click with a pinned popup.

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

`tihulu-previewd` owns preview capture work. `tihulu-mediad` owns MPRIS/D-Bus media integration and browser audio-stream volume handling. The panel process itself does not perform MPRIS or PipeWire/Pulse audio control work.

If either optional backend is unavailable, the affected rich feature falls back independently instead of breaking the normal minimized-window popup.

## One-line install

```bash
curl -fsSL https://raw.githubusercontent.com/Tihulu/tihulu-cosmic-minimized-windows/stable/scripts/quick-install.sh | bash
```

Then open **COSMIC Settings → Desktop → Dock/Panel**, remove the stock **Minimized Windows** applet if necessary, and add **Tihulu Minimized Windows**.

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
- Hover stays OFF by default because earlier hover-popup testing could restart `cosmic-panel`

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
