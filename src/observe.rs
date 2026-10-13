//! Device observations use the real kernel interfaces, without taking display ownership.

use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::path::Path;
use std::ptr::NonNull;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use drm::buffer::{DrmFourcc, DrmModifier};
use drm::control::{framebuffer, Device as ControlDevice};

use crate::device::DeviceConfig;

pub fn run(device: &DeviceConfig, args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("audio") if args.len() == 1 => {
            let audio = device.audio.as_ref().context("device has no audio profile")?;
            println!("PCM: {}", fs::read_to_string(format!("/proc/asound/card{}/pcm0p/sub0/status", audio.card))?.trim());
            let volume = crate::hw::alsa::Control::open(audio.card, &audio.control)?;
            println!("{}: actual={:?} range={}..{}", audio.control, volume.read()?, volume.min, volume.max);
            crate::hw::alsa::verify(audio)
        }
        Some("frame") if args.len() == 2 => capture(&device.display.card, Path::new(&args[1])),
        Some("boot") if args.len() == 1 => {
            print!("{}", fs::read_to_string("/run/ghazal/first-frame-ready")?);
            Ok(())
        }
        Some("press") if args.len() == 2 => press(device, &args[1]),
        _ => bail!("usage: ghazal observe audio | boot | frame OUTPUT.ppm | press BUTTON[+BUTTON...]"),
    }
}

struct Card(File);
impl AsFd for Card { fn as_fd(&self) -> BorrowedFd<'_> { self.0.as_fd() } }
impl drm::Device for Card {}
impl ControlDevice for Card {}

fn active_framebuffer(path: &Path) -> Result<(Card, framebuffer::PlanarInfo)> {
    let mut paths = if path.is_dir() {
        fs::read_dir(path)?.collect::<std::io::Result<Vec<_>>>()?.into_iter()
            .map(|entry| entry.path()).filter(|path| path.file_name().is_some_and(|name| {
                name.to_str().is_some_and(|name| name.strip_prefix("card").is_some_and(|suffix| !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())))
            })).collect::<Vec<_>>()
    } else { vec![path.to_owned()] };
    paths.sort();
    for path in paths {
        let card = Card(OpenOptions::new().read(true).write(true).open(&path)?);
        let resources = match card.resource_handles() {
            Ok(resources) => resources,
            Err(error) if error.raw_os_error() == Some(libc::EOPNOTSUPP) => continue,
            Err(error) => return Err(error).context("reading display resources"),
        };
        for crtc in resources.crtcs() {
            let info = card.get_crtc(*crtc)?;
            if let Some(handle) = info.framebuffer().filter(|_| info.mode().is_some()) {
                let framebuffer = card.get_planar_framebuffer(handle)?;
                return Ok((card, framebuffer));
            }
        }
    }
    bail!("no active framebuffer at {}", path.display())
}

/// Only a successfully mapped, cache-synchronized buffer can become a Mapping.
struct Mapping { address: NonNull<libc::c_void>, length: usize, fd: OwnedFd }
impl Mapping {
    fn open(fd: OwnedFd, length: usize) -> Result<Self> {
        // SAFETY: mmap is read-only, for a bounded validated buffer and an owned fd.
        let address = unsafe { libc::mmap(std::ptr::null_mut(), length, libc::PROT_READ, libc::MAP_SHARED, fd.as_raw_fd(), 0) };
        if address == libc::MAP_FAILED { return Err(std::io::Error::last_os_error()).context("mapping framebuffer"); }
        let Some(address) = NonNull::new(address) else {
            // SAFETY: a successful null-address mapping still needs unmapping.
            unsafe { libc::munmap(std::ptr::null_mut(), length); }
            bail!("null framebuffer mapping");
        };
        let mut flags = 1u64; // DMA_BUF_SYNC_START | DMA_BUF_SYNC_READ
        // SAFETY: ioctl takes a valid u64 pointer; this fd came from DRM PRIME export.
        if unsafe { libc::ioctl(fd.as_raw_fd(), 0x40086200, &mut flags) } < 0 {
            let error = std::io::Error::last_os_error();
            // SAFETY: unmap exactly the successful mapping above.
            unsafe { libc::munmap(address.as_ptr(), length); }
            return Err(error).context("synchronizing framebuffer");
        }
        Ok(Self { address, length, fd })
    }
    fn bytes(&self) -> &[u8] {
        // SAFETY: the read-only mapping remains live and cache synchronized for self.
        unsafe { std::slice::from_raw_parts(self.address.as_ptr().cast(), self.length) }
    }
}
impl Drop for Mapping {
    fn drop(&mut self) {
        let mut flags = 5u64; // DMA_BUF_SYNC_END | DMA_BUF_SYNC_READ
        // SAFETY: valid DMA-BUF fd and sync flags; then unmap its owned mapping.
        unsafe { libc::ioctl(self.fd.as_raw_fd(), 0x40086200, &mut flags); libc::munmap(self.address.as_ptr(), self.length); }
    }
}

