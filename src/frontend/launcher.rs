use std::ffi::OsString;
use std::fs::OpenOptions;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

use super::config::FrontendConfig;
use super::library::{Game, System};

pub struct LaunchSpec {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub log: Option<PathBuf>,
    pub env: Vec<(String, String)>,
}

impl LaunchSpec {
    pub fn retroarch(config: &FrontendConfig, system: &System, game: &Game) -> Result<Self> {
        let ra = &config.retroarch;
        let core = ra
            .core_dirs
            .iter()
            .map(|dir| dir.join(&system.core))
            .find(|p| p.is_file())
            .with_context(|| format!("core {} not found", system.core))?;

        let mut args: Vec<OsString> = Vec::new();
        if let Some(cfg) = ra.configs.iter().find(|p| p.is_file()) {
            args.extend(["-c".into(), cfg.into()]);
        }
        args.extend(ra.extra_args.iter().map(OsString::from));
        args.extend(["-L".into(), core.into(), game.path.clone().into()]);

        let env = ra.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        Ok(Self { program: ra.bin.clone(), args, log: ra.log.clone(), env })
    }

    /// Runs the emulator to completion. The caller must release the display and input first.
    pub fn run(&self) -> Result<()> {
        let mut cmd = Command::new(&self.program);
        cmd.args(&self.args).envs(self.env.iter().cloned()).stdin(Stdio::null());
        if let Some(log) = &self.log {
            if let Some(dir) = log.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(log)
                .with_context(|| format!("opening {}", log.display()))?;
            cmd.stdout(file.try_clone()?).stderr(file);
        }
        eprintln!("oxmux: launching {} {:?}", self.program.display(), self.args);
        let status = cmd
            .status()
            .with_context(|| format!("starting {}", self.program.display()))?;
        if !status.success() {
            bail!("emulator exited with {status}");
        }
        Ok(())
    }
}
