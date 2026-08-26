# Tihulu COSMIC Minimized Windows

A stability-first minimized-window applet for COSMIC.

## Current v0.4 Safe Core RC

The current runtime-validation branch intentionally keeps the panel process small and avoids preview/media work inside `cosmic-panel`.

Default interaction is **click-only**:

- single minimized window: click restores it
- grouped minimized windows: click opens the lightweight title/icon popup
- right click also opens the grouped popup
- hover popup creation is **disabled by default**

### Experimental hover popup

Real COSMIC runtime testing showed repeated hover-triggered popup surface creation can restart `cosmic-panel` on the tested system. For that reason hover is an explicit experimental opt-in and is never the default.

The preference lives in:

```text
~/.config/tihulu-cosmic-minimized-windows/config
```

Default:

```ini
mode=safe-core
hover-popups=false
```

To opt in for testing only, set:

```ini
mode=safe-core
hover-popups=true
```

Then remove/re-add the applet (or restart the panel session) so the setting is read again.

If hover causes panel flicker, popup failures, PID changes, ghost clicks, or a `cosmic-panel` restart, immediately return to `hover-popups=false`.

## Safety model

Safe Core intentionally has no:

- screencopy protocol in the panel process
- wl_shm preview buffers
- preview memfd buffers
- screenshot worker threads
- image decode/cache
- MPRIS/media subsystem
- `pactl` subprocesses
- HTTP artwork fetching

It keeps:

- minimized toplevel tracking
- grouping by application, including browser aliases
- one icon per group with a count
- exact restore
- exact close
- lightweight title/icon popup
- scrollable multi-window list

## Optional rich architecture

Future rich features stay outside the panel process:

```text
Tihulu applet
   |
   +-- tihulu-previewd  -> bounded thumbnails
   |
   +-- tihulu-mediad    -> MPRIS/media controls
```

Both daemons are optional. Failure of either must fall back to the click-only Safe Core behavior instead of destabilizing the panel.

Preview work remains runtime-gated. Media integration is planned as a separate daemon and should begin with text metadata plus play/pause/next/previous, without artwork or polling inside the panel.

## Install this branch/commit

The installer supports a branch, tag, or exact commit SHA through `REF`:

```bash
REF=v0.4-safe-core bash <(curl -fsSL https://raw.githubusercontent.com/Tihulu/tihulu-cosmic-minimized-windows/v0.4-safe-core/scripts/quick-install.sh)
```

For runtime acceptance, prefer an exact CI-green SHA rather than a moving branch.

## Runtime acceptance

CI success is not runtime acceptance. Before promotion, test on a real COSMIC session with multiple applications and grouped windows, and observe `cosmic-panel` / `cosmic-comp` PIDs plus FD/RSS/memfd behavior.

A panel PID change during ordinary interaction is a runtime failure even if CI is green.

## License

AGPL-3.0-only
