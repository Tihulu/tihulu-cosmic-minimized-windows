// SPDX-License-Identifier: AGPL-3.0-only

use std::{borrow::Cow, collections::HashSet, sync::LazyLock, time::Duration};

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
    widget::autosize::autosize,
};

use crate::{
    config::{self, FeatureMode, Settings},
    popup::{CloseGuard, CloseOutcome, OpenPlan, PopupFsm},
    wayland::{self, BridgeCommand, BridgeEvent, WindowDelta},
};

const APP_ID: &str = "io.github.tihulu.MinimizedWindows";
const SETTINGS_GROUP: &str = "__tihulu_settings__";
const LEAVE_GRACE: Duration = Duration::from_millis(650);
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
    popup: PopupFsm,
    settings: Settings,
}

#[derive(Clone, Debug)]
enum Message {
    Bridge(Box<BridgeEvent>),
    GroupPrimary(String),
    GroupOpen(String),
    GroupHoverEnter(String),
    GroupHoverExit(String),
    OpenSettings,
    PopupEnter,
    PopupExit,
    CloseDelayElapsed(CloseGuard),
    Restore(ExtForeignToplevelHandleV1),
    CloseWindow(ExtForeignToplevelHandleV1),
    PopupClosed(WindowId),
    ToggleSafeCore(bool),
    ToggleMedia(bool),
    TogglePreview(bool),
    ToggleHover(bool),
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

