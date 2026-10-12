//! Minimal ALSA control-interface client (the ioctls behind `amixer cset`), so volume
//! works without libasound. Struct layouts and ioctl numbers are from
//! <sound/asound.h> on LP64 (identical on x86_64 and aarch64).

use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;

use anyhow::{bail, Context, Result};

const ELEM_INFO: u64 = 0xc110_5511;
const ELEM_WRITE: u64 = 0xc4c8_5513;
const ELEM_READ: u64 = 0xc4c8_5512;

const IFACE_MIXER: i32 = 2;
const TYPE_INTEGER: i32 = 2;
const TYPE_BOOLEAN: i32 = 1;

#[repr(C)]
#[derive(Clone, Copy)]
struct ElemId {
    numid: u32,
    iface: i32,
    device: u32,
    subdevice: u32,
    name: [u8; 44],
    index: u32,
}

#[repr(C)]
struct ElemInfo {
    id: ElemId,
    kind: i32,
    access: u32,
    count: u32,
    owner: i32,
    // union value: for INTEGER controls, { long min; long max; long step; }
    value: [i64; 16],
    reserved: [u8; 64],
}

#[repr(C)]
struct ElemValue {
    id: ElemId,
    indirect: u32,
    _pad: u32,
    // union value: for INTEGER controls, long value[128]
    value: [i64; 128],
    reserved: [u8; 128],
}

const _: () = assert!(std::mem::size_of::<ElemInfo>() == 272);
const _: () = assert!(std::mem::size_of::<ElemValue>() == 1224);

/// An integer mixer control such as "digital volume".
pub struct Control {
    file: File,
    id: ElemId,
    pub min: i64,
    pub max: i64,
    channels: usize,
}

impl Control {
    pub fn open(card: u32, name: &str) -> Result<Self> {
        let (file, info) = open_element(card, name, TYPE_INTEGER)?;
        if info.value[0] > info.value[1] { bail!("ALSA control {name:?} has an invalid range"); }
        Ok(Self { file, id: info.id, min: info.value[0], max: info.value[1], channels: info.count as usize })
    }

    /// Sets every channel to `value` (clamped to the control's range).
    pub fn set(&self, value: i64) -> Result<()> {
        write_element(&self.file, self.id, self.channels, value.clamp(self.min, self.max))
    }

    pub fn set_percent(&self, percent: u8) -> Result<()> {
        let span = self.max - self.min;
        self.set(self.min + span * percent.min(100) as i64 / 100)
    }

    pub fn read(&self) -> Result<Vec<i64>> { read_element(&self.file, self.id, self.channels) }
}

fn open_element(card: u32, name: &str, kind: i32) -> Result<(File, ElemInfo)> {
        let path = format!("/dev/snd/controlC{card}");
        let file = OpenOptions::new().read(true).write(true).open(&path).with_context(|| format!("opening {path}"))?;
        if name.len() >= 44 {
            bail!("control name too long: {name}");
        }
        let mut id = ElemId { numid: 0, iface: IFACE_MIXER, device: 0, subdevice: 0, name: [0; 44], index: 0 };
        id.name[..name.len()].copy_from_slice(name.as_bytes());

        // SAFETY: ElemInfo matches struct snd_ctl_elem_info (size asserted above).
        let mut info: ElemInfo = unsafe { std::mem::zeroed() };
        info.id = id;
        if unsafe { libc::ioctl(file.as_raw_fd(), ELEM_INFO as _, &mut info) } < 0 {
            bail!("ALSA control {name:?} on card {card}: {}", std::io::Error::last_os_error());
        }
        if info.kind != kind {
            bail!("ALSA control {name:?} has type {}, expected {kind}", info.kind);
        }
        if !(1..=128).contains(&info.count) { bail!("ALSA control {name:?} has invalid channel count {}", info.count); }
        Ok((file, info))
}

fn write_element(file: &File, id: ElemId, channels: usize, value: i64) -> Result<()> {
        // SAFETY: ElemValue is the kernel ABI, with sizes asserted above.
        let mut v: ElemValue = unsafe { std::mem::zeroed() };
        v.id = id;
        v.value[..channels].fill(value);
        if unsafe { libc::ioctl(file.as_raw_fd(), ELEM_WRITE as _, &mut v) } < 0 {
            bail!("writing ALSA control: {}", std::io::Error::last_os_error());
        }
        Ok(())
}

fn read_element(file: &File, id: ElemId, channels: usize) -> Result<Vec<i64>> {
    // SAFETY: ElemValue is the ALSA kernel ABI, with its size asserted above.
    let mut value: ElemValue = unsafe { std::mem::zeroed() };
    value.id = id;
    // SAFETY: ioctl writes into a valid ABI-compatible structure.
    if unsafe { libc::ioctl(file.as_raw_fd(), ELEM_READ as _, &mut value) } < 0 {
        return Err(std::io::Error::last_os_error()).context("reading ALSA control");
    }
    Ok(value.value[..channels].to_vec())
}

struct Switch { file: File, id: ElemId, channels: usize }

impl Switch {
    fn open(card: u32, name: &str) -> Result<Self> {
        let (file, info) = open_element(card, name, TYPE_BOOLEAN)?;
        Ok(Self { file, id: info.id, channels: info.count as usize })
    }
    fn set(&self, enabled: bool) -> Result<()> { write_element(&self.file, self.id, self.channels, i64::from(enabled)) }
}

enum PreparedSetting { Switch(Switch, bool), Volume(Control, u8) }

pub fn initialize(audio: &crate::device::Audio) -> Result<()> {
    // Validate every named control before changing any part of the route.
    let prepared = audio.route.iter().map(|setting| match setting {
        crate::device::AudioSetting::Switch { control, enabled } => Ok(PreparedSetting::Switch(Switch::open(audio.card, control)?, *enabled)),
        crate::device::AudioSetting::Volume { control, percent } => {
            if *percent > 100 { bail!("audio route {control:?} has percent above 100"); }
            Ok(PreparedSetting::Volume(Control::open(audio.card, control)?, *percent))
        }
    }).collect::<Result<Vec<_>>>()?;
    for setting in prepared {
        match setting { PreparedSetting::Switch(control, enabled) => control.set(enabled)?, PreparedSetting::Volume(control, percent) => control.set_percent(percent)? }
    }
    verify(audio)
}

/// Read the full fixed signal path, rather than trusting PCM startup alone.
pub fn verify(audio: &crate::device::Audio) -> Result<()> {
    let mut mismatches = Vec::new();
    for setting in &audio.route {
        let (name, expected, actual) = match setting {
            crate::device::AudioSetting::Switch { control, enabled } => {
                let element = Switch::open(audio.card, control)?;
                (control, i64::from(*enabled), read_element(&element.file, element.id, element.channels)?)
            }
            crate::device::AudioSetting::Volume { control, percent } => {
                if *percent > 100 { bail!("audio route {control:?} has percent above 100"); }
                let element = Control::open(audio.card, control)?;
                (control, element.min + (element.max - element.min) * i64::from(*percent) / 100, element.read()?)
            }
        };
        println!("{name}: expected={expected} actual={actual:?}");
        if actual.iter().any(|value| *value != expected) { mismatches.push(name.as_str()); }
    }
    if !mismatches.is_empty() { bail!("audio route mismatch: {}", mismatches.join(", ")); }
    Ok(())
}
