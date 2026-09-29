use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

/// Built-in config used when no `--config` is given and no `oxmux.toml` sits next to the binary.
const DEFAULT_CONFIG: &str = include_str!("../config/muos.toml");

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Directories that contain one sub-folder per system (e.g. `ROMS/GBA`).
    pub rom_roots: Vec<PathBuf>,
    #[serde(default = "default_framebuffer")]
    pub framebuffer: PathBuf,
    pub retroarch: RetroArch,
    #[serde(default)]
    pub input: InputConfig,
    #[serde(rename = "system", default)]
    pub systems: Vec<SystemConfig>,
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
    /// RetroArch stdout/stderr are appended here. Inherited when unset.
    pub log: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemConfig {
    pub name: String,
    /// Folder names under a ROM root, matched case-insensitively.
    pub dirs: Vec<String>,
    pub extensions: Vec<String>,
    pub core: String,
}

/// Linux evdev key codes for each button. Check them on the device with `oxmux --probe-input`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct InputConfig {
    /// Only use input devices whose name contains this string. Autodetects gamepads when unset.
    pub device_name: Option<String>,
    pub a: u16,
    pub b: u16,
    pub x: u16,
    pub y: u16,
    pub l1: u16,
    pub r1: u16,
    pub l2: u16,
    pub r2: u16,
    pub select: u16,
    pub start: u16,
    pub menu: u16,
    pub up: u16,
    pub down: u16,
    pub left: u16,
    pub right: u16,
}

impl Default for InputConfig {
    fn default() -> Self {
        // Standard Linux gamepad codes (input-event-codes.h), Nintendo-style face layout.
        Self {
            device_name: None,
            a: 305,      // BTN_EAST
            b: 304,      // BTN_SOUTH
            x: 307,      // BTN_NORTH
            y: 308,      // BTN_WEST
            l1: 310,     // BTN_TL
            r1: 311,     // BTN_TR
            l2: 312,     // BTN_TL2
            r2: 313,     // BTN_TR2
            select: 314, // BTN_SELECT
            start: 315,  // BTN_START
            menu: 316,   // BTN_MODE
            up: 544,     // BTN_DPAD_UP
            down: 545,   // BTN_DPAD_DOWN
            left: 546,   // BTN_DPAD_LEFT
            right: 547,  // BTN_DPAD_RIGHT
        }
    }
}

fn default_framebuffer() -> PathBuf {
    "/dev/fb0".into()
}

impl Config {
    pub fn load(explicit: Option<&Path>) -> Result<Self> {
        let beside_exe = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join("oxmux.toml")))
            .filter(|p| p.is_file());

        match explicit.map(Path::to_path_buf).or(beside_exe) {
            Some(path) => {
                let text = std::fs::read_to_string(&path)
                    .with_context(|| format!("reading {}", path.display()))?;
                let mut config: Config =
                    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
                config.resolve_relative_to(path.parent().unwrap_or(Path::new(".")));
                eprintln!("oxmux: loaded config {}", path.display());
                Ok(config)
            }
            None => toml::from_str(DEFAULT_CONFIG).context("parsing built-in config"),
        }
    }

    /// Relative paths in a config file are relative to that file, not the working directory.
    /// `retroarch.bin` is left alone so a bare name can be looked up on `PATH`.
    fn resolve_relative_to(&mut self, base: &Path) {
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
    }
}
