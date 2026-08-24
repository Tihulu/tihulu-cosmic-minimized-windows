// SPDX-License-Identifier: AGPL-3.0-only

use std::{borrow::Cow, time::Duration};

use cctk::toplevel_info::ToplevelInfo;
use cosmic::{
    cctk::{
        sctk::reexports::calloop,
        wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    },
    desktop::{IconSourceExt, fde},
    iced::{Length, Subscription, window::Id},
};

use crate::wayland::{self, BridgeCommand, BridgeEvent, PreviewImage, WindowDelta};

const APP_ID: &str = "io.github.tihulu.MinimizedWindows";
const HOVER_DELAY: Duration = Duration::from_millis(350);
const PREVIEW_WIDTH: f32 = 320.0;
const PREVIEW_HEIGHT: f32 = 180.0;

pub(crate) fn run() -> cosmic::iced::Result {
    cosmic::applet::run::<MinimizedWindows>(())
}

struct Entry {
    handle: ExtForeignToplevelHandleV1,
    app_label: String,
    title: String,
    icon: fde::IconSource,
}

#[derive(Default)]
struct MinimizedWindows {
    core: cosmic::app::Core,
    language: Vec<String>,
    desktop_entries: Vec<fde::DesktopEntry>,
    windows: Vec<Entry>,
    command_tx: Option<calloop::channel::Sender<BridgeCommand>>,
    hovered: Option<ExtForeignToplevelHandleV1>,
    hover_token: u64,
    active_capture_token: Option<u64>,
    preview_popup: Option<Id>,
    preview_image: Option<PreviewImage>,
}

#[derive(Clone, Debug)]
enum Message {
    Bridge(Box<BridgeEvent>),
    Restore(ExtForeignToplevelHandleV1),
    HoverEntered(ExtForeignToplevelHandleV1),
    HoverLeft(ExtForeignToplevelHandleV1),
    HoverDelayElapsed {
        token: u64,
        handle: ExtForeignToplevelHandleV1,
    },
    PreviewClosed(Id),
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
        let (app_label, icon) = self.app_visuals(&info.app_id);
        let raw_title = info.title.trim();
        let title = if raw_title.is_empty() {
            app_label.clone()
        } else {
            raw_title.to_owned()
        };

        let entry = Entry {
            handle: handle.clone(),
            app_label,
            title,
            icon,
        };

