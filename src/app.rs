// SPDX-License-Identifier: AGPL-3.0-only

use std::borrow::Cow;

use cctk::toplevel_info::ToplevelInfo;
use cosmic::{
    app,
    applet::cosmic_panel_config::PanelAnchor,
    cctk::{
        sctk::reexports::calloop,
        wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    },
    desktop::{IconSourceExt, fde},
    iced::{Length, Subscription, window::Id},
};

use crate::wayland::{self, BridgeCommand, BridgeEvent, WindowDelta};

const APP_ID: &str = "io.github.tihulu.MinimizedWindows";

pub(crate) fn run() -> cosmic::iced::Result {
    cosmic::applet::run::<MinimizedWindows>(())
}

struct Entry {
    handle: ExtForeignToplevelHandleV1,
    info: ToplevelInfo,
    label: String,
    icon: fde::IconSource,
}

struct MinimizedWindows {
    core: cosmic::app::Core,
    language: Vec<String>,
    desktop_entries: Vec<fde::DesktopEntry>,
    windows: Vec<Entry>,
    command_tx: Option<calloop::channel::Sender<BridgeCommand>>,
    overflow: Option<Id>,
}

impl Default for MinimizedWindows {
    fn default() -> Self {
        Self {
            core: cosmic::app::Core::default(),
            language: Vec::new(),
            desktop_entries: Vec::new(),
            windows: Vec::new(),
            command_tx: None,
            overflow: None,
        }
    }
}

#[derive(Clone, Debug)]
enum Message {
    Bridge(BridgeEvent),
    Restore(ExtForeignToplevelHandleV1),
    ToggleOverflow,
    OverflowClosed(Id),
}

impl MinimizedWindows {
    fn reload_desktop_entries(&mut self) {
        self.desktop_entries = fde::Iter::new(fde::default_paths())
            .filter_map(|path| fde::DesktopEntry::from_path(path, Some(&self.language)).ok())
            .collect();
    }

    fn app_visuals(&mut self, app_id: &str) -> (String, fde::IconSource) {
        let key = fde::unicase::Ascii::new(app_id);
        let found = fde::find_app_by_id(&self.desktop_entries, key)
            .cloned()
            .or_else(|| {
                self.reload_desktop_entries();
                fde::find_app_by_id(&self.desktop_entries, key).cloned()
            });

        if let Some(entry) = found {
            let label = entry
                .full_name(&self.language)
                .unwrap_or(Cow::Borrowed(&entry.appid))
                .into_owned();
            let icon = fde::IconSource::from_unknown(entry.icon().unwrap_or(&entry.appid));
            (label, icon)
        } else {
            (
                app_id.to_owned(),
                fde::IconSource::from_unknown("application-x-executable-symbolic"),
            )
        }
    }

    fn upsert(&mut self, info: ToplevelInfo) {
        let handle = info.foreign_toplevel.clone();
        if let Some(existing) = self.windows.iter_mut().find(|w| w.handle == handle) {
            existing.info = info;
            return;
        }

        let (label, icon) = self.app_visuals(&info.app_id);
        self.windows.push(Entry {
            handle,
            info,
            label,
            icon,
        });
    }

    fn remove(&mut self, handle: &ExtForeignToplevelHandleV1) {
        self.windows.retain(|window| &window.handle != handle);
    }

    fn max_inline(&self) -> usize {
        let Some(bounds) = self.core.applet.suggested_bounds else {
            return self.windows.len();
        };
        let major = match self.core.applet.anchor {
            PanelAnchor::Top | PanelAnchor::Bottom => bounds.width,
            PanelAnchor::Left | PanelAnchor::Right => bounds.height,
        };
        let icon = self.core.applet.suggested_size(true).0 as f32;
        let padding = self.core.applet.suggested_padding(true).0 as f32 * 2.0;
        let slot = (icon + padding + self.core.applet.spacing as f32).max(1.0);
        let capacity = (major / slot).floor().max(1.0) as usize;

        if self.windows.len() > capacity {
            capacity.saturating_sub(1).max(1)
        } else {
            self.windows.len()
        }
    }

    fn window_button<'a>(&self, entry: &'a Entry) -> cosmic::Element<'a, Message> {
        let icon = entry.icon.as_cosmic_icon();
        let symbolic = icon.symbolic;
        let size = self.core.applet.suggested_size(symbolic);
        let (major, minor) = self.core.applet.suggested_padding(symbolic);
        let (px, py) = if self.core.applet.is_horizontal() {
            (major, minor)
        } else {
            (minor, major)
        };

        let button = cosmic::widget::button::custom(
            cosmic::widget::icon(icon)
                .width(Length::Fixed(size.0 as f32))
                .height(Length::Fixed(size.1 as f32)),
        )
        .class(cosmic::theme::Button::AppletIcon)
        .padding([py as f32, px as f32])
        .on_press_down(Message::Restore(entry.handle.clone()));

        cosmic::widget::tooltip(
            button,
            cosmic::widget::text(&entry.label),
            match self.core.applet.anchor {
                PanelAnchor::Top => cosmic::widget::tooltip::Position::Bottom,
                PanelAnchor::Bottom => cosmic::widget::tooltip::Position::Top,
                PanelAnchor::Left => cosmic::widget::tooltip::Position::Right,
                PanelAnchor::Right => cosmic::widget::tooltip::Position::Left,
            },
        )
        .snap_within_viewport(false)
        .into()
    }

    fn overflow_task(&mut self) -> cosmic::app::Task<Message> {
        use cosmic::iced::platform_specific::shell::commands::popup::{destroy_popup, get_popup};

        if let Some(id) = self.overflow.take() {
            return destroy_popup(id);
        }

        let id = Id::unique();
        self.overflow = Some(id);
        let settings = self.core.applet.get_popup_settings(
            self.core.main_window_id().unwrap(),
            id,
            None,
            None,
            None,
        );
        get_popup(settings)
    }
}

