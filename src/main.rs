//! oxmux: a Rust userland for Anbernic H700 handhelds, as one static multi-call binary.

mod daemon;
mod device;
mod frontend;
mod hw;
mod init;
mod input_bridge;
mod install;
mod profile;
mod supervisor;
mod sys;
mod system;

mod ui {
    slint::include_modules!();
}

use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};

use profile::Profile;

const HELP: &str = "\
oxmux - Rust userland for Anbernic H700 handhelds

USAGE: oxmux [--config-dir DIR] [COMMAND]

COMMANDS:
  frontend (default)     the game menu
  daemon                 hotkeys, brightness, volume, power button, sleep, battery, idle
  supervise              run the services in system.toml [--pidfile PATH]
  init                   boot the system from system.toml (as PID 1 or busybox sysinit)
  input-bridge           republish the raw gamepad as muOS-Keys (muinput replacement)
  ctl CMD...             talk to the daemon: status, brightness 50|+10, volume -5, suspend, poweroff, reboot
  install frontend|init  move a muOS install to stage 2 or 3 [--dry-run]
  uninstall              restore muOS's boot scripts
  status                 show what's installed
  check                  validate configs and service binaries
  probe-input            list input devices and print raw events

Configs (device.toml, frontend.toml, system.toml) come from --config-dir, $OXMUX_CONFIG_DIR,
etc/ next to the binary, the binary's directory, or /etc/oxmux; built-in defaults otherwise.";

fn main() {
    // Invoked as /init (stage 4 image): be PID 1.
    let argv0 = std::env::args().next().unwrap_or_default();
    let as_init = std::path::Path::new(&argv0).file_name().is_some_and(|n| n == "init");
    let result = if as_init { run_init(&Profile::locate(None)) } else { run() };
    if let Err(e) = result {
        eprintln!("oxmux: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut config_dir: Option<PathBuf> = None;
    if let Some(i) = args.iter().position(|a| a == "--config-dir") {
        args.remove(i);
        config_dir = Some(if i < args.len() { args.remove(i).into() } else { bail!("--config-dir needs a path") });
    }
    let profile = Profile::locate(config_dir.as_deref());
    let command = args.first().cloned().unwrap_or_else(|| "frontend".into());
    let rest = args.get(1..).unwrap_or(&[]);

    match command.as_str() {
        "frontend" => run_frontend(&profile, rest),
        "daemon" => daemon::run(&profile.device()?, &profile.system()?),
        "supervise" => {
            if let Some(i) = rest.iter().position(|a| a == "--pidfile") {
                let path = PathBuf::from(rest.get(i + 1).ok_or_else(|| anyhow!("--pidfile needs a path"))?);
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                std::fs::write(&path, std::process::id().to_string())?;
            }
            let system = profile.system()?;
            let pid1 = std::process::id() == 1;
            supervisor::Supervisor::new(&system.supervisor, pid1)?.run()?;
            Ok(())
        }
        "init" => run_init(&profile),
        "input-bridge" => input_bridge::run(&profile.device()?),
        "ctl" => {
            let socket = profile.system()?.daemon.socket;
            let reply = daemon::ctl::request(&socket, &rest.join(" "))?;
            println!("{reply}");
            if reply.starts_with("err") {
                std::process::exit(1);
            }
            Ok(())
        }
        "install" => {
            let stage = match rest.first().map(String::as_str) {
                Some("frontend") => install::Stage::Frontend,
                Some("init") => install::Stage::Init,
                _ => bail!("usage: oxmux install frontend|init [--dry-run]"),
            };
            install::install(stage, rest.iter().any(|a| a == "--dry-run"))
        }
        "uninstall" => install::uninstall(),
        "status" => install::status(),
        "check" => install::check(&profile),
        "probe-input" => frontend::platform::evdev_input::probe(),
        "-h" | "--help" | "help" => {
            println!("{HELP}");
            Ok(())
        }
        other => bail!("unknown command {other:?}\n\n{HELP}"),
    }
}

fn run_frontend(profile: &Profile, args: &[String]) -> Result<()> {
    let device = profile.device()?;
    let config = profile.frontend()?;
    // Dev only: `frontend --snapshot DIR BUTTONS` renders frames to PNG without a window.
    #[cfg(feature = "desktop")]
    if let Some(i) = args.iter().position(|a| a == "--snapshot") {
        let dir = args.get(i + 1).ok_or_else(|| anyhow!("--snapshot needs DIR BUTTONS"))?;
        let buttons = args.get(i + 2).cloned().unwrap_or_default();
        return frontend::platform::desktop::snapshot(config, &device, dir.as_ref(), &buttons);
    }
    if let Some(arg) = args.first() {
        bail!("unknown frontend argument {arg:?}");
    }
    frontend::platform::run(config, &device)
}

/// Boot. If oxmux can't even load its config, hand over to the init it replaced rather
/// than leave the device unbootable.
fn run_init(profile: &Profile) -> Result<()> {
    let result = std::panic::catch_unwind(|| -> Result<()> {
        let system = profile.system().context("system.toml")?;
        let device = profile.device().context("device.toml")?;
        init::run(system, &device)
    });
    let err = match result {
        Ok(Ok(())) => return Ok(()),
        Ok(Err(e)) => e,
        Err(_) => anyhow!("panicked"),
    };
    eprintln!("oxmux init failed: {err:#}");
    let fallbacks = ["/init.muos", "/opt/muos/script/init/sysinit.oxmux-orig"];
    for f in fallbacks.iter().filter(|f| std::path::Path::new(f).exists()) {
        use std::os::unix::process::CommandExt;
        eprintln!("oxmux: falling back to {f}");
        let e = std::process::Command::new(f).exec();
        eprintln!("oxmux: exec {f}: {e}");
    }
    Err(err)
}
