// SPDX-License-Identifier: AGPL-3.0-only

use std::{env, path::PathBuf};

use serde::{Deserialize, Serialize};

pub(crate) const MEDIA_PROTOCOL_VERSION: u32 = 2;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub(crate) enum MediaRequest {
    Status {
        version: u32,
        app_hint: String,
    },
    Control {
        version: u32,
        bus_name: String,
        action: MediaAction,
    },
}

impl MediaRequest {
    pub(crate) fn version(&self) -> u32 {
        match self {
            Self::Status { version, .. } | Self::Control { version, .. } => *version,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum MediaAction {
    Previous,
    PlayPause,
    Next,
    VolumeDown,
    VolumeUp,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct MediaPlayerState {
    pub(crate) bus_name: String,
    pub(crate) identity: String,
    pub(crate) playback_status: String,
    pub(crate) title: String,
    pub(crate) artist: String,
    pub(crate) position_micros: i64,
    pub(crate) length_micros: Option<i64>,
    pub(crate) volume: Option<f64>,
    pub(crate) can_previous: bool,
    pub(crate) can_play_pause: bool,
    pub(crate) can_next: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub(crate) enum MediaResponse {
    State {
        version: u32,
        player: Option<MediaPlayerState>,
    },
    Error {
        version: u32,
        message: String,
    },
}

pub(crate) fn media_socket_path() -> Option<PathBuf> {
    env::var_os("XDG_RUNTIME_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(|root| root.join("tihulu-minimized-windows").join("media.sock"))
}