fn capture(card_path: &Path, output: &Path) -> Result<()> {
    let (card, framebuffer) = active_framebuffer(card_path)?;
    // ARGB8888 (e.g. PPSSPP's scanout) has the same layout; alpha is ignored.
    if !matches!(framebuffer.pixel_format(), DrmFourcc::Xrgb8888 | DrmFourcc::Argb8888) || framebuffer.modifier().is_some_and(|modifier| modifier != DrmModifier::Linear) {
        bail!("capture requires linear XRGB8888 or ARGB8888, got {:?}/{:?}", framebuffer.pixel_format(), framebuffer.modifier());
    }
    let (width, height) = framebuffer.size();
    let pitch = framebuffer.pitches()[0] as usize;
    let offset = framebuffer.offsets()[0] as usize;
    if width == 0 || height == 0 || width > 4096 || height > 4096 || pitch < width as usize * 4 { bail!("invalid framebuffer dimensions"); }
    let length = pitch.checked_mul(height as usize).and_then(|size| size.checked_add(offset)).context("framebuffer size overflow")?;
    if length > 64 * 1024 * 1024 { bail!("framebuffer exceeds capture limit"); }
    let handle = framebuffer.buffers()[0].context("framebuffer handle unavailable; root access is required")?;
    let mapping = Mapping::open(card.buffer_to_prime_fd(handle, libc::O_CLOEXEC as u32)?, length)?;
    let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
    for row in 0..height as usize {
        for column in 0..width as usize {
            let index = offset + row * pitch + column * 4;
            rgb.extend_from_slice(&[mapping.bytes()[index + 2], mapping.bytes()[index + 1], mapping.bytes()[index]]);
        }
    }
    drop(mapping);
    // Refuse to overwrite an earlier observation.
    let mut file = BufWriter::new(OpenOptions::new().write(true).create_new(true).open(output)?);
    write!(file, "P6\n{width} {height}\n255\n")?;
    file.write_all(&rgb)?;
    file.flush()?;
    println!("captured {width}x{height} to {}", output.display());
    Ok(())
}

struct Button { file: File, code: u16 }
impl Button {
    fn emit(&self, value: i32) -> Result<()> {
        let events = [libc::input_event { time: libc::timeval { tv_sec: 0, tv_usec: 0 }, type_: 1, code: self.code, value }, libc::input_event { time: libc::timeval { tv_sec: 0, tv_usec: 0 }, type_: 0, code: 0, value: 0 }];
        // SAFETY: input_event is the kernel ABI and both records are fully initialized.
        let bytes = unsafe { std::slice::from_raw_parts(events.as_ptr().cast::<u8>(), std::mem::size_of_val(&events)) };
        (&self.file).write_all(bytes)?;
        Ok(())
    }
}
struct Pressed(Vec<Button>);
impl Drop for Pressed { fn drop(&mut self) { for button in self.0.iter().rev() { let _ = button.emit(0); } } }

fn press(device: &DeviceConfig, chord: &str) -> Result<()> {
    let mut codes = std::collections::BTreeSet::new();
    let mut buttons = Vec::new();
    for name in chord.split('+') {
        let code = *device.buttons.get(name).with_context(|| format!("unknown button {name:?}"))?;
        if !codes.insert(code) { bail!("duplicate button {name:?}"); }
        let matches = evdev::enumerate().filter(|(_, input)| input.supported_keys().is_some_and(|keys| keys.contains(evdev::Key::new(code))) &&
            (code < 0x100 || device.input.gamepad.as_ref().is_some_and(|name| input.name().is_some_and(|actual| actual.contains(name)))))
            .map(|(path, _)| path).collect::<Vec<_>>();
        if matches.len() != 1 { bail!("button {name:?} requires exactly one input device, found {}", matches.len()); }
        buttons.push(Button { file: OpenOptions::new().write(true).open(&matches[0])?, code });
    }
    let pressed = Pressed(buttons);
    for button in &pressed.0 { button.emit(1)?; }
    std::thread::sleep(Duration::from_millis(200));
    drop(pressed);
    Ok(())
}
