// SPDX-License-Identifier: AGPL-3.0-only

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::Stdio,
    time::Duration,
};

use serde_json::{Map, Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    process::Command,
    time::timeout,
};

const REQUEST_TIMEOUT: Duration = Duration::from_millis(2400);
const PROCESS_TIMEOUT: Duration = Duration::from_millis(1100);
const MAX_OUTPUT: usize = 2 * 1024 * 1024;
const MAX_REQUEST: usize = 64 * 1024;
const SELF_FD_GUARD: usize = 96;

#[derive(Clone, Debug)]
struct PlayerSnapshot {
    player: String,
    title: String,
    artist: String,
    art_url: String,
    playing: bool,
    position_us: i64,
    length_us: i64,
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

fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("tihulu-minimized-windows")
}

fn config_mode_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {
        return Some(
            PathBuf::from(path)
                .join("tihulu-cosmic-minimized-windows")
                .join("mode"),
        );
    }
    std::env::var_os("HOME").map(|home| {
        PathBuf::from(home)
            .join(".config/tihulu-cosmic-minimized-windows")
            .join("mode")
    })
}

fn enhanced_enabled() -> bool {
    config_mode_path()
        .and_then(|path| fs::read_to_string(path).ok())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("enhanced"))
}

fn self_fd_count() -> usize {
    fs::read_dir("/proc/self/fd")
        .map(|entries| entries.filter_map(Result::ok).count())
        .unwrap_or(0)
}

async fn run_program(program: &str, args: &[String]) -> Option<Vec<u8>> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .kill_on_drop(true);
    let output = timeout(PROCESS_TIMEOUT, command.output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success() || output.stdout.len() > MAX_OUTPUT {
        return None;
    }
    Some(output.stdout)
}

async fn run_status(program: &str, args: &[String]) -> bool {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    timeout(PROCESS_TIMEOUT, command.status())
        .await
        .ok()
        .and_then(Result::ok)
        .is_some_and(|status| status.success())
}

fn normalize(input: &str) -> String {
    input
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn useful_tokens(input: &str) -> Vec<String> {
    input
        .split(|character: char| !character.is_ascii_alphanumeric())
        .map(str::trim)
        .filter(|token| token.len() >= 3)
        .map(str::to_ascii_lowercase)
        .collect()
}

fn browserish(input: &str) -> bool {
    let value = normalize(input);
    [
        "brave", "chromium", "chrome", "firefox", "vivaldi", "opera", "edge",
    ]
    .iter()
    .any(|needle| value.contains(needle))
}

fn title_matches(media_title: &str, window_titles: &[String]) -> bool {
    let media = normalize(media_title);
    if media.len() < 3 {
        return false;
    }
    window_titles.iter().any(|window| {
        let window = normalize(window);
        window.contains(&media) || media.contains(&window)
    })
}

fn player_score(
    player: &str,
    title: &str,
    playing: bool,
    app_id: &str,
    app_label: &str,
    window_titles: &[String],
) -> usize {
    let player_norm = normalize(player);
    let app_id_norm = normalize(app_id);
    let app_label_norm = normalize(app_label);
    let mut score = 0_usize;

    if !app_id_norm.is_empty()
        && (player_norm.contains(&app_id_norm) || app_id_norm.contains(&player_norm))
    {
        score += 40;
    }
    if !app_label_norm.is_empty()
        && (player_norm.contains(&app_label_norm) || app_label_norm.contains(&player_norm))
    {
        score += 32;
    }
    for token in useful_tokens(app_id)
        .into_iter()
        .chain(useful_tokens(app_label))
    {
        if player_norm.contains(&token) {
            score += 10;
        }
    }
    if title_matches(title, window_titles) {
        score += 70;
    }
    if playing {
        score += 25;
    }

    if browserish(app_id) && browserish(player) && !title_matches(title, window_titles) {
        // Chromium-family MPRIS names are shared by several browsers. Require the media
        // title to agree with one of this app group's real window titles before trusting it.
        score = score.min(20);
    }
    score
}

async fn player_names() -> Vec<String> {
    let Some(output) = run_program("playerctl", &["-l".to_owned()]).await else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

async fn player_metadata(player: &str) -> Option<PlayerSnapshot> {
    const SEP: char = '\u{1f}';
    let format = format!(
        "{{{{status}}}}{SEP}{{{{xesam:title}}}}{SEP}{{{{xesam:artist}}}}{SEP}{{{{mpris:artUrl}}}}{SEP}{{{{mpris:length}}}}"
    );
    let output = run_program(
        "playerctl",
        &[
            "-p".to_owned(),
            player.to_owned(),
            "metadata".to_owned(),
            "--format".to_owned(),
            format,
        ],
    )
    .await?;
    let line = String::from_utf8_lossy(&output);
    let mut parts = line.trim_end().split(SEP);
    let status = parts.next().unwrap_or_default();
    let title = parts.next().unwrap_or_default().to_owned();
    let artist = parts.next().unwrap_or_default().to_owned();
    let art_url = parts.next().unwrap_or_default().to_owned();
    let length_us = parts
        .next()
        .and_then(|value| value.trim().parse::<i64>().ok())
        .unwrap_or(0);

    let position_us = run_program(
        "playerctl",
        &["-p".to_owned(), player.to_owned(), "position".to_owned()],
    )
    .await
    .and_then(|output| String::from_utf8(output).ok())
    .and_then(|value| value.trim().parse::<f64>().ok())
    .map(|seconds| (seconds.max(0.0) * 1_000_000.0).round() as i64)
    .unwrap_or(0);

    let volume = run_program(
        "playerctl",
        &["-p".to_owned(), player.to_owned(), "volume".to_owned()],
    )
    .await
    .and_then(|output| String::from_utf8(output).ok())
    .and_then(|value| value.trim().parse::<f64>().ok())
    .unwrap_or(1.0)
    .clamp(0.0, 1.5);

    Some(PlayerSnapshot {
        player: player.to_owned(),
        title,
        artist,
        art_url,
        playing: status.eq_ignore_ascii_case("playing"),
        position_us,
        length_us,
        volume,
        muted: volume <= 0.001,
    })
}

fn property<'a>(properties: &'a Map<String, Value>, key: &str) -> &'a str {
    properties
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
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
    (count > 0).then_some(total as f64 / count as f64 / 65_536.0)
}

