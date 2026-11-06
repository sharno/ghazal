//! frontend.toml: where games live and how to launch them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::emulator::{CoreName, Emulator, Emulators, StandaloneName};
use super::netplay;
use super::retroarch_menu::MenuKey;
use crate::device::CpuLoad;
use crate::retroarch_cfg::Edits;

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct FrontendConfig {
    /// Directories that contain one sub-folder per system (e.g. `ROMS/GBA`).
    pub rom_roots: Vec<PathBuf>,
    #[serde(default = "default_activity_file")]
    pub activity_file: PathBuf,
    /// Emulator choices per game and system. Beside the activity file when unset.
    pub emulators_file: Option<PathBuf>,
    /// Time played per game. Beside the activity file when unset.
    pub playtime_file: Option<PathBuf>,
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
    /// Every emulator launch first waits (at most 10 s) for these paths, e.g.
    /// udev's database, which RetroArch and SDL need to find the gamepad.
    #[serde(default)]
    pub launch_after: Vec<PathBuf>,
    /// Game streaming; without it the menu says Moonlight is unavailable.
    #[serde(default)]
    pub moonlight: Option<Moonlight>,
    /// PortMaster support; absent where there is none (see docs/ports.md).
    #[serde(default)]
    pub ports: Option<crate::ports::Config>,
    #[serde(default)]
    pub netplay: Netplay,
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
    /// Writable user configuration, initialized from the first template once.
    #[serde(default)]
    pub user_config: Option<PathBuf>,
    /// Searched in order for the system's core file.
    pub core_dirs: Vec<PathBuf>,
    #[serde(default)]
    pub extra_args: Vec<String>,
    /// Extra environment for RetroArch (e.g. HOME, XDG_RUNTIME_DIR).
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// RetroArch stdout/stderr are appended here. Inherited when unset.
    pub log: Option<PathBuf>,
    /// RetroAchievements credentials given at provisioning, merged into
    /// `user_config` when RetroArch next starts (see retroarch_cfg::apply_seed).
    #[serde(default = "default_account_seed")]
    pub account_seed: PathBuf,
    /// Seconds a launch that uses the network (achievements, netplay) waits for
    /// Wi-Fi when a network is saved. Games never wait without one.
    #[serde(default = "default_online_wait")]
    pub online_wait: u64,
    /// RetroArch's network command port (`network_cmd_port` in retroarch.cfg).
    #[serde(default = "default_command_port")]
    pub command_port: u16,
    #[serde(default)]
    pub menu_paths: MenuPaths,
}

fn default_account_seed() -> PathBuf {
    crate::retroarch_cfg::ACCOUNT_SEED.into()
}

fn default_online_wait() -> u64 {
    8
}

fn default_command_port() -> u16 {
    55355
}

/// Key presses that open a page of RetroArch's menu (see retroarch_menu.rs).
#[derive(Debug, Deserialize, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct MenuPaths {
    /// The Achievements settings, where the RetroAchievements login is entered.
    #[serde(default)]
    pub achievements: Vec<MenuKey>,
}

/// Hosting and joining RetroArch netplay sessions.
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields, default)]
pub struct Netplay {
    /// The TCP port a host listens on.
    pub port: u16,
    /// How long looking for hosts on the LAN listens for answers.
    pub discovery_ms: u64,
    /// Hosts to offer besides those found: one `address[:port] [name]` per line.
    pub hosts_file: Option<PathBuf>,
    /// Written here for each netplay launch and passed to RetroArch as `--appendconfig`.
    pub append_config: PathBuf,
    /// RetroArch settings for netplay launches, applied over the user's config.
    pub retroarch: Edits,
}

impl Default for Netplay {
    fn default() -> Self {
        Self { port: netplay::DEFAULT_PORT, discovery_ms: 1500, hosts_file: None, append_config: "/run/ghazal/retroarch-netplay.cfg".into(), retroarch: Edits::default() }
    }
}

/// Moonlight, the GameStream/Sunshine client: `bin` runs the stream and the
/// pair, list, unpair and quit helpers.
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Moonlight {
    pub bin: PathBuf,
    /// Pairing keys, known hosts and stream settings. On the data card, so a
    /// reflashed OS card keeps its pairings.
    pub data_dir: PathBuf,
    /// SDL controller database, so the pad is a gamepad on the host.
    pub mapping: Option<PathBuf>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Output of streams is appended here.
    pub log: Option<PathBuf>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(try_from = "RawSystemEntry")]
