// SPDX-License-Identifier: AGPL-3.0-only

use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

const CONFIG_DIR: &str = "tihulu-cosmic-minimized-windows";
const CONFIG_FILE: &str = "config";
const CONFIG_VERSION: u32 = 2;

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
        // Older v0.4 RCs forcibly wrote mode=safe-core. Those files did not
        // carry config-version=2, so treat them as legacy and migrate to the
        // new normal/default Extended mode instead of preserving a forced state.
        if parse_config_version(contents) != Some(CONFIG_VERSION) {
            return Self::Extended;
        }

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
        "config-version={}\nmode={}\nhover-popups={}\n",
        CONFIG_VERSION,
        mode.as_config_value(),
        if hover_popups { "true" } else { "false" }
    );
    fs::write(&temporary, contents)?;
    fs::rename(temporary, path)
}

fn read_config() -> Option<String> {
    config_path().and_then(|path| fs::read_to_string(path).ok())
}

fn parse_config_version(contents: &str) -> Option<u32> {
    contents.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key.trim() == "config-version")
            .then(|| value.trim().parse::<u32>().ok())
            .flatten()
    })
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
    use super::{FeatureMode, parse_config_version, parse_hover_popups};

    #[test]
    fn legacy_configs_migrate_to_extended() {
        assert_eq!(FeatureMode::parse(""), FeatureMode::Extended);
        assert_eq!(
            FeatureMode::parse("mode=safe-core\n"),
            FeatureMode::Extended
        );
        assert_eq!(FeatureMode::parse("mode=extended\n"), FeatureMode::Extended);
    }

    #[test]
    fn versioned_config_persists_explicit_mode() {
        assert_eq!(
            FeatureMode::parse("config-version=2\nmode=safe-core\n"),
            FeatureMode::SafeCore
        );
        assert_eq!(
            FeatureMode::parse("config-version=2\nmode=extended\n"),
            FeatureMode::Extended
        );
        assert_eq!(
            FeatureMode::parse("config-version=2\nmode=unknown\n"),
            FeatureMode::Extended
        );
    }

    #[test]
    fn config_version_parser_is_strict() {
        assert_eq!(parse_config_version("config-version=2\n"), Some(2));
        assert_eq!(parse_config_version("config-version=1\n"), Some(1));
        assert_eq!(parse_config_version("config-version=nope\n"), None);
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
