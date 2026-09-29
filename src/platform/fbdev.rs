//! Linux fbdev output. The H700 BSP kernel exposes the panel as /dev/fb0; a DRM/KMS
//! backend can slot in next to this once we run on a mainline kernel.

use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::path::Path;

use anyhow::{bail, Context, Result};
use slint::Rgb8Pixel;

use super::Damage;

const FBIOGET_VSCREENINFO: u64 = 0x4600;
const FBIOGET_FSCREENINFO: u64 = 0x4602;
const FBIOPAN_DISPLAY: u64 = 0x4606;

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
struct Bitfield {
    offset: u32,
    length: u32,
    msb_right: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
struct VarScreeninfo {
    xres: u32,
    yres: u32,
    xres_virtual: u32,
    yres_virtual: u32,
    xoffset: u32,
    yoffset: u32,
    bits_per_pixel: u32,
    grayscale: u32,
    red: Bitfield,
    green: Bitfield,
    blue: Bitfield,
    transp: Bitfield,
    nonstd: u32,
    activate: u32,
    height: u32,
    width: u32,
    accel_flags: u32,
    pixclock: u32,
    left_margin: u32,
    right_margin: u32,
    upper_margin: u32,
    lower_margin: u32,
    hsync_len: u32,
    vsync_len: u32,
    sync: u32,
    vmode: u32,
    rotate: u32,
    colorspace: u32,
    reserved: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct FixScreeninfo {
    id: [u8; 16],
    smem_start: libc::c_ulong,
    smem_len: u32,
    kind: u32,
    type_aux: u32,
    visual: u32,
    xpanstep: u16,
    ypanstep: u16,
    ywrapstep: u16,
    line_length: u32,
    mmio_start: libc::c_ulong,
    mmio_len: u32,
    accel: u32,
    capabilities: u16,
    reserved: [u16; 2],
}

pub struct Framebuffer {
    file: File,
    map: *mut u8,
    map_len: usize,
    var: VarScreeninfo,
    line_length: usize,
}

impl Framebuffer {
    pub fn open(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .with_context(|| format!("opening {}", path.display()))?;
        let fd = file.as_raw_fd();

        let mut fix: FixScreeninfo = unsafe { std::mem::zeroed() };
        // SAFETY: the kernel fills a struct of exactly this layout.
        if unsafe { libc::ioctl(fd, FBIOGET_FSCREENINFO as _, &mut fix) } < 0 {
            bail!("FBIOGET_FSCREENINFO: {}", std::io::Error::last_os_error());
        }
        let map_len = fix.smem_len as usize;
        // SAFETY: mapping the device memory the driver told us about; unmapped in Drop.
        let map = unsafe {
            libc::mmap(std::ptr::null_mut(), map_len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd, 0)
        };
        if map == libc::MAP_FAILED {
            bail!("mmap framebuffer: {}", std::io::Error::last_os_error());
        }

        let mut fb = Self { file, map: map.cast(), map_len, var: VarScreeninfo::default(), line_length: fix.line_length as usize };
        fb.reload()?;
        let v = &fb.var;
        eprintln!(
            "oxmux: fb {}x{} (virtual {}x{}) {}bpp r{}:{} g{}:{} b{}:{} line={}",
            v.xres, v.yres, v.xres_virtual, v.yres_virtual, v.bits_per_pixel,
            v.red.offset, v.red.length, v.green.offset, v.green.length, v.blue.offset, v.blue.length,
            fb.line_length,
        );
        Ok(fb)
    }

    /// Re-reads the mode and pans back to the first page. Emulators may change the
    /// resolution or pan offset while they run.
    pub fn reload(&mut self) -> Result<()> {
        let fd = self.file.as_raw_fd();
        // SAFETY: as above.
        if unsafe { libc::ioctl(fd, FBIOGET_VSCREENINFO as _, &mut self.var) } < 0 {
            bail!("FBIOGET_VSCREENINFO: {}", std::io::Error::last_os_error());
        }
        if !matches!(self.var.bits_per_pixel, 16 | 32) {
            bail!("unsupported framebuffer depth {}bpp", self.var.bits_per_pixel);
        }
        if self.line_length * self.var.yres as usize > self.map_len {
            bail!("framebuffer mode larger than mapped memory");
        }
        if self.var.yoffset != 0 || self.var.xoffset != 0 {
            self.var.xoffset = 0;
            self.var.yoffset = 0;
            // SAFETY: passing a valid screeninfo struct.
            unsafe { libc::ioctl(fd, FBIOPAN_DISPLAY as _, &self.var) };
        }
        Ok(())
    }

    pub fn size(&self) -> (u32, u32) {
        (self.var.xres, self.var.yres)
    }

    /// Copies the damaged rectangle of an RGB8 frame to the screen, converting to the
    /// framebuffer's pixel format. Only changed pixels touch device memory.
    pub fn present(&mut self, frame: &[Rgb8Pixel], stride: usize, (x, y, w, h): Damage) {
        let v = self.var;
        let bpp = (v.bits_per_pixel / 8) as usize;
        let w = w.min((v.xres as usize).min(stride).saturating_sub(x));
        let h = h.min((v.yres as usize).min(frame.len() / stride).saturating_sub(y));
        let pack = |c: u8, f: Bitfield| ((c as u32) >> (8 - f.length.min(8))) << f.offset;
        let alpha = if v.transp.length > 0 { pack(255, v.transp) } else { 0 };

        for row in y..y + h {
            let src = &frame[row * stride + x..][..w];
            // SAFETY: reload() checked that yres rows of line_length fit in the mapping,
            // and x + w <= xres.
            let dst = unsafe { std::slice::from_raw_parts_mut(self.map.add(row * self.line_length + x * bpp), w * bpp) };
            for (px, out) in src.iter().zip(dst.chunks_exact_mut(bpp)) {
                let value = pack(px.r, v.red) | pack(px.g, v.green) | pack(px.b, v.blue) | alpha;
                if bpp == 4 {
                    out.copy_from_slice(&value.to_ne_bytes());
                } else {
                    out.copy_from_slice(&(value as u16).to_ne_bytes());
                }
            }
        }
    }
}

impl Drop for Framebuffer {
    fn drop(&mut self) {
        // SAFETY: unmapping exactly what open() mapped.
        unsafe { libc::munmap(self.map.cast(), self.map_len) };
    }
}
