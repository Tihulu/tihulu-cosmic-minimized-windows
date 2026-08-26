// SPDX-License-Identifier: AGPL-3.0-only

#[path = "../../media_ipc.rs"]
mod media_ipc;

use std::{
    collections::{HashMap, hash_map::DefaultHasher},
    fs,
    hash::{Hash, Hasher},
    io::Read,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, SystemTime},
};

use media_ipc::{
    MEDIA_PROTOCOL_VERSION, MediaAction, MediaPlayerState, MediaRequest, MediaResponse,
    media_socket_path,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    process::Command,
};
use zbus::{
    Connection, Proxy,
    fdo::DBusProxy,
    zvariant::{ObjectPath, OwnedObjectPath, OwnedValue},
};

const MAX_REQUEST_BYTES: u64 = 64 * 1024;
const MPRIS_PATH: &str = "/org/mpris/MediaPlayer2";
const MPRIS_ROOT: &str = "org.mpris.MediaPlayer2";
const MPRIS_PLAYER: &str = "org.mpris.MediaPlayer2.Player";
const VOLUME_STEP: f64 = 0.05;
const MAX_ARTWORK_BYTES: u64 = 2 * 1024 * 1024;
const MAX_ARTWORK_CACHE_ENTRIES: usize = 12;
const ARTWORK_FAILURE_TTL: Duration = Duration::from_secs(60);
const ARTWORK_DOWNLOAD_TIMEOUT: Duration = Duration::from_millis(1800);

fn normalize(input: &str) -> String {
    input
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn app_hint_candidates(app_hint: &str) -> Vec<String> {
    let normalized = normalize(app_hint.trim_start_matches("browser:"));
    if normalized.is_empty() {
        return Vec::new();
    }

    let mut candidates = vec![normalized.clone()];
    const ALIASES: &[(&str, &str)] = &[
        ("spotify", "spotify"),
        ("brave", "brave"),
        ("firefox", "firefox"),
        ("chromium", "chromium"),
        ("googlechrome", "chrome"),
        ("vivaldi", "vivaldi"),
        ("opera", "opera"),
        ("microsoftedge", "edge"),
    ];
    for (needle, alias) in ALIASES {
        if normalized.contains(needle) && !candidates.iter().any(|candidate| candidate == alias) {
            candidates.push((*alias).to_owned());
        }
    }
    candidates
}

fn integer_micros(value: &OwnedValue) -> Option<i64> {
    i64::try_from(value.clone())
        .ok()
        .or_else(|| {
            u64::try_from(value.clone())
                .ok()
                .and_then(|value| i64::try_from(value).ok())
        })
        .or_else(|| i32::try_from(value.clone()).ok().map(i64::from))
        .or_else(|| u32::try_from(value.clone()).ok().map(i64::from))
}

fn artwork_cache_dir() -> Option<PathBuf> {
    media_socket_path()?
        .parent()
        .map(|parent| parent.join("media-art"))
}

fn artwork_hash(url: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    url.hash(&mut hasher);
    hasher.finish()
}

fn cached_artwork(dir: &Path, hash: u64) -> Option<PathBuf> {
    ["jpg", "png"]
        .into_iter()
        .map(|extension| dir.join(format!("{hash:016x}.{extension}")))
        .find(|path| {
            fs::metadata(path)
                .is_ok_and(|metadata| metadata.len() > 0 && metadata.len() <= MAX_ARTWORK_BYTES)
        })
}

fn failure_marker(dir: &Path, hash: u64) -> PathBuf {
    dir.join(format!("{hash:016x}.fail"))
}

fn failure_is_recent(path: &Path) -> bool {
    fs::metadata(path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age < ARTWORK_FAILURE_TTL)
}

fn mark_artwork_failure(path: &Path) {
    let _ = fs::write(path, b"failed\n");
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
}

fn artwork_extension(path: &Path) -> Option<&'static str> {
    let mut file = fs::File::open(path).ok()?;
    let mut header = [0_u8; 12];
    let read = file.read(&mut header).ok()?;
    if read >= 8 && header[..8] == [137, 80, 78, 71, 13, 10, 26, 10] {
        Some("png")
    } else if read >= 3 && header[..3] == [0xff, 0xd8, 0xff] {
        Some("jpg")
    } else {
        None
    }
}

fn trim_artwork_cache(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut files = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let metadata = entry.metadata().ok()?;
            if !metadata.is_file() || path.extension().is_some_and(|extension| extension == "part") {
                return None;
            }
            Some((metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH), path))
        })
        .collect::<Vec<_>>();
    files.sort_by_key(|(modified, _)| *modified);
    let remove_count = files.len().saturating_sub(MAX_ARTWORK_CACHE_ENTRIES);
    for (_, path) in files.into_iter().take(remove_count) {
        let _ = fs::remove_file(path);
    }
}

