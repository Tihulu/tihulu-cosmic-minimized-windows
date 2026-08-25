// SPDX-License-Identifier: AGPL-3.0-only

use std::{collections::HashMap, os::fd::AsFd};

use cctk::wayland_client::{
    backend::ObjectId,
    event_created_child,
    protocol::{
        wl_buffer::WlBuffer,
        wl_registry::{self, WlRegistry},
        wl_shm::{self, WlShm},
        wl_shm_pool::WlShmPool,
    },
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum,
};
use cctk::wayland_protocols::ext::{
    foreign_toplevel_list::v1::client::{
        ext_foreign_toplevel_handle_v1::{self, ExtForeignToplevelHandleV1},
        ext_foreign_toplevel_list_v1::{self, ExtForeignToplevelListV1},
    },
    image_capture_source::v1::client::{
        ext_foreign_toplevel_image_capture_source_manager_v1::ExtForeignToplevelImageCaptureSourceManagerV1,
        ext_image_capture_source_v1::ExtImageCaptureSourceV1,
    },
    image_copy_capture::v1::client::{
        ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1},
        ext_image_copy_capture_manager_v1::{ExtImageCopyCaptureManagerV1, Options},
        ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
    },
};
use rustix::fs::{MemfdFlags, ftruncate, memfd_create};

#[derive(Debug)]
struct ToplevelInfo {
    handle: ExtForeignToplevelHandleV1,
    identifier: Option<String>,
    title: Option<String>,
    app_id: Option<String>,
    closed: bool,
}

#[derive(Debug, Default)]
struct CaptureState {
    size: Option<(u32, u32)>,
    formats: Vec<u32>,
    constraints_done: bool,
    ready: bool,
    failed: bool,
    fail_reason: Option<String>,
}

#[derive(Default)]
struct State {
    shm: Option<WlShm>,
    copy_mgr: Option<ExtImageCopyCaptureManagerV1>,
    source_mgr: Option<ExtForeignToplevelImageCaptureSourceManagerV1>,
    order: Vec<ObjectId>,
    toplevels: HashMap<ObjectId, ToplevelInfo>,
    capture: CaptureState,
}

pub(crate) struct ProbeWayland {
    queue: EventQueue<State>,
    state: State,
}

impl ProbeWayland {
    pub(crate) fn connect() -> Result<Self, String> {
        let connection =
            Connection::connect_to_env().map_err(|error| format!("Wayland: {error}"))?;
        let mut queue = connection.new_event_queue::<State>();
        let qh = queue.handle();
        let _registry = connection.display().get_registry(&qh, ());
        let mut state = State::default();

        for _ in 0..3 {
            queue
                .roundtrip(&mut state)
                .map_err(|error| format!("Wayland roundtrip failed: {error}"))?;
        }

        Ok(Self { queue, state })
    }

    pub(crate) fn list_toplevels(&self) {
        println!("Available ext-foreign-toplevel handles:");
        for (index, id) in self.state.order.iter().enumerate() {
            let Some(info) = self.state.toplevels.get(id) else {
                continue;
            };
            if info.closed {
                continue;
            }
            println!(
                "[{index}] app_id={:?} title={:?} identifier={:?}",
                info.app_id, info.title, info.identifier
            );
        }
    }

    pub(crate) fn source_for(
        &mut self,
        term: Option<&str>,
    ) -> Result<ExtImageCaptureSourceV1, String> {
        let handle = self.select_toplevel(term).ok_or_else(|| {
            "no matching toplevel found; run with --list, minimize the target, then use --match"
                .to_owned()
        })?;
        let source_mgr = self.state.source_mgr.clone().ok_or_else(|| {
            "compositor does not expose ext_foreign_toplevel_image_capture_source_manager_v1"
                .to_owned()
        })?;
        Ok(source_mgr.create_source(&handle, &self.queue.handle(), ()))
    }

    pub(crate) fn verify_capture_globals(&self) -> Result<(), String> {
        if self.state.copy_mgr.is_none() {
            return Err(
                "compositor does not expose ext_image_copy_capture_manager_v1".to_owned(),
            );
        }
        if self.state.source_mgr.is_none() {
            return Err(
                "compositor does not expose ext_foreign_toplevel_image_capture_source_manager_v1"
                    .to_owned(),
            );
        }
        if self.state.shm.is_none() {
            return Err("compositor does not expose wl_shm".to_owned());
        }
        Ok(())
    }