fn audio_score(
    properties: &Map<String, Value>,
    app_id: &str,
    app_label: &str,
    media_title: &str,
) -> usize {
    let app_name = normalize(property(properties, "application.name"));
    let binary = normalize(property(properties, "application.process.binary"));
    let media_name = normalize(property(properties, "media.name"));
    let app_label = normalize(app_label);
    let media_title = normalize(media_title);
    let mut score = 0_usize;

    if !app_label.is_empty() && app_name == app_label {
        score += 36;
    }
    for token in useful_tokens(app_id) {
        if app_name.contains(&token) {
            score += 12;
        }
        if binary.contains(&token) {
            score += 12;
        }
    }
    if !media_title.is_empty()
        && !media_name.is_empty()
        && (media_name.contains(&media_title) || media_title.contains(&media_name))
    {
        score += 10;
    }
    score
}

async fn audio_candidates(
    app_id: &str,
    app_label: &str,
    media_title: &str,
) -> Vec<AudioCandidate> {
    let Some(output) = run_program(
        "pactl",
        &[
            "-f".to_owned(),
            "json".to_owned(),
            "list".to_owned(),
            "sink-inputs".to_owned(),
        ],
    )
    .await
    else {
        return Vec::new();
    };
    let Ok(root) = serde_json::from_slice::<Value>(&output) else {
        return Vec::new();
    };
    let Some(streams) = root.as_array() else {
        return Vec::new();
    };
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
        let score = audio_score(properties, app_id, app_label, media_title);
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
    candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.score));
    let Some(best_score) = candidates.first().map(|candidate| candidate.score) else {
        return candidates;
    };
    let threshold = best_score.saturating_sub(2).max(8);
    candidates
        .into_iter()
        .filter(|candidate| candidate.score >= threshold)
        .collect()
}

async fn best_snapshot(
    app_id: &str,
    app_label: &str,
    window_titles: &[String],
) -> Option<PlayerSnapshot> {
    let mut best: Option<(usize, PlayerSnapshot)> = None;
    for player in player_names().await {
        let Some(mut snapshot) = player_metadata(&player).await else {
            continue;
        };
        let score = player_score(
            &snapshot.player,
            &snapshot.title,
            snapshot.playing,
            app_id,
            app_label,
            window_titles,
        );
        if score == 0 {
            continue;
        }
        if let Some(audio) = audio_candidates(app_id, app_label, &snapshot.title).await.first() {
            snapshot.volume = audio.volume;
            snapshot.muted = audio.muted;
        }
        if best.as_ref().is_none_or(|(old, _)| score > *old) {
            best = Some((score, snapshot));
        }
    }
    best.map(|(_, snapshot)| snapshot)
}

fn snapshot_json(snapshot: PlayerSnapshot) -> Value {
    json!({
        "ok": true,
        "player": snapshot.player,
        "title": snapshot.title,
        "artist": snapshot.artist,
        "art_url": snapshot.art_url,
        "playing": snapshot.playing,
        "position_us": snapshot.position_us,
        "length_us": snapshot.length_us,
        "volume": snapshot.volume,
        "muted": snapshot.muted,
    })
}