async fn cache_artwork(url: &str) -> Option<String> {
    if !url.starts_with("https://") {
        return None;
    }
    let dir = artwork_cache_dir()?;
    if fs::create_dir_all(&dir).is_err() {
        return None;
    }
    let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));

    let hash = artwork_hash(url);
    if let Some(path) = cached_artwork(&dir, hash) {
        return Some(path.to_string_lossy().into_owned());
    }

    let fail = failure_marker(&dir, hash);
    if failure_is_recent(&fail) {
        return None;
    }

    let part = dir.join(format!("{hash:016x}.part"));
    let _ = fs::remove_file(&part);
    let mut command = Command::new("curl");
    command
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--connect-timeout",
            "1",
            "--max-time",
            "1.5",
            "--max-filesize",
            &MAX_ARTWORK_BYTES.to_string(),
            "--output",
        ])
        .arg(&part)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);

    let status = tokio::time::timeout(ARTWORK_DOWNLOAD_TIMEOUT, command.status()).await;
    let downloaded = matches!(status, Ok(Ok(status)) if status.success());
    if !downloaded {
        let _ = fs::remove_file(&part);
        mark_artwork_failure(&fail);
        trim_artwork_cache(&dir);
        return None;
    }

    let valid_size = fs::metadata(&part)
        .is_ok_and(|metadata| metadata.len() > 0 && metadata.len() <= MAX_ARTWORK_BYTES);
    let Some(extension) = valid_size.then(|| artwork_extension(&part)).flatten() else {
        let _ = fs::remove_file(&part);
        mark_artwork_failure(&fail);
        trim_artwork_cache(&dir);
        return None;
    };

    let final_path = dir.join(format!("{hash:016x}.{extension}"));
    if fs::rename(&part, &final_path).is_err() {
        let _ = fs::remove_file(&part);
        mark_artwork_failure(&fail);
        trim_artwork_cache(&dir);
        return None;
    }
    let _ = fs::set_permissions(&final_path, fs::Permissions::from_mode(0o600));
    let _ = fs::remove_file(&fail);
    trim_artwork_cache(&dir);
    Some(final_path.to_string_lossy().into_owned())
}

async fn read_player(connection: &Connection, bus_name: &str) -> Result<MediaPlayerState, String> {
    let root = Proxy::new(connection, bus_name, MPRIS_PATH, MPRIS_ROOT)
        .await
        .map_err(|error| format!("MPRIS root proxy failed: {error}"))?;
    let player = Proxy::new(connection, bus_name, MPRIS_PATH, MPRIS_PLAYER)
        .await
        .map_err(|error| format!("MPRIS player proxy failed: {error}"))?;

    let identity: String = root
        .get_property("Identity")
        .await
        .unwrap_or_else(|_| bus_name.to_owned());
    let playback_status: String = player
        .get_property("PlaybackStatus")
        .await
        .unwrap_or_else(|_| "Unknown".to_owned());
    let metadata: HashMap<String, OwnedValue> =
        player.get_property("Metadata").await.unwrap_or_default();
    let title = metadata
        .get("xesam:title")
        .and_then(|value| String::try_from(value.clone()).ok())
        .unwrap_or_default();
    let artist = metadata
        .get("xesam:artist")
        .and_then(|value| Vec::<String>::try_from(value.clone()).ok())
        .map(|artists| artists.join(", "))
        .unwrap_or_default();
    let length_micros = metadata
        .get("mpris:length")
        .and_then(integer_micros)
        .filter(|length| *length > 0);
    let track_id = metadata
        .get("mpris:trackid")
        .and_then(|value| OwnedObjectPath::try_from(value.clone()).ok())
        .map(|path| path.to_string());
    let artwork_url = metadata
        .get("mpris:artUrl")
        .and_then(|value| String::try_from(value.clone()).ok())
        .filter(|url| !url.trim().is_empty());
    let position_micros = player
        .get_property::<OwnedValue>("Position")
        .await
        .ok()
        .as_ref()
        .and_then(integer_micros)
        .unwrap_or(0)
        .max(0);
    let volume = player
        .get_property::<f64>("Volume")
        .await
        .ok()
        .filter(|value| value.is_finite());
    let can_previous: bool = player.get_property("CanGoPrevious").await.unwrap_or(false);
    let can_next: bool = player.get_property("CanGoNext").await.unwrap_or(false);
    let can_play: bool = player.get_property("CanPlay").await.unwrap_or(false);
    let can_pause: bool = player.get_property("CanPause").await.unwrap_or(false);
    let can_seek: bool = player.get_property("CanSeek").await.unwrap_or(false);
    let artwork_path = match artwork_url {
        Some(url) => cache_artwork(&url).await,
        None => None,
    };

    Ok(MediaPlayerState {
        bus_name: bus_name.to_owned(),
        identity,
        playback_status,
        title,
        artist,
        position_micros,
        length_micros,
        volume,
        track_id,
        artwork_path,
        can_previous,
        can_play_pause: can_play || can_pause,
        can_next,
        can_seek,
    })
}

