//! `oxmux init`: replaces muOS's script/init/sysinit and its S* scripts.
//!
//! Runs the ordered boot plan from system.toml, then hands over to the supervisor.
//! Works in two modes:
//! - as PID 1 (our own image): also reaps orphans and performs poweroff/reboot itself;
//! - as busybox init's `::sysinit` action (muOS rootfs): runs the plan, forks the
//!   supervisor into the background and returns so busybox init carries on.

mod modules;
mod storage;
mod provision;
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
    /// Load drivers for all detected devices (modules.alias).
    Coldplug(ColdplugStep),
    Hostname(String),
    /// Wait for a path (e.g. a device node) to appear.
    Wait(WaitStep),
    Storage(storage::Prepare),
    DataDirectories(storage::Prepare),
    Provision(PathBuf),
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

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct ColdplugStep {
    /// Modules never to load automatically (e.g. a wifi driver to save power).
    pub exclude: Vec<String>,
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
    system.validate()?;
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
        run_step(step).with_context(|| format!("required boot step {step:?}"))?;
        if t.elapsed() > Duration::from_millis(200) {
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
    let stop = Supervisor::new(&system.supervisor, true)?.run()?;
    shutdown(stop, &system.supervisor.log_dir);
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
fn shutdown(stop: Stop, log_dir: &Path) {
    // The kernel log service is gone by now: keep a durable record of the last
    // shutdown, closed just before the root filesystem becomes read-only.
    let mut trace = ShutdownTrace::open(&log_dir.join("shutdown.log"));
    trace.note(format_args!("services stopped ({stop:?})"));
    match crate::emulator::active() {
        Ok(true) => { if let Err(error) = crate::emulator::stop() { trace.note(format_args!("stopping emulator: {error:#}")); } },
        Ok(false) => {},
        Err(error) => trace.note(format_args!("checking emulator at shutdown: {error:#}")),
    }
    // Anything still alive (SSH sessions, detached helpers) can hold files open
    // for writing and make the read-only remount fail, leaving ext4 dirty.
    for (signal, grace) in [(libc::SIGTERM, Duration::from_secs(2)), (libc::SIGKILL, Duration::from_secs(1))] {
        sys::kill(-1, signal);
        if sys::wait_no_children(grace) { break; }
        trace.note(format_args!("processes survived signal {signal}"));
    }
    sys::sync();
    sys::swapoff_all();
    let mounts = std::fs::read_to_string("/proc/self/mounts").unwrap_or_default();
    let keep = ["/", "/proc", "/sys", "/dev", "/dev/pts", "/run"];
    for target in mounts.lines().rev().filter_map(|l| l.split(' ').nth(1)) {
        if !keep.contains(&target) {
            if let Err(error) = sys::umount(Path::new(target)) {
                trace.note(format_args!("{error:#}; remounting {target} read-only"));
                if let Err(error) = sys::mount("none", Path::new(target), "none", libc::MS_REMOUNT | libc::MS_RDONLY, "") {
                    trace.note(format_args!("could not make {target} read-only: {error:#}"));
                }
            }
        }
    }
    trace.note(format_args!("remounting / read-only"));
    drop(trace);
    sys::sync();
    if let Err(error) = sys::mount("none", Path::new("/"), "none", libc::MS_REMOUNT | libc::MS_RDONLY, "") {
        log(format_args!("could not remount root read-only: {error:#}"));
    }
}

/// Shutdown steps, logged and written durably to a file replaced on each shutdown.
struct ShutdownTrace(Option<std::fs::File>);

impl ShutdownTrace {
    fn open(path: &Path) -> Self {
        Self(std::fs::File::create(path).inspect_err(|error| log(format_args!("opening {}: {error}", path.display()))).ok())
    }

    fn note(&mut self, args: std::fmt::Arguments) {
        use std::io::Write;
        log(args);
        let uptime = std::fs::read_to_string("/proc/uptime").unwrap_or_default();
        let uptime = uptime.split_whitespace().next().unwrap_or("?");
        if let Some(file) = &mut self.0 {
            let _ = writeln!(file, "[{uptime}] {args}").and_then(|()| file.sync_data());
        }
    }
}

/// Recovery uses the compiled standalone profile, independent of damaged SD config.
pub fn rescue() -> Result<()> {
    let mut system: SystemConfig = toml::from_str(include_str!("../../config/rg40xxv-rocknix/system.toml"))?;
    let device: DeviceConfig = toml::from_str(include_str!("../../config/rg40xxv-rocknix/device.toml"))?;
    system.init.rescue = None;
    system.init.steps.retain(|step| !matches!(step, Step::Storage(_) | Step::DataDirectories(_) | Step::Provision(_))
        && !matches!(step, Step::Mount(mount) if mount.target == Path::new("/mnt/games")));
    system.supervisor.services.retain(|service| matches!(service.name.as_str(),
        "kernel-log" | "udev" | "udev-trigger" | "udev-settle" | "dbus" | "wifi" | "ssh-host-key" | "sshd"));
    log(format_args!("starting standalone recovery: Wi-Fi and SSH, without frontend or suspend"));
    run(system, &device)
}

fn check_rescue(rescue: &Rescue) -> Result<()> {
    if rescue.max_attempts == 0 || rescue.exec.is_empty() { bail!("rescue requires a command and positive max_attempts"); }
    let attempts: u32 = match std::fs::read_to_string(&rescue.counter) {
        Ok(contents) => contents.trim().parse().context("invalid boot counter")?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => return Err(error.into()),
    };
    std::env::set_var("OXMUX_BOOT_COUNTER", &rescue.counter);
    if attempts >= rescue.max_attempts && !rescue.exec.is_empty() {
        log(format_args!("{attempts} boots never reached the UI; handing over to {:?}", rescue.exec));
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new(&rescue.exec[0]).args(&rescue.exec[1..]).exec();
        bail!("exec rescue: {err}");
    }
    let next = attempts.saturating_add(1).to_string();
    if crate::sys::write_atomic(&rescue.counter, next.as_bytes()).is_err() {
        // The root filesystem may still be read-only this early.
        let _ = sys::mount("none", Path::new("/"), "none", libc::MS_REMOUNT, "");
        if let Err(e) = crate::sys::write_atomic(&rescue.counter, next.as_bytes()) {
            return Err(e).context("could not persist boot attempt");
        }
    }
    Ok(())
}

/// Called by the frontend once it is on screen: this boot counts as good.
pub fn mark_boot_ok() {
    // Record the first displayed frame once per kernel boot, rather than timing
    // SSH availability or resetting the result on a frontend restart.
    let observation = (|| -> Result<()> {
        use std::io::Write;
        let uptime = std::fs::read_to_string("/proc/uptime")?;
        let seconds: f64 = uptime.split_whitespace().next().context("missing kernel uptime")?.parse()?;
        let elapsed = Duration::try_from_secs_f64(seconds)?;
        std::fs::create_dir_all("/run/oxmux")?;
        let mut output = match std::fs::OpenOptions::new().write(true).create_new(true).open("/run/oxmux/first-frame-ready") {
            Ok(output) => output,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        writeln!(output, "first_frame_kernel_uptime_seconds = {}", elapsed.as_secs_f64())?;
        log(format_args!("first displayed frame at kernel uptime {elapsed:?}"));
        Ok(())
    })();
    if let Err(error) = observation { log(format_args!("boot observation failed: {error:#}")); }
    if let Some(counter) = std::env::var_os("OXMUX_BOOT_COUNTER") {
        if let Err(error) = crate::sys::write_atomic(Path::new(&counter), b"0") {
            log(format_args!("could not mark boot successful: {error:#}"));
        }
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
        Step::Coldplug(c) => {
            let loaded = modules::coldplug(&c.exclude)?;
            log(format_args!("coldplug loaded {} modules: {}", loaded.len(), loaded.join(" ")));
            Ok(())
        }
        Step::Hostname(h) => sys::sethostname(h),
        Step::Storage(_) => storage::prepare(),
        Step::DataDirectories(_) => storage::directories(),
        Step::Provision(path) => provision::apply(path),
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
