use std::fs;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;
use std::process::{Child, ExitStatus};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

const SESSION: &str = "/run/oxmux/emulator.toml";

#[derive(Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Record {
    pid: i32,
    start_ticks: u64,
}

fn start_ticks(pid: i32) -> Result<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let fields = stat.rsplit_once(") ").context("invalid process stat")?.1;
    Ok(fields.split_whitespace().nth(19).context("missing process start time")?.parse()?)
}

struct Control {
    fd: OwnedFd,
}

impl Control {
    fn open() -> Result<Option<Self>> {
        let contents = match fs::read_to_string(SESSION) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let record: Record = toml::from_str(&contents)?;
        if record.pid <= 1 { bail!("invalid emulator PID"); }
        // SAFETY: pidfd_open takes scalar arguments and returns a new owned fd.
        let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, record.pid, 0) };
        if raw < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) { return Ok(None); }
            return Err(error.into());
        }
        // SAFETY: successful pidfd_open returned an fd owned solely here.
        let fd = unsafe { OwnedFd::from_raw_fd(raw as i32) };
        match start_ticks(record.pid) {
            Ok(ticks) if ticks == record.start_ticks => Ok(Some(Self { fd })),
            Ok(_) => Ok(None),
            Err(error) if error.downcast_ref::<std::io::Error>().is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn signal(&self, signal: i32) -> Result<()> {
        // SAFETY: a live owned pidfd identifies the process without PID reuse races.
        let result = unsafe { libc::syscall(libc::SYS_pidfd_send_signal, self.fd.as_raw_fd(), signal, std::ptr::null::<libc::siginfo_t>(), 0) };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) { return Err(error.into()); }
        }
        Ok(())
    }

    fn exited(&self, timeout_ms: i32) -> Result<bool> {
        let mut fd = libc::pollfd { fd: self.fd.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        // SAFETY: valid pollfd referencing the owned pidfd.
        let result = unsafe { libc::poll(&mut fd, 1, timeout_ms) };
        if result < 0 { return Err(std::io::Error::last_os_error().into()); }
        Ok(result > 0)
    }
}

pub fn active() -> Result<bool> {
    match Control::open()? {
        Some(control) => Ok(!control.exited(0)?),
        None => Ok(false),
    }
}

/// RetroArch writes save RAM and its automatic save state while quitting; a
/// PSP state is tens of MB, which can take seconds on the SD card.
const QUIT_GRACE_MS: i32 = 10_000;

pub fn stop() -> Result<()> {
    let Some(control) = Control::open()? else { bail!("no emulator is running"); };
    control.signal(libc::SIGTERM)?;
    if !control.exited(QUIT_GRACE_MS)? {
        control.signal(libc::SIGKILL)?;
        if !control.exited(1500)? { bail!("emulator did not exit after SIGKILL"); }
    }
    Ok(())
}

pub struct Session {
    child: Child,
    record: Option<Record>,
}

impl Session {
    pub fn register(mut child: Child) -> Result<Self> {
        if child.try_wait()?.is_some() { return Ok(Self { child, record: None }); }
        let ticks = match start_ticks(child.id() as i32) {
            Ok(ticks) => ticks,
            Err(error) => {
                if child.try_wait()?.is_some() { return Ok(Self { child, record: None }); }
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        let record = Record { pid: child.id() as i32, start_ticks: ticks };
        if let Err(error) = crate::sys::write_atomic(Path::new(SESSION), toml::to_string(&record)?.as_bytes()) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        Ok(Self { child, record: Some(record) })
    }

    pub fn wait(&mut self) -> Result<ExitStatus> { Ok(self.child.wait()?) }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(record) = &self.record {
            let _ = self.child.kill();
            let _ = self.child.wait();
            if fs::read_to_string(SESSION).ok().and_then(|text| toml::from_str::<Record>(&text).ok()).as_ref() == Some(record) {
                let _ = fs::remove_file(SESSION);
            }
        }
    }
}
