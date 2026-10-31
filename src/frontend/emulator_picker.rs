//! The emulator picker: a list over the game lists, opened with X.
//!
//! Its first row makes the game follow its system's default; the others pin
//! the game to one emulator. A applies a row to the game, X to the whole
//! system. The picker is a value: pressing a button consumes it and returns
//! what happens next (see `Step`), so it holds no reference to the library
//! that may be rescanned under it.

use super::activity::GameId;
use super::emulator::Emulator;
use super::emulator_choice::Choices;
use super::input::Button;
use super::library::{Game, System};

/// One row of the picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    /// The game follows the system's default.
    FollowDefault,
    /// An emulator of the system, by its position in `Emulators::iter`.
    Emulator(usize),
}

pub struct Picker {
    game: GameId,
    cursor: usize,
}

/// What the player sees: the overlay's title and rows, and the selected row.
pub struct View {
    pub title: String,
    pub rows: Vec<String>,
    pub cursor: usize,
}

/// What a button press does to an open picker.
pub enum Step {
    /// Still open, possibly with the cursor moved.
    Open(Picker),
    Closed,
    /// A choice was saved; the message tells the player what now applies.
    Chosen(String),
    /// Saving failed; the message says why.
    Failed(String),
}

pub const HINTS: &[(&str, &str)] = &[("A", "This game"), ("X", "System default"), ("B", "Cancel")];
/// For panels narrower than the long labels.
pub const COMPACT_HINTS: &[(&str, &str)] = &[("A", "Game"), ("X", "System"), ("B", "Back")];

impl Picker {
    /// Opens for a game, or says why not: a system with one emulator has nothing to choose.
    pub fn open(system: &System, game: &Game, choices: &Choices) -> Result<Self, String> {
        if system.emulators.len() < 2 {
            return Err(format!("{} has one emulator: {}", system.name, system.emulators.default().label));
        }
        let cursor = choices.pinned(system, game)
            .and_then(|pinned| system.emulators.iter().position(|emulator| emulator.id() == pinned.id()))
            .map_or(0, |position| position + 1);
        Ok(Self { game: GameId::new(system, game), cursor })
    }

    pub fn game(&self) -> &GameId { &self.game }

    pub fn view(&self, system: &System, game: &Game, choices: &Choices) -> View {
        let pinned = choices.pinned(system, game).map(Emulator::id);
        let mark = |selected: bool| if selected { "  ✓" } else { "" };
        let mut rows = vec![format!("Default ({}){}", choices.default_for(system).label, mark(pinned.is_none()))];
        rows.extend(system.emulators.iter().map(|emulator| format!("{}{}", emulator.label, mark(pinned.as_ref() == Some(&emulator.id())))));
        View { title: format!("Emulator: {}", choices.resolve(system, game).label), rows, cursor: self.cursor }
    }

