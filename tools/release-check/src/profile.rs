//! The parts of the shipped profile (`config/rg40xxv-rocknix`) the checks read.
//!
//! Only the fields the checks need are declared and unknown fields are ignored,
//! so features that extend the profile files never break the gates.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

/// `key = "value"` lines of a RetroArch configuration file.
#[derive(Debug, Default)]
pub struct Cfg(BTreeMap<String, String>);

impl Cfg {
    pub fn parse(text: &str) -> Self {
        Self(
            text.lines()
                .map(str::trim)
                .filter(|line| !line.is_empty() && !line.starts_with('#'))
                .filter_map(|line| line.split_once('='))
                .map(|(key, value)| (key.trim().to_owned(), value.trim().trim_matches('"').to_owned()))
                .collect(),
        )
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    pub fn require(&self, key: &str, file: &str) -> Result<&str> {
        self.get(key).with_context(|| format!("{file} does not set {key}"))
    }
}

/// How the handheld's pad identifies itself to the kernel. The vendor and
/// product come from the RetroArch profile; the bus and version are what
/// `/proc/bus/input/devices` shows for the real pad (BUS_HOST, 1.00).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pad {
    pub name: String,
    pub display_name: String,
    pub bus: u16,
    pub vendor: u16,
    pub product: u16,
    pub version: u16,
}

const BUS_HOST: u16 = 0x19;
const PAD_VERSION: u16 = 0x100;

impl Pad {
    fn from_cfg(gamepad: &Cfg) -> Result<Self> {
        let number = |key: &str| -> Result<u16> {
            gamepad.require(key, "gamepad.cfg")?.parse().with_context(|| format!("gamepad.cfg {key} is not a 16-bit number"))
        };
        let name = gamepad.require("input_device", "gamepad.cfg")?.to_owned();
        Ok(Self {
            display_name: gamepad.get("input_device_display_name").unwrap_or(&name).to_owned(),
            name,
            bus: BUS_HOST,
            vendor: number("input_vendor_id")?,
            product: number("input_product_id")?,
            version: PAD_VERSION,
        })
    }
}

#[derive(Debug, Deserialize)]
struct DeviceToml {
    input: DeviceInput,
    buttons: BTreeMap<String, u16>,
}

#[derive(Debug, Deserialize)]
struct DeviceInput {
    gamepad: String,
}

#[derive(Debug, Deserialize)]
pub struct RetroArch {
    pub bin: PathBuf,
    pub core_dirs: Vec<PathBuf>,
}