    pub(crate) fn capture_once(
        &mut self,
        source: &ExtImageCaptureSourceV1,
    ) -> Result<(), String> {
        let copy_mgr = self.state.copy_mgr.clone().ok_or_else(|| {
            "ext_image_copy_capture_manager_v1 disappeared from probe state".to_owned()
        })?;
        let shm = self
            .state
            .shm
            .clone()
            .ok_or_else(|| "wl_shm disappeared from probe state".to_owned())?;
        let qh = self.queue.handle();
        self.state.capture = CaptureState::default();

        let session = copy_mgr.create_session(source, Options::empty(), &qh, ());
        let constraints = self.roundtrip_until(|capture| {
            capture.constraints_done || capture.failed
        });
        if !constraints || self.state.capture.failed {
            let reason = self
                .state
                .capture
                .fail_reason
                .clone()
                .unwrap_or_else(|| "capture constraints unavailable".to_owned());
            session.destroy();
            let _ = self.queue.roundtrip(&mut self.state);
            return Err(reason);
        }

        let layout = Self::capture_layout(&self.state.capture);
        let (width, height, stride, size, size_i32, format) = match layout {
            Ok(layout) => layout,
            Err(error) => {
                session.destroy();
                let _ = self.queue.roundtrip(&mut self.state);
                return Err(error);
            }
        };

        let fd = match memfd_create("tihulu-preview-probe", MemfdFlags::CLOEXEC) {
            Ok(fd) => fd,
            Err(error) => {
                session.destroy();
                let _ = self.queue.roundtrip(&mut self.state);
                return Err(format!("memfd_create failed: {error}"));
            }
        };
        if let Err(error) = ftruncate(&fd, size as u64) {
            session.destroy();
            let _ = self.queue.roundtrip(&mut self.state);
            return Err(format!("ftruncate failed: {error}"));
        }

        let pool = shm.create_pool(fd.as_fd(), size_i32, &qh, ());
        let buffer = pool.create_buffer(
            0,
            width as i32,
            height as i32,
            stride as i32,
            format,
            &qh,
            (),
        );
        let frame = session.create_frame(&qh, ());
        frame.attach_buffer(&buffer);
        frame.damage_buffer(0, 0, width as i32, height as i32);
        frame.capture();

        let finished = self.roundtrip_until(|capture| capture.ready || capture.failed);
        let result = if finished && self.state.capture.ready && !self.state.capture.failed {
            Ok(())
        } else {
            Err(self
                .state
                .capture
                .fail_reason
                .clone()
                .unwrap_or_else(|| "frame did not become ready".to_owned()))
        };

        frame.destroy();
        buffer.destroy();
        pool.destroy();
        session.destroy();
        let _ = self.queue.roundtrip(&mut self.state);
        result
    }

    pub(crate) fn destroy_source(&mut self, source: ExtImageCaptureSourceV1) {
        source.destroy();
        let _ = self.queue.roundtrip(&mut self.state);
    }

    fn select_toplevel(&self, term: Option<&str>) -> Option<ExtForeignToplevelHandleV1> {
        let term = term.map(str::to_lowercase);
        self.state.order.iter().find_map(|id| {
            let info = self.state.toplevels.get(id)?;
            if info.closed {
                return None;
            }
            if let Some(term) = term.as_deref() {
                let haystack = format!(
                    "{} {} {}",
                    info.app_id.as_deref().unwrap_or_default(),
                    info.title.as_deref().unwrap_or_default(),
                    info.identifier.as_deref().unwrap_or_default()
                )
                .to_lowercase();
                if !haystack.contains(term) {
                    return None;
                }
            }
            Some(info.handle.clone())
        })
    }

    fn roundtrip_until(&mut self, done: impl Fn(&CaptureState) -> bool) -> bool {
        for _ in 0..64 {
            if done(&self.state.capture) {
                return true;
            }
            if self.queue.roundtrip(&mut self.state).is_err() {
                return false;
            }
        }
        done(&self.state.capture)
    }

    fn capture_layout(
        capture: &CaptureState,
    ) -> Result<(u32, u32, u32, usize, i32, wl_shm::Format), String> {
        let (width, height) = capture
            .size
            .ok_or_else(|| "session did not advertise buffer size".to_owned())?;
        if width == 0 || height == 0 {
            return Err(format!("invalid capture size {width}x{height}"));
        }

        const XR24: u32 = 0x3432_5258;
        const AR24: u32 = 0x3432_5241;
        let has_format = |wl: wl_shm::Format, drm: u32| {
            let wl = u32::from(wl);
            capture
                .formats
                .iter()
                .any(|format| *format == wl || *format == drm)
        };
        let format = if has_format(wl_shm::Format::Xrgb8888, XR24) {
            wl_shm::Format::Xrgb8888
        } else if has_format(wl_shm::Format::Argb8888, AR24) {
            wl_shm::Format::Argb8888
        } else {
            return Err(format!(
                "no supported XRGB/ARGB shm format; advertised={:?}",
                capture.formats
            ));
        };

        let stride = width
            .checked_mul(4)
            .ok_or_else(|| format!("stride overflow for width {width}"))?;
        let size_u32 = stride
            .checked_mul(height)
            .ok_or_else(|| format!("buffer size overflow for {width}x{height}"))?;
        let size =
            usize::try_from(size_u32).map_err(|_| "buffer does not fit usize".to_owned())?;
        let size_i32 =
            i32::try_from(size).map_err(|_| format!("buffer too large: {size} bytes"))?;
        Ok((width, height, stride, size, size_i32, format))
    }
}

