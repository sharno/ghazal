//! system.toml: everything muOS spreads across shell scripts and its GET_VAR/SET_VAR
//! store, as one declarative file: daemon policy (hotkeys, idle, battery), how to power
//! off, which services to supervise, and the boot plan.

use std::path::PathBuf;

use serde::Deserialize;

use crate::init::InitConfig;
use crate::supervisor::SupervisorConfig;

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct SystemConfig {
    #[serde(default)]
    pub daemon: DaemonConfig,
    #[serde(default)]
    pub power: PowerConfig,
    #[serde(default)]
    pub supervisor: SupervisorConfig,
    #[serde(default)]
    pub init: InitConfig,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self { log_dir: "/run/oxmux/log".into(), replaces: Vec::new(), services: Vec::new() }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DaemonConfig {
    /// Where brightness/volume are remembered across boots.
    pub state: PathBuf,
    pub socket: PathBuf,
    pub brightness_step: u8,
    pub volume_step: u8,
    /// Hold the power button this long to power off (short press = sleep).
    pub power_hold_ms: u64,
    pub idle: IdleConfig,
    pub battery: BatteryPolicy,
    #[serde(rename = "hotkey")]
    pub hotkeys: Vec<Hotkey>,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            state: "/opt/oxmux/state/settings.toml".into(),
            socket: "/run/oxmux/daemon.sock".into(),
            brightness_step: 10,
            volume_step: 5,
            power_hold_ms: 2500,
            idle: IdleConfig::default(),
            battery: BatteryPolicy::default(),
            hotkeys: Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct IdleConfig {
    /// Seconds without input before dimming (0 = never).
    pub dim_after: u64,
    /// Brightness percent while dimmed.
    pub dim_to: u8,
    /// Seconds without input before sleeping (0 = never).
    pub sleep_after: u64,
    /// Don't auto-sleep while on the charger.
    pub stay_awake_charging: bool,
}

impl Default for IdleConfig {
    fn default() -> Self {
        Self { dim_after: 60, dim_to: 10, sleep_after: 600, stay_awake_charging: true }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct BatteryPolicy {
    /// Percent at which the low-battery LED comes on.
    pub low: u8,
    /// Percent at which the device powers off cleanly (when not charging).
    pub critical: u8,
    pub poll_secs: u64,
}

impl Default for BatteryPolicy {
    fn default() -> Self {
        Self { low: 10, critical: 3, poll_secs: 30 }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Hotkey {
    /// Button names from device.toml `[buttons]`; the last one triggers while the others are held.
    pub keys: Vec<String>,
    pub action: Action,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    BrightnessUp,
    BrightnessDown,
    VolumeUp,
    VolumeDown,
    Mute,
    Suspend,
    Poweroff,
    Reboot,
    /// Run a command (fire and forget).
    Exec(Vec<String>),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct PowerConfig {
    pub poweroff: PowerMethod,
    pub reboot: PowerMethod,
    /// Written to /sys/power/mem_sleep before suspending, if set (e.g. "deep").
    pub mem_sleep: Option<String>,
    /// Power off instead of resuming once asleep this many seconds (0 = never). Uses the
    /// RTC wake alarm from device.toml.
    pub sleep_poweroff_after: u64,
    /// Commands run before sleeping and after waking (e.g. unloading the wifi driver).
    pub before_suspend: Vec<Vec<String>>,
    pub after_resume: Vec<Vec<String>>,
}

impl Default for PowerConfig {
    fn default() -> Self {
        Self {
            poweroff: PowerMethod::SignalInit,
            reboot: PowerMethod::SignalInit,
            mem_sleep: None,
            sleep_poweroff_after: 3600,
            before_suspend: Vec::new(),
            after_resume: Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "kebab-case")]
pub enum PowerMethod {
    /// Signal PID 1 (busybox init or `oxmux init`): SIGUSR2 = poweroff, SIGTERM = reboot.
    SignalInit,
    Exec(Vec<String>),
}
