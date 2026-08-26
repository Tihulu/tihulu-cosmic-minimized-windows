// SPDX-License-Identifier: AGPL-3.0-only

use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

const CONFIG_DIR: &str = "tihulu-cosmic-minimized-windows";
const CONFIG_FILE: &str = "config";

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

    fn parse(contents: &str) -> Self {
        contents
            .lines()
            .find_map(|line| {
                let (key, value) = line.split_once('=')?;
                if key.trim() != "mode" {
                    return None;
                }

                match value.trim() {
                    "extended" => Some(Self::Extended),
                    "safe-core" => Some(Self::SafeCore),
                    _ => None,
                }
            })
            .unwrap_or_default()
    }

    fn as_config_value(self) -> &'static str {
        match self {
            Self::SafeCore => "safe-core",
            Self::Extended => "extended",
        }
    }
}

pub(crate) fn load_feature_mode() -> FeatureMode {
    read_config()
        .map(|contents| FeatureMode::parse(&contents))
        .unwrap_or_default()
}

pub(crate) fn load_hover_popups() -> bool {
    read_config()
        .as_deref()
        .map(parse_hover_popups)
        .unwrap_or(false)
}

pub(crate) fn save_feature_mode(mode: FeatureMode) -> io::Result<()> {
    save_settings(mode, load_hover_popups())
}

pub(crate) fn save_settings(mode: FeatureMode, hover_popups: bool) -> io::Result<()> {
    let Some(path) = config_path() else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "neither XDG_CONFIG_HOME nor HOME is available",
        ));
    };
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let temporary = path.with_extension(format!("tmp.{}", std::process::id()));

    // Extended is the normal/default mode. Safe Core remains an explicit
    // fallback switch the user can enable at any time. Hover stays independent
    // and opt-in because hover-driven popup churn was unstable in real COSMIC tests.
    let contents = format!(
        "mode={}\nhover-popups={}\n",
        mode.as_config_value(),
        if hover_popups { "true" } else { "false" }
    );
    fs::write(&temporary, contents)?;
    fs::rename(temporary, path)
}

fn read_config() -> Option<String> {
    config_path().and_then(|path| fs::read_to_string(path).ok())
}

fn parse_hover_popups(contents: &str) -> bool {
    contents.lines().any(|line| {
        let Some((key, value)) = line.split_once('=') else {
            return false;
        };
        key.trim() == "hover-popups" && value.trim().eq_ignore_ascii_case("true")
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
    use super::{FeatureMode, parse_hover_popups};

    #[test]
    fn persisted_mode_defaults_to_extended_and_accepts_safe_core() {
        assert_eq!(FeatureMode::parse(""), FeatureMode::Extended);
        assert_eq!(FeatureMode::parse("mode=unknown\n"), FeatureMode::Extended);
        assert_eq!(FeatureMode::parse("mode=safe-core\n"), FeatureMode::SafeCore);
        assert_eq!(FeatureMode::parse("mode=extended\n"), FeatureMode::Extended);
    }

    #[test]
    fn feature_mode_reports_requested_safe_core_state() {
        assert!(FeatureMode::SafeCore.safe_core());
        assert!(!FeatureMode::Extended.safe_core());
        assert_eq!(FeatureMode::SafeCore.as_config_value(), "safe-core");
        assert_eq!(FeatureMode::Extended.as_config_value(), "extended");
    }

    #[test]
    fn hover_popups_default_off_and_require_explicit_true() {
        assert!(!parse_hover_popups(""));
        assert!(!parse_hover_popups("hover-popups=false\n"));
        assert!(!parse_hover_popups("hover-popups=1\n"));
        assert!(parse_hover_popups("mode=extended\nhover-popups=true\n"));
        assert!(parse_hover_popups("hover-popups=TRUE\n"));
    }
}
