//! `ghazal ports run`: one port, in a private mount namespace, under bash.

use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use super::controller::Pad;
use super::env::{self, Spec};
use super::Config;
use crate::device::DeviceConfig;

pub const BASH: &str = "/usr/bin/bash";
/// The interpreter named by every glibc-linked aarch64 binary a port ships.
pub const LOADER: &str = "/lib/ld-linux-aarch64.so.1";
const SYS_INPUT: &str = "/sys/class/input";
const SYS_DRM: &str = "/sys/class/drm";
/// PortMaster's folder inside the ports folder.
const PORTMASTER: &str = "PortMaster";
/// How long a port gets to quit after TERM before its processes are killed.
const GRACE: Duration = Duration::from_secs(5);
/// A port that fails this soon never really started. Games exit with all sorts
/// of codes when quit, so later failures are not reported.
const LAUNCH_FAILURE_WINDOW: Duration = Duration::from_secs(5);
/// Virtual consoles ports write status text to. The first write would make
/// fbcon take over the display the port is drawing on.
const SILENCED_CONSOLES: [&str; 2] = ["/dev/tty0", "/dev/tty1"];

/// What `prepare` and `run` need to know about this board.
struct Facts {
    /// None when the pad cannot be found: ports still start, with PortMaster's own database.
    controller_mapping: Option<String>,
    display: (u32, u32),
    sticks: u8,
}

impl Facts {
    fn read(device: &DeviceConfig) -> Self {
        let mapping = device.input.gamepad.as_deref().context("device.toml sets no [input] gamepad")
            .and_then(|name| Pad::find(Path::new(SYS_INPUT), name))
            .map(|pad| pad.mapping(&device.buttons));
        if let Err(error) = &mapping { eprintln!("ghazal: no controller mapping for ports: {error:#}"); }
        Self {
            controller_mapping: mapping.ok(),
            display: display_size().unwrap_or((640, 480)),
            sticks: device_id().map_or(2, |id| env::analog_sticks(&id)),
        }
    }
}

/// The image records which board it was built for.
fn device_id() -> Option<String> {
    let image: serde_json::Value = serde_json::from_slice(&std::fs::read("/etc/ghazal/image.json").ok()?).ok()?;
    image["device"]["id"].as_str().map(str::to_owned)
}

/// The mode of the first connected display, as DRM lists it.
pub fn display_size() -> Option<(u32, u32)> {
    std::fs::read_dir(SYS_DRM).ok()?.flatten().find_map(|connector| {
        let status = std::fs::read_to_string(connector.path().join("status")).ok()?;
        if status.trim() != "connected" { return None; }
        env::parse_mode(&std::fs::read_to_string(connector.path().join("modes")).ok()?)
    })
}

/// Makes the files ports read current: our control.txt over PortMaster's (its
/// updates restore the stock one), and the controller database.
pub fn prepare(ports: &Config, device: &DeviceConfig, ports_dir: &Path) -> Result<()> {
    write_controller_db(ports, &Facts::read(device))?;
    install_files(&ports.files, &ports_dir.join(PORTMASTER)).map(drop)
}

fn write_controller_db(ports: &Config, facts: &Facts) -> Result<()> {
    match &facts.controller_mapping {
        Some(mapping) => crate::sys::write_atomic(&ports.controller_db, format!("{mapping}\n").as_bytes()),
        None => Ok(()),
    }
}

/// Copies every file of `from` into `to` unless it already matches. Does nothing
/// while PortMaster is not installed. Returns how many files changed.
pub(super) fn install_files(from: &Path, to: &Path) -> Result<usize> {
    if !to.is_dir() { return Ok(0); }
    let mut changed = 0;
    for entry in std::fs::read_dir(from).with_context(|| format!("reading {}", from.display()))?.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) { continue; }
        let bytes = std::fs::read(entry.path())?;
        let target = to.join(entry.file_name());
        if std::fs::read(&target).ok().as_deref() != Some(&bytes) {
            crate::sys::write_atomic(&target, &bytes).with_context(|| format!("installing {}", target.display()))?;
            changed += 1;
        }
    }
    Ok(changed)
}

