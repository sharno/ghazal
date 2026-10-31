use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::os::unix::process::CommandExt;

use anyhow::{bail, Context, Result};

use super::config::{FrontendConfig, Launch, Standalone};
use super::library::{Game, System};
use crate::device::CpuLoad;

/// Which kind of program a launch starts; routing settings reach each differently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine { RetroArch, Standalone }

pub struct LaunchSpec {
    pub engine: Engine,
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub log: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    /// From [`FrontendConfig::launch_after`].
    pub after: Vec<PathBuf>,
    pub cpu: CpuLoad,
}

impl LaunchSpec {
    pub fn new(config: &FrontendConfig, system: &System, game: &Game) -> Result<Self> {
        let spec = match &system.launch {
            Launch::Core(core) => Self::retroarch(config, core, game)?,
            Launch::Standalone(standalone) => Self::standalone(standalone, game),
        };
        Ok(Self { after: config.launch_after.clone(), cpu: system.cpu, ..spec })
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
        Ok(Self { engine: Engine::RetroArch, program: ra.bin.clone(), args, log: ra.log.clone(), env, after: Vec::new(), cpu: CpuLoad::Full })
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
        Ok(Self { engine: Engine::RetroArch, program: ra.bin.clone(), args, log: ra.log.clone(),
            env: ra.env.iter().map(|(key, value)| (key.clone(), value.clone())).collect(), after: config.launch_after.clone(), cpu: CpuLoad::Light })
    }

    fn standalone(command: &Standalone, game: &Game) -> Self {
        let mut argv = command.argv.iter().map(|arg| if arg == "{rom}" { game.path.clone().into_os_string() } else { arg.into() });
        let program = PathBuf::from(argv.next().expect("config rejects an empty command"));
        let env = command.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        Self { engine: Engine::Standalone, program, args: argv.collect(), log: command.log.clone(), env, after: Vec::new(), cpu: CpuLoad::Full }
    }

    /// Sends the program's sound to HDMI's ALSA `device`: RetroArch through an
    /// appended configuration, SDL programs through `AUDIODEV` (they open ALSA's
    /// default otherwise, which is the speaker). A route that cannot be written
    /// is logged and the game starts on the speaker.
    pub fn with_hdmi_sound(mut self, device: &str, retroarch_override: &Path) -> Self {
        match self.engine {
            Engine::RetroArch => match crate::sys::write_atomic(retroarch_override, format!("audio_device = \"{device}\"\n").as_bytes()) {
                Ok(()) => self.args.insert(0, format!("--appendconfig={}", retroarch_override.display()).into()),
                Err(error) => eprintln!("ghazal: sound route for RetroArch: {error:#}"),
            },
            Engine::Standalone => self.env.push(("AUDIODEV".into(), device.into())),
        }
        self
    }

    /// Runs the emulator to completion. The caller must release the display and input first.
    pub fn run(&self, on_started: impl FnOnce()) -> Result<std::process::ExitStatus> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while let Some(missing) = self.after.iter().find(|path| !path.exists()) {
            if std::time::Instant::now() >= deadline {
                eprintln!("ghazal: launching without {}", missing.display());
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
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

    #[test]
    fn hdmi_sound_reaches_each_engine_its_own_way() {
        let override_file = std::env::temp_dir().join(format!("ghazal-route-{}.cfg", std::process::id()));
        let command = Standalone { argv: vec!["/usr/bin/ppsspp".into(), "{rom}".into()], env: Default::default(), log: None };
        let game = Game { title: "GoW".into(), path: "/ROMS/PSP/a.iso".into() };

        let hdmi = LaunchSpec::standalone(&command, &game).with_hdmi_sound("plughw:HDMI,0", &override_file);
        assert_eq!(hdmi.env, [("AUDIODEV".to_owned(), "plughw:HDMI,0".to_owned())]);

        let mut retroarch = LaunchSpec::standalone(&command, &game);
        retroarch.engine = Engine::RetroArch;
        retroarch.args = vec!["-c".into(), "/etc/ghazal/retroarch.cfg".into(), "-L".into(), "/core.so".into(), "/rom".into()];
        let routed = retroarch.with_hdmi_sound("plughw:HDMI,0", &override_file);
        assert_eq!(routed.args[0], OsString::from(format!("--appendconfig={}", override_file.display())));
        assert_eq!(routed.args[1], OsString::from("-c"), "the original arguments follow unchanged");
        assert_eq!(std::fs::read_to_string(&override_file).unwrap(), "audio_device = \"plughw:HDMI,0\"\n");
        std::fs::remove_file(&override_file).unwrap();
    }
}
