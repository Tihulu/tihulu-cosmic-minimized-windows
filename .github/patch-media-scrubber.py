from pathlib import Path


def replace_exact(text: str, old: str, new: str, label: str, count: int = 1) -> str:
    actual = text.count(old)
    if actual != count:
        raise SystemExit(f"{label}: expected {count} occurrence(s), found {actual}")
    return text.replace(old, new, count)


app_path = Path("src/app.rs")
app = app_path.read_text()

app = replace_exact(
    app,
    '''    MediaSeekChanged {
        group: String,
        fraction: f32,
    },
    MediaSeekDone(String, Result<(), String>),
''',
    '''    MediaSeekChanged {
        group: String,
        fraction: f32,
    },
    MediaSeekCommit(String),
    MediaSeekTo {
        group: String,
        fraction: f32,
    },
    MediaSeekDone(String, Result<Option<MediaPlayerState>, String>),
''',
    "message variants",
)

app = replace_exact(
    app,
    '''    fn media_seek_task(
        group: String,
        bus_name: String,
        track_id: String,
        position_micros: i64,
    ) -> Task<Message> {
        Task::perform(
            media_client::seek(bus_name, track_id, position_micros),
            move |result| cosmic::Action::App(Message::MediaSeekDone(group, result)),
        )
    }

    fn reload_backends_task(reload_preview: bool, reload_media: bool) -> Task<Message> {
''',
    '''    fn media_seek_task(
        group: String,
        bus_name: String,
        track_id: String,
        position_micros: i64,
    ) -> Task<Message> {
        Task::perform(
            media_client::seek(bus_name, track_id, position_micros),
            move |result| cosmic::Action::App(Message::MediaSeekDone(group, result)),
        )
    }

    fn begin_media_seek(&mut self, group: String, fraction: f32) -> Task<Message> {
        let fraction = fraction.clamp(0.0, 1.0);
        if !self.media_requested() || self.group_count(&group) == 0 {
            self.media_seek_drafts.remove(&group);
            return cosmic::task::none();
        }

        let request = self.media_players.get(&group).and_then(|player| {
            let length = player.length_micros.filter(|length| *length > 0)?;
            let track_id = player.track_id.clone()?;
            if !player.can_seek {
                return None;
            }
            Some((
                player.bus_name.clone(),
                track_id,
                media_position_from_fraction(length, fraction),
            ))
        });

        if let Some((bus_name, track_id, position_micros)) = request {
            self.media_seek_drafts.insert(group.clone(), fraction);
            Self::media_seek_task(group, bus_name, track_id, position_micros)
        } else {
            self.media_seek_drafts.remove(&group);
            cosmic::task::none()
        }
    }

    fn reload_backends_task(reload_preview: bool, reload_media: bool) -> Task<Message> {
''',
    "seek helper",
)

app = replace_exact(
    app,
    '''    fn close_popup(&mut self) -> Task<Message> {
        use cosmic::iced::platform_specific::shell::commands::popup::destroy_popup;

        self.popup
''',
    '''    fn close_popup(&mut self) -> Task<Message> {
        use cosmic::iced::platform_specific::shell::commands::popup::destroy_popup;

        self.media_seek_drafts.clear();
        self.popup
''',
    "popup seek cleanup",
)

