use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::os::unix::process::CommandExt;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use super::config::{FrontendConfig, Launch, Standalone};
use super::emulator::Emulator;
use super::library::Game;
use super::netplay::{self, Session};
use super::retroarch_menu::{self, MenuKey};
use crate::device::CpuLoad;
use crate::retroarch_cfg::{self, Achievements, Mode};

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
    pub needs: Needs,
}

/// What a launch needs besides the emulator process.
#[derive(Default)]
pub struct Needs {
    /// Wait up to this long for Wi-Fi first (achievements sign in, netplay connects).
    pub online: Option<Duration>,
    /// Menu keys to press in RetroArch once it is running.
    pub menu: Option<MenuPath>,
}

pub struct MenuPath {
    pub keys: Vec<MenuKey>,
    pub port: u16,
    /// RetroArch confirms with its B button; see [`MenuKey`].
    pub ok_on_b: bool,
}

impl LaunchSpec {
    /// `emulator` is the one chosen for the game (see `emulator_choice`); its
    /// own rated load sets the clock policy.
    pub fn new(config: &FrontendConfig, emulator: &Emulator, game: &Game) -> Result<Self> {
        Self::build(config, emulator, game, None)
    }

    /// Hosts or joins a netplay session of a RetroArch game.
    pub fn netplay(config: &FrontendConfig, emulator: &Emulator, game: &Game, session: &Session) -> Result<Self> {
        Self::build(config, emulator, game, Some(session))
    }

    fn build(config: &FrontendConfig, emulator: &Emulator, game: &Game, session: Option<&Session>) -> Result<Self> {
        let spec = match (&emulator.launch, session) {
            (Launch::Core(core), session) => Self::retroarch(config, core.as_str(), game, session)?,
            (Launch::Standalone(standalone), None) => Self::standalone(standalone, game),
            (Launch::Standalone(_), Some(_)) => bail!("{} is a standalone emulator, which has no netplay", emulator.label),
        };
        Ok(Self { after: config.launch_after.clone(), cpu: emulator.cpu, ..spec })
    }

    fn retroarch(config: &FrontendConfig, core: &str, game: &Game, session: Option<&Session>) -> Result<Self> {
        let ra = &config.retroarch;
        let core = ra
            .core_dirs
            .iter()
            .map(|dir| dir.join(core))
            .find(|p| p.is_file())
            .with_context(|| format!("core {core} not found"))?;

        let mut args = Self::retroarch_config(config)?;
        args.extend(ra.extra_args.iter().map(OsString::from));
        let wifi = wifi_wait(config, Path::new(crate::network::IWD_STATE));
        let online = match session {
            Some(session) => {
                // The settings are a file, not arguments: RetroArch has no option for most of them.
                crate::sys::write_atomic(&config.netplay.append_config, config.netplay.retroarch.render().as_bytes()).context("writing the netplay settings")?;
                args.extend(netplay::arguments(session, config.netplay.port, &config.netplay.append_config));
                wifi
            }
            // Signing in to RetroAchievements happens as the game loads, so Wi-Fi must be up by then.
            None => wifi.filter(|_| achievements_on(ra.user_config.as_deref())),
        };
        args.extend(["-L".into(), core.into(), game.path.clone().into()]);

        let env = ra.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        Ok(Self { engine: Engine::RetroArch, program: ra.bin.clone(), args, log: ra.log.clone(), env, after: Vec::new(), cpu: CpuLoad::Full, needs: Needs { online, menu: None } })
    }

    /// RetroArch's writable config, created from the template on first use.
    /// A RetroAchievements account from provisioning and each default Ghazal
    /// changed later are merged in here, once; that is never fatal and never
    /// logged beyond its cause.
    pub fn user_config(config: &FrontendConfig) -> Result<Option<PathBuf>> {
        let ra = &config.retroarch;
        let Some(path) = &ra.user_config else { return Ok(None) };
        if !path.exists() {
            let template = ra.configs.iter().find(|path| path.is_file()).context("RetroArch configuration template is missing")?;
            crate::sys::write_atomic(path, &std::fs::read(template)?)?;
            retroarch_cfg::record_default_changes(path)?;
        }
        if let Err(error) = retroarch_cfg::apply_seed(path, &ra.account_seed) {
            eprintln!("ghazal: RetroAchievements account from provisioning: {error:#}");
        }
        if let Err(error) = retroarch_cfg::apply_default_changes(path) {
            eprintln!("ghazal: changing RetroArch's defaults: {error:#}");
        }
        Ok(Some(path.clone()))
    }

    /// The file RetroArch starts with: the user's config, else the first template.
    fn selected_config(config: &FrontendConfig) -> Result<Option<PathBuf>> {
        Ok(match Self::user_config(config)? {
            Some(path) => Some(path),
            None => config.retroarch.configs.iter().find(|path| path.is_file()).cloned(),
        })
    }

