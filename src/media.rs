// SPDX-License-Identifier: AGPL-3.0-only

use std::time::Duration;

use futures::StreamExt;
use mpris2_zbus::{media_player::MediaPlayer, player::PlaybackStatus};
use tokio::time::timeout;
use zbus::names::OwnedBusName;

const MEDIA_TIMEOUT: Duration = Duration::from_millis(1500);
const COMMAND_TIMEOUT: Duration = Duration::from_millis(1200);
const ART_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_ART_BYTES: usize = 2 * 1024 * 1024;
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
}

pub async fn snapshot(app_id: String, app_label: String) -> Option<MediaSnapshot> {
    timeout(MEDIA_TIMEOUT, snapshot_inner(&app_id, &app_label))
        .await
        .ok()
        .flatten()
}

async fn snapshot_inner(app_id: &str, app_label: &str) -> Option<MediaSnapshot> {
    let connection = zbus::Connection::session().await.ok()?;
    let players = MediaPlayer::available_players(&connection).await.ok()?;

    let mut best: Option<(usize, MediaPlayer, String, String)> = None;

    for bus_name in players {
        let player = MediaPlayer::new(&connection, bus_name.clone()).await.ok()?;
        let identity = player.identity().await.unwrap_or_default();
        let desktop_entry = player.desktop_entry().await.unwrap_or_default();
        let score = match_score(
            app_id,
            app_label,
            bus_name.as_str(),
            &identity,
            &desktop_entry,
        );

        if score == 0 {
            continue;
        }

        let replace = best
            .as_ref()
            .map(|(best_score, _, _, _)| score > *best_score)
            .unwrap_or(true);
        if replace {
            best = Some((score, player, identity, bus_name.to_string()));
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
    let volume = player.volume().await.unwrap_or(1.0).clamp(0.0, 1.5);
    let can_previous = player.can_go_previous().await.unwrap_or(false);
    let can_next = player.can_go_next().await.unwrap_or(false);
    let can_play_pause = player.can_control().await.unwrap_or(false)
        && (player.can_play().await.unwrap_or(false) || player.can_pause().await.unwrap_or(false));

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
        can_previous,
        can_next,
        can_play_pause,
    })
}

pub async fn command(bus_name: String, command: MediaCommand) -> bool {
    timeout(COMMAND_TIMEOUT, command_inner(bus_name, command))
        .await
        .unwrap_or(false)
}

async fn command_inner(bus_name: String, command: MediaCommand) -> bool {
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
        MediaCommand::PlayPause => player.play_pause().await.is_ok(),
        MediaCommand::Next => player.next().await.is_ok(),
        MediaCommand::SetVolume(volume) => player.set_volume(volume.clamp(0.0, 1.5)).await.is_ok(),
    }
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

fn match_score(app_id: &str, app_label: &str, bus: &str, identity: &str, desktop: &str) -> usize {
    let app_norm = normalize(app_id);
    let label_norm = normalize(app_label);
    let bus_norm = normalize(bus);
    let identity_norm = normalize(identity);
    let desktop_norm = normalize(desktop);

    let mut score = 0;
    if !app_norm.is_empty() && desktop_norm == app_norm {
        score += 20;
    }
    if !label_norm.is_empty() && identity_norm == label_norm {
        score += 16;
    }
    if !label_norm.is_empty() && identity_norm.contains(&label_norm) {
        score += 10;
    }

    for token in tokens(app_id).into_iter().chain(tokens(app_label)) {
        if bus_norm.contains(&token) {
            score += 8;
        }
        if desktop_norm.contains(&token) {
            score += 7;
        }
        if identity_norm.contains(&token) {
            score += 6;
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
        "client",
        "com",
        "desktop",
        "github",
        "io",
        "org",
    ];

    input
        .split(|character: char| !character.is_ascii_alphanumeric())
        .map(str::to_ascii_lowercase)
        .filter(|token| token.len() >= 3 && !STOP.contains(&token.as_str()))
        .collect()
}
