// SPDX-License-Identifier: AGPL-3.0-only

use std::{
    borrow::Cow,
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, LazyLock},
    time::{Duration, Instant},
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
    media::{self, MediaArtwork, MediaCommand, MediaSnapshot},
    wayland::{self, BridgeCommand, BridgeEvent, WindowDelta},
};

const APP_ID: &str = "io.github.tihulu.MinimizedWindows";
const HOVER_DELAY: Duration = Duration::from_millis(350);
const LEAVE_GRACE: Duration = Duration::from_millis(500);
const SINGLE_PREVIEW_WIDTH: f32 = 320.0;
const SINGLE_PREVIEW_HEIGHT: f32 = 180.0;
const GROUP_PREVIEW_WIDTH: f32 = 260.0;
const GROUP_PREVIEW_HEIGHT: f32 = 146.0;
const GROUP_COLUMNS: usize = 2;
const GROUP_GRID_GAP: f32 = 12.0;
const GROUP_MAX_VIEWPORT_HEIGHT: f32 = 520.0;
const MAX_PREVIEW_IMAGES: usize = 8;

static AUTOSIZE_MAIN_ID: LazyLock<WidgetId> =
    LazyLock::new(|| WidgetId::new("tihulu-minimized-windows-main"));

pub(crate) fn run() -> cosmic::iced::Result {
    cosmic::applet::run::<MinimizedWindows>(())
}

struct Entry {
    handle: ExtForeignToplevelHandleV1,
    group_key: String,
    app_id: String,
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
    hover_epoch: u64,
    close_epoch: u64,
    popup_hovered: bool,
    popup_pinned: bool,
    preview_popup: Option<WindowId>,
    preview_images: HashMap<ExtForeignToplevelHandleV1, Handle>,
    preview_queue: VecDeque<ExtForeignToplevelHandleV1>,
    preview_inflight: Option<ExtForeignToplevelHandleV1>,
    media: Option<MediaSnapshot>,
    media_loaded_at: Option<Instant>,
    media_art_url: Option<String>,
    media_art: Option<Handle>,
    last_nonzero_volume: f64,
}

#[derive(Clone, Copy, Debug)]
enum MediaUiAction {
    Previous,
    PlayPause,
    Next,
    VolumeDown,
    Mute,
    VolumeUp,
}

#[derive(Clone, Debug)]
enum Message {
    Bridge(Box<BridgeEvent>),
    GroupPrimary(String),
    GroupOpen(String),
    GroupHoverEnter(String),
    GroupHoverExit(String),
    HoverDelayElapsed(String, u64),
    PopupEnter,
    PopupExit,
    CloseDelayElapsed(u64),
    Restore(ExtForeignToplevelHandleV1),
    CloseWindow(ExtForeignToplevelHandleV1),
    PreviewClosed(WindowId),
    MediaLoaded(String, Option<Box<MediaSnapshot>>),
    MediaArtworkLoaded(String, String, Option<Arc<MediaArtwork>>),
    MediaControl(MediaUiAction),
    RefreshMedia(String),
    MediaTick,
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
            app_id,
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

    fn group_entry(&self, group: &str) -> Option<&Entry> {
        self.windows.iter().find(|entry| entry.group_key == group)
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

    fn group_titles(&self, group: &str) -> Vec<String> {
        self.windows
            .iter()
            .filter(|entry| entry.group_key == group)
            .map(|entry| entry.title.clone())
            .collect()
    }

    fn group_contains_handle(&self, group: &str, handle: &ExtForeignToplevelHandleV1) -> bool {
        self.windows
            .iter()
            .any(|entry| entry.group_key == group && &entry.handle == handle)
    }

    fn group_button<'a>(&self, entry: &'a Entry, _count: usize) -> cosmic::Element<'a, Message> {
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

        let button = cosmic::widget::button::custom(
            cosmic::widget::icon(icon)
                .width(Length::Fixed(size.0 as f32))
                .height(Length::Fixed(size.1 as f32)),
        )
        .class(cosmic::theme::Button::AppletIcon)
        .padding([py as f32, px as f32])
        .on_press_down(Message::GroupPrimary(group.clone()));

