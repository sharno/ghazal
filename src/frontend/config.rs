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
    /// Where emulators find BIOS files; systems' `requires_any` are relative to it.
    #[serde(default = "default_bios_dir")]
    pub bios_dir: PathBuf,
    /// Downloaded box art, one folder per libretro system name.
    pub thumbnail_cache: Option<PathBuf>,
    /// muOS's catalogue (`<system>/box/<game>.png`), reused when present.
    pub muos_catalogue: Option<PathBuf>,
    /// Menu options chosen on the device.
    #[serde(default = "default_settings_file")]
    pub settings_file: PathBuf,
    /// Socket of `ghazal daemon`; enables the Sleep/Restart/Power off menu entries.
    #[serde(default = "default_socket")]
    pub daemon_socket: PathBuf,
    #[serde(rename = "system", default)]
    pub systems: Vec<SystemEntry>,
}

fn default_socket() -> PathBuf {
    "/run/ghazal/daemon.sock".into()
}

fn default_settings_file() -> PathBuf {
    "/var/lib/ghazal/frontend-settings.toml".into()
}

fn default_bios_dir() -> PathBuf {
    "/mnt/games/BIOS".into()
}

fn default_activity_file() -> PathBuf {
    "/var/lib/ghazal/activity.toml".into()
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
    /// The system is hidden until one of these BIOS files exists, so it never
    /// shows games that can only fail to start.
    pub requires_any: Vec<PathBuf>,
    pub art: ArtSources,
}

/// Where a system's box art comes from.
#[derive(Debug, Deserialize, Clone, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ArtSources {
    /// libretro's thumbnail system name (download and cache folder).
    pub libretro: Option<String>,
    /// muOS catalogue system folder.
    pub muos: Option<String>,
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
    #[serde(default)]
    requires_any: Vec<PathBuf>,
    #[serde(default)]
    art: ArtSources,
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
        Ok(Self { name: raw.name, dirs: raw.dirs, extensions: raw.extensions, launch, requires_any: raw.requires_any, art: raw.art })
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
        fix(&mut self.bios_dir);
        fix(&mut self.settings_file);
        if let Some(cache) = &mut self.thumbnail_cache { fix(cache); }
        if let Some(catalogue) = &mut self.muos_catalogue { fix(catalogue); }
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