/// Runs the port and returns the exit code `ghazal` should report.
pub fn run(ports: &Config, device: &DeviceConfig, ports_dir: &Path, script: &Path) -> Result<i32> {
    let script = std::fs::canonicalize(script).with_context(|| format!("port {}", script.display()))?;
    let ports_dir = std::fs::canonicalize(ports_dir).with_context(|| format!("ports folder {}", ports_dir.display()))?;
    let inside = script.strip_prefix(&ports_dir)
        .with_context(|| format!("{} is outside the ports folder {}", script.display(), ports_dir.display()))?;
    let script_in_namespace = ports.mount_point.join(inside);

    let facts = Facts::read(device);
    write_controller_db(ports, &facts)?;
    if let Err(error) = install_files(&ports.files, &ports_dir.join(PORTMASTER)) {
        // A stale control.txt is a worse port, not a reason to refuse to start one.
        eprintln!("ghazal: updating PortMaster's control files: {error:#}");
    }
    if !ports_dir.join(PORTMASTER).join("control.txt").is_file() {
        // Not fatal: a plain shell script may still work.
        eprintln!("ghazal: PortMaster is not installed in {}: ports source its control.txt and use its tools", ports_dir.display());
    }
    std::fs::create_dir_all(&ports.home).with_context(|| format!("creating {}", ports.home.display()))?;
    let environment = env::port_env(&Spec {
        home: &ports.home, bin: &ports.bin, libs: &ports.libs, controller_db: &ports.controller_db,
        display: facts.display, sticks: facts.sticks,
    });

    // Single-threaded here, which unshare(CLONE_NEWNS) requires.
    enter_namespace(&ports_dir, &ports.mount_point)?;
    let signals = crate::sys::SignalFd::new()?;
    let started = Instant::now();
    let child = Command::new(BASH)
        .arg(&script_in_namespace)
        .current_dir(&ports.mount_point)
        .envs(environment)
        .stdin(Stdio::null())
        // Its own process group, so TERM and KILL reach gptokeyb and the game too.
        .process_group(0)
        .pre_exec_unblocked()
        .spawn()
        .with_context(|| format!("starting {BASH} {}", script_in_namespace.display()))?;
    eprintln!("ghazal: port {} running as {}", script_in_namespace.display(), child.id());
    let outcome = supervise(child, &signals);
    let ran = started.elapsed();
    let code = exit_code(&outcome, ran);
    if code != 0 {
        // The menu shows the last [ERROR] line of the launch log.
        eprintln!("[ERROR] {} stopped after {} s with status {code}; see {}", script.file_name().unwrap_or_default().to_string_lossy(), ran.as_secs(), ports.log.display());
    }
    Ok(code)
}

/// How the port ended.
#[derive(Debug, PartialEq)]
enum Outcome {
    /// Bash returned this status.
    Exited(i32),
    /// The player quit it through the launcher (Menu + Start).
    Stopped,
    Failed(String),
}

/// Only an immediate failure is one: the port never started (a missing library
/// or interpreter, a broken script), and the menu should say so.
fn exit_code(outcome: &Outcome, ran: Duration) -> i32 {
    match outcome {
        Outcome::Exited(code) if *code != 0 && ran < LAUNCH_FAILURE_WINDOW => *code,
        Outcome::Failed(_) => 1,
        Outcome::Exited(_) | Outcome::Stopped => 0,
    }
}

fn supervise(mut child: Child, signals: &crate::sys::SignalFd) -> Outcome {
    let group = child.id() as i32;
    let mut stop_requested: Option<Instant> = None;
    let outcome = loop {
        match signals.wait(Some(Duration::from_millis(500))) {
            Ok(Some(libc::SIGTERM | libc::SIGINT | libc::SIGHUP)) if stop_requested.is_none() => {
                stop_requested = Some(Instant::now());
                crate::sys::kill(-group, libc::SIGTERM);
            }
            Ok(_) => {}
            Err(error) => break Outcome::Failed(format!("{error:#}")),
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                break if stop_requested.is_some() { Outcome::Stopped } else { Outcome::Exited(status.code().or_else(|| status.signal().map(|signal| 128 + signal)).unwrap_or(1)) };
            }
            Ok(None) => {}
            Err(error) => break Outcome::Failed(format!("{error:#}")),
        }
        if stop_requested.is_some_and(|since| since.elapsed() > GRACE) {
            crate::sys::kill(-group, libc::SIGKILL);
        }
    };
    // What the script left running (gptokeyb, helpers) must not keep the pad or
    // the display after the port is over.
    crate::sys::kill(-group, libc::SIGKILL);
    let _ = child.wait();
    outcome
}

