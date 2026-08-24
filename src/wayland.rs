// SPDX-License-Identifier: AGPL-3.0-only

use std::{
    collections::HashSet,
    os::{
        fd::{FromRawFd, RawFd},
        unix::net::UnixStream,
    },
};

use cctk::{
    sctk::{
        self,
        reexports::{calloop, calloop_wayland_source::WaylandSource},
        seat::{SeatHandler, SeatState},
    },
    toplevel_info::{ToplevelInfo, ToplevelInfoHandler, ToplevelInfoState},
    toplevel_management::{ToplevelManagerHandler, ToplevelManagerState},
    wayland_client::{WEnum, protocol::wl_seat::WlSeat},
};
use cosmic::{
    cctk::{
        cosmic_protocols::toplevel_info::v1::client::zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1,
        wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    },
    iced::{self, Subscription, futures, stream},
};
use cosmic_protocols::{
    toplevel_info::v1::client::zcosmic_toplevel_handle_v1,
    toplevel_management::v1::client::zcosmic_toplevel_manager_v1,
};
use futures::{SinkExt, channel::mpsc};
use sctk::registry::{ProvidesRegistryState, RegistryState};
use cctk::wayland_client::{Connection, QueueHandle, globals::registry_queue_init};

#[derive(Clone, Debug)]
pub enum WindowDelta {
    Present(ToplevelInfo),
    Gone(ExtForeignToplevelHandleV1),
}

#[derive(Clone, Debug)]
pub enum BridgeEvent {
    Ready(calloop::channel::Sender<BridgeCommand>),
    Window(WindowDelta),
    Stopped,
}

#[derive(Clone, Debug)]
pub enum BridgeCommand {
    Restore(ExtForeignToplevelHandleV1),
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

struct BridgeState {
    done: bool,
    out: mpsc::Sender<BridgeEvent>,
    registry: RegistryState,
    seats: SeatState,
    toplevels: ToplevelInfoState,
    manager: ToplevelManagerState,
    shown: HashSet<ExtForeignToplevelHandleV1>,
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
                self.emit(BridgeEvent::Window(WindowDelta::Present(info)));
            }
            (false, true) => {
                self.shown.remove(handle);
                self.emit(BridgeEvent::Window(WindowDelta::Gone(handle.clone())));
            }
            (false, false) => {}
        }
    }

    fn forget(&mut self, handle: &ExtForeignToplevelHandleV1) {
        if self.shown.remove(handle) {
            self.emit(BridgeEvent::Window(WindowDelta::Gone(handle.clone())));
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

fn bridge_loop(
    out: mpsc::Sender<BridgeEvent>,
    commands: calloop::channel::Channel<BridgeCommand>,
) {
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
    let source = WaylandSource::new(connection, queue);
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
            calloop::channel::Event::Closed => state.done = true,
        })
        .is_err()
    {
        return;
    }

    let registry = RegistryState::new(&globals);
    let mut state = BridgeState {
        done: false,
        out,
        seats: SeatState::new(&globals, &qh),
        toplevels: ToplevelInfoState::new(&registry, &qh),
        manager: ToplevelManagerState::new(&registry, &qh),
        registry,
        shown: HashSet::new(),
    };

    while !state.done {
        if let Err(error) = loop_.dispatch(None, &mut state) {
            tracing::error!(?error, "Wayland bridge dispatch failed");
            break;
        }
    }
}

sctk::delegate_seat!(BridgeState);
sctk::delegate_registry!(BridgeState);
cctk::delegate_toplevel_info!(BridgeState);
cctk::delegate_toplevel_manager!(BridgeState);