app = replace_exact(
    app,
    '''            let display_position =
                ((display_fraction as f64 * length as f64).round() as i64).clamp(0, length);

            children.push(
                cosmic::iced::widget::progress_bar(0.0..=1.0, display_fraction)
                    .length(Length::Fill)
                    .girth(Length::Fixed(4.0))
                    .into(),
            );
''',
    '''            let display_position = media_position_from_fraction(length, display_fraction);
            let seekable = player.can_seek && player.track_id.is_some();

            if seekable {
                let seek_group = group.to_owned();
                children.push(
                    cosmic::iced::widget::slider(
                        0.0..=1.0,
                        display_fraction,
                        move |fraction| Message::MediaSeekChanged {
                            group: seek_group.clone(),
                            fraction,
                        },
                    )
                    .step(media_seek_fraction_step(length))
                    .on_release(Message::MediaSeekCommit(group.to_owned()))
                    .width(Length::Fill)
                    .height(18.0)
                    .handle_width(12.0)
                    .handle_height(12.0)
                    .into(),
                );
            } else {
                children.push(
                    cosmic::iced::widget::progress_bar(0.0..=1.0, display_fraction)
                        .length(Length::Fill)
                        .girth(Length::Fixed(4.0))
                        .into(),
                );
            }
''',
    "interactive timeline",
)

app = replace_exact(
    app,
    '''            if player.can_seek && player.track_id.is_some() {
''',
    '''            if seekable {
''',
    "seekable timeline controls",
)

app = replace_exact(
    app,
    '''                        .on_press(Message::MediaSeekChanged {
                            group: group.to_owned(),
                            fraction: back_fraction,
                        })
''',
    '''                        .on_press(Message::MediaSeekTo {
                            group: group.to_owned(),
                            fraction: back_fraction,
                        })
''',
    "back seek button",
)

app = replace_exact(
    app,
    '''                        .on_press(Message::MediaSeekChanged {
                            group: group.to_owned(),
                            fraction: forward_fraction,
                        })
''',
    '''                        .on_press(Message::MediaSeekTo {
                            group: group.to_owned(),
                            fraction: forward_fraction,
                        })
''',
    "forward seek button",
)

app = replace_exact(
    app,
    '''            Message::MediaLoaded(group, result) => {
                self.media_seek_drafts.remove(&group);
                self.media_snapshot_at.remove(&group);
''',
    '''            Message::MediaLoaded(group, result) => {
                self.media_snapshot_at.remove(&group);
''',
    "preserve active seek draft across status refresh",
)

app = replace_exact(
    app,
    '''            Message::MediaSeekChanged { group, fraction } => {
                let fraction = fraction.clamp(0.0, 1.0);
                if self.media_requested() && self.group_count(&group) > 0 {
                    let request = self.media_players.get(&group).and_then(|player| {
                        let length = player.length_micros.filter(|length| *length > 0)?;
                        let track_id = player.track_id.clone()?;
                        if !player.can_seek {
                            return None;
                        }
                        let position =
                            ((fraction as f64 * length as f64).round() as i64).clamp(0, length);
                        Some((player.bus_name.clone(), track_id, position))
                    });
                    if let Some((bus_name, track_id, position)) = request {
                        self.media_seek_drafts.insert(group.clone(), fraction);
                        return Self::media_seek_task(group, bus_name, track_id, position);
                    }
                }
            }
            Message::MediaSeekDone(group, result) => {
                if let Err(error) = result {
                    self.media_seek_drafts.remove(&group);
                    tracing::warn!(?error, "MPRIS seek failed");
                }
                if self.media_requested() && self.group_count(&group) > 0 {
                    return Self::media_status_task(group);
                }
            }
''',
    '''            Message::MediaSeekChanged { group, fraction } => {
                if self.media_requested() && self.group_count(&group) > 0 {
                    self.media_seek_drafts
                        .insert(group, fraction.clamp(0.0, 1.0));
                } else {
                    self.media_seek_drafts.remove(&group);
                }
            }
            Message::MediaSeekCommit(group) => {
                let Some(fraction) = self.media_seek_drafts.get(&group).copied() else {
                    return cosmic::task::none();
                };
                return self.begin_media_seek(group, fraction);
            }
            Message::MediaSeekTo { group, fraction } => {
                return self.begin_media_seek(group, fraction);
            }
            Message::MediaSeekDone(group, result) => {
                self.media_seek_drafts.remove(&group);
                let still_active = self.media_requested() && self.group_count(&group) > 0;
                match result {
                    Ok(Some(player)) if still_active => {
                        self.media_snapshot_at.insert(group.clone(), Instant::now());
                        self.media_players.insert(group, player);
                    }
                    Ok(Some(_)) => {
                        self.media_snapshot_at.remove(&group);
                        self.media_players.remove(&group);
                    }
                    Ok(None) => {
                        if still_active {
                            return Self::media_status_task(group);
                        }
                    }
                    Err(error) => {
                        tracing::warn!(?error, "MPRIS seek failed");
                        if still_active {
                            return Self::media_status_task(group);
                        }
                    }
                }
            }
''',
    "seek message handling",
)