trait CommandExtras {
    fn pre_exec_unblocked(&mut self) -> &mut Self;
}

impl CommandExtras for Command {
    /// The launcher blocks signals for its SignalFd; children inherit the mask.
    /// Also takes bash down with the launcher, which can be SIGKILLed.
    fn pre_exec_unblocked(&mut self) -> &mut Self {
        // SAFETY: only async-signal-safe calls between fork and exec.
        unsafe {
            self.pre_exec(|| {
                crate::sys::reset_signal_mask();
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL as libc::c_ulong);
                Ok(())
            })
        }
    }
}

/// The ports folder at its PortMaster path, and the consoles silenced, for this
/// process and its children only.
fn enter_namespace(ports_dir: &Path, mount_point: &Path) -> Result<()> {
    // SAFETY: plain syscall; no pointers.
    if unsafe { libc::unshare(libc::CLONE_NEWNS) } < 0 {
        bail!("unshare(CLONE_NEWNS): {}", std::io::Error::last_os_error());
    }
    // Without this the binds below would show up in the system's namespace.
    crate::sys::mount("none", Path::new("/"), "none", libc::MS_REC | libc::MS_PRIVATE, "")?;
    bind(ports_dir, mount_point)?;
    for console in SILENCED_CONSOLES.map(Path::new).iter().filter(|console| console.exists()) {
        bind(Path::new("/dev/null"), console)?;
    }
    Ok(())
}

pub fn bind(source: &Path, target: &Path) -> Result<()> {
    crate::sys::mount(source.to_str().context("path is not UTF-8")?, target, "none", libc::MS_BIND, "")
}

/// Lists the libraries a binary needs the way a port would load them, so a
/// missing one is named. glibc's loader does what ldd does.
pub fn ldd(ports: &Config, file: &Path) -> Result<()> {
    let inherited = std::env::var("LD_LIBRARY_PATH").ok().filter(|path| !path.is_empty());
    let search: PathBuf = inherited.map_or_else(|| ports.libs.clone(), |path| format!("{path}:{}", ports.libs.display()).into());
    let status = Command::new(LOADER).arg("--list").arg(file).env("LD_LIBRARY_PATH", search).status()
        .with_context(|| format!("running {LOADER}"))?;
    if !status.success() { bail!("{} is not loadable ({status})", file.display()); }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_quick_failure_is_reported() {
        let quick = Duration::from_secs(1);
        let long = Duration::from_secs(600);
        assert_eq!(exit_code(&Outcome::Exited(127), quick), 127);
        assert_eq!(exit_code(&Outcome::Exited(1), long), 0);
        assert_eq!(exit_code(&Outcome::Exited(0), quick), 0);
        assert_eq!(exit_code(&Outcome::Stopped, quick), 0);
        assert_eq!(exit_code(&Outcome::Failed("x".into()), long), 1);
    }

    #[test]
    fn control_files_are_copied_only_when_portmaster_exists_and_they_differ() {
        let root = std::env::temp_dir().join(format!("ghazal-ports-files-{}", std::process::id()));
        let (from, to) = (root.join("etc"), root.join("PortMaster"));
        std::fs::create_dir_all(&from).unwrap();
        std::fs::write(from.join("control.txt"), b"ours").unwrap();
        assert_eq!(install_files(&from, &to).unwrap(), 0);
        std::fs::create_dir_all(&to).unwrap();
        std::fs::write(to.join("control.txt"), b"stock").unwrap();
        assert_eq!(install_files(&from, &to).unwrap(), 1);
        assert_eq!(std::fs::read(to.join("control.txt")).unwrap(), b"ours");
        assert_eq!(install_files(&from, &to).unwrap(), 0);
        std::fs::remove_dir_all(root).unwrap();
    }
}
