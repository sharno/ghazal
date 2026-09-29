//! `oxmux init`: replaces muOS's script/init/sysinit and its S* scripts.
//!
//! Runs the ordered boot plan from system.toml, then hands over to the supervisor.
//! Works in two modes:
//! - as PID 1 (our own image): also reaps orphans and performs poweroff/reboot itself;
//! - as busybox init's `::sysinit` action (muOS rootfs): runs the plan, forks the
//!   supervisor into the background and returns so busybox init carries on.

mod modules;
mod zram;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::device::DeviceConfig;
use crate::supervisor::{Stop, Supervisor};
use crate::sys;
use crate::system::SystemConfig;

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct InitConfig {
    #[serde(default)]
    pub rescue: Option<Rescue>,
    /// Ordered boot steps.
    #[serde(rename = "step", default)]
    pub steps: Vec<Step>,
}

/// Falls back to the previous init if oxmux keeps failing to boot. The frontend resets
/// the counter once it has drawn its first frame.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rescue {
    pub counter: PathBuf,
    #[serde(default = "default_attempts")]
    pub max_attempts: u32,
    /// Exec'd in place of oxmux after `max_attempts` boots that never reached the UI.
    pub exec: Vec<String>,
}

fn default_attempts() -> u32 {
    3
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Step {
    Mount(MountStep),
    Modprobe(ModuleSpec),
    Write(WriteStep),
    Mkdir(PathBuf),
    Symlink(SymlinkStep),
    Zram(zram::ZramStep),
    Hostname(String),
    /// Wait for a path (e.g. a device node) to appear.
    Wait(WaitStep),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MountStep {
    pub source: String,
    /// Used instead of `source` when it exists (e.g. SD2 before SD1).
    pub prefer: Option<String>,
    pub target: PathBuf,
    /// Comma-separated list: each is tried in turn (e.g. "exfat,vfat,ext4").
    pub fstype: String,
    #[serde(default)]
    pub options: String,
    /// Seconds to wait for `source` to appear if it is a device path.
    #[serde(default)]
    pub wait: u64,
    #[serde(default)]
    pub optional: bool,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum ModuleSpec {
    Name(String),
    WithParams { name: String, params: String, #[serde(default)] optional: bool },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriteStep {
    pub path: PathBuf,
    pub value: String,
    #[serde(default)]
    pub optional: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SymlinkStep {
    pub target: PathBuf,
    pub link: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaitStep {
    pub path: PathBuf,
    #[serde(default = "default_wait")]
    pub timeout: u64,
}

fn default_wait() -> u64 {
    5
}

pub fn run(system: SystemConfig, device: &DeviceConfig) -> Result<()> {
    let pid1 = std::process::id() == 1;
    log(format_args!("starting (pid1={pid1}, kernel {})", sys::kernel_release()));
    // The kernel starts init with an almost empty environment; services use bare names.
    if std::env::var_os("PATH").is_none() {
        std::env::set_var("PATH", "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin");
    }

    if let Some(rescue) = &system.init.rescue {
        check_rescue(rescue)?;
    }

    let started = Instant::now();
    for step in &system.init.steps {
        let t = Instant::now();
        if let Err(e) = run_step(step) {
            // Keep booting: a missing optional piece shouldn't leave the device dead.
            log(format_args!("step {step:?} failed: {e:#}"));
        } else if t.elapsed() > Duration::from_millis(200) {
            log(format_args!("step took {:?}: {step:?}", t.elapsed()));
        }
    }
    log(format_args!("boot plan done in {:?}", started.elapsed()));

    if pid1 {
        run_pid1(&system, device)
    } else {
        // SAFETY: single-threaded at this point; the child only continues in Rust code.
        match unsafe { libc::fork() } {
            -1 => bail!("fork: {}", std::io::Error::last_os_error()),
            0 => {
                // SAFETY: plain setsid in the child.
                unsafe { libc::setsid() };
                let mut sup = Supervisor::new(&system.supervisor, false)?;
                sup.run()?;
                std::process::exit(0);
            }
            _ => Ok(()),
        }
    }
}

fn run_pid1(system: &SystemConfig, device: &DeviceConfig) -> Result<()> {
    let stop = match Supervisor::new(&system.supervisor, true).and_then(|mut s| s.run()) {
        Ok(stop) => stop,
        Err(e) => {
            log(format_args!("supervisor failed: {e:#}; rebooting"));
            Stop::Reboot
        }
    };
    shutdown();
    let cmd = match stop {
        Stop::Poweroff => libc::RB_POWER_OFF,
        Stop::Halt => libc::RB_HALT_SYSTEM,
        Stop::Reboot | Stop::Exit => libc::RB_AUTOBOOT,
    };
    if let Err(e) = sys::reboot(cmd) {
        log(format_args!("{e:#}"));
    }
    // Still here: some boards need a PMIC register poke to cut power.
    if let Some(w) = &device.poweroff.fallback_write {
        let _ = sys::write_file(&w.path, &w.value);
    }
    // PID 1 must never exit.
    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}

/// Unmounts everything we can (reverse mount order) after services have stopped.
fn shutdown() {
    sys::sync();
    sys::swapoff_all();
    let mounts = std::fs::read_to_string("/proc/self/mounts").unwrap_or_default();
    let keep = ["/", "/proc", "/sys", "/dev", "/dev/pts", "/run"];
    for target in mounts.lines().rev().filter_map(|l| l.split(' ').nth(1)) {
        if !keep.contains(&target) {
            let _ = sys::umount(Path::new(target));
        }
    }
    sys::sync();
}

fn check_rescue(rescue: &Rescue) -> Result<()> {
    let attempts: u32 = std::fs::read_to_string(&rescue.counter).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
    if attempts >= rescue.max_attempts && !rescue.exec.is_empty() {
        log(format_args!("{attempts} boots never reached the UI; handing over to {:?}", rescue.exec));
        let _ = std::fs::write(&rescue.counter, "0");
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new(&rescue.exec[0]).args(&rescue.exec[1..]).exec();
        bail!("exec rescue: {err}");
    }
    if let Some(dir) = rescue.counter.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let next = (attempts + 1).to_string();
    if std::fs::write(&rescue.counter, &next).is_err() {
        // The root filesystem may still be read-only this early.
        let _ = sys::mount("none", Path::new("/"), "none", libc::MS_REMOUNT, "");
        if let Err(e) = std::fs::write(&rescue.counter, &next) {
            log(format_args!("can't record boot attempt, rescue disabled: {e}"));
        }
    }
    Ok(())
}

/// Called by the frontend once it is on screen: this boot counts as good.
pub fn mark_boot_ok() {
    if let Some(counter) = std::env::var_os("OXMUX_BOOT_COUNTER") {
        let _ = std::fs::write(counter, "0");
    }
}

fn run_step(step: &Step) -> Result<()> {
    match step {
        Step::Mount(m) => mount(m),
        Step::Modprobe(spec) => {
            let (name, params, optional) = match spec {
                ModuleSpec::Name(n) => (n.as_str(), "", false),
                ModuleSpec::WithParams { name, params, optional } => (name.as_str(), params.as_str(), *optional),
            };
            match modules::load(name, params) {
                Err(e) if optional => {
                    log(format_args!("optional module {name}: {e:#}"));
                    Ok(())
                }
                r => r,
            }
        }
        Step::Write(w) => match sys::write_file(&w.path, &w.value) {
            Err(_) if w.optional && !w.path.exists() => Ok(()),
            r => r,
        },
        Step::Mkdir(p) => std::fs::create_dir_all(p).with_context(|| format!("mkdir {}", p.display())),
        Step::Symlink(s) => {
            if s.link.symlink_metadata().is_ok() {
                std::fs::remove_file(&s.link).ok();
            }
            std::os::unix::fs::symlink(&s.target, &s.link)
                .with_context(|| format!("symlink {} -> {}", s.link.display(), s.target.display()))
        }
        Step::Zram(z) => zram::setup(z),
        Step::Hostname(h) => sys::sethostname(h),
        Step::Wait(w) => {
            if wait_for(&w.path, Duration::from_secs(w.timeout)) {
                Ok(())
            } else {
                bail!("{} did not appear within {}s", w.path.display(), w.timeout)
            }
        }
    }
}

fn wait_for(path: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while !path.exists() {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    true
}

fn mount(m: &MountStep) -> Result<()> {
    let (flags, data) = parse_mount_options(&m.options);
    if flags & libc::MS_REMOUNT == 0 && sys::is_mounted(&m.target) {
        return Ok(());
    }
    let source = m.prefer.as_deref().filter(|p| Path::new(p).exists()).unwrap_or(&m.source);
    let result = (|| {
        if source.starts_with("/") && m.wait > 0 && !wait_for(Path::new(source), Duration::from_secs(m.wait)) {
            bail!("{source} did not appear");
        }
        if source.starts_with("/") && flags & libc::MS_BIND != 0 && !Path::new(source).exists() {
            bail!("{source} does not exist");
        }
        std::fs::create_dir_all(&m.target).with_context(|| format!("mkdir {}", m.target.display()))?;
        let mut last = None;
        for fstype in m.fstype.split(',').map(str::trim) {
            match sys::mount(source, &m.target, fstype, flags, &data) {
                Ok(()) => return Ok(()),
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| anyhow::anyhow!("no fstype given")))
    })();
    match result {
        Err(e) if m.optional => {
            log(format_args!("optional mount {}: {e:#}", m.target.display()));
            Ok(())
        }
        r => r,
    }
}

/// Splits `mount -o` style options into MS_* flags and the filesystem data string.
fn parse_mount_options(options: &str) -> (libc::c_ulong, String) {
    let mut flags = 0;
    let mut data = Vec::new();
    for opt in options.split(',').map(str::trim).filter(|o| !o.is_empty()) {
        let flag = match opt {
            "ro" => libc::MS_RDONLY,
            "rw" | "defaults" => 0,
            "nosuid" => libc::MS_NOSUID,
            "nodev" => libc::MS_NODEV,
            "noexec" => libc::MS_NOEXEC,
            "sync" => libc::MS_SYNCHRONOUS,
            "noatime" => libc::MS_NOATIME,
            "nodiratime" => libc::MS_NODIRATIME,
            "relatime" => libc::MS_RELATIME,
            "bind" => libc::MS_BIND,
            "rbind" => libc::MS_BIND | libc::MS_REC,
            "remount" => libc::MS_REMOUNT,
            other => {
                data.push(other);
                continue;
            }
        };
        flags |= flag;
    }
    (flags, data.join(","))
}

fn log(args: std::fmt::Arguments) {
    eprintln!("oxmux-init: {args}");
    // Also reach the kernel log: during early boot stderr may go nowhere.
    let _ = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/kmsg")
        .and_then(|mut f| std::io::Write::write_all(&mut f, format!("oxmux-init: {args}\n").as_bytes()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_options() {
        let (flags, data) = parse_mount_options("rw,noatime,uid=0,gid=0,fmask=0000");
        assert_eq!(flags, libc::MS_NOATIME);
        assert_eq!(data, "uid=0,gid=0,fmask=0000");
        assert_eq!(parse_mount_options("rbind").0, libc::MS_BIND | libc::MS_REC);
    }
}