fn request_strings(request: &Value) -> (String, String, Vec<String>) {
    let app_id = request
        .get("app_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let app_label = request
        .get("app_label")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let titles = request
        .get("window_titles")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    (app_id, app_label, titles)
}

async fn command_and_refresh(request: &Value, action: &str) -> Value {
    let (app_id, app_label, titles) = request_strings(request);
    let snapshot = if let Some(player) = request.get("player").and_then(Value::as_str) {
        player_metadata(player).await
    } else {
        best_snapshot(&app_id, &app_label, &titles).await
    };
    let Some(snapshot) = snapshot else {
        return json!({"ok": false, "error": "no matching MPRIS player"});
    };
    let player = snapshot.player.clone();

    let success = match action {
        "toggle_playback" => {
            // Fresh status at click time. Never derive the action from the applet's cached icon.
            let status = run_program(
                "playerctl",
                &["-p".to_owned(), player.clone(), "status".to_owned()],
            )
            .await
            .and_then(|output| String::from_utf8(output).ok())
            .unwrap_or_default();
            let verb = if status.trim().eq_ignore_ascii_case("playing") {
                "pause"
            } else {
                "play"
            };
            run_status(
                "playerctl",
                &["-p".to_owned(), player.clone(), verb.to_owned()],
            )
            .await
        }
        "next" | "previous" => {
            run_status(
                "playerctl",
                &["-p".to_owned(), player.clone(), action.to_owned()],
            )
            .await
        }
        "volume_up" | "volume_down" => {
            let candidates = audio_candidates(&app_id, &app_label, &snapshot.title).await;
            if !candidates.is_empty() {
                let delta = if action == "volume_up" { "+5%" } else { "-5%" };
                let mut any = false;
                for candidate in candidates {
                    any |= run_status(
                        "pactl",
                        &[
                            "set-sink-input-volume".to_owned(),
                            candidate.index.to_string(),
                            delta.to_owned(),
                        ],
                    )
                    .await;
                }
                any
            } else {
                let delta = if action == "volume_up" { "0.05+" } else { "0.05-" };
                run_status(
                    "playerctl",
                    &[
                        "-p".to_owned(),
                        player.clone(),
                        "volume".to_owned(),
                        delta.to_owned(),
                    ],
                )
                .await
            }
        }
        "toggle_mute" => {
            let candidates = audio_candidates(&app_id, &app_label, &snapshot.title).await;
            let mut any = false;
            for candidate in candidates {
                any |= run_status(
                    "pactl",
                    &[
                        "set-sink-input-mute".to_owned(),
                        candidate.index.to_string(),
                        "toggle".to_owned(),
                    ],
                )
                .await;
            }
            any
        }
        _ => false,
    };

    tokio::time::sleep(Duration::from_millis(70)).await;
    let refreshed = best_snapshot(&app_id, &app_label, &titles).await;
    match refreshed {
        Some(snapshot) => {
            let mut value = snapshot_json(snapshot);
            value["command_ok"] = Value::Bool(success);
            value
        }
        None => json!({"ok": success, "command_ok": success}),
    }
}

async fn handle_request(request: Value) -> Value {
    if !enhanced_enabled() {
        return json!({"ok": false, "disabled": true, "mode": "safe"});
    }
    let command = request
        .get("cmd")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match command {
        "ping" => json!({"ok": true, "service": "tihulu-mediad"}),
        "snapshot" => {
            let (app_id, app_label, titles) = request_strings(&request);
            match best_snapshot(&app_id, &app_label, &titles).await {
                Some(snapshot) => snapshot_json(snapshot),
                None => json!({"ok": false, "error": "no matching media player"}),
            }
        }
        "toggle_playback" | "next" | "previous" | "volume_up" | "volume_down"
        | "toggle_mute" => command_and_refresh(&request, command).await,
        _ => json!({"ok": false, "error": "unknown command"}),
    }
}

async fn serve_connection(stream: UnixStream) {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    let Ok(read) = reader.read_line(&mut line).await else {
        return;
    };
    if read == 0 || line.len() > MAX_REQUEST {
        return;
    }
    let request = match serde_json::from_str::<Value>(&line) {
        Ok(request) => request,
        Err(_) => {
            let _ = writer
                .write_all(b"{\"ok\":false,\"error\":\"invalid json\"}\n")
                .await;
            return;
        }
    };
    let response = timeout(REQUEST_TIMEOUT, handle_request(request))
        .await
        .unwrap_or_else(|_| json!({"ok": false, "error": "request timeout"}));
    if let Ok(mut bytes) = serde_json::to_vec(&response) {
        bytes.push(b'\n');
        let _ = writer.write_all(&bytes).await;
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = runtime_dir();
    fs::create_dir_all(&dir)?;
    let socket = dir.join("media.sock");
    if socket.exists() {
        let _ = fs::remove_file(&socket);
    }
    let listener = UnixListener::bind(&socket)?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
    eprintln!("tihulu-mediad listening on {}", socket.display());

    loop {
        if self_fd_count() > SELF_FD_GUARD {
            eprintln!("FD guard exceeded; exiting for systemd restart");
            std::process::exit(75);
        }
        let (stream, _) = listener.accept().await?;
        tokio::spawn(serve_connection(stream));
    }
}