async fn find_player(
    connection: &Connection,
    app_hint: &str,
) -> Result<Option<MediaPlayerState>, String> {
    let dbus = DBusProxy::new(connection)
        .await
        .map_err(|error| format!("D-Bus proxy failed: {error}"))?;
    let names = dbus
        .list_names()
        .await
        .map_err(|error| format!("D-Bus ListNames failed: {error}"))?;
    let hints = app_hint_candidates(app_hint);
    let mut fallback = None;

    for name in names {
        let bus_name = name.to_string();
        if !bus_name.starts_with("org.mpris.MediaPlayer2.") {
            continue;
        }
        let Ok(state) = read_player(connection, &bus_name).await else {
            continue;
        };
        let haystack = format!(
            "{}{}",
            normalize(&state.bus_name),
            normalize(&state.identity)
        );
        if !hints.is_empty() {
            if hints.iter().any(|hint| haystack.contains(hint)) {
                return Ok(Some(state));
            }
            continue;
        }
        if fallback.is_none() && state.playback_status.eq_ignore_ascii_case("Playing") {
            fallback = Some(state);
        }
    }
    Ok(fallback)
}

async fn adjust_volume(player: &Proxy<'_>, delta: f64) -> Result<(), String> {
    let current: f64 = player
        .get_property("Volume")
        .await
        .map_err(|error| format!("MPRIS Volume read failed: {error}"))?;
    let target = (current + delta).clamp(0.0, 1.0);
    player
        .set_property("Volume", target)
        .await
        .map_err(|error| format!("MPRIS Volume write failed: {error}"))
}

async fn control_player(
    connection: &Connection,
    bus_name: &str,
    action: MediaAction,
) -> Result<(), String> {
    if !bus_name.starts_with("org.mpris.MediaPlayer2.") {
        return Err("invalid MPRIS bus name".to_owned());
    }
    let player = Proxy::new(connection, bus_name, MPRIS_PATH, MPRIS_PLAYER)
        .await
        .map_err(|error| format!("MPRIS player proxy failed: {error}"))?;

    match action {
        MediaAction::Previous => {
            player
                .call_method("Previous", &())
                .await
                .map_err(|error| format!("MPRIS Previous failed: {error}"))?;
        }
        MediaAction::PlayPause => {
            player
                .call_method("PlayPause", &())
                .await
                .map_err(|error| format!("MPRIS PlayPause failed: {error}"))?;
        }
        MediaAction::Next => {
            player
                .call_method("Next", &())
                .await
                .map_err(|error| format!("MPRIS Next failed: {error}"))?;
        }
        MediaAction::VolumeDown => adjust_volume(&player, -VOLUME_STEP).await?,
        MediaAction::VolumeUp => adjust_volume(&player, VOLUME_STEP).await?,
    }
    Ok(())
}

async fn seek_player(
    connection: &Connection,
    bus_name: &str,
    track_id: &str,
    position_micros: i64,
) -> Result<(), String> {
    if !bus_name.starts_with("org.mpris.MediaPlayer2.") {
        return Err("invalid MPRIS bus name".to_owned());
    }
    if position_micros < 0 {
        return Err("invalid negative media seek position".to_owned());
    }
    let path = ObjectPath::try_from(track_id)
        .map_err(|error| format!("invalid MPRIS track id: {error}"))?;
    let player = Proxy::new(connection, bus_name, MPRIS_PATH, MPRIS_PLAYER)
        .await
        .map_err(|error| format!("MPRIS player proxy failed: {error}"))?;
    let can_seek: bool = player.get_property("CanSeek").await.unwrap_or(false);
    if !can_seek {
        return Err("MPRIS player does not support seek".to_owned());
    }
    player
        .call_method("SetPosition", &(path, position_micros))
        .await
        .map_err(|error| format!("MPRIS SetPosition failed: {error}"))?;
    Ok(())
}

