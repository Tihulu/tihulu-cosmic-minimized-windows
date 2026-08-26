// SPDX-License-Identifier: AGPL-3.0-only

use std::{env, path::PathBuf};

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;
pub const SOCKET_DIR: &str = "tihulu-cosmic-minimized-windows";
pub const SOCKET_FILE: &str = "previewd.sock";
pub const PREVIEW_DIR: &str = "previews";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Hello {
        version: u32,
    },
    Capture {
        version: u32,
        key: String,
        identifier: String,
    },
    Get {
        version: u32,
        key: String,
    },
    Gone {
        version: u32,
        key: String,
    },
    Clear {
        version: u32,
    },
    Status {
        version: u32,
    },
}

impl Request {
    pub fn version(&self) -> u32 {
        match self {
            Self::Hello { version }
            | Self::Capture { version, .. }
            | Self::Get { version, .. }
            | Self::Gone { version, .. }
            | Self::Clear { version }
            | Self::Status { version } => *version,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    Hello {
        version: u32,
    },
    Thumbnail {
        version: u32,
        key: String,
        generation: u64,
        width: u32,
        height: u32,
        path: PathBuf,
    },
    Missing {
        version: u32,
        key: String,
    },
    Status {
        version: u32,
        state: PreviewState,
        reason: Option<String>,
    },
    Error {
        version: u32,
        message: String,
    },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PreviewState {
    Ready,
    Degraded,
    Disabled,
}

pub fn runtime_root() -> Option<PathBuf> {
    env::var_os("XDG_RUNTIME_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(|root| root.join(SOCKET_DIR))
}

pub fn socket_path() -> Option<PathBuf> {
    runtime_root().map(|root| root.join(SOCKET_FILE))
}

pub fn preview_dir() -> Option<PathBuf> {
    runtime_root().map(|root| root.join(PREVIEW_DIR))
}

#[cfg(test)]
mod tests {
    use super::{PROTOCOL_VERSION, Request, Response};

    #[test]
    fn request_roundtrip_is_versioned() {
        let request = Request::Capture {
            version: PROTOCOL_VERSION,
            key: "stable-key".to_owned(),
            identifier: "window-id".to_owned(),
        };
        let json = serde_json::to_string(&request).unwrap();
        let decoded: Request = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.version(), PROTOCOL_VERSION);
    }

    #[test]
    fn thumbnail_response_roundtrip() {
        let response = Response::Thumbnail {
            version: PROTOCOL_VERSION,
            key: "stable-key".to_owned(),
            generation: 4,
            width: 320,
            height: 180,
            path: "/run/user/1000/test.rgba".into(),
        };
        let json = serde_json::to_string(&response).unwrap();
        let decoded: Response = serde_json::from_str(&json).unwrap();
        assert!(matches!(decoded, Response::Thumbnail { generation: 4, .. }));
    }
}
