//! `ghazal observe plan ROM` and `ghazal observe launch ROM`: what the release
//! acceptance script (scripts/acceptance.sh) needs to start a game through the
//! real menu and to judge what then runs.
//!
//! The menu's lists are built by the library scan, so the button sequence is
//! computed from that same scan rather than guessed from file names.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::device::{Activity, CpuLoad, DeviceConfig};
use crate::frontend::app::PAGE;
use crate::frontend::config::{FrontendConfig, Launch};
use crate::frontend::input::Button;
use crate::frontend::library::Library;

/// The menu needs a moment to draw between presses; a press itself holds 200 ms.
const GAP: Duration = Duration::from_millis(150);

/// B, five times, from anywhere in the menu: an open overlay takes two (Options
/// to Main to closed), a system's games one, the systems list one, and the
/// home screen's B puts its cursor on Explore Content.
const HOME: [Button; 5] = [Button::B; 5];

/// Where a game sits in the menu's lists.
#[derive(Debug, PartialEq, Eq)]
pub struct Position {
    pub systems: usize,
    pub system: usize,
    pub games: usize,
    pub game: usize,
}

/// L1 clamps at the top of a list whatever the cursor was; R1 then pages down
/// and Down steps the remainder, so the target is reached from any start.
fn reach(len: usize, target: usize) -> impl Iterator<Item = Button> {
    let page = PAGE as usize;
    std::iter::repeat_n(Button::L1, len.div_ceil(page))
        .chain(std::iter::repeat_n(Button::R1, target / page))
        .chain(std::iter::repeat_n(Button::Down, target % page))
}

/// Home, Explore Content, the system, the game, and A to launch it.
pub fn menu_path(position: &Position) -> Vec<Button> {
    HOME.into_iter()
        .chain([Button::A])
        .chain(reach(position.systems, position.system))
        .chain([Button::A])
        .chain(reach(position.games, position.game))
        .chain([Button::A])
        .collect()
}

/// The device.toml name of a menu button, as `observe press` takes it.
fn name(button: Button) -> &'static str {
    match button {
        Button::Up => "up",
        Button::Down => "down",
        Button::Left => "left",
        Button::Right => "right",
        Button::A => "a",
        Button::B => "b",
        Button::X => "x",
        Button::Y => "y",
        Button::L1 => "l1",
        Button::R1 => "r1",
        Button::L2 => "l2",
        Button::R2 => "r2",
        Button::Select => "select",
        Button::Start => "start",
        Button::Menu => "menu",
    }
}

fn load_name(load: CpuLoad) -> &'static str {
    match load {
        CpuLoad::Light => "light",
        CpuLoad::Moderate => "moderate",
        CpuLoad::Full => "full",
    }
}

pub struct Plan {
    pub rom: PathBuf,
    pub system: String,
    pub launch: Launch,
    pub cpu: CpuLoad,
    pub position: Position,
    /// The program the menu runs, and where it logs.
    pub program: PathBuf,
    pub log: Option<PathBuf>,
}

impl Plan {
    pub fn new(config: &FrontendConfig, rom: &Path) -> Result<Self> {
        let library = Library::scan(config);
        let (system, game) = library.position(rom).with_context(|| format!("{} is not in the library, or two systems list it", rom.display()))?;
        let entry = &library.systems()[system];
        let (program, log) = match &entry.launch {
            Launch::Core(_) => (config.retroarch.bin.clone(), config.retroarch.log.clone()),
            Launch::Standalone(command) => (PathBuf::from(&command.argv[0]), command.log.clone()),
        };
        Ok(Self {
            rom: rom.to_owned(),
            system: entry.name.clone(),
            launch: entry.launch.clone(),
            cpu: entry.cpu,
            position: Position { systems: library.systems().len(), system, games: entry.games.len(), game },
            program,
            log,
        })
    }

    /// `key=value` lines, one per fact, for a shell to read.
    pub fn describe(&self, device: &DeviceConfig) -> String {
        let mut lines = vec![format!("rom={}", self.rom.display()), format!("system={}", self.system)];
        match &self.launch {
            Launch::Core(core) => lines.extend(["launch=retroarch".to_owned(), format!("core={core}")]),
            Launch::Standalone(_) => lines.push("launch=standalone".to_owned()),
        }
        lines.push(format!("program={}", self.program.display()));
        lines.extend(self.log.iter().map(|log| format!("log={}", log.display())));
        lines.push(format!("cpu_load={}", load_name(self.cpu)));
        if let Some(cpu) = &device.cpu {
            let (game, menu) = (cpu.policy(Activity::Game(self.cpu)), cpu.policy(Activity::Menu));
            lines.extend([
                format!("cpu_policy_dir={}", cpu.policy.display()),
                format!("game_governor={}", game.governor),
                format!("game_min_khz={}", game.min_khz),
                format!("menu_governor={}", menu.governor),
                format!("menu_min_khz={}", menu.min_khz),
            ]);
        }
        if let Some(audio) = &device.audio {
            lines.push(format!("audio_card={}", audio.card));
        }
        let buttons: Vec<_> = menu_path(&self.position).into_iter().map(name).collect();
        lines.push(format!("buttons={}", buttons.join(" ")));
        lines.join("\n")
    }
}

