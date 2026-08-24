// SPDX-License-Identifier: AGPL-3.0-only

use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::fd::AsFd,
    path::PathBuf,
    time::Duration,
};

use cctk::{
    sctk::{
        self,
        registry::{ProvidesRegistryState, RegistryState},
    },
    toplevel_info::{ToplevelInfoHandler, ToplevelInfoState},
    wayland_client::{
        Connection, Dispatch, QueueHandle, WEnum, delegate_noop,
        globals::registry_queue_init,
        protocol::{
            wl_buffer::WlBuffer,
            wl_shm::{self, WlShm},
            wl_shm_pool::WlShmPool,
        },
    },
    wayland_protocols::ext::{
        foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
        image_capture_source::v1::client::{
            ext_foreign_toplevel_image_capture_source_manager_v1::ExtForeignToplevelImageCaptureSourceManagerV1,
            ext_image_capture_source_v1::ExtImageCaptureSourceV1,
        },
        image_copy_capture::v1::client::{
            ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1},
            ext_image_copy_capture_manager_v1::{ExtImageCopyCaptureManagerV1, Options},
            ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
        },
    },
};
use cosmic_protocols::toplevel_info::v1::client::zcosmic_toplevel_handle_v1;

const MAX_CAPTURES: usize = 500;
const DEFAULT_CAPTURES: usize = 64;
const FD_GROWTH_LIMIT: usize = 8;
const FD_GROWTH_STREAK_LIMIT: usize = 6;
const RSS_GROWTH_LIMIT_KB: u64 = 512 * 1024;

#[derive(Default)]
struct CaptureState {
    width: u32,
    height: u32,
    formats: Vec<wl_shm::Format>,
    constraints_done: bool,
    session_stopped: bool,
    frame_ready: bool,
    frame_failed: bool,
}

impl CaptureState {
    fn reset_session(&mut self) {
        self.width = 0;
        self.height = 0;
        self.formats.clear();
        self.constraints_done = false;
        self.session_stopped = false;
        self.frame_ready = false;
        self.frame_failed = false;
    }

    fn reset_frame(&mut self) {
        self.frame_ready = false;
        self.frame_failed = false;
    }
}

struct ProbeState {
    registry: RegistryState,
    toplevels: ToplevelInfoState,
    target: Option<ExtForeignToplevelHandleV1>,
    target_filter: Option<String>,
    capture: CaptureState,
}

impl ProbeState {
    fn reconsider(&mut self, handle: &ExtForeignToplevelHandleV1) {
        let Some(info) = self.toplevels.info(handle) else {
            return;
        };
        let minimized = info
            .state
            .contains(&zcosmic_toplevel_handle_v1::State::Minimized);
        if !minimized {
            if self.target.as_ref() == Some(handle) {
                self.target = None;
            }
            return;
        }

        let matches = self.target_filter.as_deref().is_none_or(|filter| {
            let filter = filter.to_ascii_lowercase();
            info.app_id.to_ascii_lowercase().contains(&filter)
                || info.title.to_ascii_lowercase().contains(&filter)
        });
        if matches && self.target.is_none() {
            eprintln!(
                "Using minimized window: app_id={:?} title={:?}",
                info.app_id, info.title
            );
            self.target = Some(handle.clone());
        }
    }
}

impl ProvidesRegistryState for ProbeState {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }

    sctk::registry_handlers!();
}

impl ToplevelInfoHandler for ProbeState {
    fn toplevel_info_state(&mut self) -> &mut ToplevelInfoState {
        &mut self.toplevels
    }

    fn new_toplevel(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        handle: &ExtForeignToplevelHandleV1,
    ) {
        self.reconsider(handle);
    }

    fn update_toplevel(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        handle: &ExtForeignToplevelHandleV1,
    ) {
        self.reconsider(handle);
    }

    fn toplevel_closed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        handle: &ExtForeignToplevelHandleV1,
    ) {
        if self.target.as_ref() == Some(handle) {
            self.target = None;
        }
    }
}

