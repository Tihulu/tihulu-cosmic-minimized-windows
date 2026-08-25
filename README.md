# Tihulu Minimized Windows

A stability-first minimized-window applet for the COSMIC desktop, licensed **AGPL-3.0-only**.

The project exists because minimized-window preview paths can be dangerous when compositor-side screenshot resources leak. The applet therefore treats window management as the always-available core and makes rich features optional and isolated.

## v0.4 Safe Core

The always-available core provides:

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

Safe Core itself owns no screenshot buffers, preview memfds, album art, MPRIS or audio-control work.

## Experimental external previews

The `feature/previewd` branch contains the runtime-test candidate for external window previews.

```text
tihulu-cosmic-minimized-windows
    lightweight panel UI
    Safe Core always works
            |
            +-- optional IPC --> tihulu-previewd
            |
            +-- future IPC ---> tihulu-mediad
```

`tihulu-previewd` is a per-user process. The panel applet never performs Wayland image capture itself. The daemon uses `ext-image-copy-capture` with a foreign-toplevel image capture source and exact foreign-toplevel identifiers.

The capture path was gated first by a real 500-capture Pop!_OS/COSMIC probe. That run completed 500/500 captures with bounded resources: probe FD 11 -> 11, cosmic-comp FD 374 -> 370 with bounded oscillation, cosmic-comp memfd 69 -> 69, capture-related compositor memfd 0 -> 0, and unchanged compositor RSS/shmem.

The preview candidate additionally enforces:

- one capture in flight globally
- 64 MiB hard limit for a full-size capture before allocation
- in-place raw-to-RGBA conversion
- full-size buffer discarded after thumbnail generation
- approximately 320x180 thumbnail target
- maximum 16 daemon thumbnails
- maximum 8 MiB daemon thumbnail cache with LRU eviction
- maximum 16 applet preview handles
- daemon/compositor FD and capture-memfd growth watchdog
- +128 MiB daemon RSS circuit breaker
- automatic Safe Core fallback when previewd is unavailable or degraded
- 15-second health/recovery polling
- systemd user service with `LimitNOFILE=256`, `NoNewPrivileges=true`, and `UMask=0077`

Hover reads cached previews only; hover does not request a new capture.

Media/MPRIS/PipeWire support is intentionally not part of the preview candidate.

## Popup lifetime safety

The popup uses an explicit state machine:

```text
Closed
HoverOpen(group, window_id, generation)
Pinned(group, window_id, generation)
```

Delayed close requests carry both the popup generation and a close token. A stale timer cannot close a newer popup. A compositor close event for an old `WindowId` is ignored after a newer popup generation has replaced it.

When hover changes to a different application group, the old popup surface is replaced so the new surface receives the correct anchor rectangle. Hovering/pinning the same group reuses the current surface.

## Install

Stable installer:

```bash
curl -fsSL https://raw.githubusercontent.com/Tihulu/tihulu-cosmic-minimized-windows/stable/scripts/quick-install.sh | bash
```

Current Safe Core RC:

```bash
REF=v0.4-safe-core bash <(curl -fsSL https://raw.githubusercontent.com/Tihulu/tihulu-cosmic-minimized-windows/v0.4-safe-core/scripts/quick-install.sh)
```

Experimental previewd candidate:

```bash
REF=feature/previewd bash <(curl -fsSL https://raw.githubusercontent.com/Tihulu/tihulu-cosmic-minimized-windows/feature/previewd/scripts/quick-install.sh)
```

Then open **COSMIC Settings → Desktop → Dock/Panel**, remove the existing applet instance and add **Tihulu Minimized Windows** again.

## Previewd runtime acceptance

CI proves formatting/build/test/lint correctness, not COSMIC runtime lifetime safety. The preview branch must pass the real-session acceptance test before it can be merged or promoted.

Preferred guided runner:

```bash
bash <(curl -fsSL https://raw.githubusercontent.com/Tihulu/tihulu-cosmic-minimized-windows/feature/previewd/scripts/run-previewd-runtime-test.sh)
```

It samples previewd/compositor resources, performs the fallback/recovery and restart-on-failure checks, records a CSV/failure bundle, checks cache and FD limits plus monotonic FD/capture-memfd growth, and asks for explicit UI pass/fail confirmation at each phase.

A successful first run ends with `VERDICT: PASS CANDIDATE`. Logout/login is then verified with:

```bash
MODE=post-login bash <(curl -fsSL https://raw.githubusercontent.com/Tihulu/tihulu-cosmic-minimized-windows/feature/previewd/scripts/run-previewd-runtime-test.sh)
```

Full manual/reference procedure: `docs/PREVIEWD_RUNTIME_ACCEPTANCE.md`.

Runtime acceptance includes Brave with 2/3/4/5 minimized windows, 100+ hover cycles, restore/close churn, daemon stop/recovery, restart-on-failure, two monitors when available, logout/login, and FD/RSS/shmem/memfd observation.

A bounded sequence such as `92 -> 93 -> 92 -> 93` is acceptable. Monotonic growth such as `92 -> 93 -> 94 -> 95` is a failure.

PR #12 remains draft until this real runtime acceptance passes.

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
