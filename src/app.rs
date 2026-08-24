// SPDX-License-Identifier: AGPL-3.0-only

use std::{borrow::Cow, sync::LazyLock, time::Duration};

use cctk::toplevel_info::ToplevelInfo;
use cosmic::{
    Task,
    applet::cosmic_panel_config::PanelAnchor,
    cctk::{
        sctk::reexports::calloop,
        wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    },
    desktop::{IconSourceExt, fde},
    iced::{self, Length, Limits, Subscription, id::Id as WidgetId, window::Id as WindowId},
    widget::{Image, autosize::autosize, image::Handle},
};

use crate::wayland::{self, BridgeCommand, BridgeEvent, PreviewImage, WindowDelta};

const APP_ID: &str = "io.github.tihulu.MinimizedWindows";
const HOVER_DELAY: Duration = Duration::from_millis(350);
const PREVIEW_WIDTH: f32 = 320.0;
const PREVIEW_HEIGHT: f32 = 180.0;

static AUTOSIZE_MAIN_ID: LazyLock<WidgetId> =
    LazyLock::new(|| WidgetId::new("tihulu-minimized-windows-main"));

pub(crate) fn run() -> cosmic::iced::Result {
    cosmic::applet::run::<MinimizedWindows>(())
}

struct Entry {
    handle: ExtForeignToplevelHandleV1,
    info: ToplevelInfo,
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
    hover_epoch: u64,
    preview_popup: Option<WindowId>,
    preview_image: Option<PreviewImage>,
}

#[derive(Clone, Debug)]
enum Message {
    Bridge(Box<BridgeEvent>),
    Restore(ExtForeignToplevelHandleV1),
    HoverEnter(ExtForeignToplevelHandleV1),
    HoverExit(ExtForeignToplevelHandleV1),
    HoverDelayElapsed(ExtForeignToplevelHandleV1, u64),
    PreviewClosed(WindowId),
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
        let title = if info.title.trim().is_empty() {
            app_label.clone()
        } else {
            info.title.trim().to_owned()
        };

