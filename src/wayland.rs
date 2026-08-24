// SPDX-License-Identifier: AGPL-3.0-only

use std::{
    collections::HashSet,
    fs::File,
    io::{Read, Seek, SeekFrom},
    os::{
        fd::{AsFd, FromRawFd, RawFd},
        unix::net::UnixStream,
    },
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use cctk::wayland_client::{
    Connection, QueueHandle, WEnum, delegate_noop,
    globals::registry_queue_init,
    protocol::{wl_buffer, wl_seat::WlSeat, wl_shm, wl_shm_pool},
};
use cctk::{
    screencopy::{
        CaptureFrame, CaptureOptions, CaptureSession, CaptureSource, Capturer, FailureReason,
        Formats, Frame, ScreencopyFrameData, ScreencopyFrameDataExt, ScreencopyHandler,
        ScreencopySessionData, ScreencopySessionDataExt, ScreencopyState,
    },
    sctk::{
        self,
        reexports::{calloop, calloop_wayland_source::WaylandSource},
        seat::{SeatHandler, SeatState},
        shm::{Shm, ShmHandler},
    },
    toplevel_info::{ToplevelInfo, ToplevelInfoHandler, ToplevelInfoState},
    toplevel_management::{ToplevelManagerHandler, ToplevelManagerState},
};
use cosmic::{
    cctk::{
        cosmic_protocols::toplevel_info::v1::client::zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1,
        wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    },
    iced::{Subscription, core::Bytes, futures, stream},
};
use cosmic_protocols::{
    toplevel_info::v1::client::zcosmic_toplevel_handle_v1,
    toplevel_management::v1::client::zcosmic_toplevel_manager_v1,
};
use futures::{SinkExt, channel::mpsc};
use sctk::registry::{ProvidesRegistryState, RegistryState};

const PREVIEW_MAX_WIDTH: u32 = 320;
const PREVIEW_MAX_HEIGHT: u32 = 180;
const CAPTURE_TIMEOUT: Duration = Duration::from_millis(1500);

#[derive(Clone, Debug)]
pub struct PreviewImage {
    pub pixels: Bytes,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug)]
pub enum WindowDelta {
    Present(Box<ToplevelInfo>),
    Gone(ExtForeignToplevelHandleV1),
}

#[derive(Clone, Debug)]
pub enum BridgeEvent {
    Ready(calloop::channel::Sender<BridgeCommand>),
    Window(Box<WindowDelta>),
    Preview {
        token: u64,
        handle: ExtForeignToplevelHandleV1,
        image: Option<PreviewImage>,
    },
    Stopped,
}

#[derive(Clone, Debug)]
pub enum BridgeCommand {
    Restore(ExtForeignToplevelHandleV1),
    CapturePreview {
        token: u64,
        handle: ExtForeignToplevelHandleV1,
    },
    CancelPreview {
        token: u64,
    },
}

pub fn subscription() -> Subscription<BridgeEvent> {
    Subscription::run_with(std::any::TypeId::of::<BridgeEvent>(), |_| {
        stream::channel(
            8,
            move |mut output: futures::channel::mpsc::Sender<BridgeEvent>| async move {
                let (command_tx, command_rx) = calloop::channel::channel();
                let runtime = tokio::runtime::Handle::current();

                std::thread::spawn(move || {
                    runtime.block_on(async move {
                        if output.send(BridgeEvent::Ready(command_tx)).await.is_err() {
                            return;
                        }
                        bridge_loop(output.clone(), command_rx);
                        let _ = output.send(BridgeEvent::Stopped).await;
                    });
                });

                futures::future::pending().await
            },
        )
    })
}

#[derive(Default)]
struct CaptureWaitInner {
    formats: Option<Formats>,
    result: Option<bool>,
}

#[derive(Default)]
struct CaptureWait {
    inner: Mutex<CaptureWaitInner>,
    condvar: Condvar,
}

