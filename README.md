# Tihulu Minimized Windows

A stability-first minimized-window applet for the COSMIC desktop, licensed **AGPL-3.0-only**.

The project exists because the stock/rich-preview paths in COSMIC have had real-world file-descriptor and shared-memory leak reports. The primary rule here is simple: **the panel applet must remain useful even when every optional rich feature is disabled.**

## Current v0.5 architecture

### Safe Mode — default

Safe Mode is the permanent fallback and contains only the small window-management core:

- tracks minimized COSMIC toplevels
- groups windows by application, including browser aliases
- one dock icon per group with a window count
- single-window click restores directly
- multi-window popup lists every window by icon/title
- restore an exact window
- close an exact window
- no screencopy
- no preview memfd buffers
- no MPRIS work
- no `pactl`
- no artwork/network access

The Safe/Enhanced choice is persisted in:

```text
~/.config/tihulu-cosmic-minimized-windows/mode
```

Safe Mode remains available even after enhanced helpers are installed.

### Hover reliability

The applet no longer guesses a popup anchor from `icon_index × icon_width`. That estimate was wrong when a group badge such as Brave `[2]`, `[3]`, or `[5]` changed the real widget width.

v0.5 uses libcosmic's `RectangleTracker` to anchor the popup to the **actual rendered dock widget rectangle**. A fresh hover can also re-arm a stale unpinned popup surface without starting any media or capture work.

### Enhanced Mode — isolated helpers

Enhanced features are kept outside `cosmic-panel` / the applet process.

`Tihulu applet → Unix socket → tihulu-mediad`

`tihulu-preview-probe / future tihulu-previewd → separate Wayland connection`

The applet itself does not run screencopy, `playerctl`, `pactl`, HTTP artwork fetches, or image decoding in its Safe core.

## Media helper

`tihulu-mediad` is an independent user process. It communicates over:

```text
$XDG_RUNTIME_DIR/tihulu-minimized-windows/media.sock
```

Safety behavior:

- requests are rejected while the applet is in Safe Mode
- short request/process timeouts
- subprocess output capped at 2 MiB
- self FD guard
- systemd `LimitNOFILE=128`
- systemd `MemoryMax=192M`
- systemd `TasksMax=32`
- restart-on-failure is isolated from the panel

Playback commands do not trust a cached play/pause icon. At click time the helper reads the real MPRIS state and sends explicit `play` or `pause`.

Browser volume/mute commands resolve live PulseAudio/PipeWire sink inputs again at command time and apply relative `+5%` / `-5%` or `mute toggle`; no stale popup volume is used to calculate a target percentage.

## Window preview safety probe

The old `cctk::screencopy` thumbnail path is not enabled in v0.5.

Instead, `tihulu-preview-probe` tests the newer Wayland `ext-image-copy-capture-v1` + foreign-toplevel image-capture-source path in a **separate process** before we trust it for real thumbnails.

The probe records after every capture:

- `cosmic-comp` FD count
- `cosmic-comp` RSS
- probe FD count

CSV output:

```text
$XDG_RUNTIME_DIR/tihulu-minimized-windows/preview-probe.csv
```

It has a circuit breaker. If `cosmic-comp` FDs grow monotonically or RSS jumps together with FD growth, the run stops early instead of blindly doing hundreds of captures.

**Live thumbnails will not be enabled by default until this path stays bounded in real COSMIC sessions.**

## Install the applet

Stable release:

```bash
curl -fsSL https://raw.githubusercontent.com/Tihulu/tihulu-cosmic-minimized-windows/stable/scripts/quick-install.sh | bash
```

For the current v0.5 test branch:

```bash
curl -fsSL https://raw.githubusercontent.com/Tihulu/tihulu-cosmic-minimized-windows/v0.5-safe-switch-daemons/scripts/quick-install.sh \
  | REF=v0.5-safe-switch-daemons bash
```

Then open **COSMIC Settings → Desktop → Dock/Panel**, remove the stock **Minimized Windows** applet and add **Tihulu Minimized Windows**.

A logout/login is preferred after replacing a dock applet. Do not manually launch `cosmic-panel` with inherited session notification FDs.

## Install enhanced helpers

Enhanced helpers are opt-in:

```bash
curl -fsSL https://raw.githubusercontent.com/Tihulu/tihulu-cosmic-minimized-windows/v0.5-safe-switch-daemons/scripts/install-enhanced.sh \
  | REF=v0.5-safe-switch-daemons bash
```

This installs `playerctl`, PulseAudio compatibility tools, `tihulu-mediad`, the systemd user service, and `tihulu-preview-probe`.

It **does not turn Safe Mode off automatically** and does not enable live thumbnails.

Media helper status:

```bash
systemctl --user status tihulu-mediad.service
```

## Run the bounded preview probe

Minimize one Brave window first, then:

```bash
~/.local/bin/tihulu-preview-probe --app brave --captures 64
```

Or from a checkout:

```bash
./scripts/run-preview-probe.sh brave 64
```

A good result is bounded/oscillating resource use. A pattern such as:

```text
92 → 93 → 92 → 93
```

is qualitatively different from:

```text
92 → 93 → 94 → 95 → 96
```

The latter must keep previews disabled.

## Monitor the real session

```bash
watch -n 2 '
echo "=== APPLET ==="
pid=$(pgrep -x tihulu-cosmic-minimized-windows | head -1)
[ -n "$pid" ] && echo "PID=$pid FD=$(ls /proc/$pid/fd 2>/dev/null | wc -l)"

echo
echo "=== PANEL ==="
for pid in $(pgrep -x cosmic-panel); do
  echo "PID=$pid FD=$(ls /proc/$pid/fd 2>/dev/null | wc -l)"
done

echo
echo "=== COMPOSITOR ==="
pid=$(pgrep -x cosmic-comp | head -1)
[ -n "$pid" ] && echo "PID=$pid FD=$(ls /proc/$pid/fd 2>/dev/null | wc -l)"
'
```

Small fluctuations are normal. A count that rises with every hover/capture and never returns is not.

## Development

```bash
cargo fmt --all -- --check
cargo check --all-targets
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
```

The repository pins the COSMIC client toolkit and libcosmic revisions to avoid incompatible Wayland protocol types.

## License

AGPL-3.0-only. See `LICENSE`.

This is a separate implementation for COSMIC interoperability and does not copy System76's `cosmic-applet-minimize` source.
