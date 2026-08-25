# Rich feature architecture after the probe gate

This document records the intended architecture only. The daemons described here must not be connected to the applet until the preview probe passes real COSMIC runtime testing.

## Applet

`tihulu-cosmic-minimized-windows` remains the authority for the Safe Core experience:

- minimized toplevel tracking
- grouping/counts
- titles/icons
- exact restore
- exact close
- popup lifecycle
- user Safe Core preference

The core must operate with both optional daemons absent, stopped, crashed, or incompatible.

The requested user mode and the effective runtime mode are separate concepts. Extended mode may be requested, but the effective mode is Safe Core whenever a required optional subsystem is unhealthy.

## `tihulu-previewd`

Only build/connect this after `tihulu-preview-probe` demonstrates bounded compositor and client resources.

Responsibilities:

- own the Wayland image-copy connection and all capture buffers;
- capture only on `normal -> minimized` transitions, never on hover;
- globally allow at most one capture in flight;
- keep at most 16 thumbnails;
- use bounded LRU eviction and a strict byte budget;
- target presentation around 320x180 after capture/scale, without retaining full-size buffers longer than necessary;
- discard a cached image when its window disappears;
- refresh once when a restored window is minimized again;
- expose compact status and thumbnail identifiers to the applet over a Unix-domain socket;
- restart independently from `cosmic-panel`.

Suggested IPC messages are length-delimited and versioned:

```text
Hello { protocol_version }
SetSafeCore { enabled }
WindowMinimized { stable_window_key, app_id, title }
WindowGone { stable_window_key }
GetThumbnail { stable_window_key }
ThumbnailReady { stable_window_key, generation, metadata, transport }
PreviewStatus { state: ready | degraded | disabled, reason }
```

The applet must treat all preview messages as optional enrichment. A missing thumbnail is rendered as the Safe Core row.

### Preview circuit breaker

At minimum sample:

- daemon FD count;
- daemon RSS;
- `cosmic-comp` FD count when accessible;
- `cosmic-comp` RSS/shared memory when accessible;
- capture-related compositor memfds.

If repeated captures show unexpected monotonic growth, stop capture for the session, release the cache, set `PreviewStatus=degraded`, and do not retry indefinitely. The applet immediately remains/returns in effective Safe Core mode.

## `tihulu-mediad`

Media work is a later phase and is independent of preview capture.

Responsibilities:

- keep persistent MPRIS/D-Bus connections;
- maintain `PlaybackStatus` from `PropertiesChanged` signals instead of aggressive polling;
- on a play/pause request, use current authoritative state: `Playing -> Pause()`, `Paused/Stopped -> Play()`;
- correlate browser MPRIS players deterministically using D-Bus service, connection PID, process tree, metadata/title, and active audio stream evidence;
- keep a persistent PipeWire/PulseAudio connection for volume and mute;
- resolve live stream identity rather than caching sink-input IDs indefinitely;
- own album-art fetch/decode/cache work outside the panel process;
- publish compact media state to the applet over versioned IPC.

If media association is ambiguous, media controls should be omitted for that group rather than guessing.

## Failure policy

Any daemon crash, incompatible protocol version, socket failure, budget violation, or circuit-breaker event has the same applet-side result:

```text
optional subsystem unavailable
        -> effective Safe Core
        -> grouping/restore/close continue
```

No optional daemon is allowed to be a prerequisite for the applet process to start or remain functional.
