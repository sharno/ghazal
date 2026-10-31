//! A virtual copy of the handheld's pad, made through `/dev/uinput`.
//!
//! It has the real pad's name, bus, vendor, product and version and the same
//! buttons and sticks, so RetroArch's autoconfiguration treats it like the
//! device it ships on (`/proc/bus/input/devices` on the RG40XX-V v2 lists
//! BTN_SOUTH..BTN_THUMBR and the four d-pad codes plus ABS_X/Y/RX/RY).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use evdev::uinput::{VirtualDevice, VirtualDeviceBuilder};
use evdev::{AbsInfo, AbsoluteAxisType, AttributeSet, BusType, EventType, InputEvent, InputId, Key, UinputAbsSetup};

use crate::profile::Profile;

/// Codes below this belong to the volume and power keys, which are separate devices.
const FIRST_PAD_BUTTON: u16 = 0x100;
const AXES: [AbsoluteAxisType; 4] = [AbsoluteAxisType::ABS_X, AbsoluteAxisType::ABS_Y, AbsoluteAxisType::ABS_RX, AbsoluteAxisType::ABS_RY];
const AXIS_RANGE: i32 = 1800;

/// How the udev database came to mark the pad as a joystick.
#[derive(Debug, PartialEq, Eq)]
pub enum Tagged {
    ByUdevd,
    /// No daemon applied rules, so the entry the image's `70-gamepad.rules`
    /// would produce was written by hand.
    Written(PathBuf),
}

pub struct VirtualPad {
    device: VirtualDevice,
    buttons: std::collections::BTreeMap<String, u16>,
    pub node: PathBuf,
    pub tagged: Tagged,
}

impl VirtualPad {
    /// Fails with the `io::Error` of opening `/dev/uinput` when the machine has none.
    pub fn create(profile: &Profile) -> Result<Self> {
        let keys: AttributeSet<Key> = profile.buttons.values().copied().filter(|code| *code >= FIRST_PAD_BUTTON).map(Key::new).collect();
        let pad = &profile.pad;
        let mut builder = VirtualDeviceBuilder::new()?.name(pad.name.as_str()).input_id(InputId::new(BusType(pad.bus), pad.vendor, pad.product, pad.version)).with_keys(&keys)?;
        for axis in AXES {
            builder = builder.with_absolute_axis(&UinputAbsSetup::new(axis, AbsInfo::new(0, -AXIS_RANGE, AXIS_RANGE, 0, 0, 0)))?;
        }
        let mut device = builder.build().context("creating the virtual pad")?;
        let syspath = device.get_syspath()?;
        let (node, major, minor) = event_node(&syspath)?;
        let tagged = tag_as_joystick(major, minor)?;
        Ok(Self { device, buttons: profile.buttons.clone(), node, tagged })
    }

    fn emit(&mut self, codes: &[u16], value: i32) -> Result<()> {
        let events: Vec<_> = codes.iter().map(|code| InputEvent::new(EventType::KEY, *code, value)).collect();
        self.device.emit(&events).context("writing to the virtual pad")
    }

    /// Presses the named buttons one after another (the first is the hotkey
    /// modifier, as on the real pad), holds them, then releases in reverse.
    pub fn press(&mut self, names: &[&str]) -> Result<()> {
        let codes = names
            .iter()
            .map(|name| self.buttons.get(*name).copied().with_context(|| format!("device.toml has no button {name:?}")))
            .collect::<Result<Vec<_>>>()?;
        for code in &codes {
            self.emit(&[*code], 1)?;
            std::thread::sleep(Duration::from_millis(120));
        }
        std::thread::sleep(Duration::from_millis(300));
        for code in codes.iter().rev() {
            self.emit(&[*code], 0)?;
            std::thread::sleep(Duration::from_millis(60));
        }
        Ok(())
    }
}

/// The `/dev/input/eventN` node of a uinput device, created when this
/// machine's `/dev` is a snapshot (a container) that never saw the device.
fn event_node(syspath: &Path) -> Result<(PathBuf, u32, u32)> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let event = fs::read_dir(syspath)?.flatten().find(|entry| entry.file_name().to_string_lossy().starts_with("event"));
        if let Some(event) = event {
            let numbers = fs::read_to_string(event.path().join("dev"))?;
            let (major, minor) = numbers.trim().split_once(':').and_then(|(major, minor)| Some((major.parse().ok()?, minor.parse().ok()?))).context("unreadable device numbers")?;
            let node = Path::new("/dev/input").join(event.file_name());
            if !node.exists() {
                fs::create_dir_all("/dev/input")?;
                let path = std::ffi::CString::new(node.to_string_lossy().as_bytes())?;
                // SAFETY: a valid NUL-terminated path and device numbers read from sysfs.
                if unsafe { libc::mknod(path.as_ptr(), libc::S_IFCHR | 0o660, libc::makedev(major, minor)) } != 0 {
                    return Err(std::io::Error::last_os_error()).with_context(|| format!("creating {}", node.display()));
                }
            }
            return Ok((node, major, minor));
        }
        if Instant::now() > deadline {
            bail!("the virtual pad has no event node under {}", syspath.display());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

const UDEV_DATA: &str = "/run/udev/data";

/// RetroArch's udev joypad driver lists only devices whose database entry says
/// `ID_INPUT_JOYSTICK=1`. The image's own rule sets it for this pad; a udev
/// daemon on the test machine may too. Without one, write the entry.
fn tag_as_joystick(major: u32, minor: u32) -> Result<Tagged> {
    let entry = Path::new(UDEV_DATA).join(format!("c{major}:{minor}"));
    let tagged = || fs::read_to_string(&entry).is_ok_and(|text| text.lines().any(|line| line == "E:ID_INPUT_JOYSTICK=1"));
    let daemon = Path::new("/run/udev/control").exists();
    let deadline = Instant::now() + Duration::from_secs(if daemon { 3 } else { 0 });
    while Instant::now() < deadline {
        if tagged() {
            return Ok(Tagged::ByUdevd);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if tagged() {
        return Ok(Tagged::ByUdevd);
    }
    fs::create_dir_all(UDEV_DATA)?;
    fs::write(&entry, "I:1\nE:ID_INPUT=1\nE:ID_INPUT_JOYSTICK=1\n").with_context(|| format!("writing {}", entry.display()))?;
    Ok(Tagged::Written(entry))
}

impl Drop for VirtualPad {
    fn drop(&mut self) {
        if let Tagged::Written(entry) = &self.tagged {
            let _ = fs::remove_file(entry);
        }
        // The uinput device disappears when `device` closes.
    }
}