        if let Some(index) = self
            .windows
            .iter()
            .position(|window| window.handle == handle)
        {
            self.windows[index] = entry;
        } else {
            self.windows.push(entry);
        }
    }

    fn remove(&mut self, handle: &ExtForeignToplevelHandleV1) {
        self.windows.retain(|window| &window.handle != handle);
    }

    fn entry(&self, handle: &ExtForeignToplevelHandleV1) -> Option<&Entry> {
        self.windows.iter().find(|entry| &entry.handle == handle)
    }

    fn send_cancel(&mut self) {
        let Some(token) = self.active_capture_token.take() else {
            return;
        };
        if let Some(tx) = &self.command_tx {
            let _ = tx.send(BridgeCommand::CancelPreview { token });
        }
    }

    fn clear_preview(&mut self) -> cosmic::app::Task<Message> {
        use cosmic::iced::platform_specific::shell::commands::popup::destroy_popup;

        self.send_cancel();
        self.preview_image = None;
        self.hovered = None;
        self.hover_token = self.hover_token.wrapping_add(1);

        if let Some(id) = self.preview_popup.take() {
            destroy_popup(id)
        } else {
            cosmic::task::none()
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

        cosmic::iced::widget::mouse_area(button)
            .on_enter(Message::HoverEntered(entry.handle.clone()))
            .on_exit(Message::HoverLeft(entry.handle.clone()))
            .into()
    }

    fn open_preview_popup(&mut self) -> cosmic::app::Task<Message> {
        use cosmic::iced::platform_specific::shell::commands::popup::{destroy_popup, get_popup};

        let id = Id::unique();
        let settings = self.core.applet.get_popup_settings(
            self.core.main_window_id().unwrap(),
            id,
            None,
            None,
            None,
        );

        if let Some(old) = self.preview_popup.replace(id) {
            cosmic::app::Task::batch([destroy_popup(old), get_popup(settings)])
        } else {
            get_popup(settings)
        }
    }

    fn preview_body(&self) -> cosmic::Element<'_, Message> {
        let Some(handle) = &self.hovered else {
            return cosmic::widget::text("").into();
        };
        let Some(entry) = self.entry(handle) else {
            return cosmic::widget::text("").into();
        };

        let visual: cosmic::Element<'_, Message> = if let Some(image) = &self.preview_image {
            cosmic::widget::Image::new(cosmic::widget::image::Handle::from_rgba(
                image.width,
                image.height,
                image.pixels.clone(),
            ))
            .width(Length::Fixed(PREVIEW_WIDTH))
            .height(Length::Fixed(PREVIEW_HEIGHT))
            .content_fit(cosmic::iced::core::ContentFit::Contain)
            .into()
        } else {
            cosmic::widget::container(
                cosmic::widget::icon(entry.icon.as_cosmic_icon())
                    .width(Length::Fixed(72.0))
                    .height(Length::Fixed(72.0)),
            )
            .center_x(Length::Fixed(PREVIEW_WIDTH))
            .center_y(Length::Fixed(PREVIEW_HEIGHT))
            .into()
        };

        let mut children: Vec<cosmic::Element<'_, Message>> =
            vec![visual, cosmic::widget::text(&entry.app_label).into()];
        if entry.title != entry.app_label {
            children.push(cosmic::widget::text(&entry.title).into());
        }

        cosmic::widget::column::with_children(children)
            .spacing(6.0)
            .width(Length::Fixed(PREVIEW_WIDTH))
            .into()
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
            ..Default::default()
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
        wayland::subscription().map(|event| Message::Bridge(Box::new(event)))
    }

    fn update(&mut self, message: Self::Message) -> cosmic::app::Task<Self::Message> {
        match message {
            Message::Bridge(event) => match *event {
                BridgeEvent::Ready(tx) => {
                    self.command_tx = Some(tx);
                }
                BridgeEvent::Stopped => {
                    self.command_tx = None;
                    tracing::error!("Minimized-window Wayland bridge stopped");
                    return self.clear_preview();
                }
                BridgeEvent::Window(delta) => match *delta {
                    WindowDelta::Present(info) => {
                        let was_empty = self.windows.is_empty();
                        self.upsert(*info);
                        if was_empty && !self.windows.is_empty() {
                            return cosmic::iced::window::maximize(
                                self.core.main_window_id().unwrap(),
                                true,
                            );
                        }
                    }
                    WindowDelta::Gone(handle) => {
                        let preview_was_for_window = self.hovered.as_ref() == Some(&handle);
                        self.remove(&handle);

                        let mut tasks = Vec::new();
                        if preview_was_for_window {
                            tasks.push(self.clear_preview());
                        }
                        if self.windows.is_empty() {
                            tasks.push(cosmic::iced::window::minimize(
                                self.core.main_window_id().unwrap(),
                                true,
                            ));
                        }
                        if !tasks.is_empty() {
                            return cosmic::app::Task::batch(tasks);
                        }
                    }
                },
                BridgeEvent::Preview {
                    token,
                    handle,
                    image,
                } => {
                    if self.hover_token == token && self.hovered.as_ref() == Some(&handle) {
                        self.active_capture_token = None;
                        self.preview_image = image;
                    }
                }
            },
            Message::Restore(handle) => {
                if let Some(tx) = &self.command_tx {
                    let _ = tx.send(BridgeCommand::Restore(handle.clone()));
                }
                if self.hovered.as_ref() == Some(&handle) {
                    return self.clear_preview();
                }
            }
            Message::HoverEntered(handle) => {
                self.send_cancel();
                self.preview_image = None;
                self.hover_token = self.hover_token.wrapping_add(1);
                let token = self.hover_token;
                self.hovered = Some(handle.clone());

                let delayed =
                    cosmic::iced::Task::perform(tokio::time::sleep(HOVER_DELAY), move |()| {
                        cosmic::Action::App(Message::HoverDelayElapsed {
                            token,
                            handle: handle.clone(),
                        })
                    });

                if let Some(id) = self.preview_popup.take() {
                    use cosmic::iced::platform_specific::shell::commands::popup::destroy_popup;
                    return cosmic::app::Task::batch([destroy_popup(id), delayed]);
                }
                return delayed;
            }
            Message::HoverLeft(handle) => {
                if self.hovered.as_ref() == Some(&handle) {
                    return self.clear_preview();
                }
            }
            Message::HoverDelayElapsed { token, handle } => {
                if self.hover_token != token || self.hovered.as_ref() != Some(&handle) {
                    return cosmic::task::none();
                }

                if let Some(tx) = &self.command_tx
                    && tx
                        .send(BridgeCommand::CapturePreview {
                            token,
                            handle: handle.clone(),
                        })
                        .is_ok()
                {
                    self.active_capture_token = Some(token);
                }
                return self.open_preview_popup();
            }
            Message::PreviewClosed(id) => {
                if self.preview_popup == Some(id) {
                    self.preview_popup = None;
                    self.send_cancel();
                    self.preview_image = None;
                    self.hovered = None;
                    self.hover_token = self.hover_token.wrapping_add(1);
                }
            }
        }

        cosmic::task::none()
    }

    fn view(&self) -> cosmic::Element<'_, Self::Message> {
        let children = self
            .windows
            .iter()
            .map(|entry| self.window_button(entry))
            .collect::<Vec<_>>();

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

    fn view_window(&self, id: Id) -> cosmic::Element<'_, Self::Message> {
        if self.preview_popup == Some(id) {
            self.core.applet.popup_container(self.preview_body()).into()
        } else {
            cosmic::widget::text("").into()
        }
    }

    fn on_close_requested(&self, id: Id) -> Option<Self::Message> {
        Some(Message::PreviewClosed(id))
    }
}
