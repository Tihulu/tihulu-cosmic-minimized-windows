// SPDX-License-Identifier: AGPL-3.0-only

use std::time::Duration;

use futures::StreamExt;
use mpris2_zbus::{media_player::MediaPlayer, player::PlaybackStatus};
use serde_json::{Map, Value};
use tokio::{process::Command, time::timeout};
use zbus::names::OwnedBusName;

const MEDIA_TIMEOUT: Duration = Duration::from_millis(1800);
const COMMAND_TIMEOUT: Duration = Duration::from_millis(2200);
const ART_TIMEOUT: Duration = Duration::from_secs(2);
const AUDIO_QUERY_TIMEOUT: Duration = Duration::from_millis(850);
const AUDIO_COMMAND_TIMEOUT: Duration = Duration::from_millis(650);
const MAX_ART_BYTES: usize = 2 * 1024 * 1024;
const MAX_PACTL_BYTES: usize = 2 * 1024 * 1024;
const ART_MAX_SIZE: u32 = 144;

#[derive(Clone, Debug)]
pub struct MediaSnapshot {
    pub bus_name: String,
    pub identity: String,
    pub title: String,
    pub artists: String,
    pub art_url: Option<String>,
    pub position_us: i64,
    pub length_us: i64,
    pub playing: bool,
    pub volume: f64,
    pub muted: bool,
    pub can_previous: bool,
    pub can_next: bool,
    pub can_play_pause: bool,
}

#[derive(Clone, Debug)]
pub struct MediaArtwork {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug)]
pub enum MediaCommand {
    Previous,
    PlayPause,
    Next,
    SetVolume(f64),
    SetMuted { muted: bool, restore_volume: f64 },
}

#[derive(Debug)]
struct AudioState {
    stream_ids: Vec<u32>,
    volume: f64,
    muted: bool,
}

#[derive(Debug)]
struct AudioCandidate {
    score: usize,
    index: u32,
    volume: f64,
    muted: bool,
}

pub async fn snapshot(
    app_id: String,
    app_label: String,
    window_titles: Vec<String>,
) -> Option<MediaSnapshot> {
    timeout(
        MEDIA_TIMEOUT,
        snapshot_inner(&app_id, &app_label, &window_titles),
    )
    .await
    .ok()
    .flatten()
}

async fn snapshot_inner(
    app_id: &str,
    app_label: &str,
    window_titles: &[String],
) -> Option<MediaSnapshot> {
    let connection = zbus::Connection::session().await.ok()?;
    let players = MediaPlayer::available_players(&connection).await.ok()?;

    let mut best: Option<(usize, MediaPlayer, String, String)> = None;

    for bus_name in players {
        let Ok(media_player) = MediaPlayer::new(&connection, bus_name.clone()).await else {
            continue;
        };
        let identity = media_player.identity().await.unwrap_or_default();
        let desktop_entry = media_player.desktop_entry().await.unwrap_or_default();
        let Ok(player) = media_player.player().await else {
            continue;
        };
        let status = player.playback_status().await.ok();
        let metadata_title = player
            .metadata()
            .await
            .ok()
            .and_then(|metadata| metadata.title())
            .unwrap_or_default();

        if looks_like_browser(
            app_id,
            app_label,
            &desktop_entry,
            &identity,
            bus_name.as_str(),
        ) && !browser_media_matches(&metadata_title, window_titles)
        {
            continue;
        }

        let base_score = match_score(
            app_id,
            app_label,
            bus_name.as_str(),
            &identity,
            &desktop_entry,
            &metadata_title,
            window_titles,
        );
        if base_score == 0 {
            continue;
        }

        let activity_bonus = match status {
            Some(PlaybackStatus::Playing) => 40,
            Some(PlaybackStatus::Paused) => 12,
            _ => 0,
        };
        let score = base_score + activity_bonus;

        let replace = best
            .as_ref()
            .map(|(best_score, _, _, _)| score > *best_score)
            .unwrap_or(true);
        if replace {
            best = Some((score, media_player, identity, bus_name.to_string()));
        }
    }

    let (_, media_player, identity, bus_name) = best?;
    let player = media_player.player().await.ok()?;
    let metadata = player.metadata().await.ok()?;
    let status = player.playback_status().await.ok()?;
    let position_us = player
        .position()
        .await
        .ok()
        .flatten()
        .map(|duration| clamp_micros(duration.as_micros()))
        .unwrap_or_default();
    let length_us = metadata
        .length()
        .map(|duration| clamp_micros(duration.as_micros()))
        .unwrap_or_default();
    let title = metadata.title().unwrap_or_else(|| identity.clone());
    let artists = metadata.artists().unwrap_or_default().join(", ");
    let art_url = metadata.art_url();
    let mpris_volume = player.volume().await.unwrap_or(1.0).clamp(0.0, 1.5);
    let can_control = player.can_control().await.unwrap_or(false);
    let can_previous = player.can_go_previous().await.unwrap_or(false);
    let can_next = player.can_go_next().await.unwrap_or(false);
    let can_play_pause = can_control
        && (player.can_play().await.unwrap_or(false) || player.can_pause().await.unwrap_or(false));

    // The popup keeps only scalar media state. PipeWire stream IDs are intentionally
    // not retained because Chromium-family browsers may recreate their sink-input.
    let audio = audio_state(app_id, app_label, &title).await;
    let (volume, muted) = if let Some(audio) = audio {
        (audio.volume, audio.muted)
    } else {
        (mpris_volume, mpris_volume <= 0.01)
    };

    Some(MediaSnapshot {
        bus_name,
        identity,
        title,
        artists,
        art_url,
        position_us,
        length_us,
        playing: status == PlaybackStatus::Playing,
        volume,
        muted,
        can_previous,
        can_next,
        can_play_pause,
    })
}

