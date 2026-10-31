//! DRM/KMS output for mainline kernels (and QEMU's virtio-gpu). The handheld's own
//! screen is one XRGB8888 dumb buffer scanned out on its connector; when the
//! board has HDMI, a second surface on another CRTC carries the menu to a
//! display. Like the fbdev backend, only damaged rectangles are copied, then
//! reported with DIRTYFB (needed by virtual/USB displays).

use std::fs::{File, OpenOptions};
use std::os::fd::{AsFd, BorrowedFd};
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use drm::buffer::{Buffer, DrmFourcc};
use drm::control::{connector, crtc, dumbbuffer::DumbBuffer, framebuffer, ClipRect, Device as ControlDevice, Mode, ModeFlags, ModeTypeFlags};
use slint::Rgb8Pixel;

use super::{scale, Damage};
use crate::device::HdmiDisplay;
use crate::hdmi::connector::{Connector, Force, DRM_CLASS};
use crate::hdmi::{self, Connection, Layout, ModeInfo};

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
    /// The board's HDMI connector, found without probing it.
    hdmi: Option<connector::Handle>,
}

fn is_hdmi(interface: connector::Interface) -> bool {
    matches!(interface, connector::Interface::HDMIA | connector::Interface::HDMIB)
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
        // The handheld's own screen is the first connected connector that is not
        // HDMI. Interfaces are read without probing, so a board with HDMI pays
        // nothing at boot for a cable that is not there.
        let kinds = resources.connectors().iter()
            .map(|&handle| Ok((handle, is_hdmi(card.get_connector(handle, false).context("reading DRM connector")?.interface()))))
            .collect::<Result<Vec<_>>>()?;
        let hdmi = kinds.iter().find(|(_, hdmi)| *hdmi).map(|(handle, _)| *handle);
        for &(handle, _) in kinds.iter().filter(|(_, hdmi)| !hdmi).chain(kinds.iter().filter(|(_, hdmi)| *hdmi)) {
            let connector = card.get_connector(handle, true).context("reading DRM connector")?;
            if connector.state() == connector::State::Connected && !connector.modes().is_empty() {
                return Ok(Probe::Connected(SelectedDisplay { card, connector, resources, hdmi }));
            }
        }
        Ok(Probe::Disconnected)
    }
}

/// One scanned-out buffer on one CRTC.
struct Surface {
    crtc: crtc::Handle,
    connector: connector::Handle,
    mode: Mode,
    buffer: DumbBuffer,
    fb: framebuffer::Handle,
}

impl Surface {
    fn new(card: &Card, connector: connector::Handle, crtc: crtc::Handle, mode: Mode) -> Result<Self> {
        let (w, h) = mode.size();
        let mut buffer = card.create_dumb_buffer((w as u32, h as u32), DrmFourcc::Xrgb8888, 32)?;
        {
            let mut map = card.map_dumb_buffer(&mut buffer)?;
            map.as_mut().fill(0);
        }
        let fb = card.add_framebuffer(&buffer, 24, 32)?;
        Ok(Self { crtc, connector, mode, buffer, fb })
    }

    fn modeset(&self, card: &Card) -> Result<()> {
        card.set_crtc(self.crtc, Some(self.fb), (0, 0), &[self.connector], Some(self.mode)).context("setting display mode")
    }

    /// Turns the CRTC off and frees the buffer.
    fn retire(self, card: &Card) {
        let _ = card.set_crtc(self.crtc, None, (0, 0), &[], None);
        let _ = card.destroy_framebuffer(self.fb);
        let _ = card.destroy_dumb_buffer(self.buffer);
    }
}

/// The board's HDMI connector. `active` exists exactly while a layout uses it.
struct Hdmi {
    connector: connector::Handle,
    sysfs: Connector,
    wanted: hdmi::Size,
    active: Option<Surface>,
}

pub struct Kms {
    rotation: crate::device::Rotation,
    card: Card,
    panel: Surface,
    panel_sysfs: Connector,
    hdmi: Option<Hdmi>,
    layout: Layout,
}

