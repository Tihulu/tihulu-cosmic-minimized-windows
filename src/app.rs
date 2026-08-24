// SPDX-License-Identifier: AGPL-3.0-only

use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    sync::LazyLock,
    time::Duration,
};

use cctk::toplevel_info::ToplevelInfo;
use cosmic::{
    app::Task,
    applet::cosmic_panel_config::PanelAnchor,
    cctk::{
        sctk::reexports::calloop,
        wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    },
    desktop::{IconSourceExt, fde},
    iced::{self, Length, Limits, Subscription, id::Id as WidgetId, window::Id as WindowId},
    widget::{
        autosize::autosize,
        rectangle_tracker::{RectangleTracker, RectangleUpdate, rectangle_tracker_subscription},
    },
};

use crate::{
    config::{self, RunMode},
    wayland::{self, BridgeCommand, BridgeEvent, WindowDelta},
};

const APP_ID: &str = "io.github.tihulu.MinimizedWindows";
const LEAVE_GRACE: Duration = Duration::from_millis(650);
const REARM_DELAY: Duration = Duration::from_millis(32);
const ANCHOR_RETRY_DELAY: Duration = Duration::from_millis(16);
const ANCHOR_RETRY_MAX: u8 = 4;
const POPUP_WIDTH: f32 = 340.0;
const POPUP_MAX_HEIGHT: f32 = 420.0;
const ROW_HEIGHT_ESTIMATE: f32 = 52.0;

static AUTOSIZE_MAIN_ID: LazyLock<WidgetId> =
    LazyLock::new(|| WidgetId::new("tihulu-minimized-windows-main"));

pub(crate) fn run() -> cosmic::iced::Result {
    cosmic::applet::run::<MinimizedWindows>(())
}

struct Entry {
    handle: ExtForeignToplevelHandleV1,
    group_key: String,
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
    hover_group: Option<String>,
    active_group: Option<String>,
    close_epoch: u64,
    open_epoch: u64,
    popup_hovered: bool,
    popup_pinned: bool,
    group_popup: Option<WindowId>,
    rectangle_tracker: Option<RectangleTracker<u64>>,
    rectangles: HashMap<u64, iced::Rectangle>,
    mode: RunMode,
}

#[derive(Clone, Debug)]
enum Message {
    Bridge(Box<BridgeEvent>),
    Rectangle(RectangleUpdate<u64>),
    GroupPrimary(String),
    GroupOpen(String),
    GroupHoverEnter(String),
    GroupHoverExit(String),
    PopupEnter,
    PopupExit,
    CloseDelayElapsed(u64),
    ReopenAfterDestroy {
        epoch: u64,
        group: String,
        pinned: bool,
    },
    OpenRetry {
        epoch: u64,
        group: String,
        pinned: bool,
        attempt: u8,
    },
    ToggleSafeMode,
    Restore(ExtForeignToplevelHandleV1),
    CloseWindow(ExtForeignToplevelHandleV1),
    PopupClosed(WindowId),
}

impl MinimizedWindows {
    fn reload_desktop_entries(&mut self) {
        self.desktop_entries = fde::Iter::new(fde::default_paths())
            .filter_map(|path| fde::DesktopEntry::from_path(path, Some(&self.language)).ok())
            .collect();
    }