impl cosmic::Application for MinimizedWindows {
    type Flags = ();
    type Message = Message;
    type Executor = cosmic::SingleThreadExecutor;

    const APP_ID: &'static str = APP_ID;

    fn init(
        core: cosmic::app::Core,
        _flags: Self::Flags,
    ) -> (Self, cosmic::app::Task<Self::Message>) {
        let mut app = Self {
            core,
            language: fde::get_languages_from_env(),
            desktop_entries: Vec::new(),
            windows: Vec::new(),
            command_tx: None,
            overflow: None,
        };
        app.reload_desktop_entries();

        let hide = cosmic::iced::window::minimize::<cosmic::Action<Message>>(
            app.core.main_window_id().unwrap(),
            true,
        );
        (app, hide)
    }

    fn core(&self) -> &cosmic::app::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::app::Core {
        &mut self.core
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        wayland::subscription().map(Message::Bridge)
    }

    fn update(&mut self, message: Self::Message) -> cosmic::app::Task<Self::Message> {
        match message {
            Message::Bridge(BridgeEvent::Ready(tx)) => {
                self.command_tx = Some(tx);
            }
            Message::Bridge(BridgeEvent::Stopped) => {
                self.command_tx = None;
                tracing::error!("Minimized-window Wayland bridge stopped");
            }
            Message::Bridge(BridgeEvent::Window(WindowDelta::Present(info))) => {
                let was_empty = self.windows.is_empty();
                self.upsert(info);
                if was_empty && !self.windows.is_empty() {
                    return cosmic::iced::window::maximize(
                        self.core.main_window_id().unwrap(),
                        true,
                    );
                }
            }
            Message::Bridge(BridgeEvent::Window(WindowDelta::Gone(handle))) => {
                self.remove(&handle);
                if self.windows.is_empty() {
                    let hide =
                        cosmic::iced::window::minimize(self.core.main_window_id().unwrap(), true);
                    if let Some(id) = self.overflow.take() {
                        use cosmic::iced::platform_specific::shell::commands::popup::destroy_popup;
                        return cosmic::app::Task::batch([destroy_popup(id), hide]);
                    }
                    return hide;
                }
            }
            Message::Restore(handle) => {
                if let Some(tx) = &self.command_tx {
                    let _ = tx.send(BridgeCommand::Restore(handle));
                }
            }
            Message::ToggleOverflow => return self.overflow_task(),
            Message::OverflowClosed(id) => {
                if self.overflow == Some(id) {
                    self.overflow = None;
                }
            }
        }

        cosmic::task::none()
    }

    fn view(&self) -> cosmic::Element<'_, Self::Message> {
        let inline = self.max_inline();
        let mut children = self.windows[..inline]
            .iter()
            .map(|entry| self.window_button(entry))
            .collect::<Vec<_>>();

        if inline < self.windows.len() {
            let name = match self.core.applet.anchor {
                PanelAnchor::Top => "go-down-symbolic",
                PanelAnchor::Bottom => "go-up-symbolic",
                PanelAnchor::Left => "go-next-symbolic",
                PanelAnchor::Right => "go-previous-symbolic",
            };
            children.push(
                self.core
                    .applet
                    .icon_button(name)
                    .on_press_down(Message::ToggleOverflow)
                    .into(),
            );
        }

        if self.core.applet.is_horizontal() {
            cosmic::widget::row::with_children(children)
                .spacing(self.core.applet.spacing as f32)
                .align_y(cosmic::iced::Alignment::Center)
                .into()
        } else {
            cosmic::widget::column::with_children(children)
                .spacing(self.core.applet.spacing as f32)
                .align_x(cosmic::iced::Alignment::Center)
                .into()
        }
    }

    fn view_window(&self, _id: Id) -> cosmic::Element<'_, Self::Message> {
        let inline = self.max_inline();
        let rest = self.windows[inline..]
            .iter()
            .map(|entry| self.window_button(entry))
            .collect::<Vec<_>>();

        let body: cosmic::Element<_> = if self.core.applet.is_horizontal() {
            cosmic::widget::row::with_children(rest)
                .spacing(self.core.applet.spacing as f32)
                .into()
        } else {
            cosmic::widget::column::with_children(rest)
                .spacing(self.core.applet.spacing as f32)
                .into()
        };

        self.core.applet.popup_container(body).into()
    }

    fn on_close_requested(&self, id: Id) -> Option<Self::Message> {
        Some(Message::OverflowClosed(id))
    }
}