        // Tooltips create a second hover surface and can generate enter/leave churn
        // while the delayed preview is being armed. Keep one pointer surface per icon.
        cosmic::widget::mouse_area(button)
            .on_enter(Message::GroupHoverEnter(group.clone()))
            .on_exit(Message::GroupHoverExit(group.clone()))
            .on_right_press(Message::GroupOpen(group))
            .into()
    }

    fn reset_popup_payload(&mut self) {
        self.preview_images.clear();
        self.preview_queue.clear();
        self.preview_inflight = None;
        self.media = None;
        self.media_loaded_at = None;
        self.media_art_url = None;
        self.media_art = None;
    }

    fn clear_popup_state(&mut self) {
        self.reset_popup_payload();
        self.active_group = None;
        self.popup_hovered = false;
        self.popup_pinned = false;
    }

    fn close_preview_surface(&mut self) -> Task<Message> {
        self.clear_popup_state();
        let Some(id) = self.preview_popup.take() else {
            return cosmic::task::none();
        };

        use cosmic::iced::platform_specific::shell::commands::popup::destroy_popup;
        destroy_popup(id)
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

    fn preview_anchor_rect(&self, group: &str) -> iced::Rectangle<i32> {
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

    fn open_preview_surface(&mut self, group: &str) -> Task<Message> {
        use cosmic::iced::platform_specific::shell::commands::popup::{destroy_popup, get_popup};

        let previous = self.preview_popup.take();
        let id = WindowId::unique();
        self.preview_popup = Some(id);

        let mut settings = self.core.applet.get_popup_settings(
            self.core.main_window_id().unwrap(),
            id,
            None,
            None,
            None,
        );
        settings.positioner.anchor_rect = self.preview_anchor_rect(group);
        let open = get_popup(settings);

        if let Some(previous) = previous {
            Task::batch([destroy_popup(previous), open])
        } else {
            open
        }
    }

    fn hover_delay_task(group: String, epoch: u64) -> Task<Message> {
        Task::perform(
            async move {
                tokio::time::sleep(HOVER_DELAY).await;
                (group, epoch)
            },
            |(group, epoch)| cosmic::Action::App(Message::HoverDelayElapsed(group, epoch)),
        )
    }

    fn close_delay_task(epoch: u64) -> Task<Message> {
        Task::perform(
            async move {
                tokio::time::sleep(LEAVE_GRACE).await;
                epoch
            },
            |epoch| cosmic::Action::App(Message::CloseDelayElapsed(epoch)),
        )
    }

    fn load_media_task(&self, group: &str) -> Task<Message> {
        let Some(entry) = self.group_entry(group) else {
            return cosmic::task::none();
        };
        let app_id = entry.app_id.clone();
        let app_label = entry.app_label.clone();
        let titles = self.group_titles(group);
        let group = group.to_owned();

        Task::perform(
            media::snapshot(app_id, app_label, titles),
            move |snapshot| {
                cosmic::Action::App(Message::MediaLoaded(group, snapshot.map(Box::new)))
            },
        )
    }

    fn load_art_task(group: String, url: String) -> Task<Message> {
        let request_url = url.clone();
        Task::perform(media::load_art(request_url), move |artwork| {
            cosmic::Action::App(Message::MediaArtworkLoaded(
                group,
                url,
                artwork.map(Arc::new),
            ))
        })
    }

    fn request_next_preview(&mut self) {
        if self.preview_inflight.is_some() {
            return;
        }
        let Some(group) = self.active_group.clone() else {
            return;
        };
        let Some(tx) = self.command_tx.clone() else {
            return;
        };

        while let Some(handle) = self.preview_queue.pop_front() {
            if !self.group_contains_handle(&group, &handle) {
                continue;
            }
            self.preview_inflight = Some(handle.clone());
            if tx.send(BridgeCommand::CapturePreview(handle)).is_ok() {
                break;
            }
            self.preview_inflight = None;
        }
    }

    fn start_preview_sequence(&mut self, group: &str) {
        self.preview_images.clear();
        self.preview_queue = self
            .group_handles(group)
            .into_iter()
            .take(MAX_PREVIEW_IMAGES)
            .collect();
        self.preview_inflight = None;
        self.request_next_preview();
    }

    fn refresh_open_group(&mut self, group: &str) -> Task<Message> {
        self.start_preview_sequence(group);
        self.load_media_task(group)
    }

    fn open_group(&mut self, group: String, pinned: bool) -> Task<Message> {
        if self.active_group.as_deref() == Some(group.as_str()) && self.preview_popup.is_some() {
            self.popup_pinned |= pinned;
            return self.refresh_open_group(&group);
        }

        self.reset_popup_payload();
        self.active_group = Some(group.clone());
        self.popup_pinned = pinned;
        self.popup_hovered = false;

        let popup = self.open_preview_surface(&group);
        self.start_preview_sequence(&group);
        let media = self.load_media_task(&group);
        Task::batch([popup, media])
    }

    fn schedule_close(&mut self) -> Task<Message> {
        self.close_epoch = self.close_epoch.wrapping_add(1);
        Self::close_delay_task(self.close_epoch)
    }

    fn preview_dimensions(count: usize) -> (f32, f32, usize) {
        if count <= 1 {
            (SINGLE_PREVIEW_WIDTH, SINGLE_PREVIEW_HEIGHT, 1)
        } else {
            (GROUP_PREVIEW_WIDTH, GROUP_PREVIEW_HEIGHT, GROUP_COLUMNS)
        }
    }

    fn preview_visual<'a>(
        &self,
        entry: &'a Entry,
        width: f32,
        height: f32,
    ) -> cosmic::Element<'a, Message> {
        if let Some(handle) = self.preview_images.get(&entry.handle) {
            Image::new(handle.clone())
                .width(Length::Fixed(width))
                .height(Length::Fixed(height))
                .content_fit(iced::ContentFit::Contain)
                .into()
        } else {
            let icon = entry.icon.as_cosmic_icon();
            cosmic::widget::container(
                cosmic::widget::icon(icon)
                    .width(Length::Fixed(72.0))
                    .height(Length::Fixed(72.0)),
            )
            .center_x(Length::Fixed(width))
            .center_y(Length::Fixed(height))
            .into()
        }
    }

    fn window_preview_card<'a>(
        &self,
        entry: &'a Entry,
        width: f32,
        height: f32,
    ) -> cosmic::Element<'a, Message> {
        let visual = self.preview_visual(entry, width, height);
        let image_button = cosmic::widget::button::custom_image_button(
            visual,
            Some(Message::CloseWindow(entry.handle.clone())),
        )
        .padding(0)
        .on_press(Message::Restore(entry.handle.clone()));

        cosmic::widget::column::with_children(vec![
            image_button.into(),
            cosmic::widget::text(&entry.title).into(),
        ])
        .spacing(5.0)
        .width(Length::Fixed(width))
        .into()
    }

    fn media_progress(&self, media: &MediaSnapshot) -> (f32, i64) {
        if media.length_us <= 0 {
            return (0.0, media.position_us.max(0));
        }

        let elapsed = if media.playing {
            self.media_loaded_at
                .map(|loaded| loaded.elapsed().as_micros())
                .and_then(|micros| i64::try_from(micros).ok())
                .unwrap_or_default()
        } else {
            0
        };
        let position = media
            .position_us
            .saturating_add(elapsed)
            .clamp(0, media.length_us);
        (
            (position as f64 / media.length_us as f64).clamp(0.0, 1.0) as f32,
            position,
        )
    }

    fn media_card(&self) -> Option<cosmic::Element<'_, Message>> {
        let media = self.media.as_ref()?;
        let (progress, position) = self.media_progress(media);

        let artwork: cosmic::Element<_> = if let Some(handle) = &self.media_art {
            Image::new(handle.clone())
                .width(Length::Fixed(112.0))
                .height(Length::Fixed(112.0))
                .content_fit(iced::ContentFit::Cover)
                .into()
        } else if let Some(entry) = self
            .active_group
            .as_deref()
            .and_then(|group| self.group_entry(group))
        {
            cosmic::widget::container(
                cosmic::widget::icon(entry.icon.as_cosmic_icon())
                    .width(Length::Fixed(64.0))
                    .height(Length::Fixed(64.0)),
            )
            .center_x(Length::Fixed(112.0))
            .center_y(Length::Fixed(112.0))
            .into()
        } else {
            cosmic::widget::space::horizontal()
                .width(Length::Fixed(112.0))
                .height(Length::Fixed(112.0))
                .into()
        };

        let identity = cosmic::widget::text(&media.identity);
        let title = cosmic::widget::text(&media.title);
        let artists = cosmic::widget::text(&media.artists);
        let info = cosmic::widget::column::with_children(vec![
            identity.into(),
            title.into(),
            artists.into(),
        ])
        .spacing(4.0)
        .width(Length::Fill);

        let top = cosmic::widget::row::with_children(vec![artwork, info.into()])
            .spacing(12.0)
            .align_y(iced::Alignment::Center);

        let previous = cosmic::widget::button::text("⏮").on_press_maybe(
            media
                .can_previous
                .then_some(Message::MediaControl(MediaUiAction::Previous)),
        );
        let play_pause = cosmic::widget::button::text(if media.playing { "⏸" } else { "▶" })
            .on_press_maybe(
                media
                    .can_play_pause
                    .then_some(Message::MediaControl(MediaUiAction::PlayPause)),
            );
        let next = cosmic::widget::button::text("⏭").on_press_maybe(
            media
                .can_next
                .then_some(Message::MediaControl(MediaUiAction::Next)),
        );
        let controls = cosmic::widget::row::with_children(vec![
            previous.into(),
            play_pause.into(),
            next.into(),
        ])
        .spacing(8.0)
        .align_y(iced::Alignment::Center);

        let progress_bar = cosmic::iced::widget::progress_bar(0.0..=1.0, progress)
            .length(Length::Fill)
            .girth(Length::Fixed(4.0));
        let times = cosmic::widget::row::with_children(vec![
            cosmic::widget::text(format_time(position)).into(),
            cosmic::widget::space::horizontal()
                .width(Length::Fill)
                .into(),
            cosmic::widget::text(format_time(media.length_us)).into(),
        ]);

        let display_volume = if media.muted { 0.0 } else { media.volume };
        let volume_down = cosmic::widget::button::text("−")
            .on_press(Message::MediaControl(MediaUiAction::VolumeDown));
        let mute = cosmic::widget::button::text(if media.muted || media.volume <= 0.01 {
            "🔈"
        } else {
            "🔇"
        })
        .on_press(Message::MediaControl(MediaUiAction::Mute));
        let volume_up = cosmic::widget::button::text("+")
            .on_press(Message::MediaControl(MediaUiAction::VolumeUp));
        let volume_bar = cosmic::iced::widget::progress_bar(0.0..=1.5, display_volume as f32)
            .length(Length::Fixed(120.0))
            .girth(Length::Fixed(4.0));
        let volume = cosmic::widget::row::with_children(vec![
            volume_down.into(),
            mute.into(),
            volume_bar.into(),
            volume_up.into(),
        ])
        .spacing(6.0)
        .align_y(iced::Alignment::Center);

        Some(
            cosmic::widget::column::with_children(vec![
                top.into(),
                controls.into(),
                progress_bar.into(),
                times.into(),
                volume.into(),
            ])
            .spacing(7.0)
            .width(Length::Fill)
            .into(),
        )
    }

    fn group_preview(&self) -> cosmic::Element<'_, Message> {
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

        let mut children: Vec<cosmic::Element<'_, Message>> = Vec::new();
        children.push(
            cosmic::widget::text(format!(
                "{} — {} minimized window{}",
                entries[0].app_label,
                entries.len(),
                if entries.len() == 1 { "" } else { "s" }
            ))
            .into(),
        );

        if let Some(media) = self.media_card() {
            children.push(media);
        }

        let (preview_width, preview_height, columns) = Self::preview_dimensions(entries.len());
        let grid_width =
            preview_width * columns as f32 + GROUP_GRID_GAP * columns.saturating_sub(1) as f32;
        let rows = entries.len().div_ceil(columns);
        let estimated_card_height = preview_height + 42.0;
        let estimated_grid_height =
            estimated_card_height * rows as f32 + GROUP_GRID_GAP * rows.saturating_sub(1) as f32;
        let viewport_height =
            estimated_grid_height.clamp(1.0, GROUP_MAX_VIEWPORT_HEIGHT);

        let mut grid = cosmic::widget::grid::grid()
            .column_spacing(GROUP_GRID_GAP as u16)
            .row_spacing(GROUP_GRID_GAP as u16)
            .max_width(grid_width);
        for (index, entry) in entries.iter().enumerate() {
            grid = grid.push(self.window_preview_card(entry, preview_width, preview_height));
            if (index + 1) % columns == 0 {
                grid = grid.insert_row();
            }
        }

        let grid_view: cosmic::Element<_> = if rows > 2 {
            cosmic::widget::scrollable::vertical(grid)
                .width(Length::Fixed(grid_width + 16.0))
                .height(Length::Fixed(viewport_height))
                .into()
        } else {
            grid.into()
        };
        children.push(grid_view);

        if entries.len() > MAX_PREVIEW_IMAGES {
            children.push(
                cosmic::widget::text(format!(
                    "Live thumbnails are capped at {MAX_PREVIEW_IMAGES}; all windows remain selectable."
                ))
                .into(),
            );
        }

        let content = cosmic::widget::column::with_children(children)
            .spacing(10.0)
            .width(Length::Shrink);

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
            last_nonzero_volume: 1.0,
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
        let bridge = wayland::subscription().map(|event| Message::Bridge(Box::new(event)));
        if self.preview_popup.is_some() && self.media.as_ref().is_some_and(|media| media.playing) {
            Subscription::batch([
                bridge,
                iced::time::every(Duration::from_secs(1)).map(|_| Message::MediaTick),
            ])
        } else {
            bridge
        }
    }

    fn update(&mut self, message: Self::Message) -> Task<Self::Message> {
        match message {
            Message::Bridge(event) => match *event {
                BridgeEvent::Ready(tx) => {
                    self.command_tx = Some(tx);
                    self.request_next_preview();
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
                        let removed_group = self.remove(&handle);
                        self.preview_images.remove(&handle);
                        self.preview_queue.retain(|queued| queued != &handle);

                        let group_disappeared = removed_group.as_deref().is_some_and(|group| {
                            self.active_group.as_deref() == Some(group)
                                && self.group_count(group) == 0
                        });
                        let close = if group_disappeared {
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
                    if self.preview_inflight.as_ref() == Some(&handle) {
                        self.preview_inflight = None;
                        if let Some(group) = self.active_group.as_deref()
                            && self.group_contains_handle(group, &handle)
                            && let Some(image) = image
                        {
                            self.preview_images.insert(
                                handle,
                                Handle::from_rgba(image.width, image.height, image.rgba),
                            );
                        }
                        self.request_next_preview();
                    } else if self.preview_popup.is_some()
                        && let (Some(current), Some(tx)) =
                            (self.preview_inflight.clone(), self.command_tx.clone())
                    {
                        let _ = tx.send(BridgeCommand::CapturePreview(current));
                    }
                }
            },
            Message::GroupPrimary(group) => {
                let handles = self.group_handles(&group);
                if handles.len() == 1 {
                    if let Some(tx) = &self.command_tx {
                        let _ = tx.send(BridgeCommand::Restore(handles[0].clone()));
                    }
                    return self.close_preview_surface();
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
                self.close_epoch = self.close_epoch.wrapping_add(1);
                self.hover_epoch = self.hover_epoch.wrapping_add(1);
                let epoch = self.hover_epoch;
                self.hover_group = Some(group.clone());

                if self.active_group.as_deref() == Some(group.as_str())
                    && self.preview_popup.is_some()
                {
                    if !self.popup_pinned {
                        return self.refresh_open_group(&group);
                    }
                    return cosmic::task::none();
                }

                // Pinned popups no longer globally disable hover. Keep the old popup
                // visible during the delay and replace it only if this hover is still active.
                return Self::hover_delay_task(group, epoch);
            }
            Message::GroupHoverExit(group) => {
                if self.hover_group.as_deref() == Some(group.as_str()) {
                    self.hover_group = None;
                    self.hover_epoch = self.hover_epoch.wrapping_add(1);
                }
                if self.preview_popup.is_some()
                    && self.active_group.as_deref() == Some(group.as_str())
                    && !self.popup_pinned
                {
                    return self.schedule_close();
                }
            }
            Message::HoverDelayElapsed(group, epoch) => {
                if self.hover_epoch == epoch && self.hover_group.as_deref() == Some(group.as_str())
                {
                    return self.open_group(group, false);
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
                    return self.close_preview_surface();
                }
            }
            Message::Restore(handle) => {
                if let Some(tx) = &self.command_tx {
                    let _ = tx.send(BridgeCommand::Restore(handle));
                }
                self.hover_epoch = self.hover_epoch.wrapping_add(1);
                self.hover_group = None;
                return self.close_preview_surface();
            }
            Message::CloseWindow(handle) => {
                if let Some(tx) = &self.command_tx {
                    let _ = tx.send(BridgeCommand::Close(handle));
                }
            }
            Message::PreviewClosed(id) => {
                if self.preview_popup == Some(id) {
                    self.preview_popup = None;
                    self.clear_popup_state();
                }
            }
            Message::MediaLoaded(group, snapshot) => {
                if self.active_group.as_deref() != Some(group.as_str())
                    || self.preview_popup.is_none()
                {
                    return cosmic::task::none();
                }

                let Some(snapshot) = snapshot else {
                    self.media = None;
                    self.media_loaded_at = None;
                    self.media_art_url = None;
                    self.media_art = None;
                    return cosmic::task::none();
                };
                let snapshot = *snapshot;
                if snapshot.volume > 0.01 {
                    self.last_nonzero_volume = snapshot.volume;
                }

                let old_art_url = self.media_art_url.clone();
                let new_art_url = snapshot.art_url.clone();
                self.media = Some(snapshot);
                self.media_loaded_at = Some(Instant::now());

                if old_art_url != new_art_url {
                    self.media_art = None;
                    self.media_art_url = new_art_url.clone();
                    if let Some(url) = new_art_url {
                        return Self::load_art_task(group, url);
                    }
                }
            }
            Message::MediaArtworkLoaded(group, url, artwork) => {
                if self.active_group.as_deref() == Some(group.as_str())
                    && self.preview_popup.is_some()
                    && self.media_art_url.as_deref() == Some(url.as_str())
                {
                    self.media_art = artwork.map(|artwork| {
                        let artwork =
                            Arc::try_unwrap(artwork).unwrap_or_else(|artwork| (*artwork).clone());
                        Handle::from_rgba(artwork.width, artwork.height, artwork.rgba)
                    });
                }
            }
            Message::MediaControl(action) => {
                let Some(media) = self.media.as_ref() else {
                    return cosmic::task::none();
                };
                let Some(group) = self.active_group.clone() else {
                    return cosmic::task::none();
                };
                let bus_name = media.bus_name.clone();
                let audio_stream_ids = media.audio_stream_ids.clone();
                let command = match action {
                    MediaUiAction::Previous => MediaCommand::Previous,
                    MediaUiAction::PlayPause => MediaCommand::PlayPause,
                    MediaUiAction::Next => MediaCommand::Next,
                    MediaUiAction::VolumeDown => {
                        MediaCommand::SetVolume((media.volume - 0.05).max(0.0))
                    }
                    MediaUiAction::VolumeUp => {
                        MediaCommand::SetVolume((media.volume + 0.05).min(1.5))
                    }
                    MediaUiAction::Mute => {
                        if !media.muted && media.volume > 0.01 {
                            self.last_nonzero_volume = media.volume;
                        }
                        MediaCommand::SetMuted {
                            muted: !media.muted,
                            restore_volume: self.last_nonzero_volume.max(0.05),
                        }
                    }
                };

                return Task::perform(
                    media::command(bus_name, audio_stream_ids, command),
                    move |_| cosmic::Action::App(Message::RefreshMedia(group)),
                );
            }
            Message::RefreshMedia(group) => {
                if self.active_group.as_deref() == Some(group.as_str())
                    && self.preview_popup.is_some()
                {
                    return self.load_media_task(&group);
                }
            }
            Message::MediaTick => {}
        }

        cosmic::task::none()
    }

    fn view(&self) -> cosmic::Element<'_, Self::Message> {
        let mut seen = HashSet::new();
        let children = self
            .windows
            .iter()
            .filter(|entry| seen.insert(entry.group_key.as_str()))
            .map(|entry| self.group_button(entry, self.group_count(&entry.group_key)))
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
        if self.preview_popup == Some(id) {
            self.core
                .applet
                .popup_container(self.group_preview())
                .into()
        } else {
            cosmic::widget::space::horizontal().into()
        }
    }

    fn on_close_requested(&self, id: WindowId) -> Option<Self::Message> {
        Some(Message::PreviewClosed(id))
    }
}

fn canonical_group_key(app_id: &str, app_label: &str) -> String {
    let raw = if app_id.trim().is_empty() {
        app_label.trim()
    } else {
        app_id.trim()
    };
    raw.trim_end_matches(".desktop").to_ascii_lowercase()
}

fn format_time(microseconds: i64) -> String {
    let seconds = (microseconds.max(0) / 1_000_000) as u64;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}