impl CaptureWait {
    fn set_formats(&self, formats: Formats) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.formats = Some(formats);
            self.condvar.notify_all();
        }
    }

    fn set_result(&self, result: bool) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.result = Some(result);
            self.condvar.notify_all();
        }
    }

    fn wait_formats(&self) -> Option<Formats> {
        let guard = self
            .condvar
            .wait_timeout_while(self.inner.lock().ok()?, CAPTURE_TIMEOUT, |inner| {
                inner.formats.is_none()
            })
            .ok()?
            .0;
        guard.formats.clone()
    }

    fn wait_result(&self) -> Option<bool> {
        let guard = self
            .condvar
            .wait_timeout_while(self.inner.lock().ok()?, CAPTURE_TIMEOUT, |inner| {
                inner.result.is_none()
            })
            .ok()?
            .0;
        guard.result
    }
}

struct PreviewSessionData {
    wait: Arc<CaptureWait>,
    base: ScreencopySessionData,
}

impl ScreencopySessionDataExt for PreviewSessionData {
    fn screencopy_session_data(&self) -> &ScreencopySessionData {
        &self.base
    }
}

struct PreviewFrameData {
    wait: Arc<CaptureWait>,
    base: ScreencopyFrameData,
    _session: CaptureSession,
}

impl ScreencopyFrameDataExt for PreviewFrameData {
    fn screencopy_frame_data(&self) -> &ScreencopyFrameData {
        &self.base
    }
}

#[derive(Clone)]
struct CaptureContext {
    qh: QueueHandle<BridgeState>,
    connection: Connection,
    wl_shm: wl_shm::WlShm,
    capturer: Capturer,
}

impl CaptureContext {
    fn capture(
        &self,
        source: ExtForeignToplevelHandleV1,
        cancelled: &AtomicBool,
    ) -> Option<PreviewImage> {
        if cancelled.load(Ordering::Acquire) {
            return None;
        }

        let wait = Arc::new(CaptureWait::default());
        let session = self
            .capturer
            .create_session(
                &CaptureSource::Toplevel(source),
                CaptureOptions::empty(),
                &self.qh,
                PreviewSessionData {
                    wait: wait.clone(),
                    base: ScreencopySessionData::default(),
                },
            )
            .ok()?;

        if self.connection.flush().is_err() {
            return None;
        }

        let formats = wait.wait_formats()?;
        if cancelled.load(Ordering::Acquire) {
            return None;
        }

        let (width, height) = formats.buffer_size;
        if width == 0 || height == 0 || !formats.shm_formats.contains(&wl_shm::Format::Abgr8888) {
            return None;
        }

        let pixel_count = width.checked_mul(height)?;
        let buffer_len = pixel_count.checked_mul(4)?;
        if buffer_len > i32::MAX as u32 {
            return None;
        }

        let fd =
            rustix::fs::memfd_create(c"tihulu-minimized-preview", rustix::fs::MemfdFlags::CLOEXEC)
                .ok()?;
        rustix::fs::ftruncate(&fd, u64::from(buffer_len)).ok()?;

        let pool = self
            .wl_shm
            .create_pool(fd.as_fd(), buffer_len as i32, &self.qh, ());
        let buffer = pool.create_buffer(
            0,
            width as i32,
            height as i32,
            width.saturating_mul(4) as i32,
            wl_shm::Format::Abgr8888,
            &self.qh,
            (),
        );

        let frame = session.capture(
            &buffer,
            &[],
            &self.qh,
            PreviewFrameData {
                wait: wait.clone(),
                base: ScreencopyFrameData::default(),
                _session: session.clone(),
            },
        );

        let flushed = self.connection.flush().is_ok();
        let ready = flushed && wait.wait_result().unwrap_or(false);

        drop(frame);
        drop(session);
        buffer.destroy();
        pool.destroy();
        let _ = self.connection.flush();

        if !ready || cancelled.load(Ordering::Acquire) {
            return None;
        }

        let mut file = File::from(fd);
        file.seek(SeekFrom::Start(0)).ok()?;
        let mut raw = vec![0_u8; buffer_len as usize];
        file.read_exact(&mut raw).ok()?;
        drop(file);

        if cancelled.load(Ordering::Acquire) {
            return None;
        }

        let image = image::RgbaImage::from_raw(width, height, raw)?;
        let thumbnail = image::imageops::thumbnail(&image, PREVIEW_MAX_WIDTH, PREVIEW_MAX_HEIGHT);
        let preview_width = thumbnail.width();
        let preview_height = thumbnail.height();
        let pixels = thumbnail.into_raw();

        Some(PreviewImage {
            pixels: Bytes::copy_from_slice(&pixels),
            width: preview_width,
            height: preview_height,
        })
    }
}