pub struct SystemEntry {
    pub name: String,
    /// Folder names under a ROM root, matched case-insensitively.
    pub dirs: Vec<String>,
    pub extensions: Vec<String>,
    /// The system's own emulator first, then its `[[system.alternative]]`s.
    pub emulators: Emulators,
    /// The system is hidden until one of these BIOS files exists, so it never
    /// shows games that can only fail to start.
    pub requires_any: Vec<PathBuf>,
    pub art: ArtSources,
    /// How many folders below the system's own to look for games.
    pub depth: usize,
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
    Core(CoreName),
    /// A standalone emulator, given by its `command`.
    Standalone(Standalone),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Standalone {
    /// Tells this emulator from its system's others in saved choices.
    pub name: StandaloneName,
    /// Program and arguments; the argument `{rom}` becomes the game's path.
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// stdout/stderr are appended here. Inherited when unset.
    pub log: Option<PathBuf>,
}

/// The TOML shape of one emulator, exactly one of `core` or `command`: a
/// `[[system.alternative]]`, or the emulator a system declares inline. Like an
/// unrated system, an unrated emulator runs at full clock.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEmulator {
    core: Option<String>,
    command: Option<Vec<String>>,
    /// Identifies a standalone `command` in saved choices; its program's file name when unset.
    name: Option<String>,
    /// How the emulator picker names it; derived from the core or `name` when unset.
    label: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    log: Option<PathBuf>,
    #[serde(default)]
    cpu: CpuLoad,
}

