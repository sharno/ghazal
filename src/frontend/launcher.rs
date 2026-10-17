use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::os::unix::process::CommandExt;

use anyhow::{bail, Context, Result};

use super::config::{FrontendConfig, Launch, Standalone};
use super::library::{Game, System};

pub struct LaunchSpec {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub log: Option<PathBuf>,
    pub env: Vec<(String, String)>,
}

impl LaunchSpec {
    pub fn new(config: &FrontendConfig, system: &System, game: &Game) -> Result<Self> {
        match &system.launch {
            Launch::Core(core) => Self::retroarch(config, core, game),
            Launch::Standalone(standalone) => Ok(Self::standalone(standalone, game)),
        }
    }

    fn retroarch(config: &FrontendConfig, core: &str, game: &Game) -> Result<Self> {
        let ra = &config.retroarch;
        let core = ra
            .core_dirs
            .iter()
            .map(|dir| dir.join(core))
            .find(|p| p.is_file())
            .with_context(|| format!("core {core} not found"))?;

        let mut args = Self::retroarch_config(config)?;
        args.extend(ra.extra_args.iter().map(OsString::from));
        args.extend(["-L".into(), core.into(), game.path.clone().into()]);

        let env = ra.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        Ok(Self { program: ra.bin.clone(), args, log: ra.log.clone(), env })
    }

    fn retroarch_config(config: &FrontendConfig) -> Result<Vec<OsString>> {
        let ra = &config.retroarch;
        let template = ra.configs.iter().find(|path| path.is_file());
        let selected = if let Some(path) = &ra.user_config {
            if !path.exists() {
                let template = template.context("RetroArch configuration template is missing")?;
                crate::sys::write_atomic(path, &std::fs::read(template)?)?;
            }
            Some(path)
        } else { template };
        Ok(selected.map_or_else(Vec::new, |path| vec!["-c".into(), path.into()]))
    }

    pub fn retroarch_settings(config: &FrontendConfig) -> Result<Self> {
        let ra = &config.retroarch;
        let mut args = Self::retroarch_config(config)?;
        args.extend(ra.extra_args.iter().map(OsString::from));
        args.push("--menu".into());
        Ok(Self { program: ra.bin.clone(), args, log: ra.log.clone(),
            env: ra.env.iter().map(|(key, value)| (key.clone(), value.clone())).collect() })
    }

    fn standalone(command: &Standalone, game: &Game) -> Self {
        let mut argv = command.argv.iter().map(|arg| if arg == "{rom}" { game.path.clone().into_os_string() } else { arg.into() });
        let program = PathBuf::from(argv.next().expect("config rejects an empty command"));
        let env = command.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        Self { program, args: argv.collect(), log: command.log.clone(), env }
    }

    /// Runs the emulator to completion. The caller must release the display and input first.
    pub fn run(&self, on_started: impl FnOnce()) -> Result<std::process::ExitStatus> {
        let mut cmd = Command::new(&self.program);
        let mut pump = None;
        cmd.args(&self.args).envs(self.env.iter().cloned()).stdin(Stdio::null());
        // SAFETY: only async-signal-safe calls between fork and exec.
        unsafe { cmd.pre_exec(|| {
            crate::sys::reset_signal_mask();
            if libc::setsid() < 0 { return Err(std::io::Error::last_os_error()); }
            Ok(())
        }); }
        if let Some(log) = &self.log {
            let (file, reader) = crate::logging::LogPump::open(log)
                .with_context(|| format!("opening {}", log.display()))?;
            cmd.stdout(file.try_clone()?).stderr(file);
            pump = Some(reader);
        }
        eprintln!("ghazal: launching {} {:?}", self.program.display(), self.args);
        let child = cmd
            .spawn()
            .with_context(|| format!("starting {}", self.program.display()))?;
        drop(cmd);
        let mut session = crate::emulator::Session::register(child)?;
        on_started();
        let status = session.wait()?;
        let report = pump.map(crate::logging::LogPump::finish).transpose()?.unwrap_or_default();
        if !status.success() {
            if let Some(diagnostic) = report.error { bail!("{diagnostic} (emulator exited with {status})"); }
            bail!("emulator exited with {status}");
        }
        if let Some(warning) = report.save_warning { bail!("{warning}"); }
        Ok(status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_substitutes_the_game_path() {
        let command = Standalone { argv: vec!["/usr/bin/ppsspp".into(), "--fullscreen".into(), "{rom}".into()], env: Default::default(), log: None };
        let spec = LaunchSpec::standalone(&command, &Game { title: "GoW".into(), path: "/ROMS/PSP/God of War.iso".into() });
        assert_eq!(spec.program, PathBuf::from("/usr/bin/ppsspp"));
        assert_eq!(spec.args, [OsString::from("--fullscreen"), OsString::from("/ROMS/PSP/God of War.iso")]);
    }
}