pub fn run(device: &DeviceConfig, config: &FrontendConfig, args: &[String]) -> Result<()> {
    let [command, rom] = args else { bail!("usage: ghazal observe plan|launch ROM") };
    let plan = Plan::new(config, Path::new(rom))?;
    match command.as_str() {
        "plan" => {
            println!("{}", plan.describe(device));
            Ok(())
        }
        // Presses the real pad's buttons and returns as the emulator starts.
        "launch" => {
            for button in menu_path(&plan.position) {
                crate::observe::press(device, name(button))?;
                std::thread::sleep(GAP);
            }
            println!("pressed the menu path to {} ({})", plan.rom.display(), plan.system);
            Ok(())
        }
        other => bail!("unknown observation {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::app::navigate;

    /// The sequence must land on every row from every starting cursor, using
    /// the menu's own navigation function.
    #[test]
    fn reaches_every_row_from_any_cursor() {
        for len in 1..=30 {
            for target in 0..len {
                for start in 0..len {
                    let end = reach(len, target).fold(start, |cursor, button| navigate(cursor, len, button));
                    assert_eq!(end, target, "list of {len}, from {start}");
                }
            }
        }
    }

    #[test]
    fn the_path_resets_to_home_then_explores_then_launches() {
        let buttons: Vec<_> = menu_path(&Position { systems: 3, system: 2, games: 20, game: 9 }).into_iter().map(name).collect();
        assert_eq!(
            buttons.join(" "),
            // five B, Explore, 1 page up and 2 down in 3 systems, open, 3 pages up and 1 down + 1 row in 20 games, launch
            "b b b b b a l1 down down a l1 l1 l1 r1 down a"
        );
    }

    fn config(root: &Path) -> FrontendConfig {
        toml::from_str(&format!(
            r#"
            rom_roots = ["{root}/ROMS"]
            bios_dir = "{root}/BIOS"
            [retroarch]
            bin = "/usr/bin/retroarch"
            core_dirs = []
            log = "/var/log/ra.log"
            [[system]]
            name = "Game Boy"
            dirs = ["GB"]
            extensions = ["gb"]
            core = "gambatte_libretro.so"
            cpu = "light"
            [[system]]
            name = "PSP"
            dirs = ["PSP"]
            extensions = ["iso"]
            command = ["/usr/bin/ppsspp", "{{rom}}"]
            log = "/var/log/ppsspp.log"
            "#,
            root = root.display()
        ))
        .unwrap()
    }

    #[test]
    fn plans_a_game_from_the_library_scan() {
        let root = std::env::temp_dir().join(format!("ghazal-plan-{}", std::process::id()));
        for rom in ["GB/Alpha.gb", "GB/Beta (USA).gb", "PSP/Zed.iso"] {
            std::fs::create_dir_all(root.join("ROMS").join(rom).parent().unwrap()).unwrap();
            std::fs::write(root.join("ROMS").join(rom), b"").unwrap();
        }
        let config = config(&root);
        let device: DeviceConfig = toml::from_str(
            "name = \"t\"\n[buttons]\na = 1\n[cpu]\npolicy = \"/sys/policy0\"\nmenu = { governor = \"schedutil\", min_khz = 480000 }\nlight = { governor = \"schedutil\", min_khz = 720000 }\nmoderate = { governor = \"schedutil\", min_khz = 1008000 }\nfull = { governor = \"performance\", min_khz = 480000 }\n",
        )
        .unwrap();

        let beta = Plan::new(&config, &root.join("ROMS/GB/Beta (USA).gb")).unwrap();
        assert_eq!(beta.position, Position { systems: 2, system: 0, games: 2, game: 1 });
        let text = beta.describe(&device);
        for line in ["system=Game Boy", "launch=retroarch", "core=gambatte_libretro.so", "program=/usr/bin/retroarch", "log=/var/log/ra.log", "cpu_load=light", "game_governor=schedutil", "game_min_khz=720000", "menu_min_khz=480000", "buttons=b b b b b a l1 a l1 down a"] {
            assert!(text.lines().any(|candidate| candidate == line), "{line}\n{text}");
        }

        let psp = Plan::new(&config, &root.join("ROMS/PSP/Zed.iso")).unwrap();
        let text = psp.describe(&device);
        for line in ["launch=standalone", "program=/usr/bin/ppsspp", "cpu_load=full", "game_governor=performance"] {
            assert!(text.lines().any(|candidate| candidate == line), "{line}\n{text}");
        }
        assert!(!text.contains("core="));

        assert!(Plan::new(&config, &root.join("ROMS/GB/Missing.gb")).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