impl Kms {
    pub fn open(path: &Path, hdmi: Option<&HdmiDisplay>) -> Result<Self> {
        // Before probing: a screen forced off by an earlier frontend would not be found.
        hdmi::connector::release_forced_panels(Path::new(DRM_CLASS));
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
                    Probe::Connected(display) => return display.activate(&node.path(), hdmi),
                    Probe::RenderOnly | Probe::Disconnected => continue,
                }
            }
            return Err(anyhow!("no connected KMS display in {}", path.display()));
        }
        match Card::probe(path)? {
            Probe::Connected(display) => display.activate(path, hdmi),
            Probe::RenderOnly => Err(anyhow!("{} is a render-only DRM node", path.display())),
            Probe::Disconnected => Err(anyhow!("no connected display on {}", path.display())),
        }
    }
}

impl SelectedDisplay {
    fn activate(self, path: &Path, hdmi: Option<&HdmiDisplay>) -> Result<Kms> {
        let Self { card, connector: conn, resources: res, hdmi: hdmi_connector } = self;
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
        let panel = Surface::new(&card, conn.handle(), crtc, mode)?;
        let card_name = path.file_name().map_or_else(String::new, |name| name.to_string_lossy().into_owned());
        let panel_sysfs = Connector::named(Path::new(DRM_CLASS), &card_name, &format!("{}-{}", conn.interface().as_str(), conn.interface_id()));
        let hdmi = hdmi.zip(hdmi_connector).and_then(|(config, connector)| {
            Some(Hdmi { connector, sysfs: Connector::find_hdmi(Path::new(DRM_CLASS), Some(&card_name))?, wanted: config.mode, active: None })
        });
        let kms = Kms { rotation: crate::device::Rotation::Normal, card, panel, panel_sysfs, hdmi, layout: Layout::Panel };
        kms.panel.modeset(&kms.card)?;
        eprintln!("ghazal: kms {} {}x{} on {:?}{}", path.display(), w, h, conn.interface(), if kms.hdmi.is_some() { " (HDMI available)" } else { "" });
        Ok(kms)
    }
}

const EDID_RETRIES: u32 = 5;
const EDID_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(200);

/// A CRTC for a connector, preferring one that is not already in use. The
/// flag says the choice shares the other connector's CRTC, which rules out
/// showing both screens at once.
fn choose_crtc<T: Copy + PartialEq>(candidates: &[T], taken: T) -> Option<(T, bool)> {
    candidates.iter().find(|candidate| **candidate != taken).map(|crtc| (*crtc, false)).or_else(|| candidates.first().map(|crtc| (*crtc, true)))
}

fn mode_info(mode: &Mode) -> ModeInfo {
    let flags = mode.flags();
    ModeInfo {
        width: mode.size().0,
        height: mode.size().1,
        millihertz: ModeInfo::millihertz(mode.clock(), mode.hsync().2, mode.vsync().2, flags.contains(ModeFlags::INTERLACE), flags.contains(ModeFlags::DBLSCAN), mode.vscan()),
        interlaced: flags.contains(ModeFlags::INTERLACE),
        preferred: mode.mode_type().contains(ModeTypeFlags::PREFERRED),
    }
}

/// Connectors forced off for the length of a game, so the emulator's
/// "first connected connector" is the screen the layout names.
#[must_use = "forced connectors stay off until restored"]
struct Steering(Vec<Connector>);

impl Steering {
    fn restore(self) {
        for connector in self.0 {
            if let Err(error) = connector.force(Force::Detect) { eprintln!("ghazal: releasing {}: {error:#}", connector.name()); }
        }
    }
}

impl Kms {
    pub fn with_rotation(mut self, rotation: crate::device::Rotation) -> Self { self.rotation = rotation; self }

    pub fn size(&self) -> (u32, u32) {
        let (w, h) = self.panel.mode.size();
        self.rotation.size(w as u32, h as u32)
    }

    pub fn layout(&self) -> Layout { self.layout }

    /// `Absent` on boards without HDMI (or whose profile does not configure it).
    pub fn hdmi_connection(&self) -> Connection {
        self.hdmi.as_ref().map_or(Connection::Absent, |hdmi| hdmi.sysfs.connection())
    }

    /// Undoes an earlier frontend's forcing of the HDMI connector (which would
    /// read as "no display"). Reading it afterwards probes the sink, so this
    /// waits until the first frame is out.
    pub fn release_forced(&self) {
        if let Some(hdmi) = &self.hdmi {
            let _ = hdmi.sysfs.force(Force::Detect);
        }
    }

