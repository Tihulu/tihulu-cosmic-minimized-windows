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
    widget::{Image, autosize::autosize, image::Handle},
};

use crate::{
    config::{self, FeatureMode, Settings},
    media_client,
    media_ipc::{MediaAction, MediaPlayerState},
    popup::{CloseGuard, CloseOutcome, OpenPlan, PopupFsm},
    preview_client::{self, PreviewPayload},
    wayland::{self, BridgeCommand, BridgeEvent, WindowDelta},
};

const APP_ID: &str = "io.github.tihulu.MinimizedWindows";
const SETTINGS_GROUP: &str = "__tihulu_settings__";
const LEAVE_GRACE: Duration = Duration::from_millis(650);
const PREVIEW_HEALTH_INTERVAL: Duration = Duration::from_secs(15);
const POPUP_WIDTH: f32 = 372.0;
const POPUP_PADDING: f32 = 12.0;
const POPUP_MAX_HEIGHT: f32 = 420.0;
const COMPACT_ROW_HEIGHT_ESTIMATE: f32 = 52.0;
const PREVIEW_ROW_HEIGHT_ESTIMATE: f32 = 224.0;
const PREVIEW_WIDTH: f32 = 320.0;
const PREVIEW_HEIGHT: f32 = 180.0;
const MAX_APPLET_PREVIEWS: usize = 16;

static AUTOSIZE_MAIN_ID: LazyLock<WidgetId> =
    LazyLock::new(|| WidgetId::new("tihulu-minimized-windows-main"));

pub(crate) fn run() -> cosmic::iced::Result {
    cosmic::applet::run::<MinimizedWindows>(())
}

struct Entry {
    handle: ExtForeignToplevelHandleV1,
    identifier: String,
    group_key: String,
    app_label: String,
    title: String,
    icon: fde::IconSource,
}

#[derive(Clone)]
struct PreviewEntry {
    generation: u64,
    handle: Handle,
}

struct RemovedWindow {
    group: Option<String>,
    identifier: Option<String>,
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
    previews: HashMap<String, PreviewEntry>,
    preview_healthy: bool,
    preview_health_generation: u64,
    media_players: HashMap<String, MediaPlayerState>,
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
    MediaLoaded(String, Result<Option<MediaPlayerState>, String>),
    MediaControl {
        group: String,
        bus_name: String,
        action: MediaAction,
    },
    MediaControlDone(String, Result<(), String>),
    PreviewLoaded(String, Result<PreviewPayload, String>),
    PreviewBatchLoaded(Vec<(String, Result<PreviewPayload, String>)>),
    PreviewHealthTick(u64),
    PreviewHealthChecked(u64, Result<(), String>),
    PreviewMaintenanceDone,
    Surface(cosmic::surface::Action),
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
        let identifier = info.identifier.trim().to_owned();
        let app_id = info.app_id.trim().to_owned();
        let (app_label, icon, group_key) = self.app_visuals(&app_id);
        let title = if info.title.trim().is_empty() {
            app_label.clone()
        } else {
            info.title.trim().to_owned()
        };

