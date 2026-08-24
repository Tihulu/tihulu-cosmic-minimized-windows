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
    iced::{
        self, Alignment, Length, Limits, Subscription, id::Id as WidgetId, window::Id as WindowId,
    },
    widget::{Image, autosize::autosize, image::Handle, menu},
};

use crate::{
    media::{self, MediaCommand, MediaState},
    wayland::{self, BridgeCommand, BridgeEvent, PreviewImage, WindowDelta},
};

const APP_ID: &str = "io.github.tihulu.MinimizedWindows";
const HOVER_DELAY: Duration = Duration::from_millis(250);
const HOVER_GRACE: Duration = Duration::from_millis(500);
const MEDIA_DEBOUNCE: Duration = Duration::from_millis(180);
const PREVIEW_WIDTH: f32 = 280.0;
const PREVIEW_HEIGHT: f32 = 158.0;
const MAX_GRID_COLUMNS: usize = 3;

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
    dock_hovered: Option<String>,
    popup_hovered: bool,
    hover_epoch: u64,
    close_epoch: u64,
    preview_generation: u64,
    preview_popup: Option<WindowId>,
    active_group: Option<String>,
    preview_images: HashMap<ExtForeignToplevelHandleV1, PreviewImage>,
    media: Option<MediaState>,
    media_action_epoch: u64,
}

#[derive(Clone, Debug)]
enum Message {
    Bridge(Box<BridgeEvent>),
    GroupPrimary(String),
    Restore(ExtForeignToplevelHandleV1),
    Close(ExtForeignToplevelHandleV1),
    CloseGroup(String),
    HoverEnter(String),
    HoverExit(String),
    HoverDelayElapsed(String, u64),
    PopupEnter,
    PopupExit,
    CloseDelayElapsed(u64),
    PreviewClosed(WindowId),
    Surface(cosmic::surface::Action),
    ContextRestore(usize, usize),
    ContextClose(usize, usize),
    ContextCloseAll(usize),
    ContextOpenGroup(usize),
    MediaLoaded(u64, Option<MediaState>),
    MediaControl(MediaCommand),
    MediaControlFinished(u64),
    MediaTick,
    MediaVolumeChanged(f64),
    MediaVolumeCommit(u64, String, f64),
    MediaSeekChanged(f64),
    MediaSeekCommit(u64, String, f64),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ContextAction {
    Restore(usize, usize),
    Close(usize, usize),
    CloseAll(usize),
    OpenGroup(usize),
}

impl menu::Action for ContextAction {
    type Message = Message;

    fn message(&self) -> Self::Message {
        match *self {
            Self::Restore(group, window) => Message::ContextRestore(group, window),
            Self::Close(group, window) => Message::ContextClose(group, window),
            Self::CloseAll(group) => Message::ContextCloseAll(group),
            Self::OpenGroup(group) => Message::ContextOpenGroup(group),
        }
    }
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
        let app_id = info.app_id.trim().to_owned();
        let group_key = app_id.to_ascii_lowercase();
        let (app_label, icon) = self.app_visuals(&app_id);
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
            .find(|window| &window.handle == handle)
            .map(|window| window.group_key.clone());
        self.windows.retain(|window| &window.handle != handle);
        self.preview_images.remove(handle);
        group
    }