pub async fn command(
    bus_name: String,
    app_id: String,
    app_label: String,
    media_title: String,
    command: MediaCommand,
) -> bool {
    timeout(
        COMMAND_TIMEOUT,
        command_inner(bus_name, app_id, app_label, media_title, command),
    )
    .await
    .unwrap_or(false)
}

async fn command_inner(
    bus_name: String,
    app_id: String,
    app_label: String,
    media_title: String,
    command: MediaCommand,
) -> bool {
    // Re-resolve the live audio stream for every audio command. This avoids stale
    // Chromium/PipeWire sink-input IDs without adding any watcher or background polling.
    match &command {
        MediaCommand::SetVolume(target) => {
            if let Some(audio) = audio_state(&app_id, &app_label, &media_title).await
                && set_audio_volume(&audio.stream_ids, *target).await
            {
                return true;
            }
        }
        MediaCommand::SetMuted { .. } => {
            if let Some(audio) = audio_state(&app_id, &app_label, &media_title).await
                && set_audio_muted(&audio.stream_ids, !audio.muted).await
            {
                return true;
            }
        }
        _ => {}
    }

    let connection = match zbus::Connection::session().await {
        Ok(connection) => connection,
        Err(_) => return false,
    };
    let name = match OwnedBusName::try_from(bus_name) {
        Ok(name) => name,
        Err(_) => return false,
    };
    let player = match mpris2_zbus::player::Player::new(&connection, name).await {
        Ok(player) => player,
        Err(_) => return false,
    };

    match command {
        MediaCommand::Previous => player.previous().await.is_ok(),
        MediaCommand::PlayPause => {
            // Never trust the cached UI boolean for the actual action. Query the player
            // at click time, then issue the idempotent Play or Pause method explicitly.
            match player.playback_status().await {
                Ok(PlaybackStatus::Playing) => player.pause().await.is_ok(),
                Ok(PlaybackStatus::Paused | PlaybackStatus::Stopped) => player.play().await.is_ok(),
                Err(_) => false,
            }
        }
        MediaCommand::Next => player.next().await.is_ok(),
        MediaCommand::SetVolume(volume) => player.set_volume(volume.clamp(0.0, 1.5)).await.is_ok(),
        MediaCommand::SetMuted {
            muted,
            restore_volume,
        } => {
            // MPRIS has no portable mute property. When PipeWire/PulseAudio stream
            // matching is unavailable, honor the requested UI state by mapping mute to
            // zero volume and unmute to the last known non-zero volume.
            let target = if muted {
                0.0
            } else {
                restore_volume.clamp(0.05, 1.5)
            };
            player.set_volume(target).await.is_ok()
        }
    }
}