impl RawEmulator {
    fn build(self, system: &str) -> Result<Emulator, String> {
        let launch = match (self.core, self.command) {
            (Some(core), None) if self.env.is_empty() && self.log.is_none() && self.name.is_none() => Launch::Core(core.into()),
            (Some(_), None) => return Err(format!("system {system:?}: env, log and name apply to a command; RetroArch uses [retroarch]")),
            (None, Some(argv)) if argv.is_empty() => return Err(format!("system {system:?} has an empty command")),
            (None, Some(argv)) => {
                let program = argv[0].rsplit('/').next().unwrap_or_default().to_owned();
                Launch::Standalone(Standalone { name: self.name.unwrap_or(program).into(), argv, env: self.env, log: self.log })
            }
            (Some(_), Some(_)) => return Err(format!("system {system:?} sets both core and command")),
            (None, None) => return Err(format!("system {system:?} needs a core or a command")),
        };
        Ok(Emulator { label: self.label.unwrap_or_else(|| launch.default_label()), launch, cpu: self.cpu })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSystemEntry {
    name: String,
    dirs: Vec<String>,
    extensions: Vec<String>,
    core: Option<String>,
    command: Option<Vec<String>>,
    /// The system's own standalone emulator name, as `name` in an alternative.
    emulator_name: Option<String>,
    label: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    log: Option<PathBuf>,
    #[serde(default)]
    cpu: CpuLoad,
    #[serde(default)]
    alternative: Vec<RawEmulator>,
    #[serde(default)]
    requires_any: Vec<PathBuf>,
    #[serde(default)]
    art: ArtSources,
    #[serde(default = "default_depth")]
    depth: usize,
}

/// Game folders nest (a disc image beside its tracks, a game per folder).
fn default_depth() -> usize { 3 }

impl TryFrom<RawSystemEntry> for SystemEntry {
    type Error = String;

    fn try_from(raw: RawSystemEntry) -> Result<Self, String> {
        let own = RawEmulator { core: raw.core, command: raw.command, name: raw.emulator_name, label: raw.label, env: raw.env, log: raw.log, cpu: raw.cpu };
        let default = own.build(&raw.name)?;
        let alternatives = raw.alternative.into_iter().map(|alternative| alternative.build(&raw.name)).collect::<Result<Vec<_>, _>>()?;
        let emulators = Emulators::new(default, alternatives).map_err(|duplicate| format!("system {:?}: {duplicate}", raw.name))?;
        Ok(Self { name: raw.name, dirs: raw.dirs, extensions: raw.extensions, emulators, requires_any: raw.requires_any, art: raw.art, depth: raw.depth })
    }
}

impl FrontendConfig {
    /// Where emulator choices are kept: beside the activity file, so they stay with the data card.
    pub fn emulators_file(&self) -> PathBuf {
        self.emulators_file.clone().unwrap_or_else(|| self.activity_file.with_file_name("emulators.toml"))
    }

    pub fn playtime_file(&self) -> PathBuf {
        self.playtime_file.clone().unwrap_or_else(|| self.activity_file.with_file_name("playtime.toml"))
    }

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
        if let Some(file) = &mut self.emulators_file { fix(file); }
        if let Some(file) = &mut self.playtime_file { fix(file); }
        fix(&mut self.bios_dir);
        fix(&mut self.settings_file);
        if let Some(cache) = &mut self.thumbnail_cache { fix(cache); }
        if let Some(catalogue) = &mut self.muos_catalogue { fix(catalogue); }
        self.retroarch.core_dirs.iter_mut().for_each(fix);
        self.retroarch.configs.iter_mut().for_each(fix);
        if let Some(config) = &mut self.retroarch.user_config { fix(config); }
        if let Some(log) = &mut self.retroarch.log {
            fix(log);
        }
        fix(&mut self.daemon_socket);
        if let Some(moonlight) = &mut self.moonlight {
            fix(&mut moonlight.data_dir);
            if let Some(mapping) = &mut moonlight.mapping { fix(mapping); }
            if let Some(log) = &mut moonlight.log { fix(log); }
        }
        fix(&mut self.retroarch.account_seed);
        if let Some(hosts) = &mut self.netplay.hosts_file { fix(hosts); }
        fix(&mut self.netplay.append_config);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::emulator::EmulatorId;

    fn system(fields: &str) -> Result<SystemEntry, toml::de::Error> {
        toml::from_str(&format!("name = \"PSP\"\ndirs = [\"PSP\"]\nextensions = [\"iso\"]\n{fields}"))
    }

    #[test]
    fn a_system_runs_exactly_one_core_or_command() {
        assert_eq!(system("core = \"ppsspp_libretro.so\"").unwrap().emulators.default().launch, Launch::Core("ppsspp_libretro.so".into()));
        let standalone = system("command = [\"/usr/bin/ppsspp\", \"{rom}\"]\nlog = \"/var/log/ppsspp.log\"").unwrap();
        assert!(matches!(standalone.emulators.default().launch, Launch::Standalone(Standalone { ref argv, log: Some(_), .. }) if argv.len() == 2));
        assert!(system("").is_err());
        assert!(system("core = \"a.so\"\ncommand = [\"b\"]").is_err());
        assert!(system("command = []").is_err());
        assert!(system("core = \"a.so\"\nlog = \"/x\"").is_err());
    }

    #[test]
    fn an_unrated_system_runs_at_full_clock() {
        assert_eq!(system("core = \"a.so\"").unwrap().emulators.default().cpu, CpuLoad::Full);
        assert_eq!(system("core = \"a.so\"\ncpu = \"light\"").unwrap().emulators.default().cpu, CpuLoad::Light);
        assert!(system("core = \"a.so\"\ncpu = \"fast\"").is_err());
    }

    #[test]
    fn alternatives_are_rated_and_named_separately() {
        let psp = system(r#"
            command = ["/usr/bin/ppsspp", "{rom}"]
            label = "PPSSPP (standalone)"
            [[alternative]]
            core = "ppsspp_libretro.so"
            label = "PPSSPP (libretro)"
            cpu = "moderate"
            [[alternative]]
            command = ["/opt/other/ppsspp-fork", "{rom}"]
        "#).unwrap().emulators;
        let all: Vec<_> = psp.iter().map(|emulator| (emulator.label.as_str(), emulator.id(), emulator.cpu)).collect();
        assert_eq!(all, [
            ("PPSSPP (standalone)", EmulatorId::Standalone("ppsspp".into()), CpuLoad::Full),
            ("PPSSPP (libretro)", EmulatorId::Core("ppsspp_libretro.so".into()), CpuLoad::Moderate),
            ("ppsspp-fork", EmulatorId::Standalone("ppsspp-fork".into()), CpuLoad::Full),
        ]);
        // A system without alternatives is unchanged.
        assert_eq!(system("core = \"snes9x_libretro.so\"").unwrap().emulators.len(), 1);
    }

    #[test]
    fn ambiguous_or_incomplete_alternatives_are_rejected() {
        let alternative = |fields: &str| system(&format!("core = \"a_libretro.so\"\n[[alternative]]\n{fields}"));
        assert!(alternative("core = \"a_libretro.so\"").unwrap_err().to_string().contains("listed twice"));
        assert!(alternative("").is_err());
        assert!(alternative("core = \"b.so\"\ncommand = [\"b\"]").is_err());
        assert!(alternative("core = \"b.so\"\nname = \"b\"").is_err());
        assert!(alternative("core = \"b.so\"\nlevel = 3").is_err());
        // Two standalone emulators need distinct names.
        let standalone = |second: &str| system(&format!("command = [\"/usr/bin/x\"]\n[[alternative]]\ncommand = [\"/other/x\"]\n{second}"));
        assert!(standalone("").is_err());
        assert!(standalone("name = \"x2\"").is_ok());
    }

    #[test]
    fn emulator_choices_live_beside_the_activity_file_unless_placed() {
        let config = |extra: &str| -> FrontendConfig { toml::from_str(&format!("rom_roots = []\nactivity_file = \"/card/.ghazal/activity.toml\"\n{extra}\n[retroarch]\nbin = \"retroarch\"\ncore_dirs = []")).unwrap() };
        assert_eq!(config("").emulators_file(), PathBuf::from("/card/.ghazal/emulators.toml"));
        assert_eq!(config("emulators_file = \"/elsewhere/choices.toml\"").emulators_file(), PathBuf::from("/elsewhere/choices.toml"));
    }

    /// The shipped system list must keep working with the shipped alternatives.
    #[test]
    fn shipped_alternatives_name_distinct_emulators() {
        let config: FrontendConfig = toml::from_str(crate::profile::DEFAULT_FRONTEND).unwrap();
        let psp = config.systems.iter().find(|system| system.name == "PSP").unwrap();
        assert!(psp.emulators.len() >= 2, "the PSP offers PPSSPP standalone and its libretro core");
        assert!(matches!(psp.emulators.default().launch, Launch::Standalone(_)));
        assert!(config.systems.iter().any(|system| system.emulators.len() > 1 && matches!(system.emulators.default().launch, Launch::Core(_))));
    }
}
