// SPDX-License-Identifier: AGPL-3.0-only

use std::{fs, io, path::PathBuf};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RunMode {
    #[default]
    Safe,
    Enhanced,
}

impl RunMode {
    pub const fn is_safe(self) -> bool {
        matches!(self, Self::Safe)
    }

    pub const fn toggled(self) -> Self {
        match self {
            Self::Safe => Self::Enhanced,
            Self::Enhanced => Self::Safe,
        }
    }
}

fn config_dir() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(path).join("tihulu-cosmic-minimized-windows"));
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".config/tihulu-cosmic-minimized-windows"))
}

fn mode_path() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("mode"))
}

pub fn load_mode() -> RunMode {
    let Some(path) = mode_path() else {
        return RunMode::Safe;
    };
    match fs::read_to_string(path) {
        Ok(value) if value.trim().eq_ignore_ascii_case("enhanced") => RunMode::Enhanced,
        _ => RunMode::Safe,
    }
}

pub fn save_mode(mode: RunMode) -> io::Result<()> {
    let Some(dir) = config_dir() else {
        return Ok(());
    };
    fs::create_dir_all(&dir)?;
    let value = if mode.is_safe() {
        "safe\n"
    } else {
        "enhanced\n"
    };
    fs::write(dir.join("mode"), value)
}