    fn group_keys(&self) -> Vec<String> {
        let mut seen = HashSet::new();
        self.windows
            .iter()
            .filter_map(|entry| {
                if seen.insert(entry.group_key.clone()) {
                    Some(entry.group_key.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    fn group_entries(&self, group_key: &str) -> Vec<&Entry> {
        self.windows
            .iter()
            .filter(|entry| entry.group_key == group_key)
            .collect()
    }

    fn group_handles(&self, group_key: &str) -> Vec<ExtForeignToplevelHandleV1> {
        self.windows
            .iter()
            .filter(|entry| entry.group_key == group_key)
            .map(|entry| entry.handle.clone())
            .collect()
    }

    fn group_at(&self, group_index: usize) -> Option<String> {
        self.group_keys().get(group_index).cloned()
    }

    fn entry_at(&self, group_index: usize, window_index: usize) -> Option<&Entry> {
        let group = self.group_at(group_index)?;
        self.windows
            .iter()
            .filter(|entry| entry.group_key == group)
            .nth(window_index)
    }

    fn group_context_menu(&self, group_index: usize) -> Option<Vec<menu::Tree<Message>>> {
        let group = self.group_at(group_index)?;
        let entries = self.group_entries(&group);
        if entries.is_empty() {
            return None;
        }

        let mut items = Vec::new();
        if entries.len() > 1 {
            items.push(menu::Item::Button(
                "Show previews",
                None,
                ContextAction::OpenGroup(group_index),
            ));
            items.push(menu::Item::Divider);
            for (window_index, entry) in entries.iter().enumerate() {
                items.push(menu::Item::Folder(
                    entry.title.clone(),
                    vec![
                        menu::Item::Button(
                            "Open",
                            None,
                            ContextAction::Restore(group_index, window_index),
                        ),
                        menu::Item::Button(
                            "Close",
                            None,
                            ContextAction::Close(group_index, window_index),
                        ),
                    ],
                ));
            }
            items.push(menu::Item::Divider);
            items.push(menu::Item::Button(
                "Close all windows",
                None,
                ContextAction::CloseAll(group_index),
            ));
        } else {
            items.push(menu::Item::Button(
                "Open",
                None,
                ContextAction::Restore(group_index, 0),
            ));
            items.push(menu::Item::Button(
                "Close",
                None,
                ContextAction::Close(group_index, 0),
            ));
        }

        Some(menu::items(&HashMap::new(), items))
    }

    fn group_button<'a>(
        &'a self,
        group_key: &'a str,
        group_index: usize,
    ) -> cosmic::Element<'a, Message> {
        let Some(entry) = self.windows.iter().find(|entry| entry.group_key == group_key) else {
            return cosmic::widget::space::horizontal().into();
        };

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
        .on_press_down(Message::GroupPrimary(group_key.to_owned()));

        let hover = cosmic::widget::mouse_area(button)
            .on_enter(Message::HoverEnter(group_key.to_owned()))
            .on_exit(Message::HoverExit(group_key.to_owned()));

        cosmic::widget::context_menu(hover, self.group_context_menu(group_index))
            .window_id(self.core.main_window_id().unwrap())
            .on_surface_action(Message::Surface)
            .into()
    }

    fn preview_anchor_rect(&self, group_key: &str) -> iced::Rectangle<i32> {
        let index = self
            .group_keys()
            .iter()
            .position(|key| key == group_key)
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

    fn open_preview_surface(&mut self, group_key: &str) -> Task<Message> {
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
        settings.positioner.anchor_rect = self.preview_anchor_rect(group_key);
        let open = get_popup(settings);

        if let Some(previous) = previous {
            Task::batch([destroy_popup(previous), open])
        } else {
            open
        }
    }

    fn close_preview_surface(&mut self) -> Task<Message> {
        self.preview_generation = self.preview_generation.wrapping_add(1);
        if let Some(tx) = &self.command_tx {
            let _ = tx.send(BridgeCommand::CancelPreviews {
                generation: self.preview_generation,
            });
        }

        self.preview_images.clear();
        self.media = None;
        self.active_group = None;
        self.popup_hovered = false;
        self.media_action_epoch = self.media_action_epoch.wrapping_add(1);

        let Some(id) = self.preview_popup.take() else {
            return cosmic::task::none();
        };

        use cosmic::iced::platform_specific::shell::commands::popup::destroy_popup;
        destroy_popup(id)
    }

    fn activate_group(&mut self, group_key: String) -> Task<Message> {
        let Some(entry) = self
            .windows
            .iter()
            .find(|entry| entry.group_key == group_key)
        else {
            return cosmic::task::none();
        };

        let app_id = entry.app_id.clone();
        let app_label = entry.app_label.clone();
        let handles = self.group_handles(&group_key);
        if handles.is_empty() {
            return cosmic::task::none();
        }

        self.active_group = Some(group_key.clone());
        self.preview_images.clear();
        self.media = None;
        self.preview_generation = self.preview_generation.wrapping_add(1);
        let generation = self.preview_generation;

        if let Some(tx) = &self.command_tx {
            let _ = tx.send(BridgeCommand::BeginPreviewBatch {
                generation,
                handles,
            });
        }

        let popup = self.open_preview_surface(&group_key);
        let media = Task::perform(media::fetch_for_app(app_id, app_label), move |state| {
            cosmic::Action::App(Message::MediaLoaded(generation, state))
        });
        Task::batch([popup, media])
    }

    fn refresh_media_task(&self, generation: u64) -> Task<Message> {
        let Some(group_key) = self.active_group.as_ref() else {
            return cosmic::task::none();
        };
        let Some(entry) = self
            .windows
            .iter()
            .find(|entry| &entry.group_key == group_key)
        else {
            return cosmic::task::none();
        };
        let app_id = entry.app_id.clone();
        let app_label = entry.app_label.clone();
        Task::perform(media::fetch_for_app(app_id, app_label), move |state| {
            cosmic::Action::App(Message::MediaLoaded(generation, state))
        })
    }

    fn hover_delay_task(group_key: String, epoch: u64) -> Task<Message> {
        Task::perform(
            async move {
                tokio::time::sleep(HOVER_DELAY).await;
                (group_key, epoch)
            },
            |(group_key, epoch)| cosmic::Action::App(Message::HoverDelayElapsed(group_key, epoch)),
        )
    }

    fn close_delay_task(epoch: u64) -> Task<Message> {
        Task::perform(
            async move {
                tokio::time::sleep(HOVER_GRACE).await;
                epoch
            },
            |epoch| cosmic::Action::App(Message::CloseDelayElapsed(epoch)),
        )
    }

    fn media_debounce_task(
        epoch: u64,
        bus: String,
        value: f64,
        is_volume: bool,
    ) -> Task<Message> {
        Task::perform(
            async move {
                tokio::time::sleep(MEDIA_DEBOUNCE).await;
                (epoch, bus, value)
            },
            move |(epoch, bus, value)| {
                cosmic::Action::App(if is_volume {
                    Message::MediaVolumeCommit(epoch, bus, value)
                } else {
                    Message::MediaSeekCommit(epoch, bus, value)
                })
            },
        )
    }

    fn preview_visual<'a>(&'a self, entry: &'a Entry) -> cosmic::Element<'a, Message> {
        let visual: cosmic::Element<_> = if let Some(image) = self.preview_images.get(&entry.handle) {
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

        cosmic::widget::button::custom(visual)
            .class(cosmic::theme::Button::Text)
            .padding(0)
            .on_press(Message::Restore(entry.handle.clone()))
            .into()
    }

    fn preview_card<'a>(&'a self, entry: &'a Entry) -> cosmic::Element<'a, Message> {
        let close = cosmic::widget::button::icon(
            cosmic::widget::icon::from_name("window-close-symbolic")
                .size(18)
                .symbolic(true),
        )
        .class(cosmic::theme::Button::Icon)
        .icon_size(18)
        .on_press(Message::Close(entry.handle.clone()));

        let title_row = cosmic::widget::row::with_children(vec![
            cosmic::widget::container(cosmic::widget::text(&entry.title))
                .width(Length::Fill)
                .into(),
            close.into(),
        ])
        .spacing(6.0)
        .align_y(Alignment::Center)
        .width(Length::Fixed(PREVIEW_WIDTH));

        cosmic::widget::column::with_children(vec![
            self.preview_visual(entry),
            title_row.into(),
        ])
        .spacing(6.0)
        .width(Length::Fixed(PREVIEW_WIDTH))
        .into()
    }

    fn media_panel(&self) -> Option<cosmic::Element<'_, Message>> {
        let media = self.media.as_ref()?;

        let art: cosmic::Element<_> = if let Some(art) = media.art.as_ref() {
            Image::new(Handle::from_rgba(
                art.width,
                art.height,
                art.rgba.clone(),
            ))
            .width(Length::Fixed(96.0))
            .height(Length::Fixed(96.0))
            .content_fit(iced::ContentFit::Cover)
            .into()
        } else {
            cosmic::widget::container(
                cosmic::widget::icon::from_name("audio-x-generic-symbolic")
                    .size(48)
                    .symbolic(true),
            )
            .center_x(Length::Fixed(96.0))
            .center_y(Length::Fixed(96.0))
            .into()
        };

        let mut metadata = cosmic::widget::column::with_children(vec![
            cosmic::widget::text(&media.title).into(),
            cosmic::widget::text(&media.artist).into(),
        ])
        .spacing(3.0);
        if !media.album.is_empty() {
            metadata = metadata.push(cosmic::widget::text(&media.album));
        }

        let header = cosmic::widget::row::with_children(vec![art, metadata.into()])
            .spacing(12.0)
            .align_y(Alignment::Center);

        let previous = cosmic::widget::button::icon(
            cosmic::widget::icon::from_name("media-skip-backward-symbolic")
                .size(22)
                .symbolic(true),
        )
        .class(cosmic::theme::Button::Icon)
        .icon_size(22);
        let previous = if media.can_previous {
            previous.on_press(Message::MediaControl(MediaCommand::Previous))
        } else {
            previous
        };

        let play_icon = if media.playing {
            "media-playback-pause-symbolic"
        } else {
            "media-playback-start-symbolic"
        };
        let play = cosmic::widget::button::icon(
            cosmic::widget::icon::from_name(play_icon)
                .size(24)
                .symbolic(true),
        )
        .class(cosmic::theme::Button::Icon)
        .icon_size(24);
        let play = if media.can_play_pause {
            play.on_press(Message::MediaControl(MediaCommand::PlayPause))
        } else {
            play
        };

        let next = cosmic::widget::button::icon(
            cosmic::widget::icon::from_name("media-skip-forward-symbolic")
                .size(22)
                .symbolic(true),
        )
        .class(cosmic::theme::Button::Icon)
        .icon_size(22);
        let next = if media.can_next {
            next.on_press(Message::MediaControl(MediaCommand::Next))
        } else {
            next
        };

        let controls = cosmic::widget::row::with_children(vec![
            previous.into(),
            play.into(),
            next.into(),
        ])
        .spacing(8.0)
        .align_y(Alignment::Center);

        let progress = if media.duration_us > 0 {
            (media.position_us as f64 / media.duration_us as f64).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let progress_widget: cosmic::Element<_> = if media.can_seek && media.duration_us > 0 {
            cosmic::widget::slider(0.0..=1.0, progress, Message::MediaSeekChanged)
                .step(0.001)
                .width(Length::Fill)
                .into()
        } else {
            cosmic::widget::progress_bar::linear::Linear::new()
                .girth(6.0)
                .progress(progress as f32)
                .width(Length::Fill)
                .into()
        };

        let time = format!(
            "{} / {}",
            format_duration(media.position_us),
            format_duration(media.duration_us)
        );
        let progress_row = cosmic::widget::row::with_children(vec![
            progress_widget,
            cosmic::widget::text(time).into(),
        ])
        .spacing(10.0)
        .align_y(Alignment::Center);

        let mut children: Vec<cosmic::Element<_>> = vec![
            header.into(),
            controls.into(),
            progress_row.into(),
        ];

        if let Some(volume) = media.volume {
            let volume_row = cosmic::widget::row::with_children(vec![
                cosmic::widget::icon::from_name("audio-volume-high-symbolic")
                    .size(20)
                    .symbolic(true)
                    .into(),
                cosmic::widget::slider(
                    0.0..=1.5,
                    volume.clamp(0.0, 1.5),
                    Message::MediaVolumeChanged,
                )
                .step(0.01)
                .width(Length::Fill)
                .into(),
                cosmic::widget::text(format!("{}%", (volume * 100.0).round() as i32)).into(),
            ])
            .spacing(10.0)
            .align_y(Alignment::Center);
            children.push(volume_row.into());
        }

        Some(
            cosmic::widget::container(
                cosmic::widget::column::with_children(children)
                    .spacing(8.0)
                    .width(Length::Fill),
            )
            .width(Length::Fill)
            .into(),
        )
    }

    fn group_preview(&self) -> cosmic::Element<'_, Message> {
        let Some(group_key) = self.active_group.as_ref() else {
            return cosmic::widget::space::horizontal().into();
        };
        let entries = self.group_entries(group_key);
        if entries.is_empty() {
            return cosmic::widget::space::horizontal().into();
        }

        let mut rows = Vec::new();
        for chunk in entries.chunks(MAX_GRID_COLUMNS) {
            let cards = chunk
                .iter()
                .map(|entry| self.preview_card(entry))
                .collect::<Vec<_>>();
            rows.push(
                cosmic::widget::row::with_children(cards)
                    .spacing(12.0)
                    .align_y(Alignment::Start)
                    .into(),
            );
        }

        let mut content = Vec::new();
        if let Some(media) = self.media_panel() {
            content.push(media);
            content.push(cosmic::widget::divider::horizontal::light().into());
        }
        content.extend(rows);

        cosmic::widget::column::with_children(content)
            .spacing(12.0)
            .width(Length::Shrink)
            .into()
    }

    fn context_restore(&mut self, group: usize, window: usize) -> Task<Message> {
        let Some(handle) = self.entry_at(group, window).map(|entry| entry.handle.clone()) else {
            return cosmic::task::none();
        };
        self.restore(handle)
    }

    fn context_close(&mut self, group: usize, window: usize) {
        let Some(handle) = self.entry_at(group, window).map(|entry| entry.handle.clone()) else {
            return;
        };
        if let Some(tx) = &self.command_tx {
            let _ = tx.send(BridgeCommand::Close(handle));
        }
    }

    fn restore(&mut self, handle: ExtForeignToplevelHandleV1) -> Task<Message> {
        if let Some(tx) = &self.command_tx {
            let _ = tx.send(BridgeCommand::Restore(handle));
        }
        self.hover_epoch = self.hover_epoch.wrapping_add(1);
        self.close_epoch = self.close_epoch.wrapping_add(1);
        self.dock_hovered = None;
        self.close_preview_surface()
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
        let mut subscriptions = vec![wayland::subscription().map(|event| Message::Bridge(Box::new(event)))];
        if self.preview_popup.is_some() && self.media.as_ref().is_some_and(|media| media.playing) {
            subscriptions.push(iced::time::every(Duration::from_secs(1)).map(|_| Message::MediaTick));
        }
        Subscription::batch(subscriptions)
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
                        let removed_group = self.remove(&handle);
                        let active_group_gone = removed_group.as_ref().is_some_and(|group| {
                            self.active_group.as_ref() == Some(group)
                                && !self.windows.iter().any(|entry| &entry.group_key == group)
                        });

                        let close = if active_group_gone {
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
                BridgeEvent::Preview {
                    generation,
                    handle,
                    image,
                } => {
                    if generation == self.preview_generation
                        && self.preview_popup.is_some()
                        && self.active_group.as_ref().is_some_and(|group| {
                            self.windows.iter().any(|entry| {
                                &entry.group_key == group && entry.handle == handle
                            })
                        })
                    {
                        if let Some(image) = image {
                            self.preview_images.insert(handle, image);
                        }
                    }
                }
            },
            Message::GroupPrimary(group_key) => {
                let handles = self.group_handles(&group_key);
                if handles.len() == 1 {
                    return self.restore(handles[0].clone());
                }
                if !handles.is_empty() {
                    return self.activate_group(group_key);
                }
            }
            Message::Restore(handle) => return self.restore(handle),
            Message::Close(handle) => {
                if let Some(tx) = &self.command_tx {
                    let _ = tx.send(BridgeCommand::Close(handle));
                }
            }
            Message::CloseGroup(group_key) => {
                if let Some(tx) = &self.command_tx {
                    for handle in self.group_handles(&group_key) {
                        let _ = tx.send(BridgeCommand::Close(handle));
                    }
                }
            }
            Message::HoverEnter(group_key) => {
                self.dock_hovered = Some(group_key.clone());
                self.hover_epoch = self.hover_epoch.wrapping_add(1);
                self.close_epoch = self.close_epoch.wrapping_add(1);
                let epoch = self.hover_epoch;
                if self.preview_popup.is_some()
                    && self.active_group.as_ref() == Some(&group_key)
                {
                    return cosmic::task::none();
                }
                return Self::hover_delay_task(group_key, epoch);
            }
            Message::HoverExit(group_key) => {
                if self.dock_hovered.as_ref() == Some(&group_key) {
                    self.dock_hovered = None;
                    self.hover_epoch = self.hover_epoch.wrapping_add(1);
                    self.close_epoch = self.close_epoch.wrapping_add(1);
                    let epoch = self.close_epoch;
                    if self.preview_popup.is_some() {
                        return Self::close_delay_task(epoch);
                    }
                }
            }
            Message::HoverDelayElapsed(group_key, epoch) => {
                if self.hover_epoch == epoch
                    && self.dock_hovered.as_ref() == Some(&group_key)
                {
                    return self.activate_group(group_key);
                }
            }
            Message::PopupEnter => {
                self.popup_hovered = true;
                self.close_epoch = self.close_epoch.wrapping_add(1);
            }
            Message::PopupExit => {
                self.popup_hovered = false;
                self.close_epoch = self.close_epoch.wrapping_add(1);
                let epoch = self.close_epoch;
                return Self::close_delay_task(epoch);
            }
            Message::CloseDelayElapsed(epoch) => {
                if epoch == self.close_epoch
                    && !self.popup_hovered
                    && self.dock_hovered.as_ref() != self.active_group.as_ref()
                {
                    return self.close_preview_surface();
                }
            }
            Message::PreviewClosed(id) => {
                if self.preview_popup == Some(id) {
                    return self.close_preview_surface();
                }
            }
            Message::Surface(action) => {
                return cosmic::task::message(cosmic::Action::Cosmic(
                    cosmic::app::Action::Surface(action),
                ));
            }
            Message::ContextRestore(group, window) => return self.context_restore(group, window),
            Message::ContextClose(group, window) => self.context_close(group, window),
            Message::ContextCloseAll(group) => {
                if let Some(group_key) = self.group_at(group) {
                    if let Some(tx) = &self.command_tx {
                        for handle in self.group_handles(&group_key) {
                            let _ = tx.send(BridgeCommand::Close(handle));
                        }
                    }
                }
            }
            Message::ContextOpenGroup(group) => {
                if let Some(group_key) = self.group_at(group) {
                    return self.activate_group(group_key);
                }
            }
            Message::MediaLoaded(generation, state) => {
                if generation == self.preview_generation && self.preview_popup.is_some() {
                    self.media = state;
                }
            }
            Message::MediaControl(command) => {
                let Some(media) = self.media.as_ref() else {
                    return cosmic::task::none();
                };
                let bus = media.bus_name.clone();
                let generation = self.preview_generation;
                return Task::perform(media::run_command(bus, command), move |_| {
                    cosmic::Action::App(Message::MediaControlFinished(generation))
                });
            }
            Message::MediaControlFinished(generation) => {
                if generation == self.preview_generation && self.preview_popup.is_some() {
                    return self.refresh_media_task(generation);
                }
            }
            Message::MediaTick => {
                if let Some(media) = self.media.as_mut()
                    && media.playing
                    && media.duration_us > 0
                {
                    media.position_us = media
                        .position_us
                        .saturating_add(1_000_000)
                        .min(media.duration_us);
                }
            }
            Message::MediaVolumeChanged(value) => {
                let Some(media) = self.media.as_mut() else {
                    return cosmic::task::none();
                };
                media.volume = Some(value);
                let bus = media.bus_name.clone();
                self.media_action_epoch = self.media_action_epoch.wrapping_add(1);
                let epoch = self.media_action_epoch;
                return Self::media_debounce_task(epoch, bus, value, true);
            }
            Message::MediaVolumeCommit(epoch, bus, value) => {
                if epoch == self.media_action_epoch && self.preview_popup.is_some() {
                    let generation = self.preview_generation;
                    return Task::perform(
                        media::run_command(bus, MediaCommand::SetVolume(value)),
                        move |_| cosmic::Action::App(Message::MediaControlFinished(generation)),
                    );
                }
            }
            Message::MediaSeekChanged(value) => {
                let Some(media) = self.media.as_mut() else {
                    return cosmic::task::none();
                };
                if media.duration_us > 0 {
                    media.position_us = (media.duration_us as f64 * value.clamp(0.0, 1.0)) as u64;
                }
                let bus = media.bus_name.clone();
                self.media_action_epoch = self.media_action_epoch.wrapping_add(1);
                let epoch = self.media_action_epoch;
                return Self::media_debounce_task(epoch, bus, value, false);
            }
            Message::MediaSeekCommit(epoch, bus, value) => {
                if epoch == self.media_action_epoch && self.preview_popup.is_some() {
                    let generation = self.preview_generation;
                    return Task::perform(
                        media::run_command(bus, MediaCommand::SeekFraction(value)),
                        move |_| cosmic::Action::App(Message::MediaControlFinished(generation)),
                    );
                }
            }
        }

        cosmic::task::none()
    }

    fn view(&self) -> cosmic::Element<'_, Self::Message> {
        let keys = self.group_keys();
        let children = keys
            .iter()
            .enumerate()
            .map(|(index, key)| self.group_button(key, index))
            .collect::<Vec<_>>();

        let content: cosmic::Element<_> = if self.core.applet.is_horizontal() {
            cosmic::widget::row::with_children(children)
                .spacing(self.core.applet.spacing as f32)
                .align_y(Alignment::Center)
                .width(Length::Shrink)
                .height(Length::Shrink)
                .into()
        } else {
            cosmic::widget::column::with_children(children)
                .spacing(self.core.applet.spacing as f32)
                .align_x(Alignment::Center)
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
            let content = self.core.applet.popup_container(self.group_preview());
            cosmic::widget::mouse_area(content)
                .on_enter(Message::PopupEnter)
                .on_exit(Message::PopupExit)
                .into()
        } else {
            cosmic::widget::space::horizontal().into()
        }
    }

    fn on_close_requested(&self, id: WindowId) -> Option<Self::Message> {
        Some(Message::PreviewClosed(id))
    }
}

fn format_duration(microseconds: u64) -> String {
    let total_seconds = microseconds / 1_000_000;
    let minutes = total_seconds / 60;
    let seconds = total_seconds % 60;
    format!("{minutes}:{seconds:02}")
}