impl Dispatch<ExtImageCopyCaptureSessionV1, ()> for ProbeState {
    fn event(
        state: &mut Self,
        _: &ExtImageCopyCaptureSessionV1,
        event: ext_image_copy_capture_session_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_image_copy_capture_session_v1::Event::BufferSize { width, height } => {
                state.capture.width = width;
                state.capture.height = height;
            }
            ext_image_copy_capture_session_v1::Event::ShmFormat {
                format: WEnum::Value(format),
            } => state.capture.formats.push(format),
            ext_image_copy_capture_session_v1::Event::Done => {
                state.capture.constraints_done = true;
            }
            ext_image_copy_capture_session_v1::Event::Stopped => {
                state.capture.session_stopped = true;
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, ()> for ProbeState {
    fn event(
        state: &mut Self,
        _: &ExtImageCopyCaptureFrameV1,
        event: ext_image_copy_capture_frame_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_image_copy_capture_frame_v1::Event::Ready => state.capture.frame_ready = true,
            ext_image_copy_capture_frame_v1::Event::Failed { .. } => {
                state.capture.frame_failed = true;
            }
            _ => {}
        }
    }
}

delegate_noop!(ProbeState: ignore WlShm);
delegate_noop!(ProbeState: ignore WlShmPool);
delegate_noop!(ProbeState: ignore WlBuffer);
delegate_noop!(ProbeState: ignore ExtImageCopyCaptureManagerV1);
delegate_noop!(ProbeState: ignore ExtForeignToplevelImageCaptureSourceManagerV1);
delegate_noop!(ProbeState: ignore ExtImageCaptureSourceV1);

sctk::delegate_registry!(ProbeState);
cctk::delegate_toplevel_info!(ProbeState);

fn pick_format(formats: &[wl_shm::Format]) -> Option<wl_shm::Format> {
    [
        wl_shm::Format::Argb8888,
        wl_shm::Format::Xrgb8888,
        wl_shm::Format::Abgr8888,
        wl_shm::Format::Xbgr8888,
    ]
    .into_iter()
    .find(|format| formats.contains(format))
}

fn roundtrip_until(
    queue: &mut cctk::wayland_client::EventQueue<ProbeState>,
    state: &mut ProbeState,
    predicate: impl Fn(&ProbeState) -> bool,
) -> Result<(), String> {
    for _ in 0..12 {
        if predicate(state) {
            return Ok(());
        }
        queue
            .roundtrip(state)
            .map_err(|error| format!("Wayland roundtrip failed: {error}"))?;
    }
    if predicate(state) {
        Ok(())
    } else {
        Err("Compositor did not complete the capture handshake".to_owned())
    }
}

fn capture_once(
    connection: &Connection,
    queue: &mut cctk::wayland_client::EventQueue<ProbeState>,
    state: &mut ProbeState,
    copy_manager: &ExtImageCopyCaptureManagerV1,
    source_manager: &ExtForeignToplevelImageCaptureSourceManagerV1,
    shm: &WlShm,
    handle: &ExtForeignToplevelHandleV1,
) -> Result<(), String> {
    let qh = queue.handle();
    state.capture.reset_session();

    let source: ExtImageCaptureSourceV1 = source_manager.create_source(handle, &qh, ());
    let session: ExtImageCopyCaptureSessionV1 =
        copy_manager.create_session(&source, Options::empty(), &qh, ());
    connection
        .flush()
        .map_err(|error| format!("Wayland flush failed: {error}"))?;

    let constraints = roundtrip_until(queue, state, |state| {
        state.capture.constraints_done || state.capture.session_stopped
    });
    if let Err(error) = constraints {
        session.destroy();
        source.destroy();
        return Err(error);
    }
    if state.capture.session_stopped {
        session.destroy();
        source.destroy();
        return Err("Capture session stopped".to_owned());
    }

    let width = state.capture.width;
    let height = state.capture.height;
    if width == 0 || height == 0 {
        session.destroy();
        source.destroy();
        return Err("Compositor returned a zero-sized capture".to_owned());
    }
    let Some(format) = pick_format(&state.capture.formats) else {
        session.destroy();
        source.destroy();
        return Err(format!(
            "No supported wl_shm format; advertised={:?}",
            state.capture.formats
        ));
    };

    let stride = width
        .checked_mul(4)
        .ok_or_else(|| "Capture stride overflow".to_owned())?;
    let byte_len = u64::from(stride)
        .checked_mul(u64::from(height))
        .ok_or_else(|| "Capture buffer size overflow".to_owned())?;
    let pool_len = i32::try_from(byte_len)
        .map_err(|_| format!("Capture buffer too large: {byte_len} bytes"))?;

    let fd = rustix::fs::memfd_create(c"tihulu-preview-probe", rustix::fs::MemfdFlags::CLOEXEC)
        .map_err(|error| format!("memfd_create failed: {error}"))?;
    rustix::fs::ftruncate(&fd, byte_len).map_err(|error| format!("ftruncate failed: {error}"))?;

    let pool = shm.create_pool(fd.as_fd(), pool_len, &qh, ());
    let buffer = pool.create_buffer(
        0,
        i32::try_from(width).map_err(|_| "Width does not fit i32".to_owned())?,
        i32::try_from(height).map_err(|_| "Height does not fit i32".to_owned())?,
        i32::try_from(stride).map_err(|_| "Stride does not fit i32".to_owned())?,
        format,
        &qh,
        (),
    );

    state.capture.reset_frame();
    let frame = session.create_frame(&qh, ());
    frame.attach_buffer(&buffer);
    frame.damage_buffer(
        0,
        0,
        i32::try_from(width).unwrap_or(i32::MAX),
        i32::try_from(height).unwrap_or(i32::MAX),
    );
    frame.capture();
    connection
        .flush()
        .map_err(|error| format!("Wayland capture flush failed: {error}"))?;

    let frame_result = roundtrip_until(queue, state, |state| {
        state.capture.frame_ready || state.capture.frame_failed || state.capture.session_stopped
    });

    frame.destroy();
    buffer.destroy();
    pool.destroy();
    session.destroy();
    source.destroy();
    drop(fd);
    let _ = connection.flush();

    frame_result?;
    if state.capture.frame_failed || state.capture.session_stopped {
        return Err("Frame capture failed or session stopped".to_owned());
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Default)]
struct ProcMetrics {
    fd: usize,
    rss_kb: u64,
}

fn pid_by_comm(name: &str) -> Option<u32> {
    let entries = fs::read_dir("/proc").ok()?;
    for entry in entries {
        let Ok(entry) = entry else {
            continue;
        };
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(comm) = fs::read_to_string(format!("/proc/{pid}/comm")) else {
            continue;
        };
        if comm.trim() == name {
            return Some(pid);
        }
    }
    None
}
fn proc_metrics(pid: u32) -> ProcMetrics {
    let fd = fs::read_dir(format!("/proc/{pid}/fd"))
        .map(|entries| entries.filter_map(Result::ok).count())
        .unwrap_or(0);
    let rss_kb = fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()
        .and_then(|status| {
            status.lines().find_map(|line| {
                line.strip_prefix("VmRSS:")?
                    .split_whitespace()
                    .next()?
                    .parse::<u64>()
                    .ok()
            })
        })
        .unwrap_or(0);
    ProcMetrics { fd, rss_kb }
}

fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("tihulu-minimized-windows")
}

fn parse_args() -> (usize, Option<String>) {
    let mut captures = DEFAULT_CAPTURES;
    let mut filter = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--captures" => {
                if let Some(value) = args.next().and_then(|value| value.parse::<usize>().ok()) {
                    captures = value.clamp(1, MAX_CAPTURES);
                }
            }
            "--app" => filter = args.next(),
            _ => {}
        }
    }
    (captures, filter)
}