    /// Size of the mode HDMI is running, while it is.
    pub fn hdmi_mode(&self) -> Option<(u16, u16)> {
        self.hdmi.as_ref()?.active.as_ref().map(|surface| surface.mode.size())
    }

    /// Switches which screens show the picture and returns the layout in
    /// effect, which falls back to the panel when HDMI cannot be brought up.
    pub fn set_layout(&mut self, wanted: Layout) -> Result<Layout> {
        if wanted == self.layout && (!wanted.game_on_hdmi() || self.hdmi_mode().is_some()) { return Ok(self.layout); }
        match wanted {
            Layout::Panel => {
                self.retire_hdmi();
                self.layout = Layout::Panel;
                self.panel.modeset(&self.card)?;
                Ok(Layout::Panel)
            }
            Layout::Hdmi | Layout::Mirror => match self.activate_hdmi() {
                Ok(shares_panel_crtc) => {
                    // A shared CRTC cannot show two connectors at once: HDMI wins.
                    self.layout = if shares_panel_crtc { Layout::Hdmi } else { wanted };
                    Ok(self.layout)
                }
                Err(error) => {
                    self.retire_hdmi();
                    self.layout = Layout::Panel;
                    // A failed attempt may have taken the panel's CRTC.
                    let _ = self.panel.modeset(&self.card);
                    Err(error)
                }
            },
        }
    }

    /// Brings HDMI up in the configured size (or the sink's nearest); returns
    /// whether it had to share the panel's CRTC.
    fn activate_hdmi(&mut self) -> Result<bool> {
        let hdmi = self.hdmi.as_mut().context("this board has no HDMI connector")?;
        if let Some(active) = hdmi.active.take() { active.retire(&self.card); }
        // Probing reads the sink's EDID, which takes a moment: only when needed. Some
        // sinks raise hot-plug before they answer on DDC, so an empty list is retried.
        let mut info = self.card.get_connector(hdmi.connector, true).context("reading the HDMI connector")?;
        for _ in 0..EDID_RETRIES {
            if info.state() != connector::State::Connected || !info.modes().is_empty() { break; }
            std::thread::sleep(EDID_RETRY_DELAY);
            info = self.card.get_connector(hdmi.connector, true).context("reading the HDMI connector")?;
        }
        if info.state() != connector::State::Connected { anyhow::bail!("no HDMI display is connected"); }
        let infos: Vec<ModeInfo> = info.modes().iter().map(mode_info).collect();
        let mode = *hdmi::pick_mode(&infos, hdmi.wanted).and_then(|index| info.modes().get(index)).context("the HDMI display lists no usable mode")?;
        let resources = self.card.resource_handles().context("reading DRM resources")?;
        let candidates: Vec<crtc::Handle> = info.encoders().iter()
            .filter_map(|&encoder| self.card.get_encoder(encoder).ok())
            .flat_map(|encoder| resources.filter_crtcs(encoder.possible_crtcs()))
            .collect();
        let (crtc, shares) = choose_crtc(&candidates, self.panel.crtc).context("no CRTC can drive HDMI")?;
        let surface = Surface::new(&self.card, hdmi.connector, crtc, mode)?;
        if let Err(error) = surface.modeset(&self.card) {
            surface.retire(&self.card);
            return Err(error);
        }
        eprintln!("ghazal: HDMI {}x{} at {} mHz{}", mode.size().0, mode.size().1, mode_info(&mode).millihertz, if shares { " (sharing the panel's CRTC)" } else { "" });
        hdmi.active = Some(surface);
        Ok(shares)
    }

    fn retire_hdmi(&mut self) {
        if let Some(surface) = self.hdmi.as_mut().and_then(|hdmi| hdmi.active.take()) { surface.retire(&self.card); }
    }

    /// Draws the frame on the screens of the current layout.
    pub fn present(&mut self, frame: &[Rgb8Pixel], stride: usize, damage: Damage) -> Result<()> {
        if self.layout.panel_lit() { self.present_panel(frame, stride, damage)?; }
        self.present_hdmi(frame, stride, damage)
    }

