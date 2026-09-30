//! DRM/KMS output for mainline kernels (and QEMU's virtio-gpu): one XRGB8888 dumb buffer
//! scanned out on the first connected connector. Like the fbdev backend, only damaged
//! rectangles are copied, then reported with DIRTYFB (needed by virtual/USB displays).

use std::fs::{File, OpenOptions};
use std::os::fd::{AsFd, BorrowedFd};
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use drm::buffer::{Buffer, DrmFourcc};
use drm::control::{connector, crtc, dumbbuffer::DumbBuffer, framebuffer, ClipRect, Device as ControlDevice, Mode, ModeTypeFlags};
use slint::Rgb8Pixel;

use super::Damage;

struct Card(File);

impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl drm::Device for Card {}
impl ControlDevice for Card {}

pub struct Kms {
    card: Card,
    crtc: crtc::Handle,
    connector: connector::Handle,
    mode: Mode,
    buffer: DumbBuffer,
    fb: framebuffer::Handle,
}

impl Kms {
    pub fn open(path: &Path) -> Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path).with_context(|| format!("opening {}", path.display()))?;
        let card = Card(file);
        let res = card.resource_handles().context("reading DRM resources")?;

        let conn = res
            .connectors()
            .iter()
            .filter_map(|&c| card.get_connector(c, true).ok())
            .find(|c| c.state() == connector::State::Connected && !c.modes().is_empty())
            .ok_or_else(|| anyhow!("no connected display on {}", path.display()))?;
        let mode = *conn
            .modes()
            .iter()
            .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
            .unwrap_or(&conn.modes()[0]);

        // Prefer the CRTC already driving this connector, else any compatible one.
        let crtc = conn
            .current_encoder()
            .and_then(|e| card.get_encoder(e).ok())
            .and_then(|e| e.crtc())
            .or_else(|| {
                conn.encoders().iter().filter_map(|&e| card.get_encoder(e).ok()).find_map(|e| {
                    res.filter_crtcs(e.possible_crtcs()).first().copied()
                })
            })
            .ok_or_else(|| anyhow!("no CRTC for the display"))?;

        let (w, h) = mode.size();
        let mut buffer = card.create_dumb_buffer((w as u32, h as u32), DrmFourcc::Xrgb8888, 32)?;
        {
            let mut map = card.map_dumb_buffer(&mut buffer)?;
            map.as_mut().fill(0);
        }
        let fb = card.add_framebuffer(&buffer, 24, 32)?;
        let kms = Self { card, crtc, connector: conn.handle(), mode, buffer, fb };
        kms.modeset()?;
        eprintln!("oxmux: kms {} {}x{} on {:?}", path.display(), w, h, conn.interface());
        Ok(kms)
    }

    fn modeset(&self) -> Result<()> {
        self.card
            .set_crtc(self.crtc, Some(self.fb), (0, 0), &[self.connector], Some(self.mode))
            .context("setting display mode")
    }

    pub fn size(&self) -> (u32, u32) {
        let (w, h) = self.mode.size();
        (w as u32, h as u32)
    }

    pub fn present(&mut self, frame: &[Rgb8Pixel], stride: usize, (x, y, w, h): Damage) -> Result<()> {
        let pitch = self.buffer.pitch() as usize;
        let (bw, bh) = self.size();
        let w = w.min((bw as usize).min(stride).saturating_sub(x));
        let h = h.min((bh as usize).min(frame.len() / stride).saturating_sub(y));
        {
            let mut map = self.card.map_dumb_buffer(&mut self.buffer)?;
            let bytes = map.as_mut();
            for row in y..y + h {
                let src = &frame[row * stride + x..][..w];
                let dst = &mut bytes[row * pitch + x * 4..][..w * 4];
                for (p, out) in src.iter().zip(dst.chunks_exact_mut(4)) {
                    // XRGB8888, little-endian in memory: B G R X.
                    out.copy_from_slice(&[p.b, p.g, p.r, 0xff]);
                }
            }
        }
        let clip = ClipRect::new(x as u16, y as u16, (x + w) as u16, (y + h) as u16);
        // Not every driver implements DIRTYFB; scanout-from-memory ones don't need it.
        let _ = self.card.dirty_framebuffer(self.fb, &[clip]);
        Ok(())
    }

    /// Hands the display to another program (RetroArch), which becomes DRM master.
    pub fn release(&self) {
        let _ = drm::Device::release_master_lock(&self.card);
    }

    /// Takes the display back and restores our mode and framebuffer.
    pub fn reacquire(&self) -> Result<()> {
        let _ = drm::Device::acquire_master_lock(&self.card);
        self.modeset()
    }
}

impl Drop for Kms {
    fn drop(&mut self) {
        let _ = self.card.destroy_framebuffer(self.fb);
    }
}