fn open_csv() -> Result<(File, PathBuf), String> {
    let dir = runtime_dir();
    fs::create_dir_all(&dir).map_err(|error| format!("Could not create {dir:?}: {error}"))?;
    let path = dir.join("preview-probe.csv");
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&path)
        .map_err(|error| format!("Could not open {path:?}: {error}"))?;
    writeln!(
        file,
        "capture,success,cosmic_comp_fd,cosmic_comp_rss_kb,probe_fd"
    )
    .map_err(|error| format!("Could not write CSV header: {error}"))?;
    Ok((file, path))
}

fn main() -> Result<(), String> {
    let (captures, filter) = parse_args();
    eprintln!("Tihulu preview probe: ext-image-copy-capture stress test");
    eprintln!("Target captures: {captures}; hard maximum: {MAX_CAPTURES}");
    eprintln!("Minimize a matching window. The probe will stop early if FD growth looks unsafe.");

    let connection = Connection::connect_to_env()
        .map_err(|error| format!("Could not connect to Wayland: {error}"))?;
    let (globals, mut queue) = registry_queue_init::<ProbeState>(&connection)
        .map_err(|error| format!("Could not initialize Wayland registry: {error}"))?;
    let qh = queue.handle();
    let registry = RegistryState::new(&globals);

    let copy_manager = globals
        .bind::<ExtImageCopyCaptureManagerV1, _, _>(&qh, 1..=1, ())
        .map_err(|_| {
            "Compositor does not advertise ext_image_copy_capture_manager_v1".to_owned()
        })?;
    let source_manager = globals
        .bind::<ExtForeignToplevelImageCaptureSourceManagerV1, _, _>(&qh, 1..=1, ())
        .map_err(|_| {
            "Compositor does not advertise ext_foreign_toplevel_image_capture_source_manager_v1"
                .to_owned()
        })?;
    let shm = globals
        .bind::<WlShm, _, _>(&qh, 1..=1, ())
        .map_err(|error| format!("Could not bind wl_shm: {error}"))?;

    let mut state = ProbeState {
        toplevels: ToplevelInfoState::new(&registry, &qh),
        registry,
        target: None,
        target_filter: filter,
        capture: CaptureState::default(),
    };

    while state.target.is_none() {
        queue
            .blocking_dispatch(&mut state)
            .map_err(|error| format!("Wayland dispatch failed: {error}"))?;
    }
    let target = state.target.clone().expect("target checked above");

    let cosmic_pid = pid_by_comm("cosmic-comp")
        .ok_or_else(|| "Could not find cosmic-comp in /proc".to_owned())?;
    let baseline = proc_metrics(cosmic_pid);
    let probe_pid = std::process::id();
    let (mut csv, csv_path) = open_csv()?;
    eprintln!(
        "Baseline cosmic-comp: pid={cosmic_pid} fd={} rss={} KiB",
        baseline.fd, baseline.rss_kb
    );

    let mut previous_fd = baseline.fd;
    let mut growth_streak = 0_usize;
    let mut completed = 0_usize;

    for index in 1..=captures {
        let result = capture_once(
            &connection,
            &mut queue,
            &mut state,
            &copy_manager,
            &source_manager,
            &shm,
            &target,
        );
        let comp = proc_metrics(cosmic_pid);
        let probe = proc_metrics(probe_pid);
        let success = result.is_ok();
        writeln!(
            csv,
            "{index},{success},{},{},{}",
            comp.fd, comp.rss_kb, probe.fd
        )
        .map_err(|error| format!("Could not write probe CSV: {error}"))?;
        csv.flush().ok();

        if comp.fd > previous_fd {
            growth_streak += 1;
        } else {
            growth_streak = 0;
        }
        previous_fd = comp.fd;
        completed = index;

        eprintln!(
            "#{index:03} success={success} cosmic-comp FD={} ({:+}) RSS={} KiB probe FD={}",
            comp.fd,
            comp.fd as isize - baseline.fd as isize,
            comp.rss_kb,
            probe.fd
        );
        if let Err(error) = result {
            eprintln!("Capture error: {error}");
            break;
        }

        let fd_growth = comp.fd.saturating_sub(baseline.fd);
        let rss_growth = comp.rss_kb.saturating_sub(baseline.rss_kb);
        if fd_growth >= FD_GROWTH_LIMIT && growth_streak >= FD_GROWTH_STREAK_LIMIT {
            eprintln!(
                "CIRCUIT BREAKER: cosmic-comp FD count is growing monotonically. Capture path marked unsafe."
            );
            break;
        }
        if rss_growth >= RSS_GROWTH_LIMIT_KB && fd_growth >= 2 {
            eprintln!(
                "CIRCUIT BREAKER: cosmic-comp RSS grew by more than 512 MiB together with FD growth."
            );
            break;
        }

        std::thread::sleep(Duration::from_millis(80));
    }

    let final_metrics = proc_metrics(cosmic_pid);
    let final_growth = final_metrics.fd.saturating_sub(baseline.fd);
    eprintln!(
        "Probe completed {completed} capture(s). CSV: {}",
        csv_path.display()
    );
    if final_growth >= FD_GROWTH_LIMIT {
        eprintln!(
            "RESULT: UNSAFE / suspicious FD growth (+{final_growth}). Keep Safe Mode enabled."
        );
    } else {
        eprintln!(
            "RESULT: no large monotonic FD growth observed in this run (+{final_growth}). Repeat before enabling previews by default."
        );
    }
    Ok(())
}
