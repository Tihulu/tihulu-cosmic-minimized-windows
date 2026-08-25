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
        matches!(self, Self::SafeCore)
    }

    fn parse(contents: &str) -> Self {
        contents
            .lines()
            .find_map(|line| line.trim().strip_prefix("mode="))
            .map(str::trim)
            .map(|mode| match mode {
                "extended" => Self::Extended,
                _ => Self::SafeCore,
            })
            .unwrap_or_default()
    }

    fn as_config(self) -> &'static str {
        match self {
            Self::SafeCore => "mode=safe-core\n",
            Self::Extended => "mode=extended\n",
        }
    }
}

pub(crate) fn load_feature_mode() -> FeatureMode {
    config_path()
        .and_then(|path| fs::read_to_string(path).ok())
        .map(|contents| FeatureMode::parse(&contents))
        .unwrap_or_default()
}

pub(crate) fn save_feature_mode(mode: FeatureMode) -> io::Result<()> {
    let Some(path) = config_path() else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "neither XDG_CONFIG_HOME nor HOME is available",
        ));
    };
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let temporary = path.with_extension(format!("tmp.{}", std::process::id()));
    fs::write(&temporary, mode.as_config())?;
    fs::rename(temporary, path)
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
    use super::FeatureMode;

    #[test]
    fn missing_or_unknown_mode_is_safe_core() {
        assert_eq!(FeatureMode::parse(""), FeatureMode::SafeCore);
        assert_eq!(FeatureMode::parse("mode=unknown\n"), FeatureMode::SafeCore);
    }

    #[test]
    fn extended_mode_is_explicit_request() {
        assert_eq!(FeatureMode::parse("mode=extended\n"), FeatureMode::Extended);
        assert!(!FeatureMode::Extended.safe_core());
        assert_eq!(FeatureMode::Extended.as_config(), "mode=extended\n");
    }
}
