//! Daemon-owned live levels in RAM; atomic replacement wakes the frontend via inotify.
use std::path::{Path, PathBuf};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct Percent(u8);
impl TryFrom<u8> for Percent {
    type Error = anyhow::Error;
    fn try_from(value: u8) -> Result<Self> {
        if value > 100 { bail!("percentage exceeds 100: {value}"); }
        Ok(Self(value))
    }
}
impl From<Percent> for u8 { fn from(value: Percent) -> Self { value.0 } }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Levels { pub brightness: Percent, pub volume: Percent }
impl Levels {
    pub fn new(brightness: u8, volume: u8) -> Result<Self> {
        Ok(Self { brightness: brightness.try_into()?, volume: volume.try_into()? })
    }
    pub fn publish(self, socket: &Path) -> Result<()> {
        crate::sys::write_atomic(&path(socket), toml::to_string(&self)?.as_bytes())
    }
}
fn path(socket: &Path) -> PathBuf { socket.with_extension("levels.toml") }
pub fn read(socket: &Path) -> Result<Option<Levels>> {
    match std::fs::read_to_string(path(socket)) {
        Ok(text) => Ok(Some(toml::from_str(&text)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub struct Watch { fd: OwnedFd }
impl Watch {
    pub fn open(socket: &Path) -> Result<Option<Self>> {
        let directory = socket.parent().context("daemon socket has no directory")?;
        if !directory.exists() { return Ok(None); }
        // SAFETY: no pointers; a successful descriptor is immediately owned.
        let raw = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if raw < 0 { return Err(std::io::Error::last_os_error().into()); }
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        use std::os::unix::ffi::OsStrExt;
        let directory = std::ffi::CString::new(directory.as_os_str().as_bytes())?;
        // SAFETY: valid descriptor and terminated directory string.
        if unsafe { libc::inotify_add_watch(fd.as_raw_fd(), directory.as_ptr(), libc::IN_MOVED_TO | libc::IN_CREATE | libc::IN_DELETE_SELF | libc::IN_MOVE_SELF) } < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(Some(Self { fd }))
    }
    pub fn fd(&self) -> std::os::fd::RawFd { self.fd.as_raw_fd() }
    pub fn drain(&self) -> Result<bool> {
        let mut changed = false;
        let mut bytes = [0u8; 4096];
        loop {
            // SAFETY: valid owned descriptor and writable buffer.
            let count = unsafe { libc::read(self.fd(), bytes.as_mut_ptr().cast(), bytes.len()) };
            if count > 0 {
                let mut events = &bytes[..count as usize];
                while events.len() >= 16 {
                    // inotify_event contains four native-endian 32-bit fields, then name[len].
                    let mask = u32::from_ne_bytes(events[4..8].try_into()?);
                    let length = u32::from_ne_bytes(events[12..16].try_into()?) as usize;
                    if mask & (libc::IN_IGNORED | libc::IN_DELETE_SELF | libc::IN_MOVE_SELF) != 0 {
                        bail!("live levels directory was removed or moved");
                    }
                    events = events.get(16..).and_then(|tail| tail.get(length..)).context("truncated inotify event")?;
                }
                if !events.is_empty() { bail!("truncated inotify header"); }
                changed = true;
                continue;
            }
            if count == 0 { bail!("levels watch closed"); }
            let error = std::io::Error::last_os_error();
            match error.kind() {
                std::io::ErrorKind::WouldBlock => return Ok(changed),
                std::io::ErrorKind::Interrupted => continue,
                _ => return Err(error.into()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn percentages_are_validated() {
        assert!(Levels::new(101, 60).is_err());
        assert!(toml::from_str::<Levels>("brightness=50\nvolume=255").is_err());
        let levels = Levels::new(100, 0).unwrap();
        assert_eq!(toml::from_str::<Levels>(&toml::to_string(&levels).unwrap()).unwrap(), levels);
    }
    #[test]
    fn atomic_publication_wakes_watch() {
        let dir = std::env::temp_dir().join(format!("oxmux-levels-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("daemon.sock");
        let watch = Watch::open(&socket).unwrap().unwrap();
        assert!(!watch.drain().unwrap());
        let levels = Levels::new(75, 60).unwrap();
        levels.publish(&socket).unwrap();
        assert!(watch.drain().unwrap());
        assert_eq!(read(&socket).unwrap(), Some(levels));
        assert!(!watch.drain().unwrap());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
