// SPDX-License-Identifier: AGPL-3.0-only

use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

const CONFIG_DIR: &str = "tihulu-cosmic-minimized-windows";
const CONFIG_FILE: &str = "config";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum FeatureMode {
    #[default]
    SafeCore,
    Extended,
}

impl FeatureMode {
    pub(crate) fn safe_core(self) -> bool {
        true
    }

    fn parse(_contents: &str) -> Self {
        // v0.4 is a runtime-validation release candidate. Extended mode stays
        // locked until the external preview/media daemons pass their safety gates.
        Self::SafeCore
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

    // Extended remains locked for the v0.4 RC. Hover is independent and is
    // deliberately opt-in because real COSMIC testing has shown hover-driven
    // popup churn can restart cosmic-panel on some systems.
    let _ = mode;
    let contents = format!(
        "mode=safe-core\nhover-popups={}\n",
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
    fn all_persisted_modes_are_forced_to_safe_core() {
        assert_eq!(FeatureMode::parse(""), FeatureMode::SafeCore);
        assert_eq!(FeatureMode::parse("mode=unknown\n"), FeatureMode::SafeCore);
        assert_eq!(FeatureMode::parse("mode=extended\n"), FeatureMode::SafeCore);
    }

    #[test]
    fn extended_request_is_not_effective_in_v04_rc() {
        assert!(FeatureMode::Extended.safe_core());
    }

    #[test]
    fn hover_popups_default_off_and_require_explicit_true() {
        assert!(!parse_hover_popups(""));
        assert!(!parse_hover_popups("hover-popups=false\n"));
        assert!(!parse_hover_popups("hover-popups=1\n"));
        assert!(parse_hover_popups("mode=safe-core\nhover-popups=true\n"));
        assert!(parse_hover_popups("hover-popups=TRUE\n"));
    }
}
