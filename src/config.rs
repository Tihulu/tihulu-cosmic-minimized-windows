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

    fn as_config(self) -> &'static str {
        // Do not persist an Extended request while the subsystem is unavailable.
        // This also repairs older v0.4 test configs containing mode=extended.
        "mode=safe-core\n"
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
    fn all_persisted_modes_are_forced_to_safe_core() {
        assert_eq!(FeatureMode::parse(""), FeatureMode::SafeCore);
        assert_eq!(FeatureMode::parse("mode=unknown\n"), FeatureMode::SafeCore);
        assert_eq!(FeatureMode::parse("mode=extended\n"), FeatureMode::SafeCore);
    }

    #[test]
    fn extended_request_is_not_effective_in_v04_rc() {
        assert!(FeatureMode::Extended.safe_core());
        assert_eq!(FeatureMode::Extended.as_config(), "mode=safe-core\n");
    }
}
