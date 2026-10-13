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
}

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
        Self { show_thumbnails: true, startup: StartupMode::Menu }
    }
}

impl Settings {
    /// Missing or unreadable settings fall back to defaults: options must
    /// never keep the menu from starting.
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|error| {
                eprintln!("oxmux: {}: {error}; using default options", path.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        crate::sys::write_atomic(path, toml::to_string(self)?.as_bytes()).with_context(|| format!("saving {}", path.display()))
    }
}
