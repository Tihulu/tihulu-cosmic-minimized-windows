# ext-image-copy preview probe

`src/bin/tihulu-preview-probe/` is an isolated stress-test utility. It is **not** wired into the COSMIC applet and does not make preview capture an approved feature.

It uses the newer Wayland chain:

```text
ext_foreign_toplevel_list_v1
        |
        v
ext_foreign_toplevel_image_capture_source_manager_v1
        |
        v
ext_image_capture_source_v1
        |
        v
ext_image_copy_capture_manager_v1
```

It does **not** use the retired cctk screencopy path from the old rich-preview implementation.

## Build

From the experimental branch:

```bash
git switch experiment/ext-image-copy-probe
cargo build --release --bin tihulu-preview-probe
```

## Select a target

The standard `ext_foreign_toplevel_list_v1` protocol exposes title/app-id/identifier but does not expose minimized state. Keep the window you want to probe minimized for the entire run.

List available handles:

```bash
./target/release/tihulu-preview-probe --list
```

Then select it by title/app-id/identifier substring, for example:

```bash
./target/release/tihulu-preview-probe --match brave --captures 500 --sample-every 10 --output brave-500.csv
```

If more than one toplevel matches, the first matching handle is used. Use a distinctive title/identifier substring when needed.

## What one capture does

Captures are strictly sequential. There is never more than one frame in flight.

For every iteration the probe:

1. creates a new `ext_image_copy_capture_session_v1` for the already-created toplevel capture source;
2. waits for buffer constraints;
3. creates one anonymous `memfd` named `tihulu-preview-probe`;
4. creates one `wl_shm_pool` and one `wl_buffer` sized exactly as requested by the compositor;
5. creates one image-copy frame and requests capture;
6. waits for `ready` or `failed`;
7. destroys the frame, buffer, pool, and session;
8. performs a Wayland roundtrip so destruction requests are processed;
9. only then takes the `/proc` sample when the configured sample interval is reached.

The probe does not decode, scale, encode, cache, or retain the captured pixels.

## CSV fields

The CSV records a baseline plus periodic capture samples:

- `phase`
- `iteration`
- `capture_ok`
- `probe_fd`
- `probe_rss_kb`
- `probe_shmem_kb`
- `probe_memfd`
- `cosmic_comp_pid`
- `cosmic_comp_fd`
- `cosmic_comp_rss_kb`
- `cosmic_comp_shmem_kb`
- `cosmic_comp_memfd`
- `cosmic_comp_capture_memfd`
- `probe_pid`

`cosmic_comp_capture_memfd` counts compositor memfd symlink targets containing capture/screencopy/minimize-applet markers. The total memfd count is also recorded because a new leak may use a different name.

## Circuit breaker

The breaker is enabled by default. It stops the run with exit code `2` when any of these conditions occurs:

- probe FD count grows monotonically by at least four descriptors across six consecutive samples;
- `cosmic-comp` FD count does the same;
- `cosmic-comp` capture-related memfd count does the same;
- three consecutive capture attempts fail.

The breaker is intentionally conservative. `--no-circuit-breaker` exists for controlled diagnosis but should not be the first test.

RSS is recorded but is not used as an automatic breaker because allocator and compositor RSS can legitimately fluctuate. It still must be inspected for a persistent upward trend.

## Approval rule

A completed probe run is not automatically a pass.

Good example:

```text
cosmic-comp FD: 92 -> 93 -> 92 -> 93 -> 92
```

Bad example:

```text
cosmic-comp FD: 92 -> 93 -> 94 -> 95 -> 96
```

Also reject the path if compositor capture-related memfds or RSS show clear persistent growth attributable to the capture loop.

Only after a real COSMIC/Pop!_OS run demonstrates bounded behavior should `tihulu-previewd` be implemented and connected to the applet. Until then, Safe Core remains the only applet runtime path.
