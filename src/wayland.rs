// SPDX-License-Identifier: AGPL-3.0-only

use std::{
    collections::HashSet,
    os::{
        fd::{AsFd, FromRawFd, RawFd},
        unix::net::UnixStream,
    },
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
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
    wayland_client::{
        Connection, QueueHandle, WEnum, delegate_noop,
        globals::registry_queue_init,
        protocol::{wl_buffer, wl_seat::WlSeat, wl_shm, wl_shm_pool},
    },
    wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
};
use cosmic::iced::{Subscription, futures, stream};
use cosmic_protocols::{
    toplevel_info::v1::client::zcosmic_toplevel_handle_v1::{self, ZcosmicToplevelHandleV1},
    toplevel_management::v1::client::zcosmic_toplevel_manager_v1,
};
use futures::{SinkExt, channel::mpsc};
use sctk::registry::{ProvidesRegistryState, RegistryState};

const CAPTURE_TIMEOUT: Duration = Duration::from_secs(2);
const PREVIEW_MAX_WIDTH: u32 = 320;
const PREVIEW_MAX_HEIGHT: u32 = 180;

#[derive(Clone, Debug)]
pub struct PreviewImage {
    pub rgba: Vec<u8>,
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
    Preview(ExtForeignToplevelHandleV1, Option<PreviewImage>),
    Stopped,
}

#[derive(Clone, Debug)]
pub enum BridgeCommand {
    Restore(ExtForeignToplevelHandleV1),
    CapturePreview(ExtForeignToplevelHandleV1),
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
struct CaptureState {
    formats: Option<Formats>,
    completed: Option<bool>,
}

#[derive(Default)]
struct CaptureWaiter {
    state: Mutex<CaptureState>,
    changed: Condvar,
}

impl CaptureWaiter {
    fn set_formats(&self, formats: Formats) {
        if let Ok(mut state) = self.state.lock() {
            state.formats = Some(formats);
            self.changed.notify_all();
        }
    }

    fn finish(&self, success: bool) {
        if let Ok(mut state) = self.state.lock() {
            if state.completed.is_none() {
                state.completed = Some(success);
            }
            self.changed.notify_all();
        }
    }

    fn wait_formats(&self) -> Option<Formats> {
        let state = self.state.lock().ok()?;
        let (state, timeout) = self
            .changed
            .wait_timeout_while(state, CAPTURE_TIMEOUT, |state| state.formats.is_none())
            .ok()?;
        if timeout.timed_out() && state.formats.is_none() {
            return None;
        }
        state.formats.clone()
    }

