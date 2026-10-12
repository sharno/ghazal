//! Hardware control used by the daemon (and read by the frontend): backlight, volume,
//! battery, LEDs, rumble, sleep. Each maps to a device.toml section.

pub mod alsa;
pub mod battery;

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::device::{self, DeviceConfig};
use crate::sys;

pub struct Backlight {
    cfg: device::Backlight,
    range: BrightnessRange,
    /// Last value set, in percent (dispdbg can't be read back).
    current: u8,
}

struct BrightnessRange { min: u32, max: u32 }

#[cfg(test)]
mod brightness_tests {
    use super::*;
    #[test]
    fn hardware_range_and_overflow() {
        let range = BrightnessRange::new(0, 2499).unwrap();
        assert_eq!(range.raw(100), 2499);
        assert_eq!(range.raw(50), 1249);
        assert_eq!(range.raw(0), 0);
        assert_eq!(BrightnessRange::new(0, u32::MAX).unwrap().raw(100), u32::MAX);
        assert!(BrightnessRange::new(0, 0).is_err());
        assert!(BrightnessRange::new(100, 100).is_err());
        assert!(BrightnessRange::new(101, 100).is_err());
    }
    #[test]
    fn kernel_limit_is_default_and_fixed_profiles_still_work() {
        let default: device::Backlight = toml::from_str("kind='sysfs'\npath='/sys/example/brightness'").unwrap();
        assert!(matches!(default, device::Backlight::Sysfs { max: device::BrightnessLimit::Source(_), .. }));
        let fixed: device::Backlight = toml::from_str("kind='sysfs'\npath='/sys/example/brightness'\nmax=255").unwrap();
        assert!(matches!(fixed, device::Backlight::Sysfs { max: device::BrightnessLimit::Fixed(_), .. }));
    }
}

impl BrightnessRange {
    fn new(min: u32, max: u32) -> Result<Self> {
        if max == 0 || min >= max { anyhow::bail!("invalid backlight range {min}..{max}"); }
        Ok(Self { min, max })
    }
    fn raw(&self, percent: u8) -> u32 {
        if percent == 0 { return 0; }
        self.min + (u64::from(self.max - self.min) * u64::from(percent.min(100)) / 100) as u32
    }
    fn percent(&self, raw: u32) -> u8 {
        (u64::from(raw.saturating_sub(self.min).min(self.max - self.min)) * 100 / u64::from(self.max - self.min)) as u8
    }
}

impl Backlight {
    pub fn new(cfg: device::Backlight) -> Result<Self> {
        let (min, max) = match &cfg {
            device::Backlight::Sysfs { path, max, min } => (*min, match max {
                device::BrightnessLimit::Fixed(max) => max.get(),
                device::BrightnessLimit::Source(device::KernelBrightness::Kernel) => std::fs::read_to_string(path.with_file_name("max_brightness"))?.trim().parse()?,
            }),
            device::Backlight::Dispdbg { min, max, .. } => (*min, *max),
        };
        let range = BrightnessRange::new(min, max)?;
        let current = match &cfg {
            device::Backlight::Sysfs { path, .. } => range.percent(std::fs::read_to_string(path)?.trim().parse()?),
            device::Backlight::Dispdbg { .. } => 70,
        };
        Ok(Self { cfg, range, current })
    }

    /// 0 turns the backlight off; 1..=100 maps onto min..=max.
    pub fn set_percent(&mut self, percent: u8) -> Result<()> {
        let percent = percent.min(100);
        let raw = self.range.raw(percent).to_string();
        match &self.cfg {
            device::Backlight::Sysfs { path, .. } => sys::write_file(path, &raw)?,
            device::Backlight::Dispdbg { dir, name, command, .. } => {
                sys::write_file(&dir.join("name"), name)?;
                sys::write_file(&dir.join("command"), command)?;
                sys::write_file(&dir.join("param"), &raw)?;
                sys::write_file(&dir.join("start"), "1")?;
            }
        }
        self.current = percent;
        Ok(())
    }
}

pub struct Volume {
    control: alsa::Control,
    ceiling: u8,
}

impl Volume {
    pub fn open(cfg: &device::Audio) -> Result<Self> {
        Ok(Self { control: alsa::Control::open(cfg.card, &cfg.control)?, ceiling: cfg.ceiling.clamp(1, 100) })
    }

    pub fn set_percent(&self, percent: u8) -> Result<()> {
        self.control.set_percent((percent.min(100) as u32 * self.ceiling as u32 / 100) as u8)
    }
}

pub fn set_led(path: &Option<std::path::PathBuf>, on: bool) {
    if let Some(p) = path {
        if let Err(e) = sys::write_file(p, if on { "1" } else { "0" }) {
            eprintln!("oxmux: led: {e:#}");
        }
    }
}

pub fn rumble(cfg: &Option<device::Rumble>, duration: Duration) {
    let Some(r) = cfg else { return };
    if sys::write_file(&r.path, &r.on).is_ok() {
        std::thread::sleep(duration);
        let _ = sys::write_file(&r.path, "0");
    }
}

/// Suspends to RAM and blocks until the device wakes up (what muOS's `mususpend` does).
/// The wakeup_count handshake makes the kernel refuse to sleep if a wakeup event (e.g.
/// another button press) arrived since we decided to suspend.
#[derive(Debug, Clone, Copy)]
enum SuspendPhase {
    Synchronizing,
    SelectingMode,
    ArmingWakeCount,
    EnteringKernel,
    Resumed,
}

/// The dedicated checkpoint is committed before moving to the next phase.
fn suspend_checkpoint(phase: SuspendPhase, checkpoint: Option<&Path>) -> Result<()> {
    eprintln!("oxmux: suspend checkpoint {phase:?}");
    if let Some(path) = checkpoint {
        sys::write_atomic(path, format!("{phase:?}\n").as_bytes()).context("persisting suspend checkpoint")?;
    }
    Ok(())
}

pub fn suspend(mem_sleep: Option<&str>, checkpoint: Option<&Path>) -> Result<()> {
    suspend_checkpoint(SuspendPhase::Synchronizing, checkpoint)?;
    sys::sync();
    if let Some(mode) = mem_sleep {
        suspend_checkpoint(SuspendPhase::SelectingMode, checkpoint)?;
        sys::write_file(Path::new("/sys/power/mem_sleep"), mode)
            .with_context(|| format!("selecting {mode} sleep"))?;
    }
    let count_path = Path::new("/sys/power/wakeup_count");
    suspend_checkpoint(SuspendPhase::ArmingWakeCount, checkpoint)?;
    if let Ok(count) = std::fs::read_to_string(count_path) {
        if sys::write_file(count_path, count.trim()).is_err() {
            anyhow::bail!("wakeup event pending, not suspending");
        }
    }
    suspend_checkpoint(SuspendPhase::EnteringKernel, checkpoint)?;
    sys::write_file(Path::new("/sys/power/state"), "mem").context("suspending")?;
    suspend_checkpoint(SuspendPhase::Resumed, checkpoint)
}

/// Everything the daemon controls, opened from the device profile. Missing pieces are
/// logged and left out rather than failing the whole daemon.
pub struct Hardware {
    pub backlight: Option<Backlight>,
    pub volume: Option<Volume>,
    pub battery: Option<device::Battery>,
    pub leds: device::Leds,
    pub rumble: Option<device::Rumble>,
}

impl Hardware {
    pub fn open(dev: &DeviceConfig) -> Self {
        let volume = dev.audio.as_ref().and_then(|a| {
            Volume::open(a).map_err(|e| eprintln!("oxmux: volume unavailable: {e:#}")).ok()
        });
        Self {
            backlight: dev.backlight.clone().and_then(|cfg| Backlight::new(cfg).map_err(|error| eprintln!("oxmux: backlight unavailable: {error:#}")).ok()),
            volume,
            battery: dev.battery.clone(),
            leds: dev.leds.clone(),
            rumble: dev.rumble.clone(),
        }
    }
}
