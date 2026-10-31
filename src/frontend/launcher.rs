use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::os::unix::process::CommandExt;

use anyhow::{bail, Context, Result};

use super::config::{FrontendConfig, Launch, Standalone};
use super::emulator::Emulator;
use super::library::Game;
use crate::device::CpuLoad;

pub struct LaunchSpec {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub log: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    /// From [`FrontendConfig::launch_after`].
    pub after: Vec<PathBuf>,
    pub cpu: CpuLoad,
}

impl LaunchSpec {
    /// `emulator` is the one chosen for the game (see `emulator_choice`); its
    /// own rated load sets the clock policy.
    pub fn new(config: &FrontendConfig, emulator: &Emulator, game: &Game) -> Result<Self> {
        let spec = match &emulator.launch {
            Launch::Core(core) => Self::retroarch(config, core.as_str(), game)?,
            Launch::Standalone(standalone) => Self::standalone(standalone, game),
        };
        Ok(Self { after: config.launch_after.clone(), cpu: emulator.cpu, ..spec })
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
        Ok(Self { program: ra.bin.clone(), args, log: ra.log.clone(), env, after: Vec::new(), cpu: CpuLoad::Full })
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
            env: ra.env.iter().map(|(key, value)| (key.clone(), value.clone())).collect(), after: config.launch_after.clone(), cpu: CpuLoad::Light })
    }

    fn standalone(command: &Standalone, game: &Game) -> Self {
        let mut argv = command.argv.iter().map(|arg| if arg == "{rom}" { game.path.clone().into_os_string() } else { arg.into() });
        let program = PathBuf::from(argv.next().expect("config rejects an empty command"));
        let env = command.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        Self { program, args: argv.collect(), log: command.log.clone(), env, after: Vec::new(), cpu: CpuLoad::Full }
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
    use crate::frontend::emulator_choice::Choices;
    use crate::frontend::library::System;

    #[test]
    fn command_substitutes_the_game_path() {
        let command = Standalone { name: "ppsspp".into(), argv: vec!["/usr/bin/ppsspp".into(), "--fullscreen".into(), "{rom}".into()], env: Default::default(), log: None };
        let spec = LaunchSpec::standalone(&command, &Game { title: "GoW".into(), path: "/ROMS/PSP/God of War.iso".into() });
        assert_eq!(spec.program, PathBuf::from("/usr/bin/ppsspp"));
        assert_eq!(spec.args, [OsString::from("--fullscreen"), OsString::from("/ROMS/PSP/God of War.iso")]);
    }

    /// From the player's choice to the process: which program, which core and which clock.
    #[test]
    fn the_chosen_emulator_decides_the_program_the_core_and_the_clock() {
        let directory = std::env::temp_dir().join(format!("ghazal-launch-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        for core in ["a_libretro.so", "b_libretro.so"] { std::fs::write(directory.join(core), b"").unwrap(); }
        let config: FrontendConfig = toml::from_str(&format!(r#"
            rom_roots = []
            launch_after = ["/run/ghazal/udev-settled"]
            [retroarch]
            bin = "/usr/bin/retroarch"
            core_dirs = ["{directory}"]
            [[system]]
            name = "GBA"
            dirs = ["GBA"]
            extensions = ["gba"]
            core = "a_libretro.so"
            cpu = "moderate"
            [[system.alternative]]
            core = "b_libretro.so"
            cpu = "light"
            [[system.alternative]]
            command = ["/usr/bin/other", "--fullscreen", "{{rom}}"]
            [[system.alternative]]
            core = "missing_libretro.so"
        "#, directory = directory.display())).unwrap();
        let game = Game { title: "Game".into(), path: "/ROMS/GBA/Game.gba".into() };
        let system = System { name: "GBA".into(), art: Default::default(), emulators: config.systems[0].emulators.clone(), games: vec![] };
        let [stock, light, other, missing] = [0, 1, 2, 3].map(|index| system.emulators.iter().nth(index).unwrap());
        let mut choices = Choices::load(&directory.join("emulators.toml")).unwrap();
        let spec = |choices: &Choices| LaunchSpec::new(&config, choices.resolve(&system, &game), &game);
        let core_argument = |spec: &LaunchSpec| spec.args.iter().skip_while(|arg| *arg != "-L").nth(1).cloned();

        let stock_spec = spec(&choices).unwrap();
        assert_eq!(stock_spec.program, PathBuf::from("/usr/bin/retroarch"));
        assert_eq!(core_argument(&stock_spec), Some(directory.join("a_libretro.so").into()));
        assert_eq!(stock_spec.cpu, CpuLoad::Moderate);
        assert_eq!(stock_spec.after, [PathBuf::from("/run/ghazal/udev-settled")]);

        // The system default applies to every game of the system, with its own clock.
        choices.set_default(&system, light).unwrap();
        let light_spec = spec(&choices).unwrap();
        assert_eq!(core_argument(&light_spec), Some(directory.join("b_libretro.so").into()));
        assert_eq!(light_spec.cpu, CpuLoad::Light);

        // A pinned standalone emulator replaces RetroArch altogether.
        choices.pin(&game, other).unwrap();
        let other_spec = spec(&choices).unwrap();
        assert_eq!(other_spec.program, PathBuf::from("/usr/bin/other"));
        assert_eq!(other_spec.args, [OsString::from("--fullscreen"), OsString::from("/ROMS/GBA/Game.gba")]);
        assert_eq!(other_spec.cpu, CpuLoad::Full);

        // Back to the default; another game is unaffected by the pin throughout.
        choices.unpin(&game).unwrap();
        assert_eq!(core_argument(&spec(&choices).unwrap()), core_argument(&light_spec));
        let elsewhere = Game { title: "Other".into(), path: "/ROMS/GBA/Other.gba".into() };
        choices.pin(&game, stock).unwrap();
        assert_eq!(core_argument(&LaunchSpec::new(&config, choices.resolve(&system, &elsewhere), &elsewhere).unwrap()), core_argument(&light_spec));

        // A choice whose core file is absent fails like any missing core.
        choices.pin(&game, missing).unwrap();
        assert!(spec(&choices).err().unwrap().to_string().contains("missing_libretro.so"));
        std::fs::remove_dir_all(directory).unwrap();
    }
}