impl Dispatch<WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };

        if interface == ExtForeignToplevelListV1::interface().name {
            registry.bind::<ExtForeignToplevelListV1, _, _>(
                name,
                version.min(ExtForeignToplevelListV1::interface().version),
                qh,
                (),
            );
        } else if interface == WlShm::interface().name {
            state.shm = Some(registry.bind::<WlShm, _, _>(
                name,
                version.min(WlShm::interface().version),
                qh,
                (),
            ));
        } else if interface == ExtImageCopyCaptureManagerV1::interface().name {
            state.copy_mgr = Some(registry.bind::<ExtImageCopyCaptureManagerV1, _, _>(
                name,
                version.min(ExtImageCopyCaptureManagerV1::interface().version),
                qh,
                (),
            ));
        } else if interface == ExtForeignToplevelImageCaptureSourceManagerV1::interface().name {
            state.source_mgr = Some(registry.bind::<
                ExtForeignToplevelImageCaptureSourceManagerV1,
                _,
                _,
            >(
                name,
                version.min(ExtForeignToplevelImageCaptureSourceManagerV1::interface().version),
                qh,
                (),
            ));
        }
    }
}

impl Dispatch<ExtForeignToplevelListV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ExtForeignToplevelListV1,
        event: ext_foreign_toplevel_list_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_foreign_toplevel_list_v1::Event::Toplevel { toplevel } = event {
            let id = toplevel.id();
            state.order.push(id.clone());
            state.toplevels.insert(
                id,
                ToplevelInfo {
                    handle: toplevel,
                    identifier: None,
                    title: None,
                    app_id: None,
                    closed: false,
                },
            );
        }
    }

    event_created_child!(State, ExtForeignToplevelListV1, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ExtForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ExtForeignToplevelHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        handle: &ExtForeignToplevelHandleV1,
        event: ext_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(info) = state.toplevels.get_mut(&handle.id()) else {
            return;
        };
        match event {
            ext_foreign_toplevel_handle_v1::Event::Identifier { identifier } => {
                info.identifier = Some(identifier);
            }
            ext_foreign_toplevel_handle_v1::Event::Title { title } => info.title = Some(title),
            ext_foreign_toplevel_handle_v1::Event::AppId { app_id } => info.app_id = Some(app_id),
            ext_foreign_toplevel_handle_v1::Event::Closed => info.closed = true,
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureSessionV1, ()> for State {
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
                state.capture.size = Some((width, height));
            }
            ext_image_copy_capture_session_v1::Event::ShmFormat { format } => {
                let raw = match format {
                    WEnum::Value(value) => value.into(),
                    WEnum::Unknown(value) => value,
                };
                state.capture.formats.push(raw);
            }
            ext_image_copy_capture_session_v1::Event::Done => {
                state.capture.constraints_done = true;
            }
            ext_image_copy_capture_session_v1::Event::Stopped => {
                state.capture.failed = true;
                state.capture.fail_reason = Some("session stopped".to_owned());
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ExtImageCopyCaptureFrameV1,
        event: ext_image_copy_capture_frame_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_image_copy_capture_frame_v1::Event::Ready => state.capture.ready = true,
            ext_image_copy_capture_frame_v1::Event::Failed { reason } => {
                state.capture.failed = true;
                state.capture.fail_reason = Some(format!("{reason:?}"));
            }
            _ => {}
        }
    }
}

macro_rules! ignore_events {
    ($($type:ty),+ $(,)?) => {$(
        impl Dispatch<$type, ()> for State {
            fn event(
                _: &mut Self,
                _: &$type,
                _: <$type as Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {}
        }
    )+};
}

ignore_events!(
    WlShm,
    WlShmPool,
    WlBuffer,
    ExtImageCaptureSourceV1,
    ExtImageCopyCaptureManagerV1,
    ExtForeignToplevelImageCaptureSourceManagerV1,
);
