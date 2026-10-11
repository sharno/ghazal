//! The initramfs has only oxmux and static BusyBox. Mount the real root, then
//! switch_root frees the initramfs and execs oxmux as PID 1 on that filesystem.
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};

use crate::sys;

pub fn run() -> Result<()> {
    if std::process::id() != 1 {
        bail!("bootstrap must run as PID 1");
    }
    for (source, target, kind) in [
        ("proc", "/proc", "proc"),
        ("sysfs", "/sys", "sysfs"),
        ("devtmpfs", "/dev", "devtmpfs"),
    ] {
        std::fs::create_dir_all(target)?;
        if !sys::is_mounted(Path::new(target)) {
            sys::mount(source, Path::new(target), kind, 0, "")?;
        }
    }
    let command_line = std::fs::read_to_string("/proc/cmdline")?;
    let root = command_line.split_whitespace()
        .find_map(|s| s.strip_prefix("oxmux.root="))
        .ok_or_else(|| anyhow::anyhow!("missing oxmux.root kernel argument"))?;
    // Explicit device paths avoid dependence on udev before the real root boots.
    if !root.starts_with("/dev/") || root.contains("..") {
        bail!("oxmux.root must be an absolute device path");
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    while !Path::new(root).exists() {
        if Instant::now() >= deadline {
            bail!("root device {root} did not appear");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    std::fs::create_dir_all("/sysroot")?;
    sys::mount(root, Path::new("/sysroot"), "ext4", libc::MS_NOATIME, "")?;
    if !Path::new("/sysroot/opt/oxmux/oxmux").is_file() {
        bail!("root filesystem has no oxmux executable");
    }
    for dir in ["dev", "proc", "sys"] {
        let target = format!("/sysroot/{dir}");
        std::fs::create_dir_all(&target)?;
        sys::mount(&format!("/{dir}"), Path::new(&target), "none", libc::MS_MOVE, "")?;
    }
    let error = std::process::Command::new("/bin/busybox")
        .args(["switch_root", "/sysroot", "/opt/oxmux/oxmux", "init"])
        .env("OXMUX_CONFIG_DIR", "/etc/oxmux")
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .exec();
    Err(error.into())
}
