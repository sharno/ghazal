//! Options chosen in the menu, kept on the OS card.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    /// Show the selected game's box art beside game lists.
    pub show_thumbnails: bool,
    pub startup: StartupMode,
    /// Left out of the file while it is the default, so a previous OS slot (which
    /// rejects unknown keys and would then drop every setting) still reads it.
    #[serde(skip_serializing_if = "is_default_policy")]
    pub hdmi: crate::hdmi::Policy,
}

fn is_default_policy(policy: &crate::hdmi::Policy) -> bool { *policy == crate::hdmi::Policy::default() }

#[derive(Debug, Default, Clone, Copy, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum StartupMode {
    #[default]
    Menu,
    LastGame,
}

impl StartupMode {
    pub fn next(self) -> Self {
        match self { Self::Menu => Self::LastGame, Self::LastGame => Self::Menu }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self { show_thumbnails: true, startup: StartupMode::Menu, hdmi: crate::hdmi::Policy::default() }
    }
}

impl Settings {
    /// Missing or unreadable settings fall back to defaults: options must
    /// never keep the menu from starting.
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|error| {
                eprintln!("ghazal: {}: {error}; using default options", path.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        crate::sys::write_atomic(path, toml::to_string(self)?.as_bytes()).with_context(|| format!("saving {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_hdmi_policy_stays_out_of_the_file() {
        let text = toml::to_string(&Settings::default()).unwrap();
        assert!(!text.contains("hdmi"), "{text}");
        let chosen = Settings { hdmi: crate::hdmi::Policy::Mirror, ..Settings::default() };
        let text = toml::to_string(&chosen).unwrap();
        assert!(text.contains("hdmi = \"mirror\""), "{text}");
        assert_eq!(toml::from_str::<Settings>(&text).unwrap(), chosen);
    }

    #[test]
    fn settings_written_before_hdmi_existed_still_load() {
        let old: Settings = toml::from_str("show_thumbnails = false\nstartup = \"last-game\"\n").unwrap();
        assert_eq!(old.hdmi, crate::hdmi::Policy::Auto);
        assert_eq!(old.startup, StartupMode::LastGame);
    }
}
