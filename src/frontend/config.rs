//! frontend.toml: where games live and how to launch them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrontendConfig {
    /// Directories that contain one sub-folder per system (e.g. `ROMS/GBA`).
    pub rom_roots: Vec<PathBuf>,
    pub retroarch: RetroArch,
    /// Socket of `oxmux daemon`; enables the Sleep/Restart/Power off menu entries.
    #[serde(default = "default_socket")]
    pub daemon_socket: PathBuf,
    #[serde(rename = "system", default)]
    pub systems: Vec<SystemEntry>,
}

fn default_socket() -> PathBuf {
    "/run/oxmux/daemon.sock".into()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetroArch {
    pub bin: PathBuf,
    /// First existing file wins. Empty means RetroArch picks its own default.
    #[serde(default)]
    pub configs: Vec<PathBuf>,
    /// Searched in order for the system's core file.
    pub core_dirs: Vec<PathBuf>,
    #[serde(default)]
    pub extra_args: Vec<String>,
    /// Extra environment for RetroArch (e.g. HOME, XDG_RUNTIME_DIR).
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// RetroArch stdout/stderr are appended here. Inherited when unset.
    pub log: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemEntry {
    pub name: String,
    /// Folder names under a ROM root, matched case-insensitively.
    pub dirs: Vec<String>,
    pub extensions: Vec<String>,
    pub core: String,
}

impl FrontendConfig {
    /// Relative paths in a config file are relative to that file, not the working directory.
    /// `retroarch.bin` is left alone so a bare name can be looked up on `PATH`.
    pub fn resolve_relative_to(&mut self, base: &Path) {
        let fix = |p: &mut PathBuf| {
            if p.is_relative() {
                *p = base.join(&*p);
            }
        };
        self.rom_roots.iter_mut().for_each(fix);
        self.retroarch.core_dirs.iter_mut().for_each(fix);
        self.retroarch.configs.iter_mut().for_each(fix);
        if let Some(log) = &mut self.retroarch.log {
            fix(log);
        }
        fix(&mut self.daemon_socket);
    }
}