    fn retroarch_config(config: &FrontendConfig) -> Result<Vec<OsString>> {
        Ok(Self::selected_config(config)?.map_or_else(Vec::new, |path| vec!["-c".into(), path.into()]))
    }

    pub fn retroarch_settings(config: &FrontendConfig) -> Result<Self> {
        Self::retroarch_menu(config, &[])
    }

    /// RetroArch's menu, opened on its Achievements settings.
    pub fn retroarch_achievements(config: &FrontendConfig) -> Result<Self> {
        Self::retroarch_menu(config, &config.retroarch.menu_paths.achievements)
    }

    /// RetroArch's own menu without a game; `keys` are pressed once it is up.
    fn retroarch_menu(config: &FrontendConfig, keys: &[MenuKey]) -> Result<Self> {
        let ra = &config.retroarch;
        let selected = Self::selected_config(config)?;
        let mut args: Vec<OsString> = selected.iter().flat_map(|path| ["-c".into(), path.clone().into()]).collect();
        args.extend(ra.extra_args.iter().map(OsString::from));
        args.push("--menu".into());
        let ok_on_b = selected.as_deref().is_none_or(confirms_with_b);
        let menu = (!keys.is_empty()).then(|| MenuPath { keys: keys.to_vec(), port: ra.command_port, ok_on_b });
        Ok(Self { engine: Engine::RetroArch, program: ra.bin.clone(), args, log: ra.log.clone(),
            env: ra.env.iter().map(|(key, value)| (key.clone(), value.clone())).collect(), after: config.launch_after.clone(), cpu: CpuLoad::Light,
            needs: Needs { online: None, menu } })
    }

    fn standalone(command: &Standalone, game: &Game) -> Self {
        let mut argv = command.argv.iter().map(|arg| if arg == "{rom}" { game.path.clone().into_os_string() } else { arg.into() });
        let program = PathBuf::from(argv.next().expect("config rejects an empty command"));
        let env = command.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        Self { engine: Engine::Standalone, program, args: argv.collect(), log: command.log.clone(), env, after: Vec::new(), cpu: CpuLoad::Full, needs: Needs::default() }
    }

