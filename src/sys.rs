//! Thin wrappers over the Linux syscalls oxmux uses in place of shell tools
//! (mount, modprobe, swapon, kill, reboot, ...).

use std::ffi::CString;
use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};

fn cstr(p: impl AsRef<std::ffi::OsStr>) -> Result<CString> {
    CString::new(p.as_ref().as_bytes()).context("path contains NUL")
}

fn last_err() -> std::io::Error {
    std::io::Error::last_os_error()
}

const HANDLED_SIGNALS: [i32; 6] = [libc::SIGCHLD, libc::SIGTERM, libc::SIGINT, libc::SIGUSR1, libc::SIGUSR2, libc::SIGHUP];

/// Receives process signals through a file descriptor instead of async handlers.
pub struct SignalFd(OwnedFd);

impl SignalFd {
    pub fn new() -> Result<Self> {
        // SAFETY: plain signal-mask manipulation on a zeroed sigset_t.
        unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            for s in HANDLED_SIGNALS {
                libc::sigaddset(&mut set, s);
            }
            if libc::sigprocmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) < 0 {
                bail!("sigprocmask: {}", last_err());
            }
            let fd = libc::signalfd(-1, &set, libc::SFD_CLOEXEC | libc::SFD_NONBLOCK);
            if fd < 0 {
                bail!("signalfd: {}", last_err());
            }
            Ok(Self(OwnedFd::from_raw_fd(fd)))
        }
    }

    /// Waits up to `timeout` (forever if None) and returns the next pending signal.
    pub fn wait(&self, timeout: Option<Duration>) -> Result<Option<i32>> {
        let mut pfd = libc::pollfd { fd: self.0.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        let ms = timeout.map_or(-1, |t| t.as_millis().min(i32::MAX as u128) as i32);
        // SAFETY: one valid pollfd.
        if unsafe { libc::poll(&mut pfd, 1, ms) } < 0 {
            let e = last_err();
            if e.kind() == std::io::ErrorKind::Interrupted {
                return Ok(None);
            }
            return Err(e.into());
        }
        let mut info: libc::signalfd_siginfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::signalfd_siginfo>();
        // SAFETY: reading one siginfo struct into a correctly sized buffer.
        let n = unsafe { libc::read(self.0.as_raw_fd(), (&mut info as *mut libc::signalfd_siginfo).cast(), size) };
        Ok((n == size as isize).then_some(info.ssi_signo as i32))
    }
}

/// Unblocks all signals. Called in children between fork and exec, because the signal
/// mask is inherited and SignalFd blocks the signals it handles.
pub fn reset_signal_mask() {
    // SAFETY: async-signal-safe.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigprocmask(libc::SIG_SETMASK, &set, std::ptr::null_mut());
    }
}

/// Reaps one exited child. Returns (pid, exit status or 128 + signal).
pub fn reap_any() -> Option<(i32, i32)> {
    let mut status = 0;
    // SAFETY: plain waitpid.
    let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
    if pid <= 0 {
        return None;
    }
    let code = if libc::WIFEXITED(status) { libc::WEXITSTATUS(status) } else { 128 + libc::WTERMSIG(status) };
    Some((pid, code))
}

pub fn kill(pid: i32, signal: i32) {
    // SAFETY: plain kill.
    unsafe { libc::kill(pid, signal) };
}

/// SIGTERMs every process whose /proc/<pid>/comm equals `name`.
pub fn kill_by_name(name: &str) {
    let me = std::process::id() as i32;
    let Ok(entries) = std::fs::read_dir("/proc") else { return };
    for entry in entries.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse::<i32>().ok()) else { continue };
        if pid == me || pid == 1 {
            continue;
        }
        let comm = std::fs::read_to_string(entry.path().join("comm")).unwrap_or_default();
        if comm.trim_end() == name {
            eprintln!("oxmux: stopping {name} (pid {pid})");
            kill(pid, libc::SIGTERM);
        }
    }
}