    fn app_visuals(&mut self, app_id: &str) -> (String, fde::IconSource, String) {
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
            let group = canonical_group_key(&entry.appid, &label);
            (label, icon, group)
        } else {
            let label = if app_id.trim().is_empty() {
                "Application".to_owned()
            } else {
                app_id.to_owned()
            };
            let group = canonical_group_key(app_id, &label);
            (
                label,
                fde::IconSource::from_unknown("application-x-executable-symbolic"),
                group,
            )
        }
    }

    fn upsert(&mut self, info: ToplevelInfo) {
        let handle = info.foreign_toplevel.clone();
        let app_id = info.app_id.trim().to_owned();
        let (app_label, icon, group_key) = self.app_visuals(&app_id);
        let title = if info.title.trim().is_empty() {
            app_label.clone()
        } else {
            info.title.trim().to_owned()
        };

        let entry = Entry {
            handle: handle.clone(),
            group_key,
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

    fn remove(&mut self, handle: &ExtForeignToplevelHandleV1) -> Option<String> {
        let group = self
            .windows
            .iter()
            .find(|entry| &entry.handle == handle)
            .map(|entry| entry.group_key.clone());
        self.windows.retain(|window| &window.handle != handle);
        group
    }

    fn group_count(&self, group: &str) -> usize {
        self.windows
            .iter()
            .filter(|entry| entry.group_key == group)
            .count()
    }

    fn group_handles(&self, group: &str) -> Vec<ExtForeignToplevelHandleV1> {
        self.windows
            .iter()
            .filter(|entry| entry.group_key == group)
            .map(|entry| entry.handle.clone())
            .collect()
    }

    fn group_index(&self, group: &str) -> u32 {
        let mut seen = HashSet::new();
        let mut index = 0_u32;
        for entry in &self.windows {
            if seen.insert(entry.group_key.as_str()) {
                if entry.group_key == group {
                    break;
                }
                index = index.saturating_add(1);
            }
        }
        index
    }

    fn group_button<'a>(&self, entry: &'a Entry, count: usize) -> cosmic::Element<'a, Message> {
        let icon = entry.icon.as_cosmic_icon();
        let symbolic = icon.symbolic;
        let size = self.core.applet.suggested_size(symbolic);
        let (major, minor) = self.core.applet.suggested_padding(symbolic);
        let (px, py) = if self.core.applet.is_horizontal() {
            (major, minor)
        } else {
            (minor, major)
        };
        let group = entry.group_key.clone();

        let mut content: Vec<cosmic::Element<'a, Message>> = vec![
            cosmic::widget::icon(icon)
                .width(Length::Fixed(size.0 as f32))
                .height(Length::Fixed(size.1 as f32))
                .into(),
        ];
        if count > 1 {
            content.push(cosmic::widget::text(count.to_string()).into());
        }

        let button = cosmic::widget::button::custom(
            cosmic::widget::row::with_children(content)
                .spacing(2.0)
                .align_y(iced::Alignment::Center),
        )
        .class(cosmic::theme::Button::AppletIcon)
        .padding([py as f32, px as f32])
        .on_press_down(Message::GroupPrimary(group.clone()));

        cosmic::widget::mouse_area(button)
            .on_enter(Message::GroupHoverEnter(group.clone()))
            .on_exit(Message::GroupHoverExit(group.clone()))
            .on_right_press(Message::GroupOpen(group))
            .into()
    }

    fn tracked_anchor_rect(&self, group: &str) -> Option<iced::Rectangle<i32>> {
        let rectangle = self.rectangles.get(&tracker_id(group))?;
        Some(iced::Rectangle {
            x: rectangle.x.round() as i32,
            y: rectangle.y.round() as i32,
            width: rectangle.width.round().max(1.0) as i32,
            height: rectangle.height.round().max(1.0) as i32,
        })
    }

    fn fallback_anchor_rect(&self, group: &str) -> iced::Rectangle<i32> {
        let index = self.group_index(group);
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

    fn create_popup(&mut self, group: String, pinned: bool, attempt: u8) -> Task<Message> {
        use cosmic::iced::platform_specific::shell::commands::popup::get_popup;

        if self.group_count(&group) == 0 {
            return cosmic::task::none();
        }

        let anchor = if let Some(anchor) = self.tracked_anchor_rect(&group) {
            anchor
        } else if attempt < ANCHOR_RETRY_MAX {
            let epoch = self.open_epoch;
            return Task::perform(
                async move {
                    tokio::time::sleep(ANCHOR_RETRY_DELAY).await;
                    (epoch, group, pinned, attempt + 1)
                },
                |(epoch, group, pinned, attempt)| {
                    cosmic::Action::App(Message::OpenRetry {
                        epoch,
                        group,
                        pinned,
                        attempt,
                    })
                },
            );
        } else {
            tracing::warn!(%group, "RectangleTracker was not ready; using fallback popup anchor");
            self.fallback_anchor_rect(&group)
        };

        self.active_group = Some(group);
        self.popup_pinned = pinned;
        let id = WindowId::unique();
        self.group_popup = Some(id);
        let mut settings = self.core.applet.get_popup_settings(
            self.core.main_window_id().unwrap(),
            id,
            None,
            None,
            None,
        );
        settings.positioner.anchor_rect = anchor;
        get_popup(settings)
    }

    fn open_group(&mut self, group: String, pinned: bool) -> Task<Message> {
        if self.group_count(&group) == 0 {
            return cosmic::task::none();
        }

        self.close_epoch = self.close_epoch.wrapping_add(1);
        self.open_epoch = self.open_epoch.wrapping_add(1);
        self.active_group = Some(group.clone());
        self.popup_hovered = false;
        if pinned {
            self.popup_pinned = true;
        } else if self.group_popup.is_none() {
            self.popup_pinned = false;
        }

        if self.group_popup.is_some() {
            cosmic::task::none()
        } else {
            self.create_popup(group, pinned, 0)
        }
    }

    fn rearm_popup(&mut self, group: String, pinned: bool) -> Task<Message> {
        use cosmic::iced::platform_specific::shell::commands::popup::destroy_popup;

        self.close_epoch = self.close_epoch.wrapping_add(1);
        self.open_epoch = self.open_epoch.wrapping_add(1);
        let epoch = self.open_epoch;
        self.active_group = None;
        self.popup_hovered = false;
        self.popup_pinned = pinned;

        let destroy = self
            .group_popup
            .take()
            .map(destroy_popup)
            .unwrap_or_else(cosmic::task::none);
        let reopen = Task::perform(
            async move {
                tokio::time::sleep(REARM_DELAY).await;
                (epoch, group, pinned)
            },
            |(epoch, group, pinned)| {
                cosmic::Action::App(Message::ReopenAfterDestroy {
                    epoch,
                    group,
                    pinned,
                })
            },
        );
        Task::batch([destroy, reopen])
    }

    fn close_popup(&mut self) -> Task<Message> {
        use cosmic::iced::platform_specific::shell::commands::popup::destroy_popup;

        self.open_epoch = self.open_epoch.wrapping_add(1);
        self.active_group = None;
        self.hover_group = None;
        self.popup_hovered = false;
        self.popup_pinned = false;
        let Some(id) = self.group_popup.take() else {
            return cosmic::task::none();
        };
        destroy_popup(id)
    }

    fn schedule_close(&mut self) -> Task<Message> {
        self.close_epoch = self.close_epoch.wrapping_add(1);
        let epoch = self.close_epoch;
        Task::perform(
            async move {
                tokio::time::sleep(LEAVE_GRACE).await;
                epoch
            },
            |epoch| cosmic::Action::App(Message::CloseDelayElapsed(epoch)),
        )
    }

    fn window_row<'a>(&self, entry: &'a Entry) -> cosmic::Element<'a, Message> {
        let icon = cosmic::widget::icon(entry.icon.as_cosmic_icon())
            .width(Length::Fixed(36.0))
            .height(Length::Fixed(36.0));
        let label = cosmic::widget::text(&entry.title).width(Length::Fill);
        let restore = cosmic::widget::button::custom(
            cosmic::widget::row::with_children(vec![icon.into(), label.into()])
                .spacing(9.0)
                .align_y(iced::Alignment::Center)
                .width(Length::Fill),
        )
        .width(Length::Fill)
        .on_press(Message::Restore(entry.handle.clone()));
        let close =
            cosmic::widget::button::text("×").on_press(Message::CloseWindow(entry.handle.clone()));

        cosmic::widget::row::with_children(vec![restore.into(), close.into()])
            .spacing(6.0)
            .align_y(iced::Alignment::Center)
            .width(Length::Fill)
            .into()
    }

    fn group_popup_view(&self) -> cosmic::Element<'_, Message> {
        let Some(group) = self.active_group.as_deref() else {
            return cosmic::widget::space::horizontal().into();
        };
        let entries = self
            .windows
            .iter()
            .filter(|entry| entry.group_key == group)
            .collect::<Vec<_>>();
        if entries.is_empty() {
            return cosmic::widget::space::horizontal().into();
        }

        let header = cosmic::widget::text(format!(
            "{} — {} minimized window{}",
            entries[0].app_label,
            entries.len(),
            if entries.len() == 1 { "" } else { "s" }
        ));

        let rows = entries
            .iter()
            .map(|entry| self.window_row(entry))
            .collect::<Vec<_>>();
        let list = cosmic::widget::column::with_children(rows)
            .spacing(6.0)
            .width(Length::Fill);
        let estimated_height = (entries.len() as f32 * ROW_HEIGHT_ESTIMATE)
            .clamp(ROW_HEIGHT_ESTIMATE, POPUP_MAX_HEIGHT);
        let list: cosmic::Element<_> = if entries.len() > 7 {
            cosmic::widget::scrollable::vertical(list)
                .height(Length::Fixed(estimated_height))
                .width(Length::Fill)
                .into()
        } else {
            list.into()
        };

        let mode_label = if self.mode.is_safe() {
            "Safe mode: ON"
        } else {
            "Enhanced mode: requested"
        };
        let mode_button = cosmic::widget::button::text(mode_label)
            .on_press(Message::ToggleSafeMode)
            .width(Length::Fill);
        let mode_note = if self.mode.is_safe() {
            "No thumbnails or media helpers"
        } else {
            "Daemon features only; safe fallback stays available"
        };

        let content = cosmic::widget::column::with_children(vec![
            header.into(),
            list,
            mode_button.into(),
            cosmic::widget::text(mode_note).into(),
        ])
        .spacing(9.0)
        .width(Length::Fixed(POPUP_WIDTH));

        cosmic::widget::mouse_area(content)
            .on_enter(Message::PopupEnter)
            .on_exit(Message::PopupExit)
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
            mode: config::load_mode(),
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
        Subscription::batch([
            wayland::subscription().map(|event| Message::Bridge(Box::new(event))),
            rectangle_tracker_subscription(0).map(|update| Message::Rectangle(update.1)),
        ])
    }

    fn update(&mut self, message: Self::Message) -> Task<Self::Message> {
        match message {
            Message::Bridge(event) => match *event {
                BridgeEvent::Ready(tx) => self.command_tx = Some(tx),
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
                        let removed_group = self.remove(&handle);
                        let active_disappeared = removed_group.as_deref().is_some_and(|group| {
                            self.active_group.as_deref() == Some(group)
                                && self.group_count(group) == 0
                        });
                        let close = if active_disappeared {
                            self.close_popup()
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
            },
            Message::Rectangle(update) => match update {
                RectangleUpdate::Rectangle((id, rectangle)) => {
                    self.rectangles.insert(id, rectangle);
                }
                RectangleUpdate::Init(tracker) => {
                    self.rectangle_tracker = Some(tracker);
                }
            },
            Message::GroupPrimary(group) => {
                let handles = self.group_handles(&group);
                if handles.len() == 1 {
                    if let Some(tx) = &self.command_tx {
                        let _ = tx.send(BridgeCommand::Restore(handles[0].clone()));
                    }
                    return self.close_popup();
                }
                if !handles.is_empty() {
                    if self.group_popup.is_some() {
                        return self.rearm_popup(group, true);
                    }
                    return self.open_group(group, true);
                }
            }
            Message::GroupOpen(group) => {
                if self.group_count(&group) > 0 {
                    if self.group_popup.is_some() {
                        return self.rearm_popup(group, true);
                    }
                    return self.open_group(group, true);
                }
            }
            Message::GroupHoverEnter(group) => {
                self.close_epoch = self.close_epoch.wrapping_add(1);
                self.hover_group = Some(group.clone());
                if !self.popup_pinned {
                    // A fresh pointer enter is also a health signal. Re-arm any old
                    // unpinned popup surface instead of trusting a stale compositor object.
                    if self.group_popup.is_some() {
                        return self.rearm_popup(group, false);
                    }
                    return self.open_group(group, false);
                }
            }
            Message::GroupHoverExit(group) => {
                if self.hover_group.as_deref() == Some(group.as_str()) {
                    self.hover_group = None;
                }
                if self.group_popup.is_some() && !self.popup_pinned {
                    return self.schedule_close();
                }
            }
            Message::PopupEnter => {
                self.popup_hovered = true;
                self.close_epoch = self.close_epoch.wrapping_add(1);
            }
            Message::PopupExit => {
                self.popup_hovered = false;
                if !self.popup_pinned {
                    return self.schedule_close();
                }
            }
            Message::CloseDelayElapsed(epoch) => {
                if self.close_epoch == epoch
                    && !self.popup_pinned
                    && self.hover_group.is_none()
                    && !self.popup_hovered
                {
                    return self.close_popup();
                }
            }
            Message::ReopenAfterDestroy {
                epoch,
                group,
                pinned,
            } => {
                if self.open_epoch == epoch
                    && self.group_count(&group) > 0
                    && (pinned || self.hover_group.as_deref() == Some(group.as_str()))
                {
                    return self.create_popup(group, pinned, 0);
                }
            }
            Message::OpenRetry {
                epoch,
                group,
                pinned,
                attempt,
            } => {
                if self.open_epoch == epoch
                    && self.group_popup.is_none()
                    && self.group_count(&group) > 0
                    && (pinned || self.hover_group.as_deref() == Some(group.as_str()))
                {
                    return self.create_popup(group, pinned, attempt);
                }
            }
            Message::ToggleSafeMode => {
                self.mode = self.mode.toggled();
                if let Err(error) = config::save_mode(self.mode) {
                    tracing::warn!(?error, "Could not save minimized-windows mode");
                }
            }
            Message::Restore(handle) => {
                if let Some(tx) = &self.command_tx {
                    let _ = tx.send(BridgeCommand::Restore(handle));
                }
                return self.close_popup();
            }
            Message::CloseWindow(handle) => {
                if let Some(tx) = &self.command_tx {
                    let _ = tx.send(BridgeCommand::Close(handle));
                }
            }
            Message::PopupClosed(id) => {
                if self.group_popup == Some(id) {
                    self.group_popup = None;
                    self.active_group = None;
                    self.hover_group = None;
                    self.popup_hovered = false;
                    self.popup_pinned = false;
                    self.open_epoch = self.open_epoch.wrapping_add(1);
                }
            }
        }

        cosmic::task::none()
    }

    fn view(&self) -> cosmic::Element<'_, Self::Message> {
        let mut seen = HashSet::new();
        let children = self
            .windows
            .iter()
            .filter(|entry| seen.insert(entry.group_key.as_str()))
            .map(|entry| {
                let button = self.group_button(entry, self.group_count(&entry.group_key));
                if let Some(tracker) = self.rectangle_tracker.as_ref() {
                    tracker
                        .container(tracker_id(&entry.group_key), button)
                        .into()
                } else {
                    button
                }
            })
            .collect::<Vec<_>>();

        let content: cosmic::Element<_> = if self.core.applet.is_horizontal() {
            cosmic::widget::row::with_children(children)
                .spacing(self.core.applet.spacing as f32)
                .align_y(iced::Alignment::Center)
                .width(Length::Shrink)
                .height(Length::Shrink)
                .into()
        } else {
            cosmic::widget::column::with_children(children)
                .spacing(self.core.applet.spacing as f32)
                .align_x(iced::Alignment::Center)
                .width(Length::Shrink)
                .height(Length::Shrink)
                .into()
        };

        autosize(content, AUTOSIZE_MAIN_ID.clone())
            .limits(Limits::NONE.min_width(1.0).min_height(1.0))
            .into()
    }

    fn view_window(&self, id: WindowId) -> cosmic::Element<'_, Self::Message> {
        if self.group_popup == Some(id) {
            self.core
                .applet
                .popup_container(self.group_popup_view())
                .into()
        } else {
            cosmic::widget::space::horizontal().into()
        }
    }

    fn on_close_requested(&self, id: WindowId) -> Option<Self::Message> {
        Some(Message::PopupClosed(id))
    }
}

fn tracker_id(group: &str) -> u64 {
    // Stable FNV-1a is enough for widget tracking and avoids storing String IDs in the
    // rectangle tracker. A collision would only affect popup positioning for that session.
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in group.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn canonical_group_key(app_id: &str, app_label: &str) -> String {
    let raw = if app_id.trim().is_empty() {
        app_label.trim()
    } else {
        app_id.trim()
    };
    let normalized = normalize_identifier(raw.trim_end_matches(".desktop"));

    const BROWSER_ALIASES: &[(&str, &str)] = &[
        ("brave", "browser:brave"),
        ("firefox", "browser:firefox"),
        ("chromium", "browser:chromium"),
        ("googlechrome", "browser:chrome"),
        ("chrome", "browser:chrome"),
        ("vivaldi", "browser:vivaldi"),
        ("opera", "browser:opera"),
        ("microsoftedge", "browser:edge"),
    ];
    for (needle, canonical) in BROWSER_ALIASES {
        if normalized.contains(needle) {
            return (*canonical).to_owned();
        }
    }

    if normalized.is_empty() {
        "application".to_owned()
    } else {
        normalized
    }
}

fn normalize_identifier(input: &str) -> String {
    input
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}