    /// Draws on the handheld's screen alone, e.g. the notice shown while a game runs on HDMI.
    pub fn present_panel(&mut self, frame: &[Rgb8Pixel], stride: usize, (x, y, w, h): Damage) -> Result<()> {
        let pitch = self.panel.buffer.pitch() as usize;
        let (bw, bh) = self.size();
        let w = w.min((bw as usize).min(stride).saturating_sub(x));
        let h = h.min((bh as usize).min(frame.len() / stride).saturating_sub(y));
        {
            let mut map = self.card.map_dumb_buffer(&mut self.panel.buffer)?;
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
            let (width, height) = self.panel.mode.size();
            ClipRect::new(0, 0, width, height)
        };
        // Not every driver implements DIRTYFB; scanout-from-memory ones don't need it.
        let _ = self.card.dirty_framebuffer(self.panel.fb, &[clip]);
        Ok(())
    }

    fn present_hdmi(&mut self, frame: &[Rgb8Pixel], stride: usize, damage: Damage) -> Result<()> {
        let source = {
            let (width, height) = self.size();
            (width as usize, height as usize)
        };
        let Some(surface) = self.hdmi.as_mut().and_then(|hdmi| hdmi.active.as_mut()) else { return Ok(()) };
        let Some(rect) = hdmi::fit((source.0 as u32, source.1 as u32), (u32::from(surface.mode.size().0), u32::from(surface.mode.size().1))) else { return Ok(()) };
        let pitch = surface.buffer.pitch() as usize;
        let mut map = self.card.map_dumb_buffer(&mut surface.buffer)?;
        scale::blit(frame, stride, source, damage, map.as_mut(), pitch, rect);
        Ok(())
    }

    /// Forces the connector the layout does not use off, so an emulator that
    /// takes the first connected connector draws on the right screen.
    fn steer(&self) -> Steering {
        let hdmi_sysfs = self.hdmi.as_ref().map(|hdmi| &hdmi.sysfs);
        let forced = match self.layout {
            Layout::Panel => hdmi_sysfs.filter(|sysfs| sysfs.connection() == Connection::Connected).into_iter().collect::<Vec<_>>(),
            Layout::Hdmi | Layout::Mirror => vec![&self.panel_sysfs],
        };
        Steering(forced.into_iter().filter(|connector| match connector.force(Force::Off) {
            Ok(()) => true,
            Err(error) => { eprintln!("ghazal: steering away from {}: {error:#}", connector.name()); false }
        }).cloned().collect())
    }

    /// Hands the display to another program (RetroArch), which becomes DRM master.
    fn release(&self) -> Result<()> {
        drm::Device::release_master_lock(&self.card).context("releasing DRM master")
    }

    /// Takes the display back and restores our modes and framebuffers. HDMI
    /// that went away during the game is dropped here; the next poll sees it.
    fn reacquire(&mut self) -> Result<()> {
        drm::Device::acquire_master_lock(&self.card).context("reacquiring DRM master")?;
        self.panel.modeset(&self.card)?;
        if let Some(surface) = self.hdmi.as_ref().and_then(|hdmi| hdmi.active.as_ref()) {
            if let Err(error) = surface.modeset(&self.card) {
                eprintln!("ghazal: restoring HDMI: {error:#}");
                self.retire_hdmi();
                self.layout = Layout::Panel;
                self.panel.modeset(&self.card)?;
            }
        }
        Ok(())
    }

    /// Borrow the renderer for the entire handoff. Callers cannot present or
    /// access Kms while the emulator owns DRM, or forget to restore the mode.
    pub fn with_released<T>(&mut self, run: impl FnOnce() -> T) -> Result<T> {
        let steering = self.steer();
        let released = self.release();
        let result = released.map(|()| run());
        steering.restore();
        self.reacquire()?;
        result
    }
}

impl Drop for Kms {
    fn drop(&mut self) {
        let _ = self.card.destroy_framebuffer(self.panel.fb);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_free_crtc_is_preferred_over_the_panels() {
        assert_eq!(choose_crtc(&[1, 2], 1), Some((2, false)));
        assert_eq!(choose_crtc(&[2, 1], 1), Some((2, false)));
        assert_eq!(choose_crtc(&[1], 1), Some((1, true)), "the only candidate is the panel's");
        assert_eq!(choose_crtc::<u32>(&[], 1), None);
    }
}
