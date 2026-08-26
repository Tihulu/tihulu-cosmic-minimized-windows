from pathlib import Path

path = Path("src/app.rs")
text = path.read_text()

replacements = [
    ("    time::Duration,\n", "    time::{Duration, Instant},\n"),
    (
        "const MEDIA_REFRESH_INTERVAL: Duration = Duration::from_millis(2500);\n",
        "const MEDIA_REFRESH_INTERVAL: Duration = Duration::from_millis(2500);\nconst MEDIA_UI_TICK_INTERVAL: Duration = Duration::from_secs(1);\n",
    ),
    (
        "    media_players: HashMap<String, MediaPlayerState>,\n    media_seek_drafts: HashMap<String, f32>,\n",
        "    media_players: HashMap<String, MediaPlayerState>,\n    media_snapshot_at: HashMap<String, Instant>,\n    media_seek_drafts: HashMap<String, f32>,\n",
    ),
    ("    MediaRefreshTick,\n", "    MediaRefreshTick,\n    MediaUiTick,\n"),
    (
        "            self.media_players.remove(group);\n            self.media_seek_drafts.remove(group);\n",
        "            self.media_players.remove(group);\n            self.media_snapshot_at.remove(group);\n            self.media_seek_drafts.remove(group);\n",
    ),
    (
        "            let position = player.position_micros.clamp(0, length);\n",
        "            let snapshot_age = self\n                .media_snapshot_at\n                .get(group)\n                .map(Instant::elapsed)\n                .unwrap_or_default();\n            let position = projected_media_position(\n                player.position_micros,\n                length,\n                &player.playback_status,\n                snapshot_age,\n            );\n",
    ),
    (
        """        if media_popup_open {
            Subscription::batch([
                wayland,
                cosmic::iced::time::every(MEDIA_REFRESH_INTERVAL)
                    .map(|_| Message::MediaRefreshTick),
            ])
        } else {
            wayland
        }
""",
        """        if media_popup_open {
            let mut subscriptions = vec![
                wayland,
                cosmic::iced::time::every(MEDIA_REFRESH_INTERVAL)
                    .map(|_| Message::MediaRefreshTick),
            ];
            let media_playing = self
                .popup
                .active_group()
                .and_then(|group| self.media_players.get(group))
                .is_some_and(|player| {
                    player.playback_status.eq_ignore_ascii_case("playing")
                        && player.length_micros.is_some_and(|length| length > 0)
                });
            if media_playing {
                subscriptions.push(
                    cosmic::iced::time::every(MEDIA_UI_TICK_INTERVAL)
                        .map(|_| Message::MediaUiTick),
                );
            }
            Subscription::batch(subscriptions)
        } else {
            wayland
        }
""",
    ),
    (
        "                    self.media_players.clear();\n                    self.media_seek_drafts.clear();\n",
        "                    self.media_players.clear();\n                    self.media_snapshot_at.clear();\n                    self.media_seek_drafts.clear();\n",
    ),
    (
        "                    self.media_players.clear();\n                    self.media_seek_drafts.clear();\n                }\n                self.persist_settings();\n",
        "                    self.media_players.clear();\n                    self.media_snapshot_at.clear();\n                    self.media_seek_drafts.clear();\n                }\n                self.persist_settings();\n",
    ),
    (
        "                self.media_players.clear();\n                self.media_seek_drafts.clear();\n                if reload_preview {\n",
        "                self.media_players.clear();\n                self.media_snapshot_at.clear();\n                self.media_seek_drafts.clear();\n                if reload_preview {\n",
    ),
    (
        """            Message::MediaLoaded(group, result) => {
                self.media_seek_drafts.remove(&group);
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
""",
        """            Message::MediaLoaded(group, result) => {
                self.media_seek_drafts.remove(&group);
                self.media_snapshot_at.remove(&group);
                if !self.media_requested() || self.group_count(&group) == 0 {
                    self.media_players.remove(&group);
                } else {
                    match result {
                        Ok(Some(player)) => {
                            self.media_snapshot_at.insert(group.clone(), Instant::now());
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
""",
    ),
    (
        """            Message::MediaRefreshTick => {
                if self.popup.is_pinned()
                    && self.media_requested()
                    && let Some(group) = self.popup.active_group().map(str::to_owned)
                    && group != SETTINGS_GROUP
                    && self.group_count(&group) > 0
                {
                    return Self::media_status_task(group);
                }
            }
""",
        """            Message::MediaRefreshTick => {
                if self.popup.is_pinned()
                    && self.media_requested()
                    && let Some(group) = self.popup.active_group().map(str::to_owned)
                    && group != SETTINGS_GROUP
                    && self.group_count(&group) > 0
                {
                    return Self::media_status_task(group);
                }
            }
            Message::MediaUiTick => {}
""",
    ),
    (
        "fn format_media_time(micros: i64) -> String {\n",
        """fn projected_media_position(
    position_micros: i64,
    length_micros: i64,
    playback_status: &str,
    snapshot_age: Duration,
) -> i64 {
    if length_micros <= 0 {
        return 0;
    }

    let mut position = position_micros.clamp(0, length_micros);
    if playback_status.eq_ignore_ascii_case("playing") {
        let elapsed_micros = i64::try_from(snapshot_age.as_micros()).unwrap_or(i64::MAX);
        position = position.saturating_add(elapsed_micros);
    }
    position.clamp(0, length_micros)
}

fn format_media_time(micros: i64) -> String {
""",
    ),
]

for old, new in replacements:
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"expected exactly one match, got {count}: {old[:100]!r}")
    text = text.replace(old, new, 1)

text += """

#[cfg(test)]
mod media_progress_tests {
    use super::*;

    #[test]
    fn playing_position_advances_from_snapshot_age() {
        assert_eq!(
            projected_media_position(5_000_000, 60_000_000, "Playing", Duration::from_secs(3)),
            8_000_000
        );
    }

    #[test]
    fn paused_position_does_not_advance() {
        assert_eq!(
            projected_media_position(5_000_000, 60_000_000, "Paused", Duration::from_secs(3)),
            5_000_000
        );
    }

    #[test]
    fn projected_position_clamps_at_track_end() {
        assert_eq!(
            projected_media_position(59_000_000, 60_000_000, "Playing", Duration::from_secs(3)),
            60_000_000
        );
    }
}
"""

path.write_text(text)
