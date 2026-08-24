# Tihulu Minimized Windows

A stability-focused minimized-window applet for the COSMIC desktop, licensed **AGPL-3.0-only**.

It provides grouped minimized-window management, on-demand hover previews, per-window restore/close controls, and optional MPRIS media controls while deliberately bounding screenshot, memory, D-Bus, and artwork activity.

> **v0.3.2 validation note:** the `v0.3.2-popup-hover-browser-audio` branch is a runtime-test candidate, not the stable release. It should be promoted only after CI is green and real COSMIC testing confirms grouped previews, repeated hover, browser media controls, and FD/resource behavior.

## Why this exists

A system that motivated this project hit `Too many open files` in the COSMIC session, followed by Wayland/EGL failures and panel restart backoff. COSMIC has also had reports around minimized-window screenshot/resource growth during window churn.

The goal here is not to claim that every COSMIC compositor or panel leak is fixed. Instead, this applet keeps its own expensive work short-lived and bounded so its resource behavior can be measured independently.

## Features

- Groups minimized windows from the same application under one dock icon
- One minimized window: left click restores it directly
- Multiple minimized windows: left click opens the group selector
- Hover for about 350 ms opens the group preview
- Right click opens and pins the group preview
- Moving from the dock icon into the popup has a short grace period so the preview remains usable
- Each window card is clickable to restore that exact window
- Each window card has an **X** control to close that exact window
- Multi-window preview uses a two-column layout
- Browser previews show the current contents of that browser window
- Supports horizontal and vertical COSMIC docks/panels

### Media controls

When the selected application exposes an MPRIS player, such as Spotify or other compatible media applications, the preview can show:

- album artwork
- track title and artist
- previous / play-pause / next
- playback progress and elapsed/total time
- volume down / mute / volume up

MPRIS integration is optional. If no matching player exists, the normal window previews continue to work.

## Resource-safety design

Resource lifetime is intentionally conservative because avoiding panel/session exhaustion is a primary design goal.

### Window previews

- no screenshot capture on minimize
- no background thumbnail polling
- no persistent thumbnail cache
- at most **one screencopy capture in flight at a time**
- live screenshot memory capped at **8 thumbnails per open group popup**
- additional windows remain selectable using icon/title fallback
- preview captures have a **2 second timeout**
- `wl_buffer`, `wl_shm_pool`, screencopy frame/session, mmap, and memfd resources are released after each bounded capture
- closing the popup immediately clears all retained preview image handles and pending preview state
- stale capture results are discarded instead of becoming a long-lived cache

### Media preview

- no always-running MPRIS watcher
- full MPRIS metadata is queried only when a relevant popup opens or immediately after a media-control action
- while a media popup is visible, a bounded 1 Hz check reads only `PlaybackStatus` so the play/pause icon follows external player changes; it does not capture screenshots, query PipeWire, reload artwork, or refresh full metadata
- audio commands resolve the live PipeWire/PulseAudio stream at click time rather than retaining Chromium sink-input IDs
- volume adjustments are applied relative to the live stream volume at click time
- only one album-art image is retained for the currently open popup
- remote/local album art is capped at **2 MiB** before decode and resized to at most **144 px**
- album-art network/file operations have timeouts
- media state and artwork are dropped when the popup closes

These limits are intended to prevent the applet from recreating the unbounded `memfd`/screencopy-style accumulation that motivated the project. Real-session testing is still important, especially across different COSMIC and GPU-driver versions.

## One-line install

```bash
curl -fsSL https://raw.githubusercontent.com/Tihulu/tihulu-cosmic-minimized-windows/stable/scripts/quick-install.sh | bash
```

Then open **COSMIC Settings → Desktop → Dock** (or Panel), remove the stock **Minimized Windows** applet, and add **Tihulu Minimized Windows**.

## Verify resource behavior

Because COSMIC can have more than one `cosmic-panel` process, inspect all of them rather than only the first PID:

```bash
for pid in $(pgrep -x cosmic-panel); do
  printf 'PID=%-8s FD=%s\n' "$pid" "$(ls /proc/$pid/fd 2>/dev/null | wc -l)"
done
```

Continuous view:

```bash
watch -n 2 '
for pid in $(pgrep -x cosmic-panel); do
  printf "panel PID=%-8s FD=%s\n" "$pid" "$(ls /proc/$pid/fd 2>/dev/null | wc -l)"
done
pid=$(pgrep -f "/tihulu-cosmic-minimized-windows$" | head -1)
[ -n "$pid" ] && printf "applet PID=%-8s FD=%s\n" "$pid" "$(ls /proc/$pid/fd 2>/dev/null | wc -l)"
'
```

Small temporary fluctuations while a preview is captured are expected. A monotonic increase tied to every hover/minimize/restore cycle is not.

A useful stress test is to repeatedly minimize/restore windows and open/close hover previews while watching both the applet and all panel FD counts.

## Development

```bash
cargo check
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
```

The repository pins the COSMIC client-toolkit revision used by the pinned `libcosmic` revision to avoid duplicate/incompatible Wayland protocol types.

## License

AGPL-3.0-only. See `LICENSE`.

This is a separate implementation for COSMIC interoperability and does not include System76's `cosmic-applet-minimize` source files.

## Related upstream work

- `pop-os/cosmic-applets`
- `cosmic-applet-minimize`
- COSMIC resource-leak reports around minimized-window handling