/// How a system's games run, as in frontend.toml: a libretro core or a command.
#[derive(Debug, Clone, PartialEq)]
pub enum Launch {
    Core(String),
    Command { argv: Vec<String>, env: BTreeMap<String, String> },
    /// A launch kind added after these checks were written: nothing here knows
    /// what to expect of it, so the checks leave it alone.
    Other,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SystemEntry {
    pub name: String,
    pub launch: Launch,
}

/// `[[system.alternative]]`: another emulator the menu offers for the system's games.
#[derive(Debug, Deserialize)]
struct RawAlternative {
    core: Option<String>,
    command: Option<Vec<String>>,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct RawSystem {
    name: String,
    core: Option<String>,
    command: Option<Vec<String>>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    alternative: Vec<RawAlternative>,
}

fn launch(system: &str, core: Option<String>, command: Option<Vec<String>>, env: BTreeMap<String, String>) -> Result<Launch, String> {
    match (core, command) {
        (Some(core), None) => Ok(Launch::Core(core)),
        (None, Some(argv)) if !argv.is_empty() => Ok(Launch::Command { argv, env }),
        (None, None) => Ok(Launch::Other),
        _ => Err(format!("system {system:?} sets both core and command, or an empty command")),
    }
}

impl RawSystem {
    /// The system's own emulator, then each alternative: every one is a launch
    /// the menu can start, so each must be present in the runtime.
    fn into_entries(self) -> Result<Vec<SystemEntry>, String> {
        let alternatives: Vec<_> = self.alternative.into_iter().map(|alternative| (alternative.core, alternative.command, alternative.env)).collect();
        std::iter::once((self.core, self.command, self.env))
            .chain(alternatives)
            .map(|(core, command, env)| launch(&self.name, core, command, env).map(|launch| SystemEntry { name: self.name.clone(), launch }))
            .collect()
    }
}

/// `[moonlight]`: present when the profile ships game streaming.
#[derive(Debug, Deserialize)]
pub struct Moonlight {
    pub bin: PathBuf,
    /// The SDL controller database Moonlight reads.
    pub mapping: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct FrontendToml {
    retroarch: RetroArch,
    moonlight: Option<Moonlight>,
    #[serde(rename = "system", default)]
    systems: Vec<RawSystem>,
}

#[derive(Debug, Deserialize, Default)]
struct SystemToml {
    #[serde(default)]
    power: Power,
    #[serde(default)]
    supervisor: Supervisor,
}

#[derive(Debug, Deserialize, Default)]
struct Power {
    #[serde(default)]
    before_suspend: Vec<Vec<String>>,
    #[serde(default)]
    after_resume: Vec<Vec<String>>,
}

#[derive(Debug, Deserialize, Default)]
struct Supervisor {
    #[serde(rename = "service", default)]
    services: Vec<Service>,
}

#[derive(Debug, Deserialize)]
struct Service {
    exec: Vec<String>,
}

#[derive(Debug)]
pub struct Profile {
    pub pad: Pad,
    /// device.toml `[input] gamepad`: the name the daemon and frontend look for.
    pub device_gamepad: String,
    /// Button name to evdev key code (device.toml `[buttons]`).
    pub buttons: BTreeMap<String, u16>,
    pub retroarch: RetroArch,
    pub moonlight: Option<Moonlight>,
    pub systems: Vec<SystemEntry>,
    /// Every command line the boot plan can run: service `exec` and power hooks.
    pub commands: Vec<Vec<String>>,
    /// The shipped RetroArch configuration, and the pad's profile as text.
    pub retroarch_cfg: Cfg,
    pub gamepad_cfg_text: String,
    pub gamepad_cfg: Cfg,
    pub dir: PathBuf,
}

impl Profile {
    pub fn load(dir: &Path) -> Result<Self> {
        let read = |name: &str| std::fs::read_to_string(dir.join(name)).with_context(|| format!("reading {}", dir.join(name).display()));
        let device: DeviceToml = toml::from_str(&read("device.toml")?).context("parsing device.toml")?;
        let frontend: FrontendToml = toml::from_str(&read("frontend.toml")?).context("parsing frontend.toml")?;
        let system: SystemToml = toml::from_str(&read("system.toml")?).context("parsing system.toml")?;
        let gamepad_cfg_text = read("gamepad.cfg")?;
        let gamepad_cfg = Cfg::parse(&gamepad_cfg_text);
        let systems: Vec<SystemEntry> = frontend
            .systems
            .into_iter()
            .map(|raw| raw.into_entries().map_err(anyhow::Error::msg))
            .collect::<Result<Vec<_>>>()
            .context("frontend.toml")?
            .into_iter()
            .flatten()
            .collect();
        if systems.is_empty() {
            bail!("frontend.toml lists no systems");
        }
        let commands = system
            .supervisor
            .services
            .into_iter()
            .map(|service| service.exec)
            .chain(system.power.before_suspend)
            .chain(system.power.after_resume)
            .filter(|argv| !argv.is_empty())
            .collect();
        Ok(Self {
            pad: Pad::from_cfg(&gamepad_cfg)?,
            device_gamepad: device.input.gamepad,
            buttons: device.buttons,
            retroarch: frontend.retroarch,
            moonlight: frontend.moonlight,
            systems,
            commands,
            retroarch_cfg: Cfg::parse(&read("retroarch.cfg")?),
            gamepad_cfg_text,
            gamepad_cfg,
            dir: dir.to_owned(),
        })
    }

