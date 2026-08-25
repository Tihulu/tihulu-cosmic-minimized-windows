// SPDX-License-Identifier: AGPL-3.0-only

use std::{
    collections::HashMap,
    fs::File,
    io::{Read, Seek, SeekFrom},
    os::fd::AsFd,
};

use cctk::wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum,
    backend::ObjectId,
    event_created_child,
    protocol::{
        wl_buffer::WlBuffer,
        wl_registry::{self, WlRegistry},
        wl_shm::{self, WlShm},
        wl_shm_pool::WlShmPool,
    },
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

#[derive(Clone, Copy, Debug)]
enum PixelLayout {
    Xrgb,
    Argb,
    Xbgr,
    Abgr,
}

pub(crate) struct CapturedFrame {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) rgba: Vec<u8>,
}

pub(crate) struct CaptureWayland {
    queue: EventQueue<State>,
    state: State,
}

impl CaptureWayland {
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

        let this = Self { queue, state };
        this.verify_globals()?;
        Ok(this)
    }

    pub(crate) fn capture_identifier(&mut self, identifier: &str) -> Result<CapturedFrame, String> {
        for _ in 0..2 {
            self.queue
                .roundtrip(&mut self.state)
                .map_err(|error| format!("Wayland refresh failed: {error}"))?;
        }

        let handle = self
            .select_identifier(identifier)
            .ok_or_else(|| format!("no toplevel with identifier {identifier:?}"))?;
        let source_mgr = self
            .state
            .source_mgr
            .clone()
            .ok_or_else(|| "capture source manager unavailable".to_owned())?;
        let source = source_mgr.create_source(&handle, &self.queue.handle(), ());
        let result = self.capture_source(&source);
        source.destroy();
        let _ = self.queue.roundtrip(&mut self.state);
        result
    }

    fn verify_globals(&self) -> Result<(), String> {
        if self.state.copy_mgr.is_none() {
            return Err("compositor does not expose ext_image_copy_capture_manager_v1".to_owned());
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

    fn capture_source(&mut self, source: &ExtImageCaptureSourceV1) -> Result<CapturedFrame, String> {
        let copy_mgr = self
            .state
            .copy_mgr
            .clone()
            .ok_or_else(|| "image-copy manager unavailable".to_owned())?;
        let shm = self
            .state
            .shm
            .clone()
            .ok_or_else(|| "wl_shm unavailable".to_owned())?;
        let qh = self.queue.handle();
        self.state.capture = CaptureState::default();

        let session = copy_mgr.create_session(source, Options::empty(), &qh, ());
        let constraints = self.roundtrip_until(|capture| capture.constraints_done || capture.failed);
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

        let (width, height, stride, size, size_i32, format, layout) =
            match Self::capture_layout(&self.state.capture) {
                Ok(layout) => layout,
                Err(error) => {
                    session.destroy();
                    let _ = self.queue.roundtrip(&mut self.state);
                    return Err(error);
                }
            };

        let fd = match memfd_create("tihulu-previewd-capture", MemfdFlags::CLOEXEC) {
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
        let mut file = File::from(fd);

        let pool = shm.create_pool(file.as_fd(), size_i32, &qh, ());
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
        if !finished || !self.state.capture.ready || self.state.capture.failed {
            let reason = self
                .state
                .capture
                .fail_reason
                .clone()
                .unwrap_or_else(|| "frame did not become ready".to_owned());
            frame.destroy();
            buffer.destroy();
            pool.destroy();
            session.destroy();
            let _ = self.queue.roundtrip(&mut self.state);
            return Err(reason);
        }

        file.seek(SeekFrom::Start(0))
            .map_err(|error| format!("capture seek failed: {error}"))?;
        let mut raw = vec![0_u8; size];
        file.read_exact(&mut raw)
            .map_err(|error| format!("capture read failed: {error}"))?;

        frame.destroy();
        buffer.destroy();
        pool.destroy();
        session.destroy();
        let _ = self.queue.roundtrip(&mut self.state);

        let rgba = Self::to_rgba(&raw, layout);
        Ok(CapturedFrame {
            width,
            height,
            rgba,
        })
    }

    fn select_identifier(&self, identifier: &str) -> Option<ExtForeignToplevelHandleV1> {
        self.state.order.iter().find_map(|id| {
            let info = self.state.toplevels.get(id)?;
            (!info.closed && info.identifier.as_deref() == Some(identifier))
                .then(|| info.handle.clone())
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
    ) -> Result<(u32, u32, u32, usize, i32, wl_shm::Format, PixelLayout), String> {
        let (width, height) = capture
            .size
            .ok_or_else(|| "session did not advertise buffer size".to_owned())?;
        if width == 0 || height == 0 {
            return Err(format!("invalid capture size {width}x{height}"));
        }

        const XR24: u32 = 0x3432_5258;
        const AR24: u32 = 0x3432_5241;
        const XB24: u32 = 0x3432_4258;
        const AB24: u32 = 0x3432_4241;
        let has_format = |wl: wl_shm::Format, drm: u32| {
            let wl = u32::from(wl);
            capture
                .formats
                .iter()
                .any(|format| *format == wl || *format == drm)
        };
        let (format, layout) = if has_format(wl_shm::Format::Xrgb8888, XR24) {
            (wl_shm::Format::Xrgb8888, PixelLayout::Xrgb)
        } else if has_format(wl_shm::Format::Argb8888, AR24) {
            (wl_shm::Format::Argb8888, PixelLayout::Argb)
        } else if has_format(wl_shm::Format::Xbgr8888, XB24) {
            (wl_shm::Format::Xbgr8888, PixelLayout::Xbgr)
        } else if has_format(wl_shm::Format::Abgr8888, AB24) {
            (wl_shm::Format::Abgr8888, PixelLayout::Abgr)
        } else {
            return Err(format!(
                "no supported 32-bit RGB shm format; advertised={:?}",
                capture.formats
            ));
        };

        let stride = width
            .checked_mul(4)
            .ok_or_else(|| format!("stride overflow for width {width}"))?;
        let size_u32 = stride
            .checked_mul(height)
            .ok_or_else(|| format!("buffer size overflow for {width}x{height}"))?;
        let size = usize::try_from(size_u32).map_err(|_| "buffer does not fit usize".to_owned())?;
        let size_i32 =
            i32::try_from(size).map_err(|_| format!("buffer too large: {size} bytes"))?;
        Ok((width, height, stride, size, size_i32, format, layout))
    }

    fn to_rgba(raw: &[u8], layout: PixelLayout) -> Vec<u8> {
        let mut rgba = Vec::with_capacity(raw.len());
        for pixel in raw.chunks_exact(4) {
            let (r, g, b, a) = match layout {
                PixelLayout::Xrgb => (pixel[2], pixel[1], pixel[0], 255),
                PixelLayout::Argb => (pixel[2], pixel[1], pixel[0], pixel[3]),
                PixelLayout::Xbgr => (pixel[0], pixel[1], pixel[2], 255),
                PixelLayout::Abgr => (pixel[0], pixel[1], pixel[2], pixel[3]),
            };
            rgba.extend_from_slice(&[r, g, b, a]);
        }
        rgba
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
            state.source_mgr = Some(
                registry.bind::<ExtForeignToplevelImageCaptureSourceManagerV1, _, _>(
                    name,
                    version.min(ExtForeignToplevelImageCaptureSourceManagerV1::interface().version),
                    qh,
                    (),
                ),
            );
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
    ($($type:ty),+ $(,)?) => {
        $(
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
        )+
    };
}

ignore_events!(
    WlShm,
    WlShmPool,
    WlBuffer,
    ExtImageCaptureSourceV1,
    ExtImageCopyCaptureManagerV1,
    ExtForeignToplevelImageCaptureSourceManagerV1,
);

#[cfg(test)]
mod tests {
    use super::{CaptureWayland, PixelLayout};

    #[test]
    fn xrgb_is_converted_to_rgba() {
        let rgba = CaptureWayland::to_rgba(&[1, 2, 3, 0], PixelLayout::Xrgb);
        assert_eq!(rgba, [3, 2, 1, 255]);
    }

    #[test]
    fn xbgr_is_converted_to_rgba() {
        let rgba = CaptureWayland::to_rgba(&[3, 2, 1, 0], PixelLayout::Xbgr);
        assert_eq!(rgba, [3, 2, 1, 255]);
    }
}
