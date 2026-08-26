// SPDX-License-Identifier: AGPL-3.0-only

use std::time::Duration;

use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};

use crate::media_ipc::{
    MEDIA_PROTOCOL_VERSION, MediaAction, MediaPlayerState, MediaRequest, MediaResponse,
    media_socket_path,
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);

pub(crate) async fn status(app_hint: String) -> Result<Option<MediaPlayerState>, String> {
    match request(MediaRequest::Status {
        version: MEDIA_PROTOCOL_VERSION,
        app_hint,
    })
    .await?
    {
        MediaResponse::State { version, player } if version == MEDIA_PROTOCOL_VERSION => Ok(player),
        MediaResponse::State { version, .. } => Err(format!("media protocol mismatch: {version}")),
        MediaResponse::Error { message, .. } => Err(message),
    }
}

pub(crate) async fn control(bus_name: String, action: MediaAction) -> Result<(), String> {
    match request(MediaRequest::Control {
        version: MEDIA_PROTOCOL_VERSION,
        bus_name,
        action,
    })
    .await?
    {
        MediaResponse::State { version, .. } if version == MEDIA_PROTOCOL_VERSION => Ok(()),
        MediaResponse::State { version, .. } => Err(format!("media protocol mismatch: {version}")),
        MediaResponse::Error { message, .. } => Err(message),
    }
}

async fn request(request: MediaRequest) -> Result<MediaResponse, String> {
    tokio::time::timeout(REQUEST_TIMEOUT, request_inner(request))
        .await
        .map_err(|_| "tihulu-mediad request timed out".to_owned())?
}

async fn request_inner(request: MediaRequest) -> Result<MediaResponse, String> {
    if request.version() != MEDIA_PROTOCOL_VERSION {
        return Err(format!(
            "media request protocol mismatch: {}",
            request.version()
        ));
    }
    let socket = media_socket_path().ok_or_else(|| "XDG_RUNTIME_DIR is unavailable".to_owned())?;
    let mut stream = UnixStream::connect(socket)
        .await
        .map_err(|error| format!("tihulu-mediad unavailable: {error}"))?;
    let mut encoded = serde_json::to_vec(&request)
        .map_err(|error| format!("media request encode failed: {error}"))?;
    encoded.push(b'\n');
    stream
        .write_all(&encoded)
        .await
        .map_err(|error| format!("media request write failed: {error}"))?;
    stream
        .shutdown()
        .await
        .map_err(|error| format!("media request shutdown failed: {error}"))?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .await
        .map_err(|error| format!("media response read failed: {error}"))?;
    if line.is_empty() {
        return Err("tihulu-mediad returned an empty response".to_owned());
    }
    serde_json::from_str(&line).map_err(|error| format!("media response decode failed: {error}"))
}