    /// Every distinct libretro core the menu can launch, in profile order.
    pub fn cores(&self) -> Vec<&str> {
        let mut cores: Vec<&str> = Vec::new();
        for system in &self.systems {
            if let Launch::Core(core) = &system.launch {
                if !cores.contains(&core.as_str()) {
                    cores.push(core);
                }
            }
        }
        cores
    }

    /// The standalone emulator commands, with their systems.
    pub fn standalone(&self) -> impl Iterator<Item = (&str, &[String], &BTreeMap<String, String>)> {
        self.systems.iter().filter_map(|system| match &system.launch {
            Launch::Command { argv, env } => Some((system.name.as_str(), argv.as_slice(), env)),
            Launch::Core(_) | Launch::Other => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cfg_ignores_comments_and_unquotes() {
        let cfg = Cfg::parse("# note\n\nvideo_driver = \"gl\"\n  a=b \nbroken line\n");
        assert_eq!(cfg.get("video_driver"), Some("gl"));
        assert_eq!(cfg.get("a"), Some("b"));
        assert_eq!(cfg.get("broken line"), None);
    }

    /// The real profile must always load, whatever other features add to it.
    #[test]
    fn the_shipped_profile_loads() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/rg40xxv-rocknix");
        let profile = Profile::load(&dir).unwrap();
        assert_eq!(profile.pad.name, "H700 Gamepad");
        assert_eq!((profile.pad.vendor, profile.pad.product), (18507, 5343));
        assert_eq!(profile.device_gamepad, profile.pad.name);
        assert!(profile.cores().contains(&"gambatte_libretro.so"));
        assert!(profile.standalone().any(|(_, argv, _)| argv[0].ends_with("ppsspp")));
        assert!(profile.commands.iter().any(|argv| argv[0] == "/opt/ghazal/ghazal"));
        // Alternatives are launches the menu offers: the Snes9x 2010 core is one.
        assert!(profile.cores().contains(&"snes9x2010_libretro.so"));
        assert!(profile.moonlight.is_some());
    }

    #[test]
    fn a_system_runs_a_core_or_a_command() {
        let system = |fields: &str| toml::from_str::<RawSystem>(&format!("name = \"X\"\n{fields}")).map(RawSystem::into_entries);
        assert!(matches!(system("core = \"a.so\"").unwrap().unwrap().as_slice(), [SystemEntry { launch: Launch::Core(_), .. }]));
        assert!(matches!(system("command = [\"/usr/bin/x\"]").unwrap().unwrap().as_slice(), [SystemEntry { launch: Launch::Command { .. }, .. }]));
        // A launch kind this checker predates is skipped, not rejected.
        assert!(matches!(system("kind = \"stream\"").unwrap().unwrap().as_slice(), [SystemEntry { launch: Launch::Other, .. }]));
        assert!(system("core = \"a\"\ncommand = [\"b\"]").unwrap().is_err());
        assert!(system("command = []").unwrap().is_err());
    }

    #[test]
    fn alternatives_are_launches_too() {
        let raw: RawSystem = toml::from_str("name = \"X\"\ncore = \"a.so\"\n[[alternative]]\ncore = \"b.so\"\n[[alternative]]\ncommand = [\"/usr/bin/c\", \"{rom}\"]\n").unwrap();
        let launches: Vec<_> = raw.into_entries().unwrap().into_iter().map(|entry| entry.launch).collect();
        assert_eq!(launches.len(), 3);
        assert_eq!(launches[1], Launch::Core("b.so".into()));
        assert!(matches!(&launches[2], Launch::Command { argv, .. } if argv[0] == "/usr/bin/c"));
    }
}
