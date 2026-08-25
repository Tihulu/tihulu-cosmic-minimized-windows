// SPDX-License-Identifier: AGPL-3.0-only

mod capture;
mod metrics;
#[path = "../../preview_ipc.rs"]
mod preview_ipc;

use std::{
    collections::HashMap,
    fs,
    hash::{DefaultHasher, Hash, Hasher},
    io::{BufRead, BufReader, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    time::Duration,
};

use capture::{CaptureWayland, CapturedFrame};
use metrics::{GrowthWatch, find_cosmic_comp, process_metrics};
use preview_ipc::{
    PROTOCOL_VERSION, PreviewState, Request, Response, preview_dir, runtime_root, socket_path,
};

const MAX_THUMBNAILS: usize = 16;
const MAX_CACHE_BYTES: usize = 8 * 1024 * 1024;
const TARGET_WIDTH: u32 = 320;
const TARGET_HEIGHT: u32 = 180;
const MAX_REQUEST_BYTES: u64 = 64 * 1024;
const RSS_BREAKER_KB: u64 = 128 * 1024;

#[derive(Clone, Debug)]
struct CacheEntry {
    key: String,
    generation: u64,
    width: u32,
    height: u32,
    bytes: usize,
    path: PathBuf,
    last_used: u64,
}

struct PreviewCache {
    dir: PathBuf,
    entries: HashMap<String, CacheEntry>,
    bytes: usize,
    tick: u64,
    generation: u64,
}

impl PreviewCache {
    fn new(dir: PathBuf) -> std::io::Result<Self> {
        if dir.exists() {
            fs::remove_dir_all(&dir)?;
        }
        fs::create_dir_all(&dir)?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        Ok(Self {
            dir,
            entries: HashMap::new(),
            bytes: 0,
            tick: 0,
            generation: 0,
        })
    }

    fn insert(
        &mut self,
        key: String,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> Result<CacheEntry, String> {
        if rgba.len() > MAX_CACHE_BYTES {
            return Err(format!(
                "thumbnail is too large for cache budget: {} bytes",
                rgba.len()
            ));
        }
        self.remove(&key);
        while self.entries.len() >= MAX_THUMBNAILS
            || self.bytes.saturating_add(rgba.len()) > MAX_CACHE_BYTES
        {
            let Some(oldest) = self
                .entries
                .values()
                .min_by_key(|entry| entry.last_used)
                .map(|entry| entry.key.clone())
            else {
                break;
            };
            self.remove(&oldest);
        }

        self.tick = self.tick.wrapping_add(1);
        self.generation = self.generation.wrapping_add(1).max(1);
        let path = self.dir.join(format!("{:016x}.rgba", hash_key(&key)));
        let temporary = path.with_extension(format!("tmp.{}", std::process::id()));
        fs::write(&temporary, rgba).map_err(|error| format!("thumbnail write failed: {error}"))?;
        fs::rename(&temporary, &path)
            .map_err(|error| format!("thumbnail publish failed: {error}"))?;
        let entry = CacheEntry {
            key: key.clone(),
            generation: self.generation,
            width,
            height,
            bytes: rgba.len(),
            path,
            last_used: self.tick,
        };
        self.bytes = self.bytes.saturating_add(entry.bytes);
        self.entries.insert(key, entry.clone());
        Ok(entry)
    }

    fn get(&mut self, key: &str) -> Option<CacheEntry> {
        self.tick = self.tick.wrapping_add(1);
        let entry = self.entries.get_mut(key)?;
        entry.last_used = self.tick;
        Some(entry.clone())
    }

    fn remove(&mut self, key: &str) {
        if let Some(entry) = self.entries.remove(key) {
            self.bytes = self.bytes.saturating_sub(entry.bytes);
            let _ = fs::remove_file(entry.path);
        }
    }

    fn clear(&mut self) {
        let keys = self.entries.keys().cloned().collect::<Vec<_>>();
        for key in keys {
            self.remove(&key);
        }
    }
}

struct Daemon {
    capture: CaptureWayland,
    cache: PreviewCache,
    state: PreviewState,
    reason: Option<String>,
    growth: GrowthWatch,
    comp_pid: Option<u32>,
    rss_baseline_kb: u64,
}

impl Daemon {
    fn new(cache_dir: PathBuf) -> Result<Self, String> {
        let capture = CaptureWayland::connect()?;
        let cache =
            PreviewCache::new(cache_dir).map_err(|error| format!("cache init failed: {error}"))?;
        let comp_pid = find_cosmic_comp();
        let rss_baseline_kb = process_metrics(std::process::id()).rss_kb;
        Ok(Self {
            capture,
            cache,
            state: PreviewState::Ready,
            reason: None,
            growth: GrowthWatch::default(),
            comp_pid,
            rss_baseline_kb,
        })
    }

    fn process(&mut self, request: Request) -> Response {
        if request.version() != PROTOCOL_VERSION {
            return Response::Error {
                version: PROTOCOL_VERSION,
                message: format!(
                    "protocol mismatch: client={} daemon={}",
                    request.version(),
                    PROTOCOL_VERSION
                ),
            };
        }

        match request {
            Request::Hello { .. } => Response::Hello {
                version: PROTOCOL_VERSION,
            },
            Request::Status { .. } => self.status(),
            Request::Clear { .. } => {
                self.cache.clear();
                self.status()
            }
            Request::Gone { key, .. } => {
                self.cache.remove(&key);
                self.status()
            }
            Request::Get { key, .. } => self.cached_response(&key),
            Request::Capture {
                key, identifier, ..
            } => self.capture(&key, &identifier),
        }
    }

    fn capture(&mut self, key: &str, identifier: &str) -> Response {
        if self.state != PreviewState::Ready {
            return self.status();
        }

        let frame = match self.capture.capture_identifier(identifier) {
            Ok(frame) => frame,
            Err(error) => {
                return Response::Error {
                    version: PROTOCOL_VERSION,
                    message: error,
                };
            }
        };
        let thumb = resize_fit(frame, TARGET_WIDTH, TARGET_HEIGHT);
        let entry = match self
            .cache
            .insert(key.to_owned(), thumb.width, thumb.height, &thumb.rgba)
        {
            Ok(entry) => entry,
            Err(error) => {
                self.degrade(error);
                return self.status();
            }
        };

        let daemon = process_metrics(std::process::id());
        let compositor = self.comp_pid.map(process_metrics);
        if let Some(reason) = self.growth.push(daemon, compositor) {
            self.degrade(reason.to_owned());
            return self.status();
        }
        if daemon.rss_kb > self.rss_baseline_kb.saturating_add(RSS_BREAKER_KB) {
            self.degrade(format!(
                "previewd RSS exceeded baseline by more than {} MiB",
                RSS_BREAKER_KB / 1024
            ));
            return self.status();
        }

        thumbnail_response(entry)
    }

    fn cached_response(&mut self, key: &str) -> Response {
        self.cache
            .get(key)
            .map(thumbnail_response)
            .unwrap_or_else(|| Response::Missing {
                version: PROTOCOL_VERSION,
                key: key.to_owned(),
            })
    }

    fn degrade(&mut self, reason: String) {
        self.cache.clear();
        self.state = PreviewState::Degraded;
        self.reason = Some(reason);
    }

    fn status(&self) -> Response {
        Response::Status {
            version: PROTOCOL_VERSION,
            state: self.state,
            reason: self.reason.clone(),
        }
    }
}

fn thumbnail_response(entry: CacheEntry) -> Response {
    Response::Thumbnail {
        version: PROTOCOL_VERSION,
        key: entry.key,
        generation: entry.generation,
        width: entry.width,
        height: entry.height,
        path: entry.path,
    }
}

fn hash_key(key: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish()
}

fn resize_fit(frame: CapturedFrame, max_width: u32, max_height: u32) -> CapturedFrame {
    if frame.width == 0 || frame.height == 0 || max_width == 0 || max_height == 0 {
        return frame;
    }
    let width_limited = u64::from(max_width) * u64::from(frame.height)
        <= u64::from(max_height) * u64::from(frame.width);
    let (target_width, target_height) = if frame.width <= max_width && frame.height <= max_height {
        (frame.width, frame.height)
    } else if width_limited {
        let height =
            (u64::from(frame.height) * u64::from(max_width) / u64::from(frame.width)).max(1);
        (
            max_width,
            u32::try_from(height).unwrap_or(max_height).min(max_height),
        )
    } else {
        let width =
            (u64::from(frame.width) * u64::from(max_height) / u64::from(frame.height)).max(1);
        (
            u32::try_from(width).unwrap_or(max_width).min(max_width),
            max_height,
        )
    };

    if target_width == frame.width && target_height == frame.height {
        return frame;
    }

    let mut rgba = vec![0_u8; target_width as usize * target_height as usize * 4];
    for y in 0..target_height {
        let source_y = u64::from(y) * u64::from(frame.height) / u64::from(target_height);
        for x in 0..target_width {
            let source_x = u64::from(x) * u64::from(frame.width) / u64::from(target_width);
            let source = ((source_y * u64::from(frame.width) + source_x) * 4) as usize;
            let target = ((u64::from(y) * u64::from(target_width) + u64::from(x)) * 4) as usize;
            rgba[target..target + 4].copy_from_slice(&frame.rgba[source..source + 4]);
        }
    }
    CapturedFrame {
        width: target_width,
        height: target_height,
        rgba,
    }
}

fn read_request(stream: &UnixStream) -> Result<Request, String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|error| format!("set read timeout failed: {error}"))?;
    let clone = stream
        .try_clone()
        .map_err(|error| format!("socket clone failed: {error}"))?;
    let mut reader = BufReader::new(clone).take(MAX_REQUEST_BYTES);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|error| format!("socket read failed: {error}"))?;
    if line.is_empty() {
        return Err("empty request".to_owned());
    }
    if !line.ends_with('\n') {
        return Err("request exceeded frame limit or lacked newline terminator".to_owned());
    }
    serde_json::from_str(line.trim_end()).map_err(|error| format!("invalid request: {error}"))
}