#[derive(Clone, Debug)]
struct CaptureJob {
    token: u64,
    handle: ExtForeignToplevelHandleV1,
}

#[derive(Debug)]
struct CaptureWorkerResult {
    job: CaptureJob,
    image: Option<PreviewImage>,
    cancelled: bool,
}

struct BridgeState {
    done: bool,
    out: mpsc::Sender<BridgeEvent>,
    connection: Connection,
    qh: QueueHandle<Self>,
    registry: RegistryState,
    seats: SeatState,
    toplevels: ToplevelInfoState,
    manager: ToplevelManagerState,
    screencopy: ScreencopyState,
    shm: Shm,
    shown: HashSet<ExtForeignToplevelHandleV1>,
    capture_done_tx: calloop::channel::Sender<CaptureWorkerResult>,
    capture_running: Option<CaptureJob>,
    capture_cancel: Option<Arc<AtomicBool>>,
    capture_pending: Option<CaptureJob>,
}

impl BridgeState {
    fn cosmic_handle(
        &self,
        foreign: &ExtForeignToplevelHandleV1,
    ) -> Option<ZcosmicToplevelHandleV1> {
        self.toplevels.info(foreign)?.cosmic_toplevel.clone()
    }

    fn emit(&mut self, event: BridgeEvent) {
        if futures::executor::block_on(self.out.send(event)).is_err() {
            self.done = true;
        }
    }

    fn reconsider(&mut self, handle: &ExtForeignToplevelHandleV1) {
        let Some(info) = self.toplevels.info(handle).cloned() else {
            return;
        };

        let minimized = info
            .state
            .contains(&zcosmic_toplevel_handle_v1::State::Minimized);

        match (minimized, self.shown.contains(handle)) {
            (true, _) => {
                self.shown.insert(handle.clone());
                self.emit(BridgeEvent::Window(Box::new(WindowDelta::Present(
                    Box::new(info),
                ))));
            }
            (false, true) => {
                self.shown.remove(handle);
                self.cancel_capture_for(handle);
                self.emit(BridgeEvent::Window(Box::new(WindowDelta::Gone(
                    handle.clone(),
                ))));
            }
            (false, false) => {}
        }
    }

    fn forget(&mut self, handle: &ExtForeignToplevelHandleV1) {
        self.cancel_capture_for(handle);
        if self.shown.remove(handle) {
            self.emit(BridgeEvent::Window(Box::new(WindowDelta::Gone(
                handle.clone(),
            ))));
        }
    }

    fn capture_context(&self) -> CaptureContext {
        CaptureContext {
            qh: self.qh.clone(),
            connection: self.connection.clone(),
            wl_shm: self.shm.wl_shm().clone(),
            capturer: self.screencopy.capturer().clone(),
        }
    }

    fn request_capture(&mut self, job: CaptureJob) {
        if self.capture_running.is_some() {
            if let Some(cancel) = &self.capture_cancel {
                cancel.store(true, Ordering::Release);
            }
            self.capture_pending = Some(job);
            return;
        }
        self.start_capture(job);
    }