        let entry = Entry {
            handle: handle.clone(),
            identifier,
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

    fn remove(&mut self, handle: &ExtForeignToplevelHandleV1) -> RemovedWindow {
        let group = self
            .windows
            .iter()
            .find(|entry| &entry.handle == handle)
            .map(|entry| entry.group_key.clone());
        let identifier = self
            .windows
            .iter()
            .find(|entry| &entry.handle == handle)
            .map(|entry| entry.identifier.clone())
            .filter(|identifier| !identifier.is_empty());
        self.windows.retain(|window| &window.handle != handle);
        if let Some(identifier) = identifier.as_deref() {
            self.previews.remove(identifier);
        }
        if let Some(group) = group.as_deref()
            && self.group_count(group) == 0
        {
            self.media_players.remove(group);
        }
        RemovedWindow { group, identifier }
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

    fn preview_requested(&self) -> bool {
        !self.settings.mode.safe_core() && self.settings.preview_enabled
    }

    fn preview_active(&self) -> bool {
        self.preview_requested() && self.preview_healthy
    }

    fn media_requested(&self) -> bool {
        !self.settings.mode.safe_core() && self.settings.media_enabled
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

        let area =
            cosmic::widget::mouse_area(button).on_right_press(Message::GroupOpen(group.clone()));
        if self.settings.hover_popups {
            area.on_enter(Message::GroupHoverEnter(group.clone()))
                .on_exit(Message::GroupHoverExit(group))
                .into()
        } else {
            let tooltip = self
                .windows
                .iter()
                .filter(|window| window.group_key == group)
                .map(|window| window.title.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            self.core
                .applet
                .applet_tooltip(area, tooltip, self.popup.is_open(), Message::Surface, None)
                .into()
        }
    }

    fn settings_button(&self) -> cosmic::Element<'_, Message> {
        let handle = cosmic::widget::icon::from_name(APP_ID).handle();
        let size = self.core.applet.suggested_size(false);
        let (major, minor) = self.core.applet.suggested_padding(false);
        let (px, py) = if self.core.applet.is_horizontal() {
            (major, minor)
        } else {
            (minor, major)
        };
        let logo = cosmic::widget::icon(handle)
            .width(Length::Fixed(size.0 as f32))
            .height(Length::Fixed(size.1 as f32));
        let button = cosmic::widget::button::custom(logo)
            .class(cosmic::theme::Button::AppletIcon)
            .padding([py as f32, px as f32])
            .on_press_down(Message::OpenSettings);

        self.core
            .applet
            .applet_tooltip(
                button,
                "Tihulu Minimizer Settings",
                self.popup.is_open(),
                Message::Surface,
                None,
            )
            .into()
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

    fn media_status_task(group: String) -> Task<Message> {
        let app_hint = group.clone();
        Task::perform(media_client::status(app_hint), move |result| {
            cosmic::Action::App(Message::MediaLoaded(group, result))
        })
    }

    fn media_control_task(group: String, bus_name: String, action: MediaAction) -> Task<Message> {
        Task::perform(media_client::control(bus_name, action), move |result| {
            cosmic::Action::App(Message::MediaControlDone(group, result))
        })
    }

    fn open_group(&mut self, group: String, pinned: bool) -> Task<Message> {
        if self.group_count(&group) == 0 {
            return cosmic::task::none();
        }

        let mut tasks = vec![self.request_popup(group.clone(), pinned)];
        if pinned && self.media_requested() {
            tasks.push(Self::media_status_task(group.clone()));
        }
        if pinned && self.preview_requested() {
            if self.preview_healthy {
                tasks.push(self.capture_group_task(&group));
            } else {
                tasks.push(Self::health_check_task(self.preview_health_generation));
            }
        }
        Task::batch(tasks)
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

    fn capture_group_task(&self, group: &str) -> Task<Message> {
        let requests = self
            .windows
            .iter()
            .filter(|entry| entry.group_key == group)
            .filter(|entry| !entry.identifier.is_empty())
            .filter(|entry| !self.previews.contains_key(&entry.identifier))
            .take(MAX_APPLET_PREVIEWS)
            .map(|entry| (entry.identifier.clone(), entry.identifier.clone()))
            .collect::<Vec<_>>();
        if requests.is_empty() {
            return cosmic::task::none();
        }
        Task::perform(preview_client::capture_many(requests), |results| {
            cosmic::Action::App(Message::PreviewBatchLoaded(results))
        })
    }

    fn health_delay_task(generation: u64) -> Task<Message> {
        Task::perform(
            async move {
                tokio::time::sleep(PREVIEW_HEALTH_INTERVAL).await;
                generation
            },
            |generation| cosmic::Action::App(Message::PreviewHealthTick(generation)),
        )
    }

    fn health_check_task(generation: u64) -> Task<Message> {
        Task::perform(preview_client::health(), move |result| {
            cosmic::Action::App(Message::PreviewHealthChecked(generation, result))
        })
    }

    fn clear_preview_task() -> Task<Message> {
        Task::perform(preview_client::clear(), |_| {
            cosmic::Action::App(Message::PreviewMaintenanceDone)
        })
    }

    fn gone_task(identifier: Option<String>) -> Task<Message> {
        let Some(identifier) = identifier else {
            return cosmic::task::none();
        };
        Task::perform(preview_client::gone(identifier), |_| {
            cosmic::Action::App(Message::PreviewMaintenanceDone)
        })
    }

    fn compact_window_row<'a>(&self, entry: &'a Entry) -> cosmic::Element<'a, Message> {
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

    fn preview_window_row<'a>(
        &self,
        entry: &'a Entry,
        preview: &PreviewEntry,
    ) -> cosmic::Element<'a, Message> {
        let image = Image::new(preview.handle.clone())
            .width(Length::Fixed(PREVIEW_WIDTH))
            .height(Length::Fixed(PREVIEW_HEIGHT))
            .content_fit(iced::ContentFit::Contain);
        let restore = cosmic::widget::button::custom(image)
            .padding(0)
            .on_press(Message::Restore(entry.handle.clone()));
        let footer = cosmic::widget::row::with_children(vec![
            cosmic::widget::text(&entry.title)
                .width(Length::Fill)
                .into(),
            cosmic::widget::button::text("×")
                .on_press(Message::CloseWindow(entry.handle.clone()))
                .into(),
        ])
        .spacing(6.0)
        .align_y(iced::Alignment::Center)
        .width(Length::Fill);

        cosmic::widget::column::with_children(vec![restore.into(), footer.into()])
            .spacing(5.0)
            .width(Length::Fill)
            .into()
    }

    fn preview_for(&self, entry: &Entry) -> Option<&PreviewEntry> {
        if !self.popup.is_pinned() {
            return None;
        }
        self.preview_active()
            .then(|| self.previews.get(&entry.identifier))
            .flatten()
    }

    fn window_row<'a>(&self, entry: &'a Entry) -> cosmic::Element<'a, Message> {
        if let Some(preview) = self.preview_for(entry) {
            self.preview_window_row(entry, preview)
        } else {
            self.compact_window_row(entry)
        }
    }

    fn media_section<'a>(&'a self, group: &str) -> Option<cosmic::Element<'a, Message>> {
        if !self.popup.is_pinned() || !self.media_requested() {
            return None;
        }
        let player = self.media_players.get(group)?;
        let title = if player.title.trim().is_empty() {
            player.identity.as_str()
        } else {
            player.title.as_str()
        };
        let detail = if player.artist.trim().is_empty() {
            player.playback_status.clone()
        } else {
            format!("{} · {}", player.artist, player.playback_status)
        };

        let control = |label: &'static str, action: MediaAction, enabled: bool| {
            let button = cosmic::widget::button::text(label);
            if enabled {
                button.on_press(Message::MediaControl {
                    group: group.to_owned(),
                    bus_name: player.bus_name.clone(),
                    action,
                })
            } else {
                button
            }
        };
        let controls = cosmic::widget::row::with_children(vec![
            control("Previous", MediaAction::Previous, player.can_previous).into(),
            control(
                "Play / Pause",
                MediaAction::PlayPause,
                player.can_play_pause,
            )
            .into(),
            control("Next", MediaAction::Next, player.can_next).into(),
        ])
        .spacing(6.0)
        .align_y(iced::Alignment::Center);

        Some(
            cosmic::widget::column::with_children(vec![
                cosmic::widget::text(title).width(Length::Fill).into(),
                cosmic::widget::text(detail).width(Length::Fill).into(),
                controls.into(),
            ])
            .spacing(5.0)
            .width(Length::Fill)
            .into(),
        )
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
            .spacing(8.0)
            .width(Length::Fill);
        let total_height = entries
            .iter()
            .map(|entry| {
                if self.preview_for(entry).is_some() {
                    PREVIEW_ROW_HEIGHT_ESTIMATE
                } else {
                    COMPACT_ROW_HEIGHT_ESTIMATE
                }
            })
            .sum::<f32>();
        let viewport_height = total_height.clamp(COMPACT_ROW_HEIGHT_ESTIMATE, POPUP_MAX_HEIGHT);
        let list: cosmic::Element<_> = if total_height > POPUP_MAX_HEIGHT {
            cosmic::widget::scrollable::vertical(list)
                .height(Length::Fixed(viewport_height))
                .width(Length::Fill)
                .into()
        } else {
            list.into()
        };

        let mut children: Vec<cosmic::Element<'_, Message>> = vec![header.into()];
        if let Some(media) = self.media_section(group) {
            children.push(media);
        }
        children.push(list);
        let content = cosmic::widget::column::with_children(children)
            .spacing(9.0)
            .width(Length::Fill);
        let content = cosmic::widget::container(content)
            .width(Length::Fixed(POPUP_WIDTH))
            .padding(POPUP_PADDING);

        cosmic::widget::mouse_area(content)
            .on_enter(Message::PopupEnter)
            .on_exit(Message::PopupExit)
            .into()
    }

    fn settings_popup_view(&self) -> cosmic::Element<'_, Message> {
        let title = cosmic::widget::text::title3("Tihulu Minimizer Settings");
        let safe_core = cosmic::widget::toggler(self.settings.mode.safe_core())
            .label(Some("Safe Core".to_owned()))
            .on_toggle(Message::ToggleSafeCore)
            .width(Length::Fill);
        let media = cosmic::widget::toggler(self.settings.media_enabled)
            .label(Some("Media".to_owned()))
            .on_toggle(Message::ToggleMedia)
            .width(Length::Fill);
        let preview = cosmic::widget::toggler(self.settings.preview_enabled)
            .label(Some("Preview".to_owned()))
            .on_toggle(Message::TogglePreview)
            .width(Length::Fill);
        let hover = cosmic::widget::toggler(self.settings.hover_popups)
            .label(Some("Hover (experimental)".to_owned()))
            .on_toggle(Message::ToggleHover)
            .width(Length::Fill);

        let mode_note = if self.settings.mode.safe_core() {
            "Safe Core is active. Media, Preview and Hover are off. Enabling any rich option exits Safe Core."
        } else {
            "Safe Core is off. Rich features use external daemons and fall back independently when unavailable."
        };
        let media_note = if self.settings.media_enabled {
            "Media backend: enabled · tihulu-mediad is queried when a click popup opens"
        } else {
            "Media backend: disabled"
        };
        let preview_note = if !self.settings.preview_enabled {
            "Preview backend: disabled"
        } else if self.preview_healthy {
            "Preview backend: active · tihulu-previewd healthy"
        } else {
            "Preview backend: enabled · tihulu-previewd unavailable or waiting; compact fallback active"
        };

        let content = cosmic::widget::column::with_children(vec![
            title.into(),
            safe_core.into(),
            media.into(),
            preview.into(),
            hover.into(),
            cosmic::widget::text(mode_note).width(Length::Fill).into(),
            cosmic::widget::text(media_note).width(Length::Fill).into(),
            cosmic::widget::text(preview_note)
                .width(Length::Fill)
                .into(),
        ])
        .spacing(10.0)
        .width(Length::Fill);

        cosmic::widget::container(content)
            .width(Length::Fixed(POPUP_WIDTH))
            .padding(POPUP_PADDING)
            .into()
    }

    fn accept_preview(&mut self, key: String, payload: PreviewPayload) {
        if !self.preview_requested()
            || payload.key != key
            || !self.windows.iter().any(|entry| entry.identifier == key)
        {
            return;
        }
        if self
            .previews
            .get(&key)
            .is_some_and(|current| current.generation > payload.generation)
        {
            return;
        }
        if !self.previews.contains_key(&key)
            && self.previews.len() >= MAX_APPLET_PREVIEWS
            && let Some(victim) = self.previews.keys().next().cloned()
        {
            self.previews.remove(&victim);
        }
        let handle = Handle::from_rgba(payload.width, payload.height, payload.rgba);
        self.previews.insert(
            key,
            PreviewEntry {
                generation: payload.generation,
                handle,
            },
        );
    }

    fn preview_window_failed(&mut self, key: &str, error: &str) {
        self.previews.remove(key);
        tracing::debug!(
            preview_key = key,
            ?error,
            "preview capture failed; compact fallback kept for this window"
        );
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
        if app.preview_requested() {
            app.preview_health_generation = 1;
            let generation = app.preview_health_generation;
            (app, Self::health_check_task(generation))
        } else {
            (app, cosmic::task::none())
        }
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
                        let preview_was_requested = self.preview_requested();
                        let removed = self.remove(&handle);
                        let active_disappeared = removed.group.as_deref().is_some_and(|group| {
                            self.popup.active_group() == Some(group) && self.group_count(group) == 0
                        });
                        let close = if active_disappeared {
                            self.close_popup()
                        } else {
                            cosmic::task::none()
                        };
                        let maintenance = if preview_was_requested {
                            Self::gone_task(removed.identifier)
                        } else {
                            cosmic::task::none()
                        };
                        return Task::batch([close, maintenance]);
                    }
                },
            },
            Message::GroupPrimary(group) => {
                if self.group_count(&group) > 0 {
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
                if enabled {
                    let close_hover_popup = self.popup.is_hover_open();
                    self.settings.mode = FeatureMode::SafeCore;
                    self.settings.media_enabled = false;
                    self.settings.preview_enabled = false;
                    self.settings.hover_popups = false;
                    self.media_players.clear();
                    self.preview_health_generation = self.preview_health_generation.wrapping_add(1);
                    self.preview_healthy = false;
                    self.previews.clear();
                    self.persist_settings();
                    let close = if close_hover_popup {
                        self.close_popup()
                    } else {
                        cosmic::task::none()
                    };
                    return Task::batch([close, Self::clear_preview_task()]);
                }
                self.settings.mode = FeatureMode::Extended;
                self.persist_settings();
            }
            Message::ToggleMedia(enabled) => {
                self.settings.media_enabled = enabled;
                if enabled {
                    self.settings.mode = FeatureMode::Extended;
                } else {
                    self.media_players.clear();
                }
                self.persist_settings();
            }
            Message::TogglePreview(enabled) => {
                self.settings.preview_enabled = enabled;
                self.preview_health_generation = self.preview_health_generation.wrapping_add(1);
                let generation = self.preview_health_generation;
                self.preview_healthy = false;
                self.previews.clear();
                if enabled {
                    self.settings.mode = FeatureMode::Extended;
                    self.persist_settings();
                    return Self::health_check_task(generation);
                }
                self.persist_settings();
                return Self::clear_preview_task();
            }
            Message::ToggleHover(enabled) => {
                self.settings.hover_popups = enabled;
                if enabled {
                    self.settings.mode = FeatureMode::Extended;
                }
                self.persist_settings();
                if !enabled && self.popup.is_hover_open() {
                    return self.close_popup();
                }
            }
            Message::MediaLoaded(group, result) => {
                if !self.media_requested() || self.group_count(&group) == 0 {
                    self.media_players.remove(&group);
                } else {
                    match result {
                        Ok(Some(player)) => {
                            self.media_players.insert(group, player);
                        }
                        Ok(None) => {
                            self.media_players.remove(&group);
                        }
                        Err(error) => {
                            self.media_players.remove(&group);
                            tracing::debug!(
                                ?error,
                                "mediad unavailable; normal popup remains active"
                            );
                        }
                    }
                }
            }
            Message::MediaControl {
                group,
                bus_name,
                action,
            } => {
                if self.media_requested() && self.group_count(&group) > 0 {
                    return Self::media_control_task(group, bus_name, action);
                }
            }
            Message::MediaControlDone(group, result) => {
                if let Err(error) = result {
                    tracing::warn!(?error, "MPRIS control failed");
                }
                if self.media_requested() && self.group_count(&group) > 0 {
                    return Self::media_status_task(group);
                }
            }
            Message::PreviewLoaded(key, result) => match result {
                Ok(payload) => self.accept_preview(key, payload),
                Err(error) => self.preview_window_failed(&key, &error),
            },
            Message::PreviewBatchLoaded(results) => {
                for (key, result) in results {
                    match result {
                        Ok(payload) => self.accept_preview(key, payload),
                        Err(error) => self.preview_window_failed(&key, &error),
                    }
                }
            }
            Message::PreviewHealthTick(generation) => {
                if generation == self.preview_health_generation && self.preview_requested() {
                    return Self::health_check_task(generation);
                }
            }
            Message::PreviewHealthChecked(generation, result) => {
                if generation != self.preview_health_generation || !self.preview_requested() {
                    return cosmic::task::none();
                }

                let recovery = match result {
                    Ok(()) => {
                        let was_healthy = self.preview_healthy;
                        self.preview_healthy = true;
                        if was_healthy || !self.popup.is_pinned() {
                            cosmic::task::none()
                        } else if let Some(group) = self.popup.active_group().map(str::to_owned) {
                            if group == SETTINGS_GROUP || self.group_count(&group) == 0 {
                                cosmic::task::none()
                            } else {
                                self.capture_group_task(&group)
                            }
                        } else {
                            cosmic::task::none()
                        }
                    }
                    Err(error) => {
                        if self.preview_healthy || !self.previews.is_empty() {
                            tracing::warn!(
                                ?error,
                                "previewd health check failed; compact fallback active"
                            );
                        }
                        self.preview_healthy = false;
                        self.previews.clear();
                        cosmic::task::none()
                    }
                };
                return Task::batch([recovery, Self::health_delay_task(generation)]);
            }
            Message::PreviewMaintenanceDone => {}
            Message::Surface(action) => {
                return cosmic::task::message(cosmic::Action::Cosmic(
                    cosmic::app::Action::Surface(action),
                ));
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
