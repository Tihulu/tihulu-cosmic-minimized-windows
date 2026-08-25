# Runtime acceptance checklist

CI success is necessary but is not evidence that COSMIC runtime resources are bounded.

## Safe Core

Test on the actual Pop!_OS/COSMIC session before merging v0.4 to stable:

- Brave groups with 2, 3, 4, and 5 minimized windows;
- 100 repeated hover cycles on the same Brave group;
- repeated hover switching between at least two application groups;
- icon -> popup -> icon pointer transitions during the leave grace period;
- pin/unpin behavior by click/right-click;
- exact restore and close for each row;
- repeated minimize/restore cycles;
- two monitors when available;
- horizontal and vertical panel/dock placement when practical;
- logout/login behavior;
- Safe Core preference persistence across applet restart/login.

Record before/during/after:

- Tihulu applet FD/RSS;
- every `cosmic-panel` process FD/RSS;
- `cosmic-comp` FD/RSS/shared memory;
- unexpected compositor memfds.

Acceptance requires no feature-attributable monotonic FD/memfd/RSS growth and no intermittent dead popup state.

## Preview probe

Do not run the preview probe until Safe Core has passed its own hover/runtime test.

Then follow `PREVIEW_PROBE.md`, preferably with an otherwise quiet desktop. Run a 500-capture test against one minimized toplevel and preserve the CSV.

The capture path is rejected if the client or compositor shows monotonic capture-attributable FD/memfd growth, repeated failures, or an unexplained sustained RSS increase.

## Media

Media acceptance is deferred until after the preview gate. When `tihulu-mediad` exists, test at minimum:

- Spotify play/pause/previous/next repeatedly;
- Brave with multiple browser windows/tabs exposing MPRIS players;
- authoritative Playing/Paused transitions with no inverted controls;
- repeated volume/mute changes while browser streams disappear/reappear;
- player/stream association after tab close/reopen;
- daemon restart while the applet remains functional;
- Safe Core switch immediately removing/avoiding media enrichment;
- applet, media daemon, `cosmic-panel`, and `cosmic-comp` FD/RSS trends.