fn write_response(mut stream: &UnixStream, response: &Response) -> Result<(), String> {
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .map_err(|error| format!("set write timeout failed: {error}"))?;
    let mut encoded =
        serde_json::to_vec(response).map_err(|error| format!("response encode failed: {error}"))?;
    encoded.push(b'\n');
    stream
        .write_all(&encoded)
        .map_err(|error| format!("socket write failed: {error}"))
}

fn prepare_socket(path: &Path) -> Result<UnixListener, String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("runtime dir create failed: {error}"))?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("runtime dir permissions failed: {error}"))?;
    }
    if path.exists() {
        fs::remove_file(path).map_err(|error| format!("stale socket removal failed: {error}"))?;
    }
    let listener =
        UnixListener::bind(path).map_err(|error| format!("socket bind failed: {error}"))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("socket permissions failed: {error}"))?;
    Ok(listener)
}

fn run() -> Result<(), String> {
    let root = runtime_root().ok_or_else(|| "XDG_RUNTIME_DIR is not available".to_owned())?;
    let socket = socket_path().ok_or_else(|| "preview socket path unavailable".to_owned())?;
    let previews = preview_dir().ok_or_else(|| "preview cache path unavailable".to_owned())?;
    fs::create_dir_all(&root).map_err(|error| format!("runtime root create failed: {error}"))?;
    let listener = prepare_socket(&socket)?;
    let mut daemon = Daemon::new(previews)?;

    eprintln!("tihulu-previewd ready: {}", socket.display());
    for incoming in listener.incoming() {
        let stream = match incoming {
            Ok(stream) => stream,
            Err(error) => {
                eprintln!("previewd accept failed: {error}");
                continue;
            }
        };
        let response = match read_request(&stream) {
            Ok(request) => daemon.process(request),
            Err(error) => Response::Error {
                version: PROTOCOL_VERSION,
                message: error,
            },
        };
        if let Err(error) = write_response(&stream, &response) {
            eprintln!("previewd response failed: {error}");
        }
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("tihulu-previewd: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::{CapturedFrame, resize_fit};

    #[test]
    fn resize_keeps_widescreen_inside_320x180() {
        let frame = CapturedFrame {
            width: 1920,
            height: 1080,
            rgba: vec![255; 1920 * 1080 * 4],
        };
        let thumb = resize_fit(frame, 320, 180);
        assert_eq!((thumb.width, thumb.height), (320, 180));
        assert_eq!(thumb.rgba.len(), 320 * 180 * 4);
    }

    #[test]
    fn resize_keeps_portrait_aspect_ratio() {
        let frame = CapturedFrame {
            width: 900,
            height: 1600,
            rgba: vec![255; 900 * 1600 * 4],
        };
        let thumb = resize_fit(frame, 320, 180);
        assert_eq!(thumb.height, 180);
        assert!(thumb.width < 180);
    }
}
