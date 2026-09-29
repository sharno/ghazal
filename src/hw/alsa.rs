//! Minimal ALSA control-interface client (the ioctls behind `amixer cset`), so volume
//! works without libasound. Struct layouts and ioctl numbers are from
//! <sound/asound.h> on LP64 (identical on x86_64 and aarch64).

use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;

use anyhow::{bail, Context, Result};

const ELEM_INFO: u64 = 0xc110_5511;
const ELEM_WRITE: u64 = 0xc4c8_5513;

const IFACE_MIXER: i32 = 2;
const TYPE_INTEGER: i32 = 2;

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
        if info.kind != TYPE_INTEGER {
            bail!("ALSA control {name:?} is not an integer control");
        }
        Ok(Self { file, id: info.id, min: info.value[0], max: info.value[1], channels: info.count.clamp(1, 128) as usize })
    }

    /// Sets every channel to `value` (clamped to the control's range).
    pub fn set(&self, value: i64) -> Result<()> {
        let mut v: ElemValue = unsafe { std::mem::zeroed() };
        v.id = self.id;
        v.value[..self.channels].fill(value.clamp(self.min, self.max));
        if unsafe { libc::ioctl(self.file.as_raw_fd(), ELEM_WRITE as _, &mut v) } < 0 {
            bail!("writing ALSA control: {}", std::io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn set_percent(&self, percent: u8) -> Result<()> {
        let span = self.max - self.min;
        self.set(self.min + span * percent.min(100) as i64 / 100)
    }
}
