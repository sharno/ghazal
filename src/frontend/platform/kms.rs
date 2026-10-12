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

/// Discovery owns its file descriptor. Only a connected connector with a
/// usable mode can become a SelectedDisplay; activation consumes this state.
enum Probe {
    RenderOnly,
    Disconnected,
    Connected(SelectedDisplay),
}

struct SelectedDisplay {
    card: Card,
    connector: connector::Info,
    resources: drm::control::ResourceHandles,
}

impl Card {
    fn probe(path: &Path) -> Result<Probe> {
        let card = Self(OpenOptions::new().read(true).write(true).open(path)
            .with_context(|| format!("opening {}", path.display()))?);
        let resources = match card.resource_handles() {
            Ok(resources) => resources,
            Err(error) if error.raw_os_error() == Some(libc::EOPNOTSUPP) => return Ok(Probe::RenderOnly),
            Err(error) => return Err(error).context("reading DRM resources"),
        };
        for &handle in resources.connectors() {
            let connector = card.get_connector(handle, true).context("reading DRM connector")?;
            if connector.state() == connector::State::Connected && !connector.modes().is_empty() {
                return Ok(Probe::Connected(SelectedDisplay { card, connector, resources }));
            }
        }
        Ok(Probe::Disconnected)
    }
}

pub struct Kms {
    rotation: crate::device::Rotation,
    card: Card,
    crtc: crtc::Handle,
    connector: connector::Handle,
    mode: Mode,
    buffer: DumbBuffer,
    fb: framebuffer::Handle,
}

impl Kms {
    pub fn open(path: &Path) -> Result<Self> {
        if path.is_dir() {
            // DRM card numbers depend on probe order. Render-only GPUs have no
            // connectors; select the node that actually owns a connected display.
            let mut nodes = std::fs::read_dir(path)?.collect::<std::io::Result<Vec<_>>>()?;
            nodes.sort_by_key(|entry| entry.file_name());
            for node in nodes {
                let name = node.file_name();
                let name = name.to_string_lossy();
                if !name.strip_prefix("card").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())) {
                    continue;
                }
                match Card::probe(&node.path())? {
                    Probe::Connected(display) => return display.activate(&node.path()),
                    Probe::RenderOnly | Probe::Disconnected => continue,
                }
            }
            return Err(anyhow!("no connected KMS display in {}", path.display()));
        }
        match Card::probe(path)? {
            Probe::Connected(display) => display.activate(path),
            Probe::RenderOnly => Err(anyhow!("{} is a render-only DRM node", path.display())),
            Probe::Disconnected => Err(anyhow!("no connected display on {}", path.display())),
        }
    }
}

impl SelectedDisplay {
    fn activate(self, path: &Path) -> Result<Kms> {
        let Self { card, connector: conn, resources: res } = self;
        drm::Device::acquire_master_lock(&card).context("acquiring DRM master")?;
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
        let kms = Kms { rotation: crate::device::Rotation::Normal, card, crtc, connector: conn.handle(), mode, buffer, fb };
        kms.modeset()?;
        eprintln!("oxmux: kms {} {}x{} on {:?}", path.display(), w, h, conn.interface());
        Ok(kms)
    }
}

impl Kms {
    pub fn with_rotation(mut self, rotation: crate::device::Rotation) -> Self { self.rotation = rotation; self }
    fn modeset(&self) -> Result<()> {
        self.card
            .set_crtc(self.crtc, Some(self.fb), (0, 0), &[self.connector], Some(self.mode))
            .context("setting display mode")
    }

    pub fn size(&self) -> (u32, u32) {
        let (w, h) = self.mode.size();
        self.rotation.size(w as u32, h as u32)
    }

    pub fn present(&mut self, frame: &[Rgb8Pixel], stride: usize, (x, y, w, h): Damage) -> Result<()> {
        let pitch = self.buffer.pitch() as usize;
        let (bw, bh) = self.size();
        let w = w.min((bw as usize).min(stride).saturating_sub(x));
        let h = h.min((bh as usize).min(frame.len() / stride).saturating_sub(y));
        {
            let mut map = self.card.map_dumb_buffer(&mut self.buffer)?;
            let bytes = map.as_mut();
            if self.rotation != crate::device::Rotation::Normal {
                for row in y..y + h {
                    for column in x..x + w {
                        let pixel = frame[row * stride + column];
                        let (dx, dy) = self.rotation.point(column, row, bw as usize, bh as usize);
                        bytes[dy * pitch + dx * 4..][..4].copy_from_slice(&[pixel.b, pixel.g, pixel.r, 0xff]);
                    }
                }
            } else {
            for row in y..y + h {
                let src = &frame[row * stride + x..][..w];
                let dst = &mut bytes[row * pitch + x * 4..][..w * 4];
                for (p, out) in src.iter().zip(dst.as_chunks_mut::<4>().0) {
                    // XRGB8888, little-endian in memory: B G R X.
                    *out = [p.b, p.g, p.r, 0xff];
                }
            }
            }
        }
        let clip = if self.rotation == crate::device::Rotation::Normal {
            ClipRect::new(x as u16, y as u16, (x + w) as u16, (y + h) as u16)
        } else {
            let (width, height) = self.mode.size();
            ClipRect::new(0, 0, width, height)
        };
        // Not every driver implements DIRTYFB; scanout-from-memory ones don't need it.
        let _ = self.card.dirty_framebuffer(self.fb, &[clip]);
        Ok(())
    }

    /// Hands the display to another program (RetroArch), which becomes DRM master.
    fn release(&self) -> Result<()> {
        drm::Device::release_master_lock(&self.card).context("releasing DRM master")
    }

    /// Takes the display back and restores our mode and framebuffer.
    fn reacquire(&self) -> Result<()> {
        drm::Device::acquire_master_lock(&self.card).context("reacquiring DRM master")?;
        self.modeset()
    }

    /// Borrow the renderer for the entire handoff. Callers cannot present or
    /// access Kms while the emulator owns DRM, or forget to restore the mode.
    pub fn with_released<T>(&mut self, run: impl FnOnce() -> T) -> Result<T> {
        self.release()?;
        let result = run();
        self.reacquire()?;
        Ok(result)
    }
}

impl Drop for Kms {
    fn drop(&mut self) {
        let _ = self.card.destroy_framebuffer(self.fb);
    }
}