app = replace_exact(
    app,
    '''fn projected_media_position(
''',
    '''fn media_position_from_fraction(length_micros: i64, fraction: f32) -> i64 {
    if length_micros <= 0 {
        return 0;
    }
    ((f64::from(fraction.clamp(0.0, 1.0)) * length_micros as f64).round() as i64)
        .clamp(0, length_micros)
}

fn media_seek_fraction_step(length_micros: i64) -> f32 {
    if length_micros <= 0 {
        return 1.0;
    }
    (1_000_000.0_f64 / length_micros as f64)
        .clamp(f64::from(f32::EPSILON), 1.0) as f32
}

fn projected_media_position(
''',
    "seek conversion helpers",
)

app = replace_exact(
    app,
    '''    #[test]
    fn projected_position_clamps_at_track_end() {
        assert_eq!(
            projected_media_position(59_000_000, 60_000_000, "Playing", Duration::from_secs(3)),
            60_000_000
        );
    }
}
''',
    '''    #[test]
    fn projected_position_clamps_at_track_end() {
        assert_eq!(
            projected_media_position(59_000_000, 60_000_000, "Playing", Duration::from_secs(3)),
            60_000_000
        );
    }

    #[test]
    fn seek_fraction_maps_and_clamps_to_track_position() {
        assert_eq!(media_position_from_fraction(300_000_000, 0.5), 150_000_000);
        assert_eq!(media_position_from_fraction(300_000_000, -1.0), 0);
        assert_eq!(media_position_from_fraction(300_000_000, 2.0), 300_000_000);
    }

    #[test]
    fn seek_slider_step_is_one_second_of_track_length() {
        let step = media_seek_fraction_step(300_000_000);
        assert!((step - (1.0 / 300.0)).abs() < 0.000_001);
    }
}
''',
    "seek helper tests",
)

app_path.write_text(app)

client_path = Path("src/media_client.rs")
client = client_path.read_text()
client = replace_exact(
    client,
    '''pub(crate) async fn seek(
    bus_name: String,
    track_id: String,
    position_micros: i64,
) -> Result<(), String> {
    match request(MediaRequest::Seek {
        version: MEDIA_PROTOCOL_VERSION,
        bus_name,
        track_id,
        position_micros,
    })
    .await?
    {
        MediaResponse::State { version, .. } if version == MEDIA_PROTOCOL_VERSION => Ok(()),
        MediaResponse::State { version, .. } => Err(format!("media protocol mismatch: {version}")),
        MediaResponse::Error { message, .. } => Err(message),
    }
}
''',
    '''pub(crate) async fn seek(
    bus_name: String,
    track_id: String,
    position_micros: i64,
) -> Result<Option<MediaPlayerState>, String> {
    match request(MediaRequest::Seek {
        version: MEDIA_PROTOCOL_VERSION,
        bus_name,
        track_id,
        position_micros,
    })
    .await?
    {
        MediaResponse::State { version, player } if version == MEDIA_PROTOCOL_VERSION => Ok(player),
        MediaResponse::State { version, .. } => Err(format!("media protocol mismatch: {version}")),
        MediaResponse::Error { message, .. } => Err(message),
    }
}
''',
    "seek response state",
)
client_path.write_text(client)
