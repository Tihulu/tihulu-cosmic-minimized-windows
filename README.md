# Tihulu Minimized Windows

A stability-focused minimized-window applet for the COSMIC desktop, licensed **AGPL-3.0-only**.

It implements the familiar minimized-window dock behavior using COSMIC's public toplevel-management APIs, but intentionally does **not** capture live window thumbnails. Minimized windows are represented by their application icons; clicking an icon restores the window.

## Why this exists

A system that motivated this project hit `Too many open files` in the COSMIC session, followed by Wayland/EGL failures and panel restart backoff. COSMIC has also had reports around the stock Minimized Windows applet and resource growth during window churn.

This implementation avoids the whole screenshot path by design:

- no screencopy sessions
- no preview `memfd`
- no thumbnail `wl_shm` buffers
- no applet-created GPU synchronization FDs
- no thumbnail worker thread per minimize event

That does **not** claim to fix every possible COSMIC compositor/panel FD leak. It removes the minimized-window screenshot path from this applet so it can be tested independently.

## Features

- Shows currently minimized windows in the COSMIC dock or panel
- Restores a window when its icon is clicked
- Uses application icons and names from desktop entries
- Supports horizontal and vertical panels/docks
- Overflow popup when there are more minimized windows than fit inline
- Removes window state immediately on restore/close
- Keeps the applet alive if its Wayland bridge exits instead of intentionally panicking the panel process

## One-line install

```bash
curl -fsSL https://raw.githubusercontent.com/Tihulu/tihulu-cosmic-minimized-windows/stable/scripts/quick-install.sh | bash
```

Then open **COSMIC Settings → Desktop → Dock** (or Panel), remove the stock **Minimized Windows** applet, and add **Tihulu Minimized Windows**.

## Verify resource behavior

Watch the panel FD count while minimizing/restoring windows repeatedly:

```bash
PID=$(pgrep -x cosmic-panel | head -1)
watch -n 2 "printf 'panel FDs: '; ls /proc/$PID/fd 2>/dev/null | wc -l"
```

Find this applet and watch its FD count:

```bash
PID=$(pgrep -f '/tihulu-cosmic-minimized-windows$' | head -1)
watch -n 2 "printf 'applet FDs: '; ls /proc/$PID/fd 2>/dev/null | wc -l"
```

Small fluctuations are normal. A monotonic increase tied to each minimize/restore cycle is not.

## Development

```bash
cargo check
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
