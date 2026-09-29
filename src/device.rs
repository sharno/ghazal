//! device.toml: the hardware profile (muOS keeps this as one file per key under
//! /opt/muos/device/config). Everything board-specific lives here.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceConfig {
    pub name: String,
    #[serde(default = "default_fb")]
    pub framebuffer: PathBuf,
    #[serde(default)]
    pub input: InputDevices,
    /// Button name -> Linux evdev key code. Names are used by the frontend (a, b, x, y,
    /// l1, r1, l2, r2, select, start, menu, up, down, left, right) and by hotkeys.
    pub buttons: BTreeMap<String, u16>,
    #[serde(default)]
    pub backlight: Option<Backlight>,
    #[serde(default)]
    pub audio: Option<Audio>,
    #[serde(default)]
    pub battery: Option<Battery>,
    #[serde(default)]
    pub leds: Leds,
    #[serde(default)]
    pub rumble: Option<Rumble>,
    #[serde(default)]
    pub display: Display,
    #[serde(default)]
    pub rtc: Rtc,
    #[serde(default)]
    pub poweroff: Poweroff,
    pub input_bridge: Option<InputBridge>,
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct Display {
    /// fbdev blank file: "4" powers the panel down, "0" wakes it.
    pub blank: Option<PathBuf>,
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct Rtc {
    pub wakealarm: Option<PathBuf>,
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct Poweroff {
    pub fallback_write: Option<FileWrite>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct FileWrite {
    pub path: PathBuf,
    pub value: String,
}

/// Republishes a raw input device under another identity with remapped codes
/// (what muOS's `muinput` does).
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct InputBridge {
    /// Name of the raw source device.
    pub source: String,
    pub name: String,
    pub bus: u16,
    pub vendor: u16,
    pub product: u16,
    pub version: u16,
    /// Raw key code -> published code (TOML keys are strings).
    #[serde(default)]
    pub keys: BTreeMap<String, u16>,
    /// Raw axis -> published axis.
    #[serde(default)]
    pub axes: BTreeMap<String, u16>,
}

fn default_fb() -> PathBuf {
    "/dev/fb0".into()
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct InputDevices {
    /// Name (substring) of the gamepad evdev device. Autodetected when unset.
    pub gamepad: Option<String>,
    /// Exclusively grab the gamepad while the frontend is in the foreground.
    #[serde(default)]
    pub grab: bool,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Backlight {
    /// /sys/class/backlight/*/brightness style file.
    Sysfs { path: PathBuf, max: u32, #[serde(default)] min: u32 },
    /// Allwinner display debug interface (debugfs): name/command/param then start.
    Dispdbg {
        #[serde(default = "default_dispdbg")]
        dir: PathBuf,
        name: String,
        command: String,
        max: u32,
        #[serde(default)]
        min: u32,
    },
}

fn default_dispdbg() -> PathBuf {
    "/sys/kernel/debug/dispdbg".into()
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Audio {
    pub card: u32,
    /// ALSA mixer control that sets speaker/headphone volume.
    pub control: String,
    /// Percent of the control's range used as 100% (some codecs clip near the top).
    #[serde(default = "default_ceiling")]
    pub ceiling: u8,
}

fn default_ceiling() -> u8 {
    100
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Battery {
    /// power_supply directory with capacity/status/voltage_now.
    pub path: PathBuf,
    /// File that reads 1 when the charger is connected.
    pub charger_online: Option<PathBuf>,
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct Leds {
    /// Written "1"/"0" while the battery is low.
    pub low_battery: Option<PathBuf>,
    /// Written "1" while running normally, "0" before sleep/poweroff.
    pub power: Option<PathBuf>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Rumble {
    pub path: PathBuf,
    /// Value written to start vibrating (the motor is stopped with "0").
    #[serde(default = "default_rumble_on")]
    pub on: String,
}

fn default_rumble_on() -> String {
    "1".into()
}

impl DeviceConfig {
    pub fn button(&self, name: &str) -> Option<u16> {
        self.buttons.get(name).copied()
    }

    pub fn button_name(&self, code: u16) -> Option<&str> {
        self.buttons.iter().find(|(_, c)| **c == code).map(|(n, _)| n.as_str())
    }
}
