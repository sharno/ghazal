//! frontend.toml: where games live and how to launch them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct FrontendConfig {
    /// Directories that contain one sub-folder per system (e.g. `ROMS/GBA`).
    pub rom_roots: Vec<PathBuf>,
    #[serde(default = "default_activity_file")]
    pub activity_file: PathBuf,
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

fn default_activity_file() -> PathBuf {
    "/var/lib/oxmux/activity.toml".into()
}

#[derive(Debug, Deserialize, Clone)]
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

#[derive(Debug, Deserialize, Clone)]
#[serde(try_from = "RawSystemEntry")]
pub struct SystemEntry {
    pub name: String,
    /// Folder names under a ROM root, matched case-insensitively.
    pub dirs: Vec<String>,
    pub extensions: Vec<String>,
    pub launch: Launch,
}

/// How a system's games run.
#[derive(Debug, Clone, PartialEq)]
pub enum Launch {
    /// A libretro core file, run by RetroArch.
    Core(String),
    /// A standalone emulator, given by its `command`.
    Standalone(Standalone),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Standalone {
    /// Program and arguments; the argument `{rom}` becomes the game's path.
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// stdout/stderr are appended here. Inherited when unset.
    pub log: Option<PathBuf>,
}

/// The TOML shape: exactly one of `core` or `command`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSystemEntry {
    name: String,
    dirs: Vec<String>,
    extensions: Vec<String>,
    core: Option<String>,
    command: Option<Vec<String>>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    log: Option<PathBuf>,
}

impl TryFrom<RawSystemEntry> for SystemEntry {
    type Error = String;

    fn try_from(raw: RawSystemEntry) -> Result<Self, String> {
        let launch = match (raw.core, raw.command) {
            (Some(core), None) if raw.env.is_empty() && raw.log.is_none() => Launch::Core(core),
            (Some(_), None) => return Err(format!("system {:?}: env and log apply to a command; RetroArch uses [retroarch]", raw.name)),
            (None, Some(argv)) if argv.is_empty() => return Err(format!("system {:?} has an empty command", raw.name)),
            (None, Some(argv)) => Launch::Standalone(Standalone { argv, env: raw.env, log: raw.log }),
            (Some(_), Some(_)) => return Err(format!("system {:?} sets both core and command", raw.name)),
            (None, None) => return Err(format!("system {:?} needs a core or a command", raw.name)),
        };
        Ok(Self { name: raw.name, dirs: raw.dirs, extensions: raw.extensions, launch })
    }
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
        fix(&mut self.activity_file);
        self.retroarch.core_dirs.iter_mut().for_each(fix);
        self.retroarch.configs.iter_mut().for_each(fix);
        if let Some(log) = &mut self.retroarch.log {
            fix(log);
        }
        fix(&mut self.daemon_socket);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn system(fields: &str) -> Result<SystemEntry, toml::de::Error> {
        toml::from_str(&format!("name = \"PSP\"\ndirs = [\"PSP\"]\nextensions = [\"iso\"]\n{fields}"))
    }

    #[test]
    fn a_system_runs_exactly_one_core_or_command() {
        assert_eq!(system("core = \"ppsspp_libretro.so\"").unwrap().launch, Launch::Core("ppsspp_libretro.so".into()));
        let standalone = system("command = [\"/usr/bin/ppsspp\", \"{rom}\"]\nlog = \"/var/log/ppsspp.log\"").unwrap();
        assert!(matches!(standalone.launch, Launch::Standalone(Standalone { ref argv, log: Some(_), .. }) if argv.len() == 2));
        assert!(system("").is_err());
        assert!(system("core = \"a.so\"\ncommand = [\"b\"]").is_err());
        assert!(system("command = []").is_err());
        assert!(system("core = \"a.so\"\nlog = \"/x\"").is_err());
    }
}