        let area = cosmic::widget::mouse_area(button).on_right_press(Message::GroupOpen(group.clone()));
        if self.settings.hover_popups {
            area.on_enter(Message::GroupHoverEnter(group.clone()))
                .on_exit(Message::GroupHoverExit(group))
                .into()
        } else {
            area.into()
        }
    }

    fn settings_button(&self) -> cosmic::Element<'_, Message> {
        use cosmic::widget::tooltip::Position;

        let handle = cosmic::widget::icon::from_name(APP_ID).handle();
        let button = self
            .core
            .applet
            .icon_button_from_handle(handle)
            .on_press_down(Message::OpenSettings);
        let position = match self.core.applet.anchor {
            PanelAnchor::Top => Position::Bottom,
            PanelAnchor::Bottom => Position::Top,
            PanelAnchor::Left => Position::Right,
            PanelAnchor::Right => Position::Left,
        };

        cosmic::widget::tooltip(button, cosmic::widget::text("Settings"), position).into()
    }

    fn popup_anchor_rect(&self, group: &str) -> iced::Rectangle<i32> {
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

    fn popup_task(&self, group: &str, id: WindowId) -> Task<Message> {
        use cosmic::iced::platform_specific::shell::commands::popup::get_popup;

        let mut settings = self.core.applet.get_popup_settings(
            self.core.main_window_id().unwrap(),
            id,
            None,
            None,
            None,
        );
        settings.positioner.anchor_rect = self.popup_anchor_rect(group);
        get_popup(settings)
    }

    fn request_popup(&mut self, group: String, pinned: bool) -> Task<Message> {
        use cosmic::iced::platform_specific::shell::commands::popup::destroy_popup;

        let proposed_id = WindowId::unique();
        match self.popup.request_open(group, pinned, proposed_id) {
            OpenPlan::None => cosmic::task::none(),
            OpenPlan::Create { window_id, group } => self.popup_task(&group, window_id),
            OpenPlan::CloseForSwitch { old_window_id } => destroy_popup(old_window_id),
        }
    }

    fn open_group(&mut self, group: String, pinned: bool) -> Task<Message> {
        if self.group_count(&group) == 0 {
            return cosmic::task::none();
        }
        self.request_popup(group, pinned)
    }

    fn request_settings_open(&mut self) -> Task<Message> {
        self.request_popup(SETTINGS_GROUP.to_owned(), true)
    }

    fn toggle_settings_popup(&mut self) -> Task<Message> {
        if self.popup.active_group() == Some(SETTINGS_GROUP) {
            self.close_popup()
        } else {
            self.request_settings_open()
        }
    }

    fn close_popup(&mut self) -> Task<Message> {
        use cosmic::iced::platform_specific::shell::commands::popup::destroy_popup;

        self.popup
            .close_current()
            .map(destroy_popup)
            .unwrap_or_else(cosmic::task::none)
    }

    fn schedule_close(&mut self) -> Task<Message> {
        let Some(guard) = self.popup.schedule_close() else {
            return cosmic::task::none();
        };
        Task::perform(
            async move {
                tokio::time::sleep(LEAVE_GRACE).await;
                guard
            },
            |guard| cosmic::Action::App(Message::CloseDelayElapsed(guard)),
        )
    }

    fn persist_settings(&self) {
        if let Err(error) = config::save_settings(self.settings) {
            tracing::warn!(?error, "Could not persist applet settings");
        }
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
        let Some(group) = self.popup.active_group() else {
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

        let content = cosmic::widget::column::with_children(vec![header.into(), list])
            .spacing(9.0)
            .width(Length::Fixed(POPUP_WIDTH));

        cosmic::widget::mouse_area(content)
            .on_enter(Message::PopupEnter)
            .on_exit(Message::PopupExit)
            .into()
    }

    fn settings_popup_view(&self) -> cosmic::Element<'_, Message> {
        let title = cosmic::widget::text::title3("Tihulu Minimized Windows");
        let safe_core = cosmic::widget::toggler(self.settings.mode.safe_core())
            .label(Some("Safe Core".to_owned()))
            .on_toggle(Message::ToggleSafeCore)
            .width(Length::Fill);
        let media = cosmic::widget::toggler(self.settings.media_enabled)
            .label(Some("Media controls".to_owned()))
            .on_toggle(Message::ToggleMedia)
            .width(Length::Fill);
        let preview = cosmic::widget::toggler(self.settings.preview_enabled)
            .label(Some("Window previews".to_owned()))
            .on_toggle(Message::TogglePreview)
            .width(Length::Fill);
        let hover = cosmic::widget::toggler(self.settings.hover_popups)
            .label(Some("Hover popups (experimental)".to_owned()))
            .on_toggle(Message::ToggleHover)
            .width(Length::Fill);

        let note = if self.settings.mode.safe_core() {
            "Safe Core is active. Media and Preview preferences are kept, but rich subsystems stay inactive until Safe Core is turned off."
        } else {
            "Extended mode is active. Media and Preview are enabled by default. Their daemon integrations are still being connected on the feature branches."
        };

        cosmic::widget::column::with_children(vec![
            title.into(),
            safe_core.into(),
            media.into(),
            preview.into(),
            hover.into(),
            cosmic::widget::text(note).into(),
        ])
        .spacing(10.0)
        .width(Length::Fixed(POPUP_WIDTH))
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
            settings: config::load_settings(),
            ..Default::default()
        };
        app.reload_desktop_entries();
        (app, cosmic::task::none())
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
                BridgeEvent::Ready(tx) => self.command_tx = Some(tx),
                BridgeEvent::Stopped => {
                    self.command_tx = None;
                    tracing::error!("Minimized-window Wayland bridge stopped");
                }
                BridgeEvent::Window(delta) => match *delta {
                    WindowDelta::Present(info) => self.upsert(*info),
                    WindowDelta::Gone(handle) => {
                        let removed_group = self.remove(&handle);
                        let active_disappeared = removed_group.as_deref().is_some_and(|group| {
                            self.popup.active_group() == Some(group) && self.group_count(group) == 0
                        });
                        if active_disappeared {
                            return self.close_popup();
                        }
                    }
                },
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
                    return self.open_group(group, true);
                }
            }
            Message::GroupOpen(group) => {
                if self.group_count(&group) > 0 {
                    return self.open_group(group, true);
                }
            }
            Message::GroupHoverEnter(group) => {
                if self.settings.hover_popups {
                    self.popup.group_enter(group.clone());
                    if !self.popup.is_pinned() {
                        return self.open_group(group, false);
                    }
                }
            }
            Message::GroupHoverExit(group) => {
                if self.settings.hover_popups {
                    self.popup.group_exit(&group);
                    if self.popup.is_open() && !self.popup.is_pinned() {
                        return self.schedule_close();
                    }
                }
            }
            Message::OpenSettings => return self.toggle_settings_popup(),
            Message::PopupEnter => self.popup.popup_enter(),
            Message::PopupExit => {
                self.popup.popup_exit();
                if !self.popup.is_pinned() {
                    return self.schedule_close();
                }
            }
            Message::CloseDelayElapsed(guard) => {
                if self.popup.should_close(guard) {
                    return self.close_popup();
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
            Message::PopupClosed(id) => match self.popup.compositor_closed(id) {
                CloseOutcome::Ignored | CloseOutcome::Closed => {}
                CloseOutcome::OpenPending { group, pinned } => {
                    if group == SETTINGS_GROUP {
                        return self.request_settings_open();
                    }
                    if self.group_count(&group) > 0 {
                        return self.open_group(group, pinned);
                    }
                }
            },
            Message::ToggleSafeCore(enabled) => {
                self.settings.mode = if enabled {
                    FeatureMode::SafeCore
                } else {
                    FeatureMode::Extended
                };
                self.persist_settings();
            }
            Message::ToggleMedia(enabled) => {
                self.settings.media_enabled = enabled;
                self.persist_settings();
            }
            Message::TogglePreview(enabled) => {
                self.settings.preview_enabled = enabled;
                self.persist_settings();
            }
            Message::ToggleHover(enabled) => {
                self.settings.hover_popups = enabled;
                self.persist_settings();
                if !enabled && self.popup.is_hover_open() {
                    return self.close_popup();
                }
            }
        }

        cosmic::task::none()
    }

    fn view(&self) -> cosmic::Element<'_, Self::Message> {
        let mut seen = HashSet::new();
        let mut children = self
            .windows
            .iter()
            .filter(|entry| seen.insert(entry.group_key.as_str()))
            .map(|entry| self.group_button(entry, self.group_count(&entry.group_key)))
            .collect::<Vec<_>>();
        children.push(self.settings_button());

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
        if self.popup.window_id() == Some(id) {
            let content = if self.popup.active_group() == Some(SETTINGS_GROUP) {
                self.settings_popup_view()
            } else {
                self.group_popup_view()
            };
            self.core.applet.popup_container(content).into()
        } else {
            cosmic::widget::space::horizontal().into()
        }
    }

    fn on_close_requested(&self, id: WindowId) -> Option<Self::Message> {
        Some(Message::PopupClosed(id))
    }
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