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
    /// Last value set, in percent (dispdbg can't be read back).
    current: u8,
}

impl Backlight {
    pub fn new(cfg: device::Backlight) -> Self {
        let mut b = Self { cfg, current: 70 };
        if let device::Backlight::Sysfs { path, max, min } = &b.cfg {
            if let Some(raw) = std::fs::read_to_string(path).ok().and_then(|s| s.trim().parse::<u32>().ok()) {
                b.current = (raw.saturating_sub(*min) * 100 / (max - min).max(1)) as u8;
            }
        }
        b
    }

    /// 0 turns the backlight off; 1..=100 maps onto min..=max.
    pub fn set_percent(&mut self, percent: u8) -> Result<()> {
        let percent = percent.min(100);
        let raw = |min: u32, max: u32| if percent == 0 { 0 } else { min + (max - min) * percent as u32 / 100 };
        match &self.cfg {
            device::Backlight::Sysfs { path, max, min } => sys::write_file(path, &raw(*min, *max).to_string())?,
            device::Backlight::Dispdbg { dir, name, command, max, min } => {
                sys::write_file(&dir.join("name"), name)?;
                sys::write_file(&dir.join("command"), command)?;
                sys::write_file(&dir.join("param"), &raw(*min, *max).to_string())?;
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

/// The daemon's stderr is a supervisor-owned regular log file. Commit each
/// checkpoint before the next operation so a hardware reset preserves it.
fn suspend_checkpoint(phase: SuspendPhase) -> Result<()> {
    eprintln!("oxmux: suspend checkpoint {phase:?}");
    let log = std::fs::File::open("/proc/self/fd/2")?;
    if log.metadata()?.is_file() {
        log.sync_all().context("persisting suspend checkpoint")?;
    }
    Ok(())
}

pub fn suspend(mem_sleep: Option<&str>) -> Result<()> {
    suspend_checkpoint(SuspendPhase::Synchronizing)?;
    sys::sync();
    if let Some(mode) = mem_sleep {
        suspend_checkpoint(SuspendPhase::SelectingMode)?;
        sys::write_file(Path::new("/sys/power/mem_sleep"), mode)
            .with_context(|| format!("selecting {mode} sleep"))?;
    }
    let count_path = Path::new("/sys/power/wakeup_count");
    suspend_checkpoint(SuspendPhase::ArmingWakeCount)?;
    if let Ok(count) = std::fs::read_to_string(count_path) {
        if sys::write_file(count_path, count.trim()).is_err() {
            anyhow::bail!("wakeup event pending, not suspending");
        }
    }
    suspend_checkpoint(SuspendPhase::EnteringKernel)?;
    sys::write_file(Path::new("/sys/power/state"), "mem").context("suspending")?;
    suspend_checkpoint(SuspendPhase::Resumed)
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
            backlight: dev.backlight.clone().map(Backlight::new),
            volume,
            battery: dev.battery.clone(),
            leds: dev.leds.clone(),
            rumble: dev.rumble.clone(),
        }
    }
}
