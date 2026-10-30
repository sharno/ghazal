//! Analog-stick RGB rings (RG40XX H/V, RG CubeXX): a microcontroller behind a
//! UART, powered through a GPIO the kernel exposes as an LED. The protocol
//! follows muOS's rgb.sh: one frame `[mode, brightness, payload…, checksum]`
//! at 115200 baud 8N1, where the checksum is the byte sum modulo 256.

use std::fs::File;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::device;

/// The choices offered in Configuration, persisted by the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Preset {
    #[default]
    Off,
    White,
    Red,
    Green,
    Blue,
    Purple,
    Cyan,
    Orange,
    Breathing,
    Rainbow,
}

const PRESETS: [Preset; 10] = [Preset::Off, Preset::White, Preset::Red, Preset::Green, Preset::Blue,
    Preset::Purple, Preset::Cyan, Preset::Orange, Preset::Breathing, Preset::Rainbow];

type Rgb = [u8; 3];

/// What the rings show; `Off` cuts the microcontroller's power instead.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Effect {
    Solid(Rgb),
    Breathe(Rgb),
    Rainbow,
}

/// muOS lights the rings red at low battery.
pub const LOW_BATTERY: Effect = Effect::Solid([255, 0, 0]);

/// One row per preset: stored id, Configuration label and what the rings show.
struct Row { id: &'static str, label: &'static str, effect: Option<Effect> }

impl Preset {
    fn row(self) -> Row {
        let row = |id, label, effect| Row { id, label, effect };
        match self {
            Self::Off => row("off", "Stick lights: Off", None),
            Self::White => row("white", "Stick lights: White", Some(Effect::Solid([255, 255, 255]))),
            Self::Red => row("red", "Stick lights: Red", Some(Effect::Solid([255, 0, 0]))),
            Self::Green => row("green", "Stick lights: Green", Some(Effect::Solid([0, 255, 0]))),
            Self::Blue => row("blue", "Stick lights: Blue", Some(Effect::Solid([0, 0, 255]))),
            Self::Purple => row("purple", "Stick lights: Purple", Some(Effect::Solid([160, 0, 255]))),
            Self::Cyan => row("cyan", "Stick lights: Cyan", Some(Effect::Solid([0, 255, 255]))),
            Self::Orange => row("orange", "Stick lights: Orange", Some(Effect::Solid([255, 96, 0]))),
            Self::Breathing => row("breathing", "Stick lights: Breathing", Some(Effect::Breathe([255, 255, 255]))),
            Self::Rainbow => row("rainbow", "Stick lights: Rainbow", Some(Effect::Rainbow)),
        }
    }

    pub fn id(self) -> &'static str { self.row().id }
    pub fn menu_label(self) -> &'static str { self.row().label }
    pub fn effect(self) -> Option<Effect> { self.row().effect }

    pub fn parse(id: &str) -> Option<Self> {
        PRESETS.into_iter().find(|preset| preset.id() == id)
    }

    /// `PRESETS` lists the variants in declaration order, so the discriminant
    /// is the index.
    pub fn step(self, forward: bool) -> Self {
        let step = if forward { 1 } else { PRESETS.len() - 1 };
        PRESETS[(self as usize + step) % PRESETS.len()]
    }
}

/// Bright enough to see in daylight without glaring in the dark.
const BRIGHTNESS: u8 = 160;
/// Rainbow animation speed, 0-255.
const RAINBOW_SPEED: u8 = 32;

fn frame(effect: Effect) -> Vec<u8> {
    let mut bytes = match effect {
        // Eight LEDs per ring: the right ring first, then the left.
        Effect::Solid(color) => [1, BRIGHTNESS].into_iter().chain(color.repeat(16)).collect::<Vec<_>>(),
        // Mode 3: medium breathing, sixteen LEDs.
        Effect::Breathe(color) => [3, BRIGHTNESS].into_iter().chain(color.repeat(16)).collect(),
        // Mode 6: a different hue per LED.
        Effect::Rainbow => vec![6, BRIGHTNESS, 1, 1, RAINBOW_SPEED],
    };
    bytes.push(bytes.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)));
    bytes
}

pub struct Lights {
    power: PathBuf,
    port: File,
}

impl Lights {
    /// Present only on boards whose device tree exposes the ring controller.
    pub fn open(config: &device::Lights) -> Result<Self> {
        // /sys/class/tty/ttySn links to …/soc/<uart>/<uart>:0/<uart>:0.0/tty/ttySn.
        let tty = std::fs::read_dir("/sys/class/tty")?.flatten()
            .find(|entry| std::fs::read_link(entry.path())
                .is_ok_and(|device| device.components().any(|part| part.as_os_str() == config.uart.as_str())))
            .with_context(|| format!("no tty for {}", config.uart))?;
        let port = std::fs::OpenOptions::new().write(true).custom_flags(libc::O_NOCTTY).open(PathBuf::from("/dev").join(tty.file_name()))?;
        // SAFETY: termios is plain data filled by tcgetattr before use.
        unsafe {
            let mut termios: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(port.as_raw_fd(), &mut termios) != 0 { return Err(std::io::Error::last_os_error().into()); }
            libc::cfmakeraw(&mut termios);
            libc::cfsetspeed(&mut termios, libc::B115200);
            if libc::tcsetattr(port.as_raw_fd(), libc::TCSANOW, &termios) != 0 { return Err(std::io::Error::last_os_error().into()); }
        }
        Ok(Self { power: config.power.clone(), port })
    }

    /// The controller drops frames for ~100 ms after power-up.
    pub fn power(&self, on: bool) -> Result<()> {
        crate::sys::write_file(&self.power, if on { "1" } else { "0" })
    }

    pub fn show(&mut self, effect: Effect) -> Result<()> {
        self.port.write_all(&frame(effect)).context("writing stick light frame")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_match_the_controller_protocol() {
        let solid = frame(Effect::Solid([255, 0, 0]));
        assert_eq!(solid.len(), 2 + 16 * 3 + 1);
        assert_eq!(&solid[..5], &[1, BRIGHTNESS, 255, 0, 0]);
        let sum = solid[..solid.len() - 1].iter().map(|byte| u32::from(*byte)).sum::<u32>();
        assert_eq!(u32::from(*solid.last().unwrap()), sum % 256);
        assert_eq!(frame(Effect::Rainbow), vec![6, BRIGHTNESS, 1, 1, RAINBOW_SPEED, 6u8.wrapping_add(BRIGHTNESS).wrapping_add(2).wrapping_add(RAINBOW_SPEED)]);
    }

    #[test]
    fn presets_cycle_and_round_trip() {
        assert!(PRESETS.iter().enumerate().all(|(index, preset)| *preset as usize == index));
        assert_eq!(Preset::Off.step(false), Preset::Rainbow);
        assert_eq!(Preset::Rainbow.step(true), Preset::Off);
        for preset in PRESETS {
            assert_eq!(Preset::parse(preset.id()), Some(preset));
            assert!(preset.menu_label().ends_with(&preset.id()[1..]));
        }
    }
}
