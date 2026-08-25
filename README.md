# Tihulu Minimized Windows

A stability-first minimized-window applet for the COSMIC desktop, licensed **AGPL-3.0-only**.

The project exists because minimized-window preview paths can be dangerous when compositor-side screenshot resources leak. The applet therefore treats window management as the always-available core and makes every future rich feature optional.

## v0.4 Safe Core

The v0.4 release-candidate branch intentionally contains **no screenshot or media implementation in the panel applet**.

It provides:

- minimized-toplevel tracking on Wayland
- grouping by application, including browser aliases
- one dock/panel icon per group with a window count
- exact-window restore
- exact-window close
- lightweight icon/title group popup
- immediate hover with a 650 ms icon-to-popup leave grace
- click/right-click pinning for multi-window groups
- horizontal and vertical COSMIC panel/dock support
- a persisted **Safe Core mode** switch

Safe Core means:

- no screencopy
- no `wl_shm` thumbnail buffers
- no preview memfds
- no image decoding/cache
- no album art
- no MPRIS or audio control work
- no HTTP artwork fetches
- no preview/media background daemons required for grouping, restore, or close

The default requested mode is Safe Core. Selecting Extended mode only permits optional rich subsystems in future builds; if those subsystems are unavailable or degraded, the applet remains in effective Safe Core operation.

The setting is stored under the user's XDG config directory as:

```text
~/.config/tihulu-cosmic-minimized-windows/config
```

## Popup lifetime safety

v0.4 uses an explicit popup state machine instead of loosely coupled popup booleans.

Logical states are:

```text
Closed
HoverOpen(group, window_id, generation)
Pinned(group, window_id, generation)
```

Delayed close requests carry both the popup generation and a close token. A stale timer cannot close a newer popup. A compositor close event for an old `WindowId` is ignored after a newer popup generation has replaced it.

When hover changes to a different application group, the old popup surface is deliberately replaced so the new surface receives the correct anchor rectangle. Hovering/pinning the same group reuses the current surface.

The FSM has regression tests for stale close timers, generation replacement, compositor close races, pinning, and group switching.

## Why previews are not in the applet

Historical COSMIC minimize-preview failures included compositor-side `/memfd:minimize-applet-screencopy` growth. Moving the same capture request into another process does not automatically make that compositor-side leak safe.

The planned rich-preview architecture is therefore:

```text
tihulu-cosmic-minimized-windows
    lightweight panel UI
    Safe Core always works
            |
            +-- optional IPC --> tihulu-previewd
            |
            +-- optional IPC --> tihulu-mediad
```

The panel applet itself will not own screenshot buffers, MPRIS sessions, PipeWire stream handling, artwork downloads, or rich-image caches.

## Preview safety gate

Before `tihulu-previewd` is integrated, the repository will use a standalone `tihulu-preview-probe` to stress-test COSMIC's newer toplevel image-capture path based on `ext-image-copy-capture` plus a foreign-toplevel capture source.

The probe must show bounded resource behavior during repeated capture. At minimum, testing should record:

- probe FD count
- probe RSS
- `cosmic-comp` FD count
- `cosmic-comp` RSS
- `cosmic-comp` shared memory when useful
- relevant compositor memfd counts

A bounded pattern such as `92 -> 93 -> 92 -> 93` is acceptable. Monotonic growth such as `92 -> 93 -> 94 -> 95` means that capture method is rejected and is **not** integrated into the applet.

Even after a capture path passes the probe, a future preview daemon must keep one capture in flight globally, use a bounded thumbnail cache, and contain a circuit breaker that disables capture for the session if resource growth becomes suspicious.

## One-line install

The stable installer remains:

```bash
curl -fsSL https://raw.githubusercontent.com/Tihulu/tihulu-cosmic-minimized-windows/stable/scripts/quick-install.sh | bash
```

Then open **COSMIC Settings → Desktop → Dock/Panel**, remove the stock **Minimized Windows** applet, and add **Tihulu Minimized Windows**.

To test the current v0.4 branch directly:

```bash
REF=v0.4-safe-core bash <(curl -fsSL https://raw.githubusercontent.com/Tihulu/tihulu-cosmic-minimized-windows/v0.4-safe-core/scripts/quick-install.sh)
```

## Runtime acceptance test

CI proves formatting/build/test/lint correctness, not COSMIC runtime stability. Before promotion to stable, test at least:

- Brave with 2, 3, 4, and 5 minimized windows
- 50-100+ repeated hover cycles
- switching repeatedly between different minimized application groups
- restore/close cycles
- two monitors when available
- logout/login behavior
- applet FD/RSS
- all `cosmic-panel` process FD counts
- `cosmic-comp` FD/RSS

Useful panel/applet observation:

```bash
watch -n 2 '
for pid in $(pgrep -x cosmic-panel); do
  printf "panel PID=%-8s FD=%s\n" "$pid" "$(ls /proc/$pid/fd 2>/dev/null | wc -l)"
done
pid=$(pgrep -f "/tihulu-cosmic-minimized-windows$" | head -1)
[ -n "$pid" ] && printf "applet PID=%-8s FD=%s\n" "$pid" "$(ls /proc/$pid/fd 2>/dev/null | wc -l)"
comp=$(pgrep -x cosmic-comp | head -1)
[ -n "$comp" ] && printf "comp   PID=%-8s FD=%s\n" "$comp" "$(ls /proc/$comp/fd 2>/dev/null | wc -l)"
'
```

The acceptance criterion is simple: the project must not cause monotonically growing FD/memfd/RSS use.

## Development

```bash
cargo fmt --all -- --check
cargo check --all-targets
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
cargo build --release
```

The repository pins the COSMIC client-toolkit revision used by the pinned `libcosmic` revision to avoid duplicate/incompatible Wayland protocol types.

Experimental preview/media work belongs on separate branches and PRs. Runtime-untested rich features must not be merged into stable.

## License

AGPL-3.0-only. See `LICENSE`.

This is a separate implementation for COSMIC interoperability and does not include System76's `cosmic-applet-minimize` source files.

## Related upstream work

- `pop-os/cosmic-applets`
- `pop-os/cosmic-comp` issue #2073
- COSMIC foreign-toplevel and image-copy-capture protocols
