# Tihulu Minimized Windows

A stability-first minimized-window applet for the COSMIC desktop, licensed **AGPL-3.0-only**.

The project keeps window management in a small Safe Core and treats previews/media as optional external subsystems. This is intentional: historical minimize-preview paths could leak compositor-side screenshot resources, so rich features are not allowed to make the panel applet unstable.

## v0.4 Safe Core

The Safe Core provides:

- minimized-toplevel tracking on Wayland
- grouping by application, including browser aliases
- one dock/panel icon per group with a window count
- exact-window restore and close
- lightweight icon/title group popup
- immediate hover with a 650 ms icon-to-popup leave grace
- click/right-click pinning for multi-window groups
- horizontal and vertical COSMIC panel/dock support
- an explicit persisted Safe Core/Extended preference

Safe Core performs no screenshot capture, owns no preview memfds, and does not depend on preview/media daemons for grouping, restore, or close.

The setting is stored under the user's XDG config directory as:

```text
~/.config/tihulu-cosmic-minimized-windows/config
```

## Preview daemon runtime candidate

The `feature/previewd` branch contains the first external `tihulu-previewd` runtime candidate.

The panel applet does **not** perform Wayland image capture itself. Capture is isolated in the per-user daemon and uses `ext-image-copy-capture` plus the foreign-toplevel image capture source. The applet communicates with the daemon over a versioned Unix socket under `XDG_RUNTIME_DIR`.

The isolated COSMIC probe passed the integration gate before this branch was created:

- 500/500 captures succeeded
- probe FD: `11 -> 11`
- `cosmic-comp` FD: `374 -> 370`, bounded with maximum 374
- `cosmic-comp` memfd: `69 -> 69`
- `cosmic-comp` capture-related memfd: `0 -> 0`
- compositor RSS/shared-memory measurements remained bounded

That probe result permits runtime testing of the daemon architecture; it does **not** by itself approve a stable release.

### Preview safety invariants

The current candidate enforces:

- one capture request in flight through the applet capture gate
- serial capture processing in `tihulu-previewd`
- capture on minimize / Extended-mode recovery, never continuously on hover
- hover reads only the bounded preview cache already held by the applet
- 320x180 thumbnail target
- maximum 16 cached daemon thumbnails
- maximum 8 MiB daemon thumbnail cache
- LRU eviction
- maximum 16 applet preview handles
- 64 MiB hard limit for a full-size capture buffer before allocation
- in-place raw-to-RGBA conversion to avoid a second full-size image allocation
- immediate destruction of Wayland frame/buffer/pool/session objects after capture
- daemon/compositor FD and capture-memfd growth watchdog
- 128 MiB daemon RSS growth circuit breaker
- degraded daemon state clears preview cache and stops further capture
- 15-second health checks in Extended mode
- automatic Safe Core fallback if the daemon is unavailable or degraded
- automatic re-capture/recovery when the daemon becomes healthy again
- systemd user service with `LimitNOFILE=256`, `NoNewPrivileges=true`, and `UMask=0077`

If previewd fails, grouping, restore, close, titles and icons remain available through Safe Core.

## Popup lifetime safety

Popup lifetime is controlled by an explicit FSM:

```text
Closed
HoverOpen(group, window_id, generation)
Pinned(group, window_id, generation)
```

Delayed close requests carry both a popup generation and close token. Stale timers cannot close a newer popup, and stale compositor-close events for an old `WindowId` are ignored. Switching to a different application group deliberately replaces/reanchors the popup surface.

## Media status

Media controls are **not** part of this preview candidate. `tihulu-mediad`/MPRIS/PipeWire work remains a later stage and will not be added until previewd passes the full real-runtime acceptance test.

## Install

Stable installer:

```bash
curl -fsSL https://raw.githubusercontent.com/Tihulu/tihulu-cosmic-minimized-windows/stable/scripts/quick-install.sh | bash
```

Previewd runtime candidate:

```bash
REF=feature/previewd bash <(curl -fsSL https://raw.githubusercontent.com/Tihulu/tihulu-cosmic-minimized-windows/feature/previewd/scripts/quick-install.sh)
```

The candidate installer builds and installs both `tihulu-cosmic-minimized-windows` and `tihulu-previewd`, installs the systemd user unit, reloads user units, and attempts to enable/start `tihulu-previewd.service`.

Then open **COSMIC Settings -> Desktop -> Dock/Panel**, remove the previous applet instance, and add **Tihulu Minimized Windows** again.

Check the daemon with:

```bash
systemctl --user is-active tihulu-previewd.service
systemctl --user status tihulu-previewd.service --no-pager
```

## Using previews

Safe Core is the default. Minimize at least two windows from an application, open the group popup, then turn **Safe Core mode** off. This requests Extended mode.

When previewd is healthy, the popup note changes to:

```text
Window previews are provided by tihulu-previewd. Media controls are not enabled yet.
```

Existing minimized windows are captured sequentially when Extended mode is enabled. Newly minimized windows are captured once when they enter the minimized set. Repeated hover does not request fresh captures.

If previewd becomes unavailable/degraded, the popup falls back to title/icon rows and reports that Safe Core fallback is active.

## Runtime acceptance

CI proves build/test/lint correctness, not COSMIC runtime safety. The preview branch must pass a second real Pop!_OS/COSMIC acceptance run before merge or promotion.

Use the resource watcher while testing:

```bash
bash <(curl -fsSL https://raw.githubusercontent.com/Tihulu/tihulu-cosmic-minimized-windows/feature/previewd/scripts/watch-previewd-resources.sh)
```

It reports applet, previewd, `cosmic-panel`, and `cosmic-comp` FD/RSS/shared-memory/memfd counts plus the preview cache size.

The required functional/resource test is documented in [`docs/PREVIEWD_RUNTIME_ACCEPTANCE.md`](docs/PREVIEWD_RUNTIME_ACCEPTANCE.md).

A bounded pattern such as `92 -> 93 -> 92 -> 93` is acceptable. Monotonic FD/capture-memfd growth such as `92 -> 93 -> 94 -> 95` is a failure. Do not merge the preview branch if the compositor or daemon shows monotonic resource growth, if fallback fails, or if hover causes repeated capture.

## Development

```bash
cargo fmt --all -- --check
cargo check --all-targets
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
cargo build --release
```

The repository pins the COSMIC client-toolkit revision used by the pinned `libcosmic` revision to avoid duplicate/incompatible Wayland protocol types.

Experimental rich-feature work stays on separate branches/PRs. Runtime-untested rich features are not merged into stable.

## License

AGPL-3.0-only. See `LICENSE`.

This is a separate implementation for COSMIC interoperability and does not include System76's `cosmic-applet-minimize` source files.

## Related upstream work

- `pop-os/cosmic-applets`
- `pop-os/cosmic-comp` issue #2073
- COSMIC foreign-toplevel and image-copy-capture protocols