pub fn mount(source: &str, target: &Path, fstype: &str, flags: libc::c_ulong, data: &str) -> Result<()> {
    let (s, t, f, d) = (cstr(source)?, cstr(target)?, cstr(fstype)?, cstr(data)?);
    let data_ptr = if data.is_empty() { std::ptr::null() } else { d.as_ptr().cast() };
    // SAFETY: all pointers are valid NUL-terminated strings (or null for no data).
    if unsafe { libc::mount(s.as_ptr(), t.as_ptr(), f.as_ptr(), flags, data_ptr) } < 0 {
        bail!("mount {source} on {} ({fstype}): {}", target.display(), last_err());
    }
    Ok(())
}

pub fn umount(target: &Path) -> Result<()> {
    let t = cstr(target)?;
    // SAFETY: valid path.
    if unsafe { libc::umount2(t.as_ptr(), libc::MNT_DETACH) } < 0 {
        bail!("umount {}: {}", target.display(), last_err());
    }
    Ok(())
}

/// True if something is mounted exactly at `target`.
pub fn is_mounted(target: &Path) -> bool {
    std::fs::read_to_string("/proc/self/mounts")
        .map(|m| m.lines().any(|l| l.split(' ').nth(1).is_some_and(|t| Path::new(t) == target)))
        .unwrap_or(false)
}

/// Loads a kernel module from an open .ko file (the syscall behind insmod).
pub fn finit_module(file: &File, params: &str) -> Result<bool> {
    let p = cstr(params)?;
    // SAFETY: valid fd and NUL-terminated params.
    let r = unsafe { libc::syscall(libc::SYS_finit_module, file.as_raw_fd(), p.as_ptr(), 0) };
    if r < 0 {
        let e = last_err();
        if e.raw_os_error() == Some(libc::EEXIST) {
            return Ok(false);
        }
        bail!("finit_module: {e}");
    }
    Ok(true)
}

pub fn swapon(device: &Path, priority: i32) -> Result<()> {
    let d = cstr(device)?;
    // <sys/swap.h>: SWAP_FLAG_PREFER, priority in the low 15 bits.
    const SWAP_FLAG_PREFER: i32 = 0x8000;
    const SWAP_FLAG_PRIO_MASK: i32 = 0x7fff;
    let flags = SWAP_FLAG_PREFER | (priority & SWAP_FLAG_PRIO_MASK);
    // SAFETY: valid path.
    if unsafe { libc::swapon(d.as_ptr(), flags) } < 0 {
        bail!("swapon {}: {}", device.display(), last_err());
    }
    Ok(())
}

pub fn swapoff_all() {
    let Ok(swaps) = std::fs::read_to_string("/proc/swaps") else { return };
    for line in swaps.lines().skip(1) {
        if let Some(Ok(dev)) = line.split_whitespace().next().map(cstr) {
            // SAFETY: valid path.
            unsafe { libc::swapoff(dev.as_ptr()) };
        }
    }
}

pub fn sethostname(name: &str) -> Result<()> {
    // SAFETY: pointer/length pair describes `name`.
    if unsafe { libc::sethostname(name.as_ptr().cast(), name.len()) } < 0 {
        bail!("sethostname: {}", last_err());
    }
    Ok(())
}

pub fn sync() {
    // SAFETY: no arguments.
    unsafe { libc::sync() };
}

/// Final step of shutdown when we are PID 1. Does not return on success.
pub fn reboot(cmd: libc::c_int) -> Result<()> {
    sync();
    // SAFETY: plain reboot syscall.
    if unsafe { libc::reboot(cmd) } < 0 {
        bail!("reboot: {}", last_err());
    }
    Ok(())
}

pub fn kernel_release() -> String {
    // SAFETY: uname fills the struct.
    unsafe {
        let mut u: libc::utsname = std::mem::zeroed();
        libc::uname(&mut u);
        std::ffi::CStr::from_ptr(u.release.as_ptr()).to_string_lossy().into_owned()
    }
}

/// Writes `value` to a sysfs/procfs/debugfs file (no trailing newline added).
pub fn write_file(path: &Path, value: &str) -> Result<()> {
    std::fs::write(path, value).with_context(|| format!("writing {:?} to {}", value, path.display()))
}