    /// Sends the program's sound to HDMI's ALSA `device`: RetroArch through an
    /// appended configuration, SDL programs through `AUDIODEV` (they open ALSA's
    /// default otherwise, which is the speaker). A route that cannot be written
    /// is logged and the game starts on the speaker.
    pub fn with_hdmi_sound(mut self, device: &str, retroarch_override: &Path) -> Self {
        match self.engine {
            Engine::RetroArch => match crate::sys::write_atomic(retroarch_override, format!("audio_device = \"{device}\"\n").as_bytes()) {
                Ok(()) => add_appendconfig(&mut self.args, retroarch_override),
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
        if let Some(limit) = self.needs.online {
            if !crate::network::wait_online(limit) { eprintln!("ghazal: starting without a network connection"); }
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
        if let Some(menu) = &self.needs.menu { retroarch_menu::open(menu.keys.clone(), menu.port, menu.ok_on_b); }
        on_started();
        let status = session.wait()?;
        let report = pump.map(crate::logging::LogPump::finish).transpose()?.unwrap_or_default();
        if !status.success() {
            if let Some(diagnostic) = report.error { bail!("{diagnostic} (emulator exited with {status})"); }
            bail!("emulator exited with {status}");
        }
        if let Some(warning) = report.save_warning { bail!("{warning}"); }
        if let Some(reason) = report.stream_ended { bail!("{reason}"); }
        Ok(status)
    }
}

/// RetroArch's `--appendconfig` takes several files joined with `|`; a second option may
/// replace the first. Netplay already passes one, so the route joins it.
fn add_appendconfig(args: &mut Vec<OsString>, file: &Path) {
    match args.iter().position(|arg| arg == "--appendconfig") {
        Some(at) if at + 1 < args.len() => {
            let mut joined = std::mem::take(&mut args[at + 1]);
            joined.push("|");
            joined.push(file);
            args[at + 1] = joined;
        }
        _ => args.insert(0, format!("--appendconfig={}", file.display()).into()),
    }
}

/// How long a launch that uses the network waits for Wi-Fi: only when a
/// network is saved, since an offline handheld would wait for nothing.
fn wifi_wait(config: &FrontendConfig, iwd_state: &Path) -> Option<Duration> {
    crate::network::configured(iwd_state).then(|| Duration::from_secs(config.retroarch.online_wait))
}

/// RetroArch's menu confirms with its B button unless `menu_swap_ok_cancel_buttons`
/// is "false". The key is absent from a fresh config, and RetroArch 1.22 defaults it to true.
fn confirms_with_b(config_file: &Path) -> bool {
    std::fs::read_to_string(config_file).ok().is_none_or(|text| retroarch_cfg::value(&text, "menu_swap_ok_cancel_buttons") != Some("false"))
}

/// RetroAchievements is switched on in RetroArch's user config.
fn achievements_on(user_config: Option<&Path>) -> bool {
    user_config.and_then(|path| std::fs::read_to_string(path).ok()).is_some_and(|text| Achievements::read(&text).mode != Mode::Off)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::emulator_choice::Choices;
    use crate::frontend::library::System;
    use crate::frontend::netplay::Role;

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

    /// A data card in a temp dir: a core, a RetroArch template and the frontend config pointing at them.
    struct Card(PathBuf);

    impl Card {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("ghazal-launch-{name}-{}", std::process::id()));
            std::fs::create_dir_all(dir.join("cores")).unwrap();
            std::fs::write(dir.join("cores/gambatte_libretro.so"), b"").unwrap();
            std::fs::write(dir.join("template.cfg"), "video_driver = \"gl\"\n").unwrap();
            Self(dir)
        }

        fn config(&self) -> FrontendConfig {
            toml::from_str(&format!(r#"
                rom_roots = ["{d}/ROMS"]
                [retroarch]
                bin = "/usr/bin/retroarch"
                configs = ["{d}/template.cfg"]
                user_config = "{d}/user.cfg"
                core_dirs = ["{d}/cores"]
                extra_args = ["-f"]
                account_seed = "{d}/seed.cfg"
                online_wait = 3
                [retroarch.menu_paths]
                achievements = ["up", "ok"]
                [netplay]
                append_config = "{d}/netplay.cfg"
                [netplay.retroarch]
                config_save_on_exit = "false"
                netplay_nat_traversal = "false"
            "#, d = self.0.display())).unwrap()
        }

        fn user_config(&self) -> String { std::fs::read_to_string(self.0.join("user.cfg")).unwrap() }
    }

    impl Drop for Card {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }

    fn game_boy() -> (Emulator, Game) {
        let game = Game { title: "Tetris".into(), path: "/ROMS/GB/Tetris.gb".into() };
        let launch = Launch::Core("gambatte_libretro.so".into());
        (Emulator { label: launch.default_label(), launch, cpu: CpuLoad::Light }, game)
    }

    fn strings(spec: &LaunchSpec) -> Vec<String> {
        spec.args.iter().map(|arg| arg.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn hosting_passes_netplay_options_before_the_game_and_writes_the_netplay_settings() {
        let card = Card::new("host");
        let (emulator, game) = game_boy();
        let session = Session { role: Role::Host, nick: "ghazal-9f3a".into() };
        let spec = LaunchSpec::netplay(&card.config(), &emulator, &game, &session).unwrap();
        let args = strings(&spec);
        let at = |arg: &str| args.iter().position(|candidate| candidate == arg).unwrap();
        assert_eq!(args[0], "-c");
        assert_eq!(args[1], format!("{}/user.cfg", card.0.display()));
        assert!(at("--host") < at("-L"), "RetroArch options go before the content: {args:?}");
        assert_eq!(args[at("--nick") + 1], "ghazal-9f3a");
        assert_eq!(args[at("--appendconfig") + 1], format!("{}/netplay.cfg", card.0.display()));
        assert_eq!(args.last().unwrap(), "/ROMS/GB/Tetris.gb");
        assert_eq!(spec.cpu, CpuLoad::Light);
        assert!(!args.iter().any(|arg| arg == "--connect"));
        // Netplay replaces the game's state and must not rewrite the solo config afterwards.
        let appended = std::fs::read_to_string(card.0.join("netplay.cfg")).unwrap();
        assert_eq!(appended, "config_save_on_exit = \"false\"\nnetplay_nat_traversal = \"false\"\n");
        assert_eq!(card.user_config(), "video_driver = \"gl\"\n");
    }

    #[test]
    fn joining_connects_to_the_host_and_leaves_local_saves_alone() {
        let card = Card::new("join");
        let (emulator, game) = game_boy();
        let session = Session { role: Role::Join { address: "192.168.1.20".into(), port: 55435 }, nick: "pocket".into() };
        let args = strings(&LaunchSpec::netplay(&card.config(), &emulator, &game, &session).unwrap());
        let at = |arg: &str| args.iter().position(|candidate| candidate == arg).unwrap();
        assert_eq!(args[at("--connect") + 1], "192.168.1.20");
        assert_eq!(args[at("--sram-mode") + 1], "noload-nosave");
        assert!(at("--connect") < at("-L"));
        assert!(!args.iter().any(|arg| arg == "--host"));
    }

    #[test]
    fn standalone_emulators_have_no_netplay() {
        let card = Card::new("standalone");
        let (mut emulator, game) = game_boy();
        emulator.launch = Launch::Standalone(Standalone { name: "ppsspp".into(), argv: vec!["/usr/bin/ppsspp".into(), "{rom}".into()], env: Default::default(), log: None });
        let session = Session { role: Role::Host, nick: "x".into() };
        let error = LaunchSpec::netplay(&card.config(), &emulator, &game, &session).err().unwrap();
        assert!(error.to_string().contains("no netplay"));
    }

    #[test]
    fn a_provisioned_account_reaches_retroarch_on_the_next_launch() {
        let card = Card::new("seed");
        let (emulator, game) = game_boy();
        std::fs::write(card.0.join("seed.cfg"), retroarch_cfg::account_seed("sharno", "pass word").unwrap().render()).unwrap();
        let spec = LaunchSpec::new(&card.config(), &emulator, &game).unwrap();
        let merged = card.user_config();
        assert_eq!(retroarch_cfg::value(&merged, "cheevos_username"), Some("sharno"));
        assert_eq!(retroarch_cfg::value(&merged, "cheevos_enable"), Some("true"));
        assert_eq!(retroarch_cfg::value(&merged, "video_driver"), Some("gl"));
        assert!(!card.0.join("seed.cfg").exists());
        // The password is only in the config RetroArch reads, never on its command line or in the launch log.
        assert!(!strings(&spec).iter().any(|arg| arg.contains("pass word")));
    }

    #[test]
    fn a_broken_account_seed_does_not_stop_a_game() {
        let card = Card::new("badseed");
        let (emulator, game) = game_boy();
        std::fs::write(card.0.join("seed.cfg"), "system_directory = \"/\"\n").unwrap();
        assert!(LaunchSpec::new(&card.config(), &emulator, &game).is_ok());
        assert_eq!(card.user_config(), "video_driver = \"gl\"\n");
    }

    #[test]
    fn only_launches_that_use_the_network_wait_for_wifi_and_only_with_a_saved_network() {
        let card = Card::new("wifi");
        let config = card.config();
        let iwd = card.0.join("iwd");
        assert_eq!(wifi_wait(&config, &iwd), None);
        std::fs::create_dir_all(&iwd).unwrap();
        std::fs::write(iwd.join("=4d79.psk"), b"").unwrap();
        assert_eq!(wifi_wait(&config, &iwd), Some(Duration::from_secs(3)));
        let user = card.0.join("user.cfg");
        assert!(!achievements_on(Some(&user)));
        assert!(!achievements_on(None));
        std::fs::write(&user, "cheevos_enable = \"true\"\ncheevos_hardcore_mode_enable = \"false\"\n").unwrap();
        assert!(achievements_on(Some(&user)));
        std::fs::write(&user, "cheevos_enable = \"false\"\n").unwrap();
        assert!(!achievements_on(Some(&user)));
    }

    #[test]
    fn the_achievements_menu_opens_retroarch_with_its_key_path() {
        let card = Card::new("menu");
        let config = card.config();
        let spec = LaunchSpec::retroarch_achievements(&config).unwrap();
        assert_eq!(strings(&spec).last().unwrap(), "--menu");
        let menu = spec.needs.menu.unwrap();
        assert_eq!((menu.keys, menu.port), (vec![MenuKey::Up, MenuKey::Ok], 55355));
        // The test card's config says nothing about swapping OK and Cancel: RetroArch's default applies.
        assert!(menu.ok_on_b);
        std::fs::write(card.0.join("user.cfg"), "menu_swap_ok_cancel_buttons = \"false\"\n").unwrap();
        assert!(!LaunchSpec::retroarch_achievements(&config).unwrap().needs.menu.unwrap().ok_on_b);
        // The plain settings entry presses nothing.
        assert!(LaunchSpec::retroarch_settings(&config).unwrap().needs.menu.is_none());
    }

    #[test]
    fn hdmi_sound_reaches_each_engine_its_own_way() {
        let override_file = std::env::temp_dir().join(format!("ghazal-route-{}.cfg", std::process::id()));
        let command = Standalone { name: "ppsspp".into(), argv: vec!["/usr/bin/ppsspp".into(), "{rom}".into()], env: Default::default(), log: None };
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

    #[test]
    fn the_sound_route_joins_netplays_appended_config() {
        let mut args: Vec<OsString> = ["-c", "/etc/retroarch.cfg", "--appendconfig", "/run/ghazal/retroarch-netplay.cfg", "--host", "-L", "/core.so", "/rom"].map(OsString::from).to_vec();
        add_appendconfig(&mut args, Path::new("/run/ghazal/retroarch-route.cfg"));
        assert_eq!(args[3], OsString::from("/run/ghazal/retroarch-netplay.cfg|/run/ghazal/retroarch-route.cfg"));
        assert_eq!(args.iter().filter(|arg| *arg == "--appendconfig").count(), 1);
        assert_eq!(args.len(), 8);
    }
}
