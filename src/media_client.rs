// SPDX-License-Identifier: AGPL-3.0-only

use std::{path::PathBuf, time::Duration};

use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    time::timeout,
};

const SOCKET_TIMEOUT: Duration = Duration::from_millis(900);
const MAX_RESPONSE: u64 = 64 * 1024;

#[derive(Clone, Debug)]
pub struct MediaSnapshot {
    pub player: String,
    pub title: String,
    pub artist: String,
    pub playing: bool,
    pub position_us: i64,
    pub length_us: i64,
    pub volume: f64,
    pub muted: bool,
}

#[derive(Clone, Copy, Debug)]
pub enum MediaAction {
    Previous,
    TogglePlayback,
    Next,
    VolumeDown,
    ToggleMute,
    VolumeUp,
}

impl MediaAction {
    const fn command(self) -> &'static str {
        match self {
            Self::Previous => "previous",
            Self::TogglePlayback => "toggle_playback",
            Self::Next => "next",
            Self::VolumeDown => "volume_down",
            Self::ToggleMute => "toggle_mute",
            Self::VolumeUp => "volume_up",
        }
    }
}

fn socket_path() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("tihulu-minimized-windows/media.sock")
}

fn snapshot_from_value(value: &Value) -> Option<MediaSnapshot> {
    if !value.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        return None;
    }
    Some(MediaSnapshot {
        player: value.get("player")?.as_str()?.to_owned(),
        title: value
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        artist: value
            .get("artist")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        playing: value
            .get("playing")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        position_us: value
            .get("position_us")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        length_us: value
            .get("length_us")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        volume: value
            .get("volume")
            .and_then(Value::as_f64)
            .unwrap_or(1.0)
            .clamp(0.0, 1.5),
        muted: value
            .get("muted")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

async fn request(value: Value) -> Option<MediaSnapshot> {
    let future = async move {
        let mut stream = UnixStream::connect(socket_path()).await.ok()?;
        let mut payload = serde_json::to_vec(&value).ok()?;
        payload.push(b'\n');
        stream.write_all(&payload).await.ok()?;
        stream.shutdown().await.ok()?;

        let mut reader = BufReader::new(stream).take(MAX_RESPONSE);
        let mut line = String::new();
        reader.read_line(&mut line).await.ok()?;
        let value = serde_json::from_str::<Value>(&line).ok()?;
        snapshot_from_value(&value)
    };
    timeout(SOCKET_TIMEOUT, future).await.ok().flatten()
}

pub async fn snapshot(
    app_id: String,
    app_label: String,
    window_titles: Vec<String>,
) -> Option<MediaSnapshot> {
    request(json!({
        "cmd": "snapshot",
        "app_id": app_id,
        "app_label": app_label,
        "window_titles": window_titles,
    }))
    .await
}

pub async fn command(
    action: MediaAction,
    player: String,
    app_id: String,
    app_label: String,
    window_titles: Vec<String>,
) -> Option<MediaSnapshot> {
    request(json!({
        "cmd": action.command(),
        "player": player,
        "app_id": app_id,
        "app_label": app_label,
        "window_titles": window_titles,
    }))
    .await
}