    fn wait_completed(&self) -> Option<bool> {
        let state = self.state.lock().ok()?;
        let (state, timeout) = self
            .changed
            .wait_timeout_while(state, CAPTURE_TIMEOUT, |state| state.completed.is_none())
            .ok()?;
        if timeout.timed_out() && state.completed.is_none() {
            return None;
        }
        state.completed
    }
}

struct CaptureSessionData {
    waiter: Arc<CaptureWaiter>,
    protocol: ScreencopySessionData,
}

impl ScreencopySessionDataExt for CaptureSessionData {
    fn screencopy_session_data(&self) -> &ScreencopySessionData {
        &self.protocol
    }
}

struct CaptureFrameData {
    waiter: Arc<CaptureWaiter>,
    protocol: ScreencopyFrameData,
    _session: CaptureSession,
}

impl ScreencopyFrameDataExt for CaptureFrameData {
    fn screencopy_frame_data(&self) -> &ScreencopyFrameData {
        &self.protocol
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
    fn capture(&self, handle: ExtForeignToplevelHandleV1) -> Option<PreviewImage> {
        let waiter = Arc::new(CaptureWaiter::default());
        let session = self
            .capturer
            .create_session(
                &CaptureSource::Toplevel(handle),
                CaptureOptions::empty(),
                &self.qh,
                CaptureSessionData {
                    waiter: waiter.clone(),
                    protocol: ScreencopySessionData::default(),
                },
            )
            .ok()?;
        self.connection.flush().ok()?;

        let formats = waiter.wait_formats()?;
        let (width, height) = formats.buffer_size;
        if width == 0 || height == 0 || !formats.shm_formats.contains(&wl_shm::Format::Abgr8888) {
            return None;
        }

        let byte_len = usize::try_from(width)
            .ok()?
            .checked_mul(usize::try_from(height).ok()?)?
            .checked_mul(4)?;
        let pool_len = i32::try_from(byte_len).ok()?;

        let fd =
            rustix::fs::memfd_create(c"tihulu-minimized-preview", rustix::fs::MemfdFlags::CLOEXEC)
                .ok()?;
        rustix::fs::ftruncate(&fd, u64::try_from(byte_len).ok()?).ok()?;

        let pool = self.wl_shm.create_pool(fd.as_fd(), pool_len, &self.qh, ());
        let buffer = pool.create_buffer(
            0,
            i32::try_from(width).ok()?,
            i32::try_from(height).ok()?,
            i32::try_from(width.checked_mul(4)?).ok()?,
            wl_shm::Format::Abgr8888,
            &self.qh,
            (),
        );

        let frame = session.capture(
            &buffer,
            &[],
            &self.qh,
            CaptureFrameData {
                waiter: waiter.clone(),
                protocol: ScreencopyFrameData::default(),
                _session: session.clone(),
            },
        );
        if self.connection.flush().is_err() {
            buffer.destroy();
            pool.destroy();
            return None;
        }

        let completed = waiter.wait_completed().unwrap_or(false);
        buffer.destroy();
        pool.destroy();
        drop(frame);
        drop(session);
        if !completed {
            return None;
        }

        let mmap = unsafe { memmap2::MmapOptions::new().len(byte_len).map(&fd).ok()? };
        let mut image = image::RgbaImage::from_raw(width, height, mmap.to_vec())?;
        drop(mmap);
        drop(fd);

        let scale = (PREVIEW_MAX_WIDTH as f64 / f64::from(width))
            .min(PREVIEW_MAX_HEIGHT as f64 / f64::from(height))
            .min(1.0);
        if scale < 1.0 {
            let target_width = (f64::from(width) * scale).round().max(1.0) as u32;
            let target_height = (f64::from(height) * scale).round().max(1.0) as u32;
            image = image::imageops::resize(
                &image,
                target_width,
                target_height,
                image::imageops::FilterType::Triangle,
            );
        }

        Some(PreviewImage {
            width: image.width(),
            height: image.height(),
            rgba: image.into_raw(),
        })
    }
}

struct BridgeState {
    done: bool,
    out: mpsc::Sender<BridgeEvent>,
    registry: RegistryState,
    seats: SeatState,
    toplevels: ToplevelInfoState,
    manager: ToplevelManagerState,
    screencopy: ScreencopyState,
    shm: Shm,
    qh: QueueHandle<Self>,
    connection: Connection,
    shown: HashSet<ExtForeignToplevelHandleV1>,
    capture_busy: Arc<AtomicBool>,
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
                self.emit(BridgeEvent::Window(Box::new(WindowDelta::Gone(
                    handle.clone(),
                ))));
            }
            (false, false) => {}
        }
    }

    fn forget(&mut self, handle: &ExtForeignToplevelHandleV1) {
        if self.shown.remove(handle) {
            self.emit(BridgeEvent::Window(Box::new(WindowDelta::Gone(
                handle.clone(),
            ))));
        }
    }

    fn request_preview(&mut self, handle: ExtForeignToplevelHandleV1) {
        if self.capture_busy.swap(true, Ordering::AcqRel) {
            return;
        }

        let context = CaptureContext {
            qh: self.qh.clone(),
            connection: self.connection.clone(),
            wl_shm: self.shm.wl_shm().clone(),
            capturer: self.screencopy.capturer().clone(),
        };
        let busy = self.capture_busy.clone();
        let mut out = self.out.clone();

        std::thread::spawn(move || {
            let result = catch_unwind(AssertUnwindSafe(|| context.capture(handle.clone())))
                .ok()
                .flatten();
            busy.store(false, Ordering::Release);
            let _ = futures::executor::block_on(out.send(BridgeEvent::Preview(handle, result)));
        });
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
        if let Some(data) = session.data::<CaptureSessionData>() {
            data.waiter.set_formats(formats.clone());
        }
    }

    fn stopped(&mut self, _: &Connection, _: &QueueHandle<Self>, session: &CaptureSession) {
        if let Some(data) = session.data::<CaptureSessionData>() {
            data.waiter.finish(false);
        }
    }

    fn ready(&mut self, _: &Connection, _: &QueueHandle<Self>, frame: &CaptureFrame, _: Frame) {
        if let Some(data) = frame.data::<CaptureFrameData>() {
            data.waiter.finish(true);
        }
    }

    fn failed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        frame: &CaptureFrame,
        _: WEnum<FailureReason>,
    ) {
        if let Some(data) = frame.data::<CaptureFrameData>() {
            data.waiter.finish(false);
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

    if loop_handle
        .insert_source(commands, |event, (), state| match event {
            calloop::channel::Event::Msg(BridgeCommand::Restore(handle)) => {
                let seat = state.seats.seats().next();
                let cosmic = state.cosmic_handle(&handle);
                if let (Some(seat), Some(cosmic)) = (seat, cosmic) {
                    state.manager.manager.activate(&cosmic, &seat);
                }
            }
            calloop::channel::Event::Msg(BridgeCommand::CapturePreview(handle)) => {
                state.request_preview(handle);
            }
            calloop::channel::Event::Closed => state.done = true,
        })
        .is_err()
    {
        return;
    }

    let registry = RegistryState::new(&globals);
    let Ok(shm) = Shm::bind(&globals, &qh) else {
        tracing::error!("Could not bind wl_shm for hover previews");
        return;
    };
    let screencopy = ScreencopyState::new(&globals, &qh);
    let mut state = BridgeState {
        done: false,
        out,
        seats: SeatState::new(&globals, &qh),
        toplevels: ToplevelInfoState::new(&registry, &qh),
        manager: ToplevelManagerState::new(&registry, &qh),
        screencopy,
        shm,
        qh,
        connection,
        registry,
        shown: HashSet::new(),
        capture_busy: Arc::new(AtomicBool::new(false)),
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
delegate_noop!(BridgeState: ignore wl_shm_pool::WlShmPool);
