// SPDX-License-Identifier: AGPL-3.0-only

use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

const CONFIG_DIR: &str = "tihulu-cosmic-minimized-windows";
const CONFIG_FILE: &str = "config";
const CONFIG_VERSION: u32 = 3;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum FeatureMode {
    SafeCore,
    #[default]
    Extended,
}

impl FeatureMode {
    pub(crate) fn safe_core(self) -> bool {
        matches!(self, Self::SafeCore)
    }

    fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "extended" => Some(Self::Extended),
            "safe-core" => Some(Self::SafeCore),
            _ => None,
        }
    }

    fn as_config_value(self) -> &'static str {
        match self {
            Self::SafeCore => "safe-core",
            Self::Extended => "extended",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Settings {
    pub(crate) mode: FeatureMode,
    pub(crate) media_enabled: bool,
    pub(crate) preview_enabled: bool,
    pub(crate) hover_popups: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            mode: FeatureMode::Extended,
            media_enabled: true,
            preview_enabled: true,
            hover_popups: false,
        }
    }
}

pub(crate) fn load_settings() -> Settings {
    read_config()
        .as_deref()
        .map(parse_settings)
        .unwrap_or_default()
}

pub(crate) fn save_settings(settings: Settings) -> io::Result<()> {
    let Some(path) = config_path() else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "neither XDG_CONFIG_HOME nor HOME is available",
        ));
    };
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let temporary = path.with_extension(format!("tmp.{}", std::process::id()));

    let contents = format!(
        "config-version={}\nmode={}\nmedia={}\npreview={}\nhover-popups={}\n",
        CONFIG_VERSION,
        settings.mode.as_config_value(),
        bool_value(settings.media_enabled),
        bool_value(settings.preview_enabled),
        bool_value(settings.hover_popups),
    );
    fs::write(&temporary, contents)?;
    fs::rename(temporary, path)
}

fn bool_value(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

fn read_config() -> Option<String> {
    config_path().and_then(|path| fs::read_to_string(path).ok())
}

fn parse_settings(contents: &str) -> Settings {
    // v0.4 RC configs before v3 contained forced Safe Core state and partial
    // hover-only settings. Migrate them to the new user-facing defaults once.
    if parse_config_version(contents) != Some(CONFIG_VERSION) {
        return Settings::default();
    }

    let mut settings = Settings::default();
    for line in contents.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "mode" => {
                if let Some(mode) = FeatureMode::parse(value) {
                    settings.mode = mode;
                }
            }
            "media" => {
                if let Some(enabled) = parse_bool(value) {
                    settings.media_enabled = enabled;
                }
            }
            "preview" => {
                if let Some(enabled) = parse_bool(value) {
                    settings.preview_enabled = enabled;
                }
            }
            "hover-popups" => {
                if let Some(enabled) = parse_bool(value) {
                    settings.hover_popups = enabled;
                }
            }
            _ => {}
        }
    }
    settings
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn parse_config_version(contents: &str) -> Option<u32> {
    contents.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key.trim() == "config-version")
            .then(|| value.trim().parse::<u32>().ok())
            .flatten()
    })
}

fn config_path() -> Option<PathBuf> {
    env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".config"))
        })
        .map(|base| base.join(CONFIG_DIR).join(CONFIG_FILE))
}

#[cfg(test)]
mod tests {
    use super::{FeatureMode, Settings, parse_config_version, parse_settings};

    #[test]
    fn defaults_match_normal_user_mode() {
        let settings = Settings::default();
        assert_eq!(settings.mode, FeatureMode::Extended);
        assert!(settings.media_enabled);
        assert!(settings.preview_enabled);
        assert!(!settings.hover_popups);
    }

    #[test]
    fn old_rc_configs_migrate_to_new_defaults() {
        assert_eq!(parse_settings(""), Settings::default());
        assert_eq!(
            parse_settings("config-version=2\nmode=safe-core\nhover-popups=true\n"),
            Settings::default()
        );
    }

    #[test]
    fn versioned_settings_persist_independently() {
        assert_eq!(
            parse_settings(
                "config-version=3\nmode=safe-core\nmedia=false\npreview=true\nhover-popups=true\n"
            ),
            Settings {
                mode: FeatureMode::SafeCore,
                media_enabled: false,
                preview_enabled: true,
                hover_popups: true,
            }
        );
    }

    #[test]
    fn invalid_values_keep_defaults() {
        assert_eq!(
            parse_settings(
                "config-version=3\nmode=nope\nmedia=nope\npreview=nope\nhover-popups=nope\n"
            ),
            Settings::default()
        );
    }

    #[test]
    fn config_version_parser_is_strict() {
        assert_eq!(parse_config_version("config-version=3\n"), Some(3));
        assert_eq!(parse_config_version("config-version=2\n"), Some(2));
        assert_eq!(parse_config_version("config-version=nope\n"), None);
    }
}