    /// The picker after `button`. Up/Down move; A applies the row to the game, X to the system; B or Menu closes.
    pub fn press(mut self, button: Button, system: &System, game: &Game, choices: &mut Choices) -> Step {
        let rows = system.emulators.len() + 1;
        let row = match self.cursor { 0 => Row::FollowDefault, position => Row::Emulator(position - 1) };
        match button {
            Button::Up => { self.cursor = (self.cursor + rows - 1) % rows; Step::Open(self) }
            Button::Down => { self.cursor = (self.cursor + 1) % rows; Step::Open(self) }
            Button::A => apply(Scope::Game, row, system, game, choices),
            Button::X => apply(Scope::System, row, system, game, choices),
            Button::B | Button::Menu | Button::Start => Step::Closed,
            _ => Step::Open(self),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope { Game, System }

fn apply(scope: Scope, row: Row, system: &System, game: &Game, choices: &mut Choices) -> Step {
    let emulator = match row {
        Row::FollowDefault => None,
        Row::Emulator(position) => system.emulators.iter().nth(position),
    };
    let saved = match (scope, emulator) {
        (Scope::Game, None) => choices.unpin(game),
        (Scope::Game, Some(emulator)) => choices.pin(game, emulator),
        (Scope::System, None) => choices.reset_default(system),
        (Scope::System, Some(emulator)) => choices.set_default(system, emulator),
    };
    match saved {
        Err(error) => Step::Failed(format!("Saving emulator choice: {error:#}")),
        Ok(()) => Step::Chosen(match (scope, emulator) {
            (Scope::Game, None) => format!("Follows the {} default: {}", system.name, choices.default_for(system).label),
            // Saves are filed under the core's name, so a game's save does not follow it to another core.
            (Scope::Game, Some(emulator)) => format!("{} for this game; saves are kept per emulator", emulator.label),
            (Scope::System, None) => format!("{} default restored: {}", system.name, system.emulators.default().label),
            (Scope::System, Some(emulator)) => format!("{} default: {}", system.name, emulator.label),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::CpuLoad;
    use crate::frontend::config::Launch;
    use crate::frontend::emulator::Emulators;

    fn core(file: &str) -> Emulator {
        let launch = Launch::Core(file.into());
        Emulator { label: launch.default_label(), launch, cpu: CpuLoad::Full }
    }

    fn system(emulators: Emulators) -> System {
        System { name: "NES".into(), art: Default::default(), emulators, games: vec![Game { title: "Game".into(), path: "/ROMS/NES/Game.nes".into() }] }
    }

    fn choices(name: &str) -> (Choices, std::path::PathBuf) {
        let directory = std::env::temp_dir().join(format!("ghazal-picker-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        (Choices::load(&directory.join("emulators.toml")).unwrap(), directory)
    }

    fn nes() -> System {
        system(Emulators::new(core("fceumm_libretro.so"), vec![core("nestopia_libretro.so"), core("quicknes_libretro.so")]).unwrap())
    }

    fn press(picker: Picker, buttons: &[Button], system: &System, choices: &mut Choices) -> Step {
        buttons.iter().fold(Step::Open(picker), |step, button| match step {
            Step::Open(picker) => picker.press(*button, system, &system.games[0], choices),
            finished => finished,
        })
    }

    #[test]
    fn a_system_with_one_emulator_has_nothing_to_pick() {
        let (choices, directory) = choices("single");
        let system = system(Emulators::only(Launch::Core("only_libretro.so".into()), CpuLoad::Full));
        let reason = Picker::open(&system, &system.games[0], &choices).err().unwrap();
        assert_eq!(reason, "NES has one emulator: only");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn rows_name_the_default_and_mark_the_pinned_emulator() {
        let (mut choices, directory) = choices("rows");
        let system = nes();
        let game = &system.games[0];
        let picker = Picker::open(&system, game, &choices).unwrap();
        let view = picker.view(&system, game, &choices);
        assert_eq!(view.title, "Emulator: fceumm");
        assert_eq!(view.rows, ["Default (fceumm)  ✓", "fceumm", "nestopia", "quicknes"]);
        assert_eq!(view.cursor, 0);

        choices.pin(game, system.emulators.iter().nth(2).unwrap()).unwrap();
        let picker = Picker::open(&system, game, &choices).unwrap();
        let view = picker.view(&system, game, &choices);
        assert_eq!(view.title, "Emulator: quicknes");
        assert_eq!(view.rows, ["Default (fceumm)", "fceumm", "nestopia", "quicknes  ✓"]);
        assert_eq!(view.cursor, 3, "opens on the pinned emulator");

        // The default row names the system's default as the player changed it.
        choices.set_default(&system, system.emulators.iter().nth(1).unwrap()).unwrap();
        assert_eq!(picker.view(&system, game, &choices).rows[0], "Default (nestopia)");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn the_cursor_wraps_and_a_pins_the_game() {
        let (mut choices, directory) = choices("pin");
        let system = nes();
        let picker = Picker::open(&system, &system.games[0], &choices).unwrap();
        let Step::Open(picker) = press(picker, &[Button::Up], &system, &mut choices) else { panic!("still open") };
        assert_eq!(picker.view(&system, &system.games[0], &choices).cursor, 3);
        let Step::Open(picker) = press(picker, &[Button::Down, Button::Down], &system, &mut choices) else { panic!("still open") };
        assert_eq!(picker.view(&system, &system.games[0], &choices).cursor, 1);
        let Step::Chosen(message) = press(picker, &[Button::A], &system, &mut choices) else { panic!("chosen") };
        assert_eq!(message, "fceumm for this game; saves are kept per emulator");
        assert_eq!(choices.pinned(&system, &system.games[0]).unwrap().label, "fceumm", "picking the system's own emulator still pins");

        let picker = Picker::open(&system, &system.games[0], &choices).unwrap();
        let Step::Chosen(message) = press(picker, &[Button::Down, Button::A], &system, &mut choices) else { panic!("chosen") };
        assert!(message.starts_with("nestopia for this game"), "{message}");
        assert_eq!(choices.resolve(&system, &system.games[0]).label, "nestopia");

        // The first row releases the pin.
        let picker = Picker::open(&system, &system.games[0], &choices).unwrap();
        let Step::Chosen(message) = press(picker, &[Button::Up, Button::Up, Button::A], &system, &mut choices) else { panic!("chosen") };
        assert_eq!(message, "Follows the NES default: fceumm");
        assert!(choices.pinned(&system, &system.games[0]).is_none());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn x_changes_the_default_of_the_whole_system() {
        let (mut choices, directory) = choices("default");
        let system = nes();
        let picker = Picker::open(&system, &system.games[0], &choices).unwrap();
        let Step::Chosen(message) = press(picker, &[Button::Down, Button::Down, Button::X], &system, &mut choices) else { panic!("chosen") };
        assert_eq!(message, "NES default: nestopia");
        assert_eq!(choices.resolve(&system, &system.games[0]).label, "nestopia");
        assert!(choices.pinned(&system, &system.games[0]).is_none(), "the game itself was not pinned");

        // X on the first row restores the configured default.
        let picker = Picker::open(&system, &system.games[0], &choices).unwrap();
        let Step::Chosen(message) = press(picker, &[Button::X], &system, &mut choices) else { panic!("chosen") };
        assert_eq!(message, "NES default restored: fceumm");
        assert_eq!(choices.resolve(&system, &system.games[0]).label, "fceumm");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn b_and_menu_close_without_changing_anything() {
        let (mut choices, directory) = choices("close");
        let system = nes();
        for button in [Button::B, Button::Menu, Button::Start] {
            let picker = Picker::open(&system, &system.games[0], &choices).unwrap();
            assert!(matches!(press(picker, &[Button::Down, button], &system, &mut choices), Step::Closed));
        }
        assert!(!directory.join("emulators.toml").exists());
        // Buttons the picker does not use leave it open where it was.
        let picker = Picker::open(&system, &system.games[0], &choices).unwrap();
        let Step::Open(picker) = press(picker, &[Button::L1, Button::Left], &system, &mut choices) else { panic!("still open") };
        assert_eq!(picker.view(&system, &system.games[0], &choices).cursor, 0);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_failed_save_is_reported_and_closes_the_picker() {
        let (_, directory) = choices("failure");
        std::fs::write(directory.join("emulators.toml"), "version = '99'").unwrap();
        let (mut choices, _) = Choices::open(&directory.join("emulators.toml"));
        let system = nes();
        let picker = Picker::open(&system, &system.games[0], &choices).unwrap();
        let Step::Failed(message) = press(picker, &[Button::Down, Button::A], &system, &mut choices) else { panic!("failed") };
        assert!(message.contains("Emulator choices are off"), "{message}");
        std::fs::remove_dir_all(directory).unwrap();
    }
}
