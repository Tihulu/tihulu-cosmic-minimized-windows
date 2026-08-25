# Previewd Runtime Acceptance

This test is mandatory before the `feature/previewd` branch can be merged or promoted. CI is necessary but not sufficient; this procedure must be run in a real Pop!_OS/COSMIC Wayland session.

## 1. Install the candidate

```bash
REF=feature/previewd bash <(curl -fsSL https://raw.githubusercontent.com/Tihulu/tihulu-cosmic-minimized-windows/feature/previewd/scripts/quick-install.sh)
```

Then remove the existing **Tihulu Minimized Windows** applet from the COSMIC dock/panel and add it again.

Verify the user daemon:

```bash
systemctl --user is-active tihulu-previewd.service
systemctl --user status tihulu-previewd.service --no-pager
journalctl --user -u tihulu-previewd.service -n 50 --no-pager
```

Expected: the service is `active` and the journal contains a `tihulu-previewd ready:` line. If the service cannot start, stop here and collect the journal output.

## 2. Start the resource watcher

Keep this running in a terminal during the complete test:

```bash
bash <(curl -fsSL https://raw.githubusercontent.com/Tihulu/tihulu-cosmic-minimized-windows/feature/previewd/scripts/watch-previewd-resources.sh)
```

The watcher reports:

- applet FD, RSS, RssShmem, memfd and capture-related memfd
- `tihulu-previewd` FD, RSS, RssShmem, memfd and capture-related memfd
- all matching `cosmic-panel` process metrics
- `cosmic-comp` metrics
- preview cache file count and byte size

Record the initial values before enabling previews.

## 3. Safe Core baseline

Safe Core should be enabled initially.

1. Minimize two Brave windows.
2. Hover the Brave group repeatedly.
3. Open/pin the group popup.
4. Restore one window and minimize it again.
5. Close one minimized window from the popup.

Expected:

- title/icon rows only
- grouping/restore/close work correctly
- no preview capture activity is required
- popup does not disappear because of stale hover timers
- resource counts remain bounded

## 4. Enable Extended mode

With at least two minimized windows visible in a group popup, turn **Safe Core mode** off.

Expected when the daemon is healthy:

```text
Window previews are provided by tihulu-previewd. Media controls are not enabled yet.
```

Existing minimized windows should be captured sequentially and their cached thumbnails should appear. Newly minimized windows should be captured once when they enter the minimized set.

The preview cache must remain within:

- maximum 16 `.rgba` files
- maximum 8 MiB total daemon thumbnail cache
- approximately 320x180 maximum target thumbnail size per window

## 5. Hover stress

Perform at least 100 icon-to-popup hover cycles across minimized groups.

Test Brave with 2, then 3, 4 and 5 minimized windows. Also switch rapidly between Brave and at least one other minimized application group.

Expected:

- hover reads cached previews only
- hover must not cause a new capture every time
- the popup follows the correct group and anchor
- no stale popup closes a newer popup
- no blank/mismatched preview belongs to another window
- applet/daemon/compositor FD and capture-related memfd counts oscillate within a bounded range instead of increasing monotonically

## 6. Restore/close churn

Repeat for at least 20 cycles:

1. Restore a minimized window by clicking its thumbnail.
2. Minimize it again.
3. Close another minimized window from the popup.
4. Open a new application window and minimize it.

Expected:

- removed windows disappear from both UI and preview cache
- newly minimized windows receive one fresh preview
- cache count never exceeds 16
- restore/close targets the exact intended window
- no persistent FD/memfd growth

## 7. Daemon failure fallback

While Extended mode is enabled and previews are visible:

```bash
systemctl --user stop tihulu-previewd.service
```

Wait at least 15 seconds and reopen the group popup.

Expected:

- title/icon Safe Core rows remain usable
- previews are cleared from the applet
- grouping/restore/close continue to work
- the popup reports that Extended mode was requested but Safe Core fallback is active
- the applet itself does not crash or hang

## 8. Daemon recovery

Restart the daemon:

```bash
systemctl --user start tihulu-previewd.service
```

Wait up to roughly 15 seconds.

Expected:

- health polling detects the daemon again
- already minimized windows are re-captured sequentially
- previews return without restarting `cosmic-panel`
- Safe Core fallback remains available if recovery fails

Also verify the service's restart-on-failure path once:

```bash
systemctl --user kill -s SIGKILL tihulu-previewd.service
sleep 3
systemctl --user is-active tihulu-previewd.service
```

Expected: systemd restarts the service and it becomes `active` again.

## 9. Two-monitor test

If two monitors are available:

1. Minimize windows from both monitors.
2. Open groups repeatedly from the panel/dock.
3. Restore windows originating on each display.
4. Repeat hover and group switching while moving normal windows between monitors.

Expected: no preview/group mix-ups, no popup lifetime regression, and no monotonic compositor resource growth.

## 10. Logout/login test

Logout and log back into the COSMIC session.

Expected:

- `tihulu-previewd.service` starts as a user service for the graphical session
- stale runtime socket/cache state does not prevent startup
- the applet starts in the persisted mode
- if Extended was persisted and the daemon is healthy, previews recover; otherwise Safe Core remains functional

## PASS criteria

The candidate passes only if all of the following are true:

- no applet/panel/compositor crash
- no persistent popup/hover regression
- exact restore/close behavior remains correct
- previewd failure automatically falls back to Safe Core
- previewd recovery restores previews without panel restart
- daemon cache remains <=16 entries and <=8 MiB
- previewd stays below its systemd `LimitNOFILE=256`
- no monotonically increasing previewd FD count
- no monotonically increasing `cosmic-comp` FD count attributable to capture
- no monotonically increasing compositor capture-related memfd count
- daemon RSS remains bounded; its internal +128 MiB growth breaker must not trip during normal use
- repeated hover does not trigger repeated capture

A bounded sequence such as `92 -> 93 -> 92 -> 93` is acceptable. A monotonic sequence such as `92 -> 93 -> 94 -> 95` is a failure.

## Failure bundle

If anything fails, collect these outputs before restarting the session:

```bash
ONCE=1 bash <(curl -fsSL https://raw.githubusercontent.com/Tihulu/tihulu-cosmic-minimized-windows/feature/previewd/scripts/watch-previewd-resources.sh)

systemctl --user status tihulu-previewd.service --no-pager
journalctl --user -u tihulu-previewd.service --since '20 minutes ago' --no-pager

printf '\nApplet processes:\n'
pgrep -af tihulu-cosmic-minimized-windows || true
printf '\nPreview daemon:\n'
pgrep -af tihulu-previewd || true
printf '\nCOSMIC compositor:\n'
pgrep -af cosmic-comp || true
```

Do not merge the branch when this acceptance test fails even if GitHub Actions is green.