        if let Some(index) = self
            .windows
            .iter()
            .position(|window| window.handle == handle)
        {
            self.windows[index] = Entry {
                handle,
                info,
                app_label,
                title,
                icon,
            };
        } else {
            self.windows.push(Entry {
                handle,
                info,
                app_label,
                title,
                icon,
            });
        }
    }

    fn remove(&mut self, handle: &ExtForeignToplevelHandleV1) {
        self.windows.retain(|window| &window.handle != handle);
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

        cosmic::widget::mouse_area(button)
            .on_enter(Message::HoverEnter(entry.handle.clone()))
            .on_exit(Message::HoverExit(entry.handle.clone()))
            .into()
    }

    fn close_preview_surface(&mut self) -> Task<Message> {
        self.preview_image = None;
        let Some(id) = self.preview_popup.take() else {
            return cosmic::task::none();
        };
        use cosmic::iced::platform_specific::shell::commands::popup::destroy_popup;
        destroy_popup(id)
    }

    fn preview_anchor_rect(&self, handle: &ExtForeignToplevelHandleV1) -> iced::Rectangle<i32> {
        let index = self
            .windows
            .iter()
            .position(|entry| &entry.handle == handle)
            .unwrap_or_default() as u32;
        let (icon_width, icon_height) = self.core.applet.suggested_size(false);
        let (major, minor) = self.core.applet.suggested_padding(false);
        let (width, height, stride) = if self.core.applet.is_horizontal() {
            (
                u32::from(icon_width) + u32::from(major) * 2,
                u32::from(icon_height) + u32::from(minor) * 2,
                u32::from(icon_width) + u32::from(major) * 2 + self.core.applet.spacing,
            )
        } else {
            (
                u32::from(icon_width) + u32::from(minor) * 2,
                u32::from(icon_height) + u32::from(major) * 2,
                u32::from(icon_height) + u32::from(major) * 2 + self.core.applet.spacing,
            )
        };
        let offset = index.saturating_mul(stride);

        match self.core.applet.anchor {
            PanelAnchor::Top | PanelAnchor::Bottom => iced::Rectangle {
                x: i32::try_from(offset).unwrap_or(i32::MAX),
                y: 0,
                width: i32::try_from(width).unwrap_or(i32::MAX),
                height: i32::try_from(height).unwrap_or(i32::MAX),
            },
            PanelAnchor::Left | PanelAnchor::Right => iced::Rectangle {
                x: 0,
                y: i32::try_from(offset).unwrap_or(i32::MAX),
                width: i32::try_from(width).unwrap_or(i32::MAX),
                height: i32::try_from(height).unwrap_or(i32::MAX),
            },
        }
    }

    fn open_preview_surface(&mut self, handle: &ExtForeignToplevelHandleV1) -> Task<Message> {
        use cosmic::iced::platform_specific::shell::commands::popup::{destroy_popup, get_popup};

        let old = self.preview_popup.take();
        let id = WindowId::unique();
        self.preview_popup = Some(id);
        let mut settings = self.core.applet.get_popup_settings(
            self.core.main_window_id().unwrap(),
            id,
            None,
            None,
            None,
        );
        settings.positioner.anchor_rect = self.preview_anchor_rect(handle);
        let open = get_popup(settings);

        if let Some(old) = old {
            Task::batch([destroy_popup(old), open])
        } else {
            open
        }
    }

    fn hover_delay_task(handle: ExtForeignToplevelHandleV1, epoch: u64) -> Task<Message> {
        Task::perform(
            async move {
                tokio::time::sleep(HOVER_DELAY).await;
                (handle, epoch)
            },
            |(handle, epoch)| Message::HoverDelayElapsed(handle, epoch),
        )
    }

    fn preview_card(&self) -> cosmic::Element<'_, Message> {
        let Some(handle) = self.hovered.as_ref() else {
            return cosmic::widget::space::horizontal().into();
        };
        let Some(entry) = self.windows.iter().find(|entry| &entry.handle == handle) else {
            return cosmic::widget::space::horizontal().into();
        };

        let visual: cosmic::Element<_> = if let Some(image) = self.preview_image.as_ref() {
            Image::new(Handle::from_rgba(
                image.width,
                image.height,
                image.rgba.clone(),
            ))
            .width(Length::Fixed(PREVIEW_WIDTH))
            .height(Length::Fixed(PREVIEW_HEIGHT))
            .content_fit(iced::ContentFit::Contain)
            .into()
        } else {
            let icon = entry.icon.as_cosmic_icon();
            cosmic::widget::container(
                cosmic::widget::icon(icon)
                    .width(Length::Fixed(72.0))
                    .height(Length::Fixed(72.0)),
            )
            .center_x(Length::Fixed(PREVIEW_WIDTH))
            .center_y(Length::Fixed(PREVIEW_HEIGHT))
            .into()
        };

        let app_name = cosmic::widget::text(&entry.app_label);
        let title = cosmic::widget::text(&entry.title);
        cosmic::widget::column::with_children(vec![visual, app_name.into(), title.into()])
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

    fn init(core: cosmic::app::Core, _flags: Self::Flags) -> (Self, Task<Self::Message>) {
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

    fn update(&mut self, message: Self::Message) -> Task<Self::Message> {
        match message {
            Message::Bridge(event) => match *event {
                BridgeEvent::Ready(tx) => {
                    self.command_tx = Some(tx);
                }
                BridgeEvent::Stopped => {
                    self.command_tx = None;
                    tracing::error!("Minimized-window Wayland bridge stopped");
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
                        let hovered_gone = self.hovered.as_ref() == Some(&handle);
                        self.remove(&handle);
                        let close = if hovered_gone {
                            self.hover_epoch = self.hover_epoch.wrapping_add(1);
                            self.hovered = None;
                            self.close_preview_surface()
                        } else {
                            cosmic::task::none()
                        };

                        if self.windows.is_empty() {
                            let hide = cosmic::iced::window::minimize(
                                self.core.main_window_id().unwrap(),
                                true,
                            );
                            return Task::batch([close, hide]);
                        }
                        return close;
                    }
                },
                BridgeEvent::Preview(handle, image) => {
                    if self.hovered.as_ref() == Some(&handle) && self.preview_popup.is_some() {
                        self.preview_image = image;
                    } else if let (Some(current), Some(tx)) =
                        (self.hovered.clone(), self.command_tx.as_ref())
                    {
                        if self.preview_popup.is_some() && current != handle {
                            let _ = tx.send(BridgeCommand::CapturePreview(current));
                        }
                    }
                }
            },
            Message::Restore(handle) => {
                if let Some(tx) = &self.command_tx {
                    let _ = tx.send(BridgeCommand::Restore(handle));
                }
                self.hover_epoch = self.hover_epoch.wrapping_add(1);
                self.hovered = None;
                return self.close_preview_surface();
            }
            Message::HoverEnter(handle) => {
                self.hover_epoch = self.hover_epoch.wrapping_add(1);
                let epoch = self.hover_epoch;
                self.hovered = Some(handle.clone());
                let close = self.close_preview_surface();
                return Task::batch([close, Self::hover_delay_task(handle, epoch)]);
            }
            Message::HoverExit(handle) => {
                if self.hovered.as_ref() == Some(&handle) {
                    self.hover_epoch = self.hover_epoch.wrapping_add(1);
                    self.hovered = None;
                    return self.close_preview_surface();
                }
            }
            Message::HoverDelayElapsed(handle, epoch) => {
                if self.hover_epoch == epoch && self.hovered.as_ref() == Some(&handle) {
                    self.preview_image = None;
                    if let Some(tx) = &self.command_tx {
                        let _ = tx.send(BridgeCommand::CapturePreview(handle.clone()));
                    }
                    return self.open_preview_surface(&handle);
                }
            }
            Message::PreviewClosed(id) => {
                if self.preview_popup == Some(id) {
                    self.preview_popup = None;
                    self.preview_image = None;
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

        let content: cosmic::Element<_> = if self.core.applet.is_horizontal() {
            cosmic::widget::row::with_children(children)
                .spacing(self.core.applet.spacing as f32)
                .align_y(cosmic::iced::Alignment::Center)
                .width(Length::Shrink)
                .height(Length::Shrink)
                .into()
        } else {
            cosmic::widget::column::with_children(children)
                .spacing(self.core.applet.spacing as f32)
                .align_x(cosmic::iced::Alignment::Center)
                .width(Length::Shrink)
                .height(Length::Shrink)
                .into()
        };

        autosize(content, AUTOSIZE_MAIN_ID.clone())
            .limits(Limits::NONE.min_width(1.0).min_height(1.0))
            .into()
    }

    fn view_window(&self, id: WindowId) -> cosmic::Element<'_, Self::Message> {
        if self.preview_popup == Some(id) {
            self.core.applet.popup_container(self.preview_card()).into()
        } else {
            cosmic::widget::space::horizontal().into()
        }
    }

    fn on_close_requested(&self, id: WindowId) -> Option<Self::Message> {
        Some(Message::PreviewClosed(id))
    }
}