async fn process(connection: &Connection, request: MediaRequest) -> MediaResponse {
    if request.version() != MEDIA_PROTOCOL_VERSION {
        return MediaResponse::Error {
            version: MEDIA_PROTOCOL_VERSION,
            message: format!(
                "protocol mismatch: client={} daemon={}",
                request.version(),
                MEDIA_PROTOCOL_VERSION
            ),
        };
    }

    match request {
        MediaRequest::Status { app_hint, .. } => match find_player(connection, &app_hint).await {
            Ok(player) => MediaResponse::State {
                version: MEDIA_PROTOCOL_VERSION,
                player,
            },
            Err(message) => MediaResponse::Error {
                version: MEDIA_PROTOCOL_VERSION,
                message,
            },
        },
        MediaRequest::Control {
            bus_name, action, ..
        } => match control_player(connection, &bus_name, action).await {
            Ok(()) => MediaResponse::State {
                version: MEDIA_PROTOCOL_VERSION,
                player: read_player(connection, &bus_name).await.ok(),
            },
            Err(message) => MediaResponse::Error {
                version: MEDIA_PROTOCOL_VERSION,
                message,
            },
        },
        MediaRequest::Seek {
            bus_name,
            track_id,
            position_micros,
            ..
        } => match seek_player(connection, &bus_name, &track_id, position_micros).await {
            Ok(()) => MediaResponse::State {
                version: MEDIA_PROTOCOL_VERSION,
                player: read_player(connection, &bus_name).await.ok(),
            },
            Err(message) => MediaResponse::Error {
                version: MEDIA_PROTOCOL_VERSION,
                message,
            },
        },
    }
}

async fn handle_connection(connection: &Connection, stream: UnixStream) -> Result<(), String> {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read).take(MAX_REQUEST_BYTES);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .await
        .map_err(|error| format!("media socket read failed: {error}"))?;
    if line.is_empty() || !line.ends_with('\n') {
        return Err("invalid or oversized media request".to_owned());
    }
    let request: MediaRequest = serde_json::from_str(line.trim_end())
        .map_err(|error| format!("invalid media request: {error}"))?;
    let response = process(connection, request).await;
    let mut encoded = serde_json::to_vec(&response)
        .map_err(|error| format!("media response encode failed: {error}"))?;
    encoded.push(b'\n');
    write
        .write_all(&encoded)
        .await
        .map_err(|error| format!("media socket write failed: {error}"))
}

fn prepare_socket(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("runtime dir create failed: {error}"))?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("runtime dir permissions failed: {error}"))?;
    }
    if path.exists() {
        fs::remove_file(path).map_err(|error| format!("stale socket removal failed: {error}"))?;
    }
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let result = async {
        let path =
            media_socket_path().ok_or_else(|| "XDG_RUNTIME_DIR is unavailable".to_owned())?;
        prepare_socket(&path)?;
        let listener = UnixListener::bind(&path)
            .map_err(|error| format!("media socket bind failed: {error}"))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("media socket permissions failed: {error}"))?;
        let connection = Connection::session()
            .await
            .map_err(|error| format!("session D-Bus connection failed: {error}"))?;
        eprintln!("tihulu-mediad ready: {}", path.display());

        loop {
            let (stream, _) = listener
                .accept()
                .await
                .map_err(|error| format!("media accept failed: {error}"))?;
            if let Err(error) = handle_connection(&connection, stream).await {
                eprintln!("tihulu-mediad request failed: {error}");
            }
        }
        #[allow(unreachable_code)]
        Ok::<(), String>(())
    }
    .await;

    if let Err(error) = result {
        eprintln!("tihulu-mediad: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::{app_hint_candidates, artwork_extension};
    use std::{fs, path::PathBuf};

    #[test]
    fn spotify_flatpak_id_matches_spotify_mpris_identity() {
        let candidates = app_hint_candidates("com.spotify.Client");
        assert!(candidates.iter().any(|candidate| candidate == "spotify"));
    }

    #[test]
    fn browser_group_keys_keep_specific_player_matching() {
        assert_eq!(app_hint_candidates("browser:brave"), vec!["brave"]);
        assert_eq!(app_hint_candidates("browser:firefox"), vec!["firefox"]);
    }

    #[test]
    fn artwork_magic_accepts_png_and_jpeg_only() {
        let root = std::env::temp_dir();
        let png = root.join(format!("tihulu-media-test-{}-png", std::process::id()));
        let jpg = root.join(format!("tihulu-media-test-{}-jpg", std::process::id()));
        let bad = root.join(format!("tihulu-media-test-{}-bad", std::process::id()));
        fs::write(&png, [137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 0]).unwrap();
        fs::write(&jpg, [0xff, 0xd8, 0xff, 0, 0, 0, 0, 0, 0, 0, 0, 0]).unwrap();
        fs::write(&bad, b"not-an-image").unwrap();
        assert_eq!(artwork_extension(&png), Some("png"));
        assert_eq!(artwork_extension(&jpg), Some("jpg"));
        assert_eq!(artwork_extension(&bad), None);
        for path in [png, jpg, bad] {
            let _ = fs::remove_file(path);
        }
        let _ = PathBuf::new();
    }
}
