// SPDX-License-Identifier: AGPL-3.0-only

use std::{path::Path, time::Duration};

use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};

use crate::preview_ipc::{
    PROTOCOL_VERSION, PreviewState, Request, Response, preview_dir, socket_path,
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_THUMBNAIL_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug)]
pub(crate) struct PreviewPayload {
    pub(crate) key: String,
    pub(crate) generation: u64,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) rgba: Vec<u8>,
}

pub(crate) async fn capture(key: String, identifier: String) -> Result<PreviewPayload, String> {
    let response = request(Request::Capture {
        version: PROTOCOL_VERSION,
        key,
        identifier,
    })
    .await?;
    payload_from_response(response).await
}

pub(crate) async fn gone(key: String) {
    let _ = request(Request::Gone {
        version: PROTOCOL_VERSION,
        key,
    })
    .await;
}

pub(crate) async fn clear() {
    let _ = request(Request::Clear {
        version: PROTOCOL_VERSION,
    })
    .await;
}

async fn request(request: Request) -> Result<Response, String> {
    tokio::time::timeout(REQUEST_TIMEOUT, request_inner(request))
        .await
        .map_err(|_| "previewd request timed out".to_owned())?
}

async fn request_inner(request: Request) -> Result<Response, String> {
    let version = request.version();
    if version != PROTOCOL_VERSION {
        return Err(format!("preview request protocol mismatch: {version}"));
    }

    let socket = socket_path().ok_or_else(|| "XDG_RUNTIME_DIR is unavailable".to_owned())?;
    let mut stream = UnixStream::connect(socket)
        .await
        .map_err(|error| format!("previewd unavailable: {error}"))?;
    let mut encoded = serde_json::to_vec(&request)
        .map_err(|error| format!("preview request encode failed: {error}"))?;
    encoded.push(b'\n');
    stream
        .write_all(&encoded)
        .await
        .map_err(|error| format!("preview request write failed: {error}"))?;
    stream
        .shutdown()
        .await
        .map_err(|error| format!("preview request shutdown failed: {error}"))?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .await
        .map_err(|error| format!("preview response read failed: {error}"))?;
    if line.is_empty() {
        return Err("previewd returned an empty response".to_owned());
    }
    serde_json::from_str(&line).map_err(|error| format!("preview response decode failed: {error}"))
}

fn expected_rgba_bytes(width: u32, height: u32) -> Option<usize> {
    usize::try_from(width)
        .ok()
        .and_then(|width| {
            usize::try_from(height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .and_then(|pixels| pixels.checked_mul(4))
}

async fn payload_from_response(response: Response) -> Result<PreviewPayload, String> {
    match response {
        Response::Thumbnail {
            version,
            key,
            generation,
            width,
            height,
            path,
        } => {
            if version != PROTOCOL_VERSION {
                return Err(format!("preview protocol mismatch: {version}"));
            }
            validate_thumbnail_path(&path)?;
            let expected = expected_rgba_bytes(width, height)
                .ok_or_else(|| "thumbnail dimensions overflow".to_owned())?;
            if expected == 0 || expected > MAX_THUMBNAIL_BYTES {
                return Err(format!("thumbnail byte size outside budget: {expected}"));
            }
            let rgba = tokio::fs::read(&path)
                .await
                .map_err(|error| format!("thumbnail read failed: {error}"))?;
            if rgba.len() != expected {
                return Err(format!(
                    "thumbnail size mismatch: expected {expected}, got {}",
                    rgba.len()
                ));
            }
            Ok(PreviewPayload {
                key,
                generation,
                width,
                height,
                rgba,
            })
        }
        Response::Status { state, reason, .. } => Err(match state {
            PreviewState::Ready => reason.unwrap_or_else(|| "preview is not ready yet".to_owned()),
            PreviewState::Degraded => reason.unwrap_or_else(|| "previewd is degraded".to_owned()),
            PreviewState::Disabled => reason.unwrap_or_else(|| "previewd is disabled".to_owned()),
        }),
        Response::Missing { key, .. } => Err(format!("preview missing for {key}")),
        Response::Error { message, .. } => Err(message),
        Response::Hello { .. } => Err("unexpected previewd hello response".to_owned()),
    }
}

fn validate_thumbnail_path(path: &Path) -> Result<(), String> {
    let root = preview_dir().ok_or_else(|| "preview cache root unavailable".to_owned())?;
    if !path.starts_with(&root) {
        return Err("previewd returned a path outside its runtime cache".to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{MAX_THUMBNAIL_BYTES, expected_rgba_bytes};

    #[test]
    fn rgba_byte_count_and_budget_are_bounded() {
        let bytes = expected_rgba_bytes(320, 180).unwrap();
        assert_eq!(bytes, 230_400);
        assert!(bytes < MAX_THUMBNAIL_BYTES);
        assert_eq!(expected_rgba_bytes(u32::MAX, u32::MAX), None);
    }
}