async fn audio_state(app_id: &str, app_label: &str, media_title: &str) -> Option<AudioState> {
    let mut command = Command::new("pactl");
    command.kill_on_drop(true);
    command.args(["-f", "json", "list", "sink-inputs"]);
    let output = timeout(AUDIO_QUERY_TIMEOUT, command.output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success() || output.stdout.len() > MAX_PACTL_BYTES {
        return None;
    }

    let root: Value = serde_json::from_slice(&output.stdout).ok()?;
    let streams = root.as_array()?;
    let mut candidates = Vec::new();

    for stream in streams {
        let Some(index) = stream
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
        else {
            continue;
        };
        let Some(properties) = stream.get("properties").and_then(Value::as_object) else {
            continue;
        };
        let score = audio_match_score(app_id, app_label, media_title, properties);
        if score == 0 {
            continue;
        }
        let Some(volume) = stream_volume(stream) else {
            continue;
        };
        candidates.push(AudioCandidate {
            score,
            index,
            volume: volume.clamp(0.0, 1.5),
            muted: stream.get("mute").and_then(Value::as_bool).unwrap_or(false),
        });
    }

    let best_score = candidates.iter().map(|candidate| candidate.score).max()?;
    let threshold = best_score.saturating_sub(2).max(8);
    let selected = candidates
        .into_iter()
        .filter(|candidate| candidate.score >= threshold)
        .collect::<Vec<_>>();
    if selected.is_empty() {
        return None;
    }

    let volume = selected
        .iter()
        .map(|candidate| candidate.volume)
        .sum::<f64>()
        / selected.len() as f64;
    let muted = selected.iter().all(|candidate| candidate.muted);
    let stream_ids = selected
        .into_iter()
        .map(|candidate| candidate.index)
        .collect();

    Some(AudioState {
        stream_ids,
        volume: volume.clamp(0.0, 1.5),
        muted,
    })
}

fn stream_volume(stream: &Value) -> Option<f64> {
    let channels = stream.get("volume")?.as_object()?;
    let mut total = 0_u128;
    let mut count = 0_u128;
    for channel in channels.values() {
        if let Some(value) = channel.get("value").and_then(Value::as_u64) {
            total = total.saturating_add(u128::from(value));
            count = count.saturating_add(1);
        }
    }
    if count == 0 {
        return None;
    }
    Some(total as f64 / count as f64 / 65_536.0)
}

fn audio_match_score(
    app_id: &str,
    app_label: &str,
    media_title: &str,
    properties: &Map<String, Value>,
) -> usize {
    let app_name = property(properties, "application.name");
    let binary = property(properties, "application.process.binary");
    let media_name = property(properties, "media.name");
    let app_name_norm = normalize(app_name);
    let binary_norm = normalize(binary);
    let label_norm = normalize(app_label);
    let media_norm = normalize(media_name);
    let title_norm = normalize(media_title);

    let mut score = 0;
    if !label_norm.is_empty() && app_name_norm == label_norm {
        score += 36;
    }
    for token in tokens(app_id).into_iter().chain(tokens(app_label)) {
        if app_name_norm.contains(&token) {
            score += 12;
        }
        if binary_norm.contains(&token) {
            score += 12;
        }
    }
    if !title_norm.is_empty()
        && !media_norm.is_empty()
        && (media_norm.contains(&title_norm) || title_norm.contains(&media_norm))
    {
        score += 10;
    }
    for token in tokens(media_title) {
        if media_norm.contains(&token) {
            score += 2;
        }
    }
    score
}

fn property<'a>(properties: &'a Map<String, Value>, key: &str) -> &'a str {
    properties
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
}

async fn set_audio_volume(stream_ids: &[u32], volume: f64) -> bool {
    let percent = (volume.clamp(0.0, 1.5) * 100.0).round() as u32;
    let value = format!("{percent}%");
    let mut any = false;

    for index in stream_ids {
        if volume > 0.0 {
            let _ = run_pactl(vec![
                "set-sink-input-mute".to_owned(),
                index.to_string(),
                "0".to_owned(),
            ])
            .await;
        }
        any |= run_pactl(vec![
            "set-sink-input-volume".to_owned(),
            index.to_string(),
            value.clone(),
        ])
        .await;
    }
    any
}

async fn set_audio_muted(stream_ids: &[u32], muted: bool) -> bool {
    let mut any = false;
    for index in stream_ids {
        any |= run_pactl(vec![
            "set-sink-input-mute".to_owned(),
            index.to_string(),
            if muted { "1" } else { "0" }.to_owned(),
        ])
        .await;
    }
    any
}

async fn run_pactl(args: Vec<String>) -> bool {
    let mut command = Command::new("pactl");
    command.kill_on_drop(true);
    command.args(args);
    timeout(AUDIO_COMMAND_TIMEOUT, command.status())
        .await
        .ok()
        .and_then(Result::ok)
        .is_some_and(|status| status.success())
}

pub async fn load_art(url: String) -> Option<MediaArtwork> {
    timeout(ART_TIMEOUT, load_art_inner(&url))
        .await
        .ok()
        .flatten()
}

