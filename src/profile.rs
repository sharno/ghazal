//! Finds and parses the three config files: device.toml (hardware), frontend.toml
//! (games and emulators), system.toml (daemon, power, services, boot plan).
//!
//! Directory: `--config-dir`, else `$OXMUX_CONFIG_DIR`, else `etc/` next to the binary,
//! else the binary's own directory if it holds any of the files (muOS application
//! layout), else `/etc/oxmux`. A file missing from that directory falls back to the
//! built-in RG40XXV default.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;

use crate::device::DeviceConfig;
use crate::frontend::config::FrontendConfig;
use crate::system::SystemConfig;

pub const DEFAULT_DEVICE: &str = include_str!("../config/rg40xxv/device.toml");
pub const DEFAULT_FRONTEND: &str = include_str!("../config/rg40xxv/frontend.toml");
pub const DEFAULT_SYSTEM: &str = include_str!("../config/rg40xxv/system-app.toml");

const FILES: [&str; 3] = ["device.toml", "frontend.toml", "system.toml"];

pub struct Profile {
    pub dir: Option<PathBuf>,
}

impl Profile {
    pub fn locate(explicit: Option<&Path>) -> Self {
        if let Some(dir) = explicit.map(Path::to_path_buf).or_else(|| std::env::var_os("OXMUX_CONFIG_DIR").map(PathBuf::from)) {
            return Self { dir: Some(dir) };
        }
        let exe_dir = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf));
        let has_files = |d: &Path| FILES.iter().any(|f| d.join(f).is_file());
        let candidates = exe_dir.iter().flat_map(|d| [d.join("etc"), d.clone()]).chain([PathBuf::from("/etc/oxmux")]);
        Self { dir: candidates.into_iter().find(|d| has_files(d)) }
    }

    pub fn device(&self) -> Result<DeviceConfig> {
        self.load("device.toml", DEFAULT_DEVICE).map(|(c, _)| c)
    }

    pub fn frontend(&self) -> Result<FrontendConfig> {
        let (mut cfg, path): (FrontendConfig, _) = self.load("frontend.toml", DEFAULT_FRONTEND)?;
        if let Some(path) = path {
            cfg.resolve_relative_to(path.parent().unwrap_or(Path::new(".")));
        }
        Ok(cfg)
    }

    pub fn system(&self) -> Result<SystemConfig> {
        self.load("system.toml", DEFAULT_SYSTEM).map(|(c, _)| c)
    }

    fn load<T: DeserializeOwned>(&self, name: &str, default: &str) -> Result<(T, Option<PathBuf>)> {
        match self.dir.as_ref().map(|d| d.join(name)).filter(|p| p.is_file()) {
            Some(path) => {
                let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
                let cfg = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
                Ok((cfg, Some(path)))
            }
            None => Ok((toml::from_str(default).with_context(|| format!("parsing built-in {name}"))?, None)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every shipped config must parse, so a typo can't brick a device at boot.
    #[test]
    fn shipped_configs_parse() {
        let p = Profile { dir: None };
        p.device().unwrap();
        p.frontend().unwrap();
        p.system().unwrap();
        for dir in ["config/rg40xxv", "dev/etc"] {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                let name = path.file_name().unwrap().to_string_lossy().to_string();
                let text = std::fs::read_to_string(&path).unwrap();
                let result: Result<(), toml::de::Error> = if name.starts_with("device") {
                    toml::from_str::<DeviceConfig>(&text).map(drop)
                } else if name.starts_with("frontend") {
                    toml::from_str::<FrontendConfig>(&text).map(drop)
                } else if name.starts_with("system") {
                    toml::from_str::<SystemConfig>(&text).map(drop)
                } else {
                    continue;
                };
                result.unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            }
        }
    }
}