    fn start_capture(&mut self, job: CaptureJob) {
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancelled_worker = cancelled.clone();
        let context = self.capture_context();
        let done = self.capture_done_tx.clone();
        let worker_job = job.clone();

        self.capture_running = Some(job);
        self.capture_cancel = Some(cancelled);

        std::thread::spawn(move || {
            let image = context.capture(worker_job.handle.clone(), &cancelled_worker);
            let result = CaptureWorkerResult {
                job: worker_job,
                image,
                cancelled: cancelled_worker.load(Ordering::Acquire),
            };
            let _ = done.send(result);
        });
    }

    fn cancel_token(&mut self, token: u64) {
        if self.capture_running.as_ref().map(|job| job.token) == Some(token)
            && let Some(cancel) = &self.capture_cancel
        {
            cancel.store(true, Ordering::Release);
        }
        if self.capture_pending.as_ref().map(|job| job.token) == Some(token) {
            self.capture_pending = None;
        }
    }

    fn cancel_capture_for(&mut self, handle: &ExtForeignToplevelHandleV1) {
        if self
            .capture_running
            .as_ref()
            .is_some_and(|job| &job.handle == handle)
            && let Some(cancel) = &self.capture_cancel
        {
            cancel.store(true, Ordering::Release);
        }
        if self
            .capture_pending
            .as_ref()
            .is_some_and(|job| &job.handle == handle)
        {
            self.capture_pending = None;
        }
    }

    fn capture_finished(&mut self, result: CaptureWorkerResult) {
        let is_current =
            self.capture_running.as_ref().map(|job| job.token) == Some(result.job.token);
        if is_current {
            self.capture_running = None;
            self.capture_cancel = None;
        }

        if is_current && !result.cancelled {
            self.emit(BridgeEvent::Preview {
                token: result.job.token,
                handle: result.job.handle,
                image: result.image,
            });
        }

        if self.capture_running.is_none()
            && let Some(next) = self.capture_pending.take()
        {
            self.start_capture(next);
        }
    }
}

impl ProvidesRegistryState for BridgeState {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }

    sctk::registry_handlers!();
}

impl SeatHandler for BridgeState {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}
    fn new_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: WlSeat,
        _: sctk::seat::Capability,
    ) {
    }
    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: WlSeat,
        _: sctk::seat::Capability,
    ) {
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}
}

impl ShmHandler for BridgeState {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ScreencopyHandler for BridgeState {
    fn screencopy_state(&mut self) -> &mut ScreencopyState {
        &mut self.screencopy
    }

    fn init_done(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        session: &CaptureSession,
        formats: &Formats,
    ) {
        if let Some(data) = session.data::<PreviewSessionData>() {
            data.wait.set_formats(formats.clone());
        }
    }

    fn stopped(&mut self, _: &Connection, _: &QueueHandle<Self>, session: &CaptureSession) {
        if let Some(data) = session.data::<PreviewSessionData>() {
            data.wait.set_result(false);
        }
    }

    fn ready(&mut self, _: &Connection, _: &QueueHandle<Self>, frame: &CaptureFrame, _: Frame) {
        if let Some(data) = frame.data::<PreviewFrameData>() {
            data.wait.set_result(true);
        }
    }

    fn failed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        frame: &CaptureFrame,
        _: WEnum<FailureReason>,
    ) {
        if let Some(data) = frame.data::<PreviewFrameData>() {
            data.wait.set_result(false);
        }
    }
}

impl ToplevelInfoHandler for BridgeState {
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
        self.forget(handle);
    }
}

impl ToplevelManagerHandler for BridgeState {
    fn toplevel_manager_state(&mut self) -> &mut ToplevelManagerState {
        &mut self.manager
    }

    fn capabilities(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: Vec<WEnum<zcosmic_toplevel_manager_v1::ZcosmicToplelevelManagementCapabilitiesV1>>,
    ) {
    }
}

