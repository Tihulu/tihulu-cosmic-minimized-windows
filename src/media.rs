// SPDX-License-Identifier: AGPL-3.0-only

use std::{fs, process::Command, time::Duration};

use mpris::{PlaybackStatus, Player, PlayerFinder};

const DBUS_TIMEOUT_MS: i32 = 450;
const ART_MAX_BYTES: usize = 4 * 1024 * 1024;
const ART_MAX_SIDE: u32 = 160;

#[derive(Clone, Debug)]
pub struct MediaArt {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug)]
pub struct MediaState {
    pub bus_name: String,
    pub identity: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub art: Option<MediaArt>,
    pub duration_us: u64,
    pub position_us: u64,
    pub volume: Option<f64>,
    pub playing: bool,
    pub can_previous: bool,
    pub can_play_pause: bool,
    pub can_next: bool,
    pub can_seek: bool,
}

#[derive(Clone, Debug)]
pub enum MediaCommand {
    Previous,
    PlayPause,
    Next,
    SeekFraction(f64),
    SetVolume(f64),
}

pub async fn fetch_for_app(app_id: String, app_label: String) -> Option<MediaState> {
    tokio::task::spawn_blocking(move || fetch_for_app_blocking(&app_id, &app_label))
        .await
        .ok()
        .flatten()
}

pub async fn run_command(bus_name: String, command: MediaCommand) -> bool {
    tokio::task::spawn_blocking(move || run_command_blocking(&bus_name, command))
        .await
        .unwrap_or(false)
}

fn fetch_for_app_blocking(app_id: &str, app_label: &str) -> Option<MediaState> {
    let mut finder = PlayerFinder::new().ok()?;
    finder.set_player_timeout_ms(DBUS_TIMEOUT_MS);

    let mut best: Option<(i32, Player)> = None;
    for player in finder.find_all().ok()? {
        let score = player_match_score(&player, app_id, app_label);
        if score <= 0 {
            continue;
        }
        if best.as_ref().is_none_or(|(best_score, _)| score > *best_score) {
            best = Some((score, player));
        }
    }

    let (_, player) = best?;
    let metadata = player.get_metadata().ok()?;
    let duration = metadata.length().unwrap_or_default();
    let position = player.checked_get_position().ok().flatten().unwrap_or_default();
    let volume = player.checked_get_volume().ok().flatten();
    let status = player.get_playback_status().ok();

    let art = metadata.art_url().and_then(load_album_art);
    let artist = metadata
        .artists()
        .unwrap_or_default()
        .into_iter()
        .collect::<Vec<_>>()
        .join(", ");

    Some(MediaState {
        bus_name: player.bus_name().to_owned(),
        identity: player.identity().to_owned(),
        title: metadata.title().unwrap_or(player.identity()).to_owned(),
        artist,
        album: metadata.album_name().unwrap_or_default().to_owned(),
        art,
        duration_us: duration.as_micros().try_into().unwrap_or(u64::MAX),
        position_us: position.as_micros().try_into().unwrap_or(u64::MAX),
        volume,
        playing: status == Some(PlaybackStatus::Playing),
        can_previous: player.can_go_previous().unwrap_or(false),
        can_play_pause: player.can_play().unwrap_or(false) || player.can_pause().unwrap_or(false),
        can_next: player.can_go_next().unwrap_or(false),
        can_seek: player.can_seek().unwrap_or(false),
    })
}

fn run_command_blocking(bus_name: &str, command: MediaCommand) -> bool {
    let mut finder = match PlayerFinder::new() {
        Ok(finder) => finder,
        Err(_) => return false,
    };
    finder.set_player_timeout_ms(DBUS_TIMEOUT_MS);

    let player = match finder.iter_players() {
        Ok(players) => players
            .filter_map(Result::ok)
            .find(|player| player.bus_name() == bus_name),
        Err(_) => None,
    };
    let Some(player) = player else {
        return false;
    };

    match command {
        MediaCommand::Previous => player.checked_previous().unwrap_or(false),
        MediaCommand::PlayPause => player.checked_play_pause().unwrap_or(false),
        MediaCommand::Next => player.checked_next().unwrap_or(false),
        MediaCommand::SetVolume(volume) => player
            .checked_set_volume(volume.clamp(0.0, 1.5))
            .unwrap_or(false),
        MediaCommand::SeekFraction(fraction) => {
            let Ok(metadata) = player.get_metadata() else {
                return false;
            };
            let Some(track_id) = metadata.track_id() else {
                return false;
            };
            let Some(length) = metadata.length() else {
                return false;
            };
            let fraction = fraction.clamp(0.0, 1.0);
            let target = Duration::from_secs_f64(length.as_secs_f64() * fraction);
            player
                .checked_set_position(track_id, &target)
                .unwrap_or(false)
        }
    }
}

fn player_match_score(player: &Player, app_id: &str, app_label: &str) -> i32 {
    let app = normalize(app_id);
    let label = normalize(app_label);
    let desktop = player
        .get_desktop_entry()
        .ok()
        .flatten()
        .map(|value| normalize(&value))
        .unwrap_or_default();
    let identity = normalize(player.identity());
    let bus = normalize(player.bus_name_trimmed());

    let mut score = 0;
    for wanted in [&app, &label] {
        if wanted.is_empty() {
            continue;
        }
        for candidate in [&desktop, &identity, &bus] {
            if candidate.is_empty() {
                continue;
            }
            if wanted == candidate {
                score = score.max(100);
            } else if wanted.contains(candidate) || candidate.contains(wanted) {
                score = score.max(60);
            }
        }
    }
    score
}

fn normalize(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn load_album_art(art_url: &str) -> Option<MediaArt> {
    let bytes = if art_url.starts_with("file://") {
        let url = url::Url::parse(art_url).ok()?;
        let path = url.to_file_path().ok()?;
        let metadata = fs::metadata(&path).ok()?;
        if usize::try_from(metadata.len()).ok()? > ART_MAX_BYTES {
            return None;
        }
        fs::read(path).ok()?
    } else if art_url.starts_with("https://") || art_url.starts_with("http://") {
        let output = Command::new("curl")
            .args([
                "--location",
                "--silent",
                "--fail",
                "--connect-timeout",
                "1",
                "--max-time",
                "2",
                "--max-filesize",
                "4194304",
                "--proto",
                "=http,https",
                art_url,
            ])
            .output()
            .ok()?;
        if !output.status.success() || output.stdout.len() > ART_MAX_BYTES {
            return None;
        }
        output.stdout
    } else {
        return None;
    };

    if bytes.len() > ART_MAX_BYTES {
        return None;
    }

    let image = image::load_from_memory(&bytes).ok()?.to_rgba8();
    let max_side = image.width().max(image.height());
    let image = if max_side > ART_MAX_SIDE {
        let scale = ART_MAX_SIDE as f64 / f64::from(max_side);
        let width = (f64::from(image.width()) * scale).round().max(1.0) as u32;
        let height = (f64::from(image.height()) * scale).round().max(1.0) as u32;
        image::imageops::resize(
            &image,
            width,
            height,
            image::imageops::FilterType::Triangle,
        )
    } else {
        image
    };

    Some(MediaArt {
        width: image.width(),
        height: image.height(),
        rgba: image.into_raw(),
    })
}
