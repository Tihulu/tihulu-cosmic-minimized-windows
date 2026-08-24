# Development and leak regression testing

The stability goal of this applet is simple: minimized-window state may churn, but resources must remain bounded by the currently active state rather than historical window events.

## Build checks

```bash
cargo fmt --all -- --check
cargo check --all-targets
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
```

## Runtime regression test

1. Add **Tihulu Minimized Windows** to the COSMIC dock.
2. Record the applet and panel FD counts.
3. Repeatedly open, minimize, restore and close windows.
4. Verify FD counts fluctuate around a bounded range instead of increasing monotonically per cycle.
5. Repeat with enough minimized windows to exercise the overflow popup.

This implementation intentionally avoids the screencopy/thumbnail pipeline so this test isolates minimized-window toplevel tracking and activation.