impl cctk::wayland_client::Dispatch<wl_shm_pool::WlShmPool, ()> for BridgeState {
    fn event(
        _: &mut Self,
        _: &wl_shm_pool::WlShmPool,
        _: wl_shm_pool::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

fn bridge_loop(out: mpsc::Sender<BridgeEvent>, commands: calloop::channel::Channel<BridgeCommand>) {
    let privileged = std::env::var("X_PRIVILEGED_WAYLAND_SOCKET")
        .ok()
        .and_then(|value| value.parse::<RawFd>().ok())
        .map(|fd| unsafe { UnixStream::from_raw_fd(fd) });

    let connection = match privileged {
        Some(socket) => Connection::from_socket(socket),
        None => Connection::connect_to_env(),
    };
    let Ok(connection) = connection else {
        tracing::error!("Could not connect minimized-windows bridge to Wayland");
        return;
    };

    let Ok((globals, queue)) = registry_queue_init(&connection) else {
        tracing::error!("Could not initialize Wayland registry");
        return;
    };

    let Ok(mut loop_) = calloop::EventLoop::<BridgeState>::try_new() else {
        tracing::error!("Could not create Wayland event loop");
        return;
    };

    let qh = queue.handle();
    let source = WaylandSource::new(connection.clone(), queue);
    let loop_handle = loop_.handle();
    if source.insert(loop_handle.clone()).is_err() {
        return;
    }

    let (capture_done_tx, capture_done_rx) = calloop::channel::channel();
    if loop_handle
        .insert_source(capture_done_rx, |event, (), state| {
            if let calloop::channel::Event::Msg(result) = event {
                state.capture_finished(result);
            }
        })
        .is_err()
    {
        return;
    }

    if loop_handle
        .insert_source(commands, |event, (), state| match event {
            calloop::channel::Event::Msg(BridgeCommand::Restore(handle)) => {
                let seat = state.seats.seats().next();
                let cosmic = state.cosmic_handle(&handle);
                if let (Some(seat), Some(cosmic)) = (seat, cosmic) {
                    state.manager.manager.activate(&cosmic, &seat);
                }
            }
            calloop::channel::Event::Msg(BridgeCommand::CapturePreview { token, handle }) => {
                state.request_capture(CaptureJob { token, handle });
            }
            calloop::channel::Event::Msg(BridgeCommand::CancelPreview { token }) => {
                state.cancel_token(token);
            }
            calloop::channel::Event::Closed => state.done = true,
        })
        .is_err()
    {
        return;
    }

    let registry = RegistryState::new(&globals);
    let screencopy = ScreencopyState::new(&globals, &qh);
    let Ok(shm) = Shm::bind(&globals, &qh) else {
        tracing::error!("Could not bind Wayland SHM for minimized-window previews");
        return;
    };

    let mut state = BridgeState {
        done: false,
        out,
        connection,
        qh: qh.clone(),
        seats: SeatState::new(&globals, &qh),
        toplevels: ToplevelInfoState::new(&registry, &qh),
        manager: ToplevelManagerState::new(&registry, &qh),
        screencopy,
        shm,
        registry,
        shown: HashSet::new(),
        capture_done_tx,
        capture_running: None,
        capture_cancel: None,
        capture_pending: None,
    };

    while !state.done {
        if let Err(error) = loop_.dispatch(None, &mut state) {
            tracing::error!(?error, "Wayland bridge dispatch failed");
            break;
        }
    }
}

sctk::delegate_shm!(BridgeState);
sctk::delegate_seat!(BridgeState);
sctk::delegate_registry!(BridgeState);
cctk::delegate_toplevel_info!(BridgeState);
cctk::delegate_toplevel_manager!(BridgeState);
cctk::delegate_screencopy!(BridgeState);
delegate_noop!(BridgeState: ignore wl_buffer::WlBuffer);
