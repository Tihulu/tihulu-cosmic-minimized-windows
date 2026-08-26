# Previewd Runtime Acceptance

This test is mandatory before the `feature/previewd` branch can be merged or promoted. CI is necessary but not sufficient; this procedure must be run in a real Pop!_OS/COSMIC Wayland session.

## Guided runner

The preferred path is the guided runtime runner. It samples previewd and compositor resources every two seconds, records a CSV and failure bundle, performs daemon stop/recovery and SIGKILL restart tests, checks cache/FD caps and monotonic FD/capture-memfd growth, and asks for explicit UI pass/fail confirmation at each phase.

Current runtime-test candidate: `e438780974bc3fcdb03e35d00b3690584a981e07`.

Install that exact candidate first:

```bash
REF=e438780974bc3fcdb03e35d00b3690584a981e07 bash <(curl -fsSL https://raw.githubusercontent.com/Tihulu/tihulu-cosmic-minimized-windows/e438780974bc3fcdb03e35d00b3690584a981e07/scripts/quick-install.sh)
```

Remove/re-add the applet in COSMIC Settings, then run the exact runner from the same candidate:

```bash
bash <(curl -fsSL https://raw.githubusercontent.com/Tihulu/tihulu-cosmic-minimized-windows/e438780974bc3fcdb03e35d00b3690584a981e07/scripts/run-previewd-runtime-test.sh)
```

A successful first-stage run ends with `VERDICT: PASS CANDIDATE`. Logout/login coverage is still required before merge; after logging back in run:

```bash
MODE=post-login bash <(curl -fsSL https://raw.githubusercontent.com/Tihulu/tihulu-cosmic-minimized-windows/e438780974bc3fcdb03e35d00b3690584a981e07/scripts/run-previewd-runtime-test.sh)
```

The runner stores its CSV, manual check results, service status, journal and process snapshots under `~/tihulu-previewd-runtime-results/`. A runner `FAIL` means the branch must not be merged.

The manual procedure below remains the reference for what each runner phase is validating.

## 1. Install the candidate

Use the pinned candidate command above. Then remove the existing **Tihulu Minimized Windows** applet from the COSMIC dock/panel and add it again.

Verify the user daemon:

```bash
systemctl --user is-active tihulu-previewd.service
systemctl --user status tihulu-previewd.service --no-pager
journalctl --user -u tihulu-previewd.service -n 50 --no-pager
```

Expected: the service is `active` and the journal contains a `tihulu-previewd ready:` line. If the service cannot start, stop here and collect the journal output.

## 2. Start the resource watcher

Keep this running in a terminal during the complete manual test:

```bash
bash <(curl -fsSL https://raw.githubusercontent.com/Tihulu/tihulu-cosmic-minimized-windows/e438780974bc3fcdb03e35d00b3690584a981e07/scripts/watch-previewd-resources.sh)
```

The watcher reports applet, previewd, panel and compositor FD/RSS/shmem/memfd metrics plus preview-cache usage.

## 3. Safe Core baseline

Safe Core should be enabled initially. Minimize two Brave windows, stress hover/pin, restore/minimize and exact close. Expect title/icon rows only, correct grouping and bounded resources.

## 4. Enable Extended mode

With at least two minimized windows visible in a group popup, turn **Safe Core mode** off. Expected when healthy:

```text
Window previews are provided by tihulu-previewd. Media controls are not enabled yet.
```

Existing minimized windows should be captured sequentially and newly minimized windows once. Cache limits are 16 `.rgba` files and 8 MiB total, with approximately 320x180 target thumbnails.

## 5. Hover stress

Perform at least 100 icon-to-popup hover cycles. Test Brave with 2, 3, 4 and 5 minimized windows and rapidly switch to another minimized application group. Hover must read cache only, with no wrong preview, stale close, popup-anchor regression or monotonic FD/memfd growth.

## 6. Restore/close churn

Repeat at least 20 cycles of restore, minimize, close and new-window minimize. Removed windows must disappear from UI/cache, new minimized windows must receive one preview, exact-window actions must remain correct, and resource growth must stay bounded.

## 7. Daemon failure fallback

Stop previewd while Extended mode is active, wait at least 15 seconds and reopen the popup. Expected: title/icon Safe Core rows remain usable, previews clear, restore/close continue working and the popup reports Safe Core fallback.

## 8. Daemon recovery

Restart previewd and wait up to roughly 15 seconds. Existing minimized windows should re-capture sequentially and previews return without restarting `cosmic-panel`. Also SIGKILL previewd once and verify `Restart=on-failure` brings it back.

## 9. Two-monitor test

If two monitors are available, test minimized windows from both displays, group switching, exact restore and hover while moving normal windows between monitors. No preview/group mix-up or compositor growth is acceptable.

## 10. Logout/login test

Logout and log back in. `tihulu-previewd.service` must start for the graphical session; stale socket/cache state must not block startup; persisted mode must behave correctly; Extended should recover previews when the daemon is healthy.

## PASS criteria

The candidate passes only if all of the following are true:

- no applet/panel/compositor crash
- no persistent popup/hover regression
- exact restore/close behavior remains correct
- previewd failure automatically falls back to Safe Core
- previewd recovery restores previews without panel restart
- daemon cache remains <=16 entries and <=8 MiB
- previewd stays below `LimitNOFILE=256`
- no monotonically increasing previewd FD count
- no monotonically increasing `cosmic-comp` FD count attributable to capture
- no monotonically increasing compositor capture-related memfd count
- daemon RSS remains bounded and its +128 MiB breaker does not trip during normal use
- repeated hover does not trigger repeated capture

A bounded sequence such as `92 -> 93 -> 92 -> 93` is acceptable. A monotonic sequence such as `92 -> 93 -> 94 -> 95` is a failure.

## Failure bundle

The guided runner automatically writes the bundle under `~/tihulu-previewd-runtime-results/`. For manual collection use the resource watcher, `systemctl --user status tihulu-previewd.service`, the previewd journal, and `pgrep -af` snapshots for the applet, previewd and cosmic-comp.

Do not merge the branch when this acceptance test fails even if GitHub Actions is green.
