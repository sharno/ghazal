//! PortMaster support: runs ports (`ROMS/PORTS/*.sh`) and installs PortMaster.
//!
//! PortMaster's ports are shell scripts that source `control.txt` from the
//! PortMaster folder, expect `/roms/ports` and glibc libraries in the usual FHS
//! places, and drive the pad through gptokeyb. Ghazal's runtime is Nix-built, so
//! `nix/runtime/ports.nix` provides the loader, bash, tools and libraries, and
//! `ghazal ports run` gives each port a private mount namespace where the ports
//! folder appears at `/roms/ports`. See docs/ports.md.

mod check;
mod controller;
mod env;
mod install;
mod run;

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::frontend::config::FrontendConfig;
use crate::frontend::launcher::LaunchSpec;
use crate::profile::Profile;

pub use install::Progress;

/// `[ports]` in frontend.toml. Its presence turns the feature on; every field
/// has the device default.
#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// The ports folder under a ROM root. PortMaster installs inside it.
    pub dir: String,
    /// Where a port's mount namespace shows that folder: PortMaster scripts
    /// hard-code `/roms/ports`.
    pub mount_point: PathBuf,
    /// Writable home for ports, on the data card (the root is read-only).
    pub home: PathBuf,
    /// Tools ports expect (bash scripts call python3, unzip, mount, pkill...).
    pub bin: PathBuf,
    /// Libraries ports expect the system to have (SDL2, EGL, libstdc++...).
    pub libs: PathBuf,
    /// control.txt and friends, copied into PortMaster's folder before every run.
    pub files: PathBuf,
    /// The SDL controller database generated for this pad, one per boot.
    pub controller_db: PathBuf,
    pub release_url: String,
    pub log: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            dir: "PORTS".into(),
            mount_point: "/roms/ports".into(),
            home: "/mnt/games/.ghazal/ports/home".into(),
            bin: "/usr/lib/ghazal/ports/bin".into(),
            libs: "/usr/lib/ghazal/ports/lib".into(),
            files: "/etc/ghazal/ports".into(),
            controller_db: "/run/ghazal/ports/gamecontrollerdb.txt".into(),
            release_url: "https://github.com/PortsMaster/PortMaster-GUI/releases/latest/download/PortMaster.zip".into(),
            log: "/var/log/ghazal/ports.log".into(),
        }
    }
}

/// PortMaster's own launcher, inside its folder in the ports folder.
const LAUNCHER: &str = "PortMaster/PortMaster.sh";

impl Config {
    /// The ports folder: an existing one under a ROM root (matched ignoring case,
    /// like systems' folders), else where it will be created.
    pub fn ports_dir(&self, rom_roots: &[PathBuf]) -> Option<PathBuf> {
        let existing = rom_roots.iter().find_map(|root| {
            std::fs::read_dir(root).ok()?.flatten()
                .find(|entry| entry.file_name().to_string_lossy().eq_ignore_ascii_case(&self.dir) && entry.path().is_dir())
                .map(|entry| entry.path())
        });
        existing.or_else(|| rom_roots.first().map(|root| root.join(&self.dir)))
    }
}

/// What the Applications menu offers for PortMaster itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortMaster { Missing, Installed }

pub fn portmaster_state(ports_dir: &Path) -> PortMaster {
    if ports_dir.join(LAUNCHER).is_file() { PortMaster::Installed } else { PortMaster::Missing }
}

/// A frontend launch of one port script (or PortMaster's own launcher).
pub fn launch_spec(config: &FrontendConfig, ports: &Config, script: &Path) -> Result<LaunchSpec> {
    Ok(LaunchSpec {
        program: std::env::current_exe().context("finding the ghazal binary")?,
        args: vec!["ports".into(), "run".into(), script.into()],
        log: Some(ports.log.clone()),
        env: Vec::new(),
        after: config.launch_after.clone(),
        cpu: crate::device::CpuLoad::Full,
    })
}

/// The launcher the Applications menu starts.
pub fn portmaster_script(ports_dir: &Path) -> PathBuf {
    ports_dir.join(LAUNCHER)
}

/// Downloads and installs PortMaster in a worker; the receiver reports progress.
pub fn install_in_background(frontend: &FrontendConfig) -> Result<std::sync::mpsc::Receiver<Progress>> {
    let ports = frontend.ports.clone().context("this profile has no [ports] section")?;
    let ports_dir = ports.ports_dir(&frontend.rom_roots).context("no ROM folder for ports")?;
    install::spawn(ports, ports_dir)
}

/// `ghazal ports run SCRIPT | install | prepare | check | ldd FILE`.
pub fn cli(profile: &Profile, args: &[String]) -> Result<()> {
    let frontend = profile.frontend()?;
    let ports = frontend.ports.clone().context("this profile has no [ports] section")?;
    let ports_dir = || ports.ports_dir(&frontend.rom_roots).context("no ROM folder for ports");
    match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["run", script] => {
            let code = run::run(&ports, &profile.device()?, &ports_dir()?, Path::new(script))?;
            if code != 0 { std::process::exit(code); }
            Ok(())
        }
        ["install"] => install::install(&ports, &ports_dir()?, &mut |step| println!("{step}")),
        ["prepare"] => run::prepare(&ports, &profile.device()?, &ports_dir()?),
        ["check"] => check::check(&ports, &profile.device()?, &ports_dir()?),
        ["ldd", file] => run::ldd(&ports, Path::new(file)),
        _ => bail!("usage: ghazal ports run SCRIPT | install | prepare | check | ldd FILE"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ports_folder_matches_ignoring_case_and_falls_back_to_the_first_root() {
        let root = std::env::temp_dir().join(format!("ghazal-ports-dir-{}", std::process::id()));
        let (first, second) = (root.join("a"), root.join("b"));
        std::fs::create_dir_all(second.join("Ports")).unwrap();
        std::fs::create_dir_all(&first).unwrap();
        let config = Config::default();
        assert_eq!(config.ports_dir(&[first.clone(), second.clone()]), Some(second.join("Ports")));
        assert_eq!(config.ports_dir(std::slice::from_ref(&first)), Some(first.join("PORTS")));
        assert_eq!(config.ports_dir(&[]), None);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn portmaster_counts_as_installed_once_its_launcher_exists() {
        let dir = std::env::temp_dir().join(format!("ghazal-ports-state-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("PortMaster")).unwrap();
        assert_eq!(portmaster_state(&dir), PortMaster::Missing);
        std::fs::write(portmaster_script(&dir), b"#!/bin/bash\n").unwrap();
        assert_eq!(portmaster_state(&dir), PortMaster::Installed);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_bundled_frontend_config_enables_ports_with_defaults() {
        let config: FrontendConfig = toml::from_str(crate::profile::DEFAULT_FRONTEND).unwrap();
        assert_eq!(config.ports, Some(Config::default()));
        let ports = config.systems.iter().find(|system| system.name == "Ports").expect("a Ports system");
        assert_eq!(ports.depth, 0);
        assert_eq!(ports.extensions, ["sh"]);
    }
}