async fn load_art_inner(url: &str) -> Option<MediaArtwork> {
    let bytes = if url.starts_with("file://") {
        let parsed = reqwest::Url::parse(url).ok()?;
        let path = parsed.to_file_path().ok()?;
        let metadata = tokio::fs::metadata(&path).await.ok()?;
        if metadata.len() > MAX_ART_BYTES as u64 {
            return None;
        }
        tokio::fs::read(path).await.ok()?
    } else if url.starts_with("https://") || url.starts_with("http://") {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_millis(800))
            .timeout(ART_TIMEOUT)
            .build()
            .ok()?;
        let response = client.get(url).send().await.ok()?.error_for_status().ok()?;
        if response
            .content_length()
            .is_some_and(|length| length > MAX_ART_BYTES as u64)
        {
            return None;
        }

        let mut data = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.ok()?;
            if data.len().saturating_add(chunk.len()) > MAX_ART_BYTES {
                return None;
            }
            data.extend_from_slice(&chunk);
        }
        data
    } else {
        return None;
    };

    let decoded = image::load_from_memory(&bytes).ok()?.to_rgba8();
    let thumbnail = image::imageops::thumbnail(&decoded, ART_MAX_SIZE, ART_MAX_SIZE);
    Some(MediaArtwork {
        width: thumbnail.width(),
        height: thumbnail.height(),
        rgba: thumbnail.into_raw(),
    })
}

fn clamp_micros(value: i128) -> i64 {
    value.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

fn looks_like_browser(
    app_id: &str,
    app_label: &str,
    desktop: &str,
    identity: &str,
    bus: &str,
) -> bool {
    const BROWSERS: &[&str] = &[
        "brave",
        "chromium",
        "chrome",
        "firefox",
        "vivaldi",
        "opera",
        "edge",
        "librewolf",
        "zen",
    ];
    let haystack = format!(
        "{}{}{}{}{}",
        normalize(app_id),
        normalize(app_label),
        normalize(desktop),
        normalize(identity),
        normalize(bus)
    );
    BROWSERS.iter().any(|browser| haystack.contains(browser))
}

fn browser_media_matches(media_title: &str, window_titles: &[String]) -> bool {
    let media_norm = normalize(media_title);
    if media_norm.len() < 4 {
        return false;
    }

    let media_tokens = tokens(media_title)
        .into_iter()
        .filter(|token| token.len() >= 4)
        .collect::<Vec<_>>();

    window_titles.iter().any(|window_title| {
        let window_norm = normalize(window_title);
        if window_norm.is_empty() {
            return false;
        }
        if window_norm.contains(&media_norm) || media_norm.contains(&window_norm) {
            return true;
        }

        let window_tokens = tokens(window_title);
        let matches = media_tokens
            .iter()
            .filter(|token| window_tokens.contains(token))
            .count();
        matches >= 2
            || (media_tokens.len() == 1
                && media_tokens[0].len() >= 6
                && window_tokens.contains(&media_tokens[0]))
    })
}

fn match_score(
    app_id: &str,
    app_label: &str,
    bus: &str,
    identity: &str,
    desktop: &str,
    media_title: &str,
    window_titles: &[String],
) -> usize {
    let app_norm = normalize(app_id.trim_end_matches(".desktop"));
    let label_norm = normalize(app_label);
    let bus_norm = normalize(bus);
    let identity_norm = normalize(identity);
    let desktop_norm = normalize(desktop.trim_end_matches(".desktop"));

    let mut score = 0;
    if !app_norm.is_empty() && desktop_norm == app_norm {
        score += 60;
    } else if !app_norm.is_empty() && desktop_norm.contains(&app_norm) {
        score += 30;
    }
    if !label_norm.is_empty() && identity_norm == label_norm {
        score += 36;
    }
    if !label_norm.is_empty() && identity_norm.contains(&label_norm) {
        score += 18;
    }

    for token in tokens(app_id).into_iter().chain(tokens(app_label)) {
        if bus_norm.contains(&token) {
            score += 8;
        }
        if desktop_norm.contains(&token) {
            score += 10;
        }
        if identity_norm.contains(&token) {
            score += 8;
        }
    }

    let media_norm = normalize(media_title);
    for window_title in window_titles {
        let window_norm = normalize(window_title);
        if !media_norm.is_empty()
            && !window_norm.is_empty()
            && (window_norm.contains(&media_norm) || media_norm.contains(&window_norm))
        {
            score += 18;
            break;
        }
        for token in tokens(media_title) {
            if window_norm.contains(&token) {
                score += 2;
            }
        }
    }

    score
}

fn normalize(input: &str) -> String {
    input
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn tokens(input: &str) -> Vec<String> {
    const STOP: &[&str] = &[
        "app",
        "application",
        "brave",
        "browser",
        "chrome",
        "chromium",
        "client",
        "com",
        "desktop",
        "firefox",
        "github",
        "io",
        "org",
        "youtube",
    ];

    input
        .split(|character: char| !character.is_ascii_alphanumeric())
        .map(str::to_ascii_lowercase)
        .filter(|token| token.len() >= 3 && !STOP.contains(&token.as_str()))
        .collect()
}
