//! Which emulator runs a game: the player's choices over `frontend.toml`.
//!
//! A game uses, in order: the emulator pinned to that game, the emulator the
//! player made their default for its system, and the system's own emulator.
//! Choices live on the data card beside `activity.toml`. They name emulators
//! by core file or standalone name (see `EmulatorId`), so they survive
//! changes to the alternatives a system lists: a choice naming an emulator
//! the system no longer has is ignored, and stays in the file until the
//! player picks again, in case a later update brings the emulator back.
//!
//! The file is versioned like `activity.toml`. A version this build does not
//! know is left alone, never replaced: it was most likely written by a newer
//! system on the other A/B slot.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::activity::Activity;
use super::emulator::{Emulator, EmulatorId};
use super::library::{Game, System};

#[derive(Deserialize, Serialize)]
#[serde(tag = "version", deny_unknown_fields)]
enum Stored {
    #[serde(rename = "1")]
    V1 {
        /// System name -> the player's default for it.
        #[serde(default)]
        systems: BTreeMap<String, EmulatorId>,
        #[serde(default)]
        games: Vec<StoredGame>,
    },
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredGame {
    path: PathBuf,
    emulator: EmulatorId,
}

/// The current in-memory shape. A new file version adds a `Stored` variant and
/// a conversion here; nothing else reads the stored form.
#[derive(Debug, Clone, Default, PartialEq)]
struct Data {
    systems: BTreeMap<String, EmulatorId>,
    games: BTreeMap<PathBuf, EmulatorId>,
}

impl Stored {
    fn into_current(self) -> Data {
        match self {
            Self::V1 { systems, games } => Data { systems, games: games.into_iter().map(|game| (game.path, game.emulator)).collect() },
        }
    }
}

impl Data {
    fn to_stored(&self) -> Stored {
        Stored::V1 {
            systems: self.systems.clone(),
            games: self.games.iter().map(|(path, emulator)| StoredGame { path: path.clone(), emulator: emulator.clone() }).collect(),
        }
    }
}

/// Why the file could not be used. Each case calls for different treatment.
#[derive(Debug)]
pub enum LoadError {
    /// A read error (a card hiccup): nothing is known about the contents, so
    /// the file must not be replaced.
    Unreadable(anyhow::Error),
    /// Not valid TOML, or not the shape its version promises. Set aside.
    Damaged(anyhow::Error),
    /// A version this build does not know. Left alone.
    UnknownVersion(String),
}

enum Backing {
    File(PathBuf),
    /// Choices could not be loaded and the file must not be replaced:
    /// launches use the system defaults and changes are refused.
    Unavailable { reason: String },
}

pub struct Choices {
    backing: Backing,
    data: Data,
}

impl Choices {
    pub fn load(path: &Path) -> Result<Self, LoadError> {
        let read = |path: &Path| match std::fs::read_to_string(path) {
            Ok(contents) => Ok(Some(contents)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(LoadError::Unreadable(error.into())),
        };
        // exFAT has no journal: a power cut during the rename can leave only the staged copy.
        let contents = match read(path)? {
            Some(contents) => Some(contents),
            None => read(&crate::sys::atomic_pending(path))?,
        };
        let data = match contents {
            Some(contents) => parse(&contents).map_err(|error| match error {
                ParseError::Damaged(error) => LoadError::Damaged(error.context(format!("reading emulator choices {}", path.display()))),
                ParseError::UnknownVersion(version) => LoadError::UnknownVersion(version),
            })?,
            None => Data::default(),
        };
        Ok(Self { backing: Backing::File(path.to_owned()), data })
    }

    /// What the menu would launch with, for observers: a file that cannot be
    /// used means the system defaults, and the file is never touched.
    pub fn read_only(path: &Path) -> Self {
        Self::load(path).unwrap_or_else(|error| Self { backing: Backing::Unavailable { reason: format!("{error:?}") }, data: Data::default() })
    }

    /// Loads for the menu, which must start whatever the file holds: returns
    /// usable choices and a warning for the player when something was wrong.
    pub fn open(path: &Path) -> (Self, Option<String>) {
        let unavailable = |reason: String| Self { backing: Backing::Unavailable { reason }, data: Data::default() };
        match Self::load(path) {
            Ok(choices) => (choices, None),
            Err(LoadError::Damaged(error)) => match Activity::set_aside(path) {
                Ok(aside) => {
                    eprintln!("ghazal: {error:#}; moved it to {}", aside.display());
                    let choices = Self { backing: Backing::File(path.to_owned()), data: Data::default() };
                    (choices, Some(format!("Emulator choices were unreadable; kept them as {}", aside.display())))
                }
                Err(aside_error) => {
                    eprintln!("ghazal: {error:#}; {aside_error:#}");
                    (unavailable(format!("{error:#}")), Some("Emulator choices are unreadable; using system defaults".into()))
                }
            },
            Err(LoadError::Unreadable(error)) => {
                eprintln!("ghazal: reading emulator choices: {error:#}");
                (unavailable(format!("{error:#}")), Some("Emulator choices could not be read; using system defaults".into()))
            }
            Err(LoadError::UnknownVersion(version)) => {
                eprintln!("ghazal: {}: unknown format version {version:?}; leaving it alone", path.display());
                (unavailable(format!("format version {version:?} is newer than this system")), Some("Emulator choices are from a newer system; using defaults".into()))
            }
        }
    }

    /// The emulator pinned to this game, if the system still has it.
    pub fn pinned<'a>(&self, system: &'a System, game: &Game) -> Option<&'a Emulator> {
        system.emulators.get(self.data.games.get(&game.path)?)
    }

    /// The player's default for the system, if it still has that emulator.
    fn chosen_default<'a>(&self, system: &'a System) -> Option<&'a Emulator> {
        system.emulators.get(self.data.systems.get(&system.name)?)
    }

    /// What games of the system run with unless pinned.
    pub fn default_for<'a>(&self, system: &'a System) -> &'a Emulator {
        self.chosen_default(system).unwrap_or_else(|| system.emulators.default())
    }

    /// The emulator that runs the game.
    pub fn resolve<'a>(&self, system: &'a System, game: &Game) -> &'a Emulator {
        self.pinned(system, game).unwrap_or_else(|| self.default_for(system))
    }

    /// Run this game with `emulator`, whatever the system's default becomes.
    pub fn pin(&mut self, game: &Game, emulator: &Emulator) -> Result<()> {
        let mut candidate = self.data.clone();
        candidate.games.insert(game.path.clone(), emulator.id());
        self.commit(candidate)
    }

    /// The game follows its system's default again.
    pub fn unpin(&mut self, game: &Game) -> Result<()> {
        let mut candidate = self.data.clone();
        candidate.games.remove(&game.path);
        self.commit(candidate)
    }

    /// Games of `system` without a pin run with `emulator`. Choosing the
    /// system's own emulator forgets the choice.
    pub fn set_default(&mut self, system: &System, emulator: &Emulator) -> Result<()> {
        let mut candidate = self.data.clone();
        if emulator.id() == system.emulators.default().id() {
            candidate.systems.remove(&system.name);
        } else {
            candidate.systems.insert(system.name.clone(), emulator.id());
        }
        self.commit(candidate)
    }

    pub fn reset_default(&mut self, system: &System) -> Result<()> {
        let mut candidate = self.data.clone();
        candidate.systems.remove(&system.name);
        self.commit(candidate)
    }

    fn commit(&mut self, candidate: Data) -> Result<()> {
        let path = match &self.backing {
            Backing::File(path) => path,
            Backing::Unavailable { reason } => bail!("Emulator choices are off: {reason}"),
        };
        let text = toml::to_string(&candidate.to_stored())?;
        crate::sys::write_atomic(path, text.as_bytes()).with_context(|| format!("saving {}", path.display()))?;
        self.data = candidate;
        Ok(())
    }
}

enum ParseError {
    Damaged(anyhow::Error),
    UnknownVersion(String),
}

fn parse(text: &str) -> Result<Data, ParseError> {
    /// Read first, on its own, so that a future version's other fields cannot
    /// make it look damaged.
    #[derive(Deserialize)]
    struct Header {
        version: Option<String>,
    }
    let header: Header = toml::from_str(text).map_err(|error| ParseError::Damaged(error.into()))?;
    match header.version.as_deref() {
        Some("1") => toml::from_str::<Stored>(text).map(Stored::into_current).map_err(|error| ParseError::Damaged(error.into())),
        Some(other) => Err(ParseError::UnknownVersion(other.to_owned())),
        None => Err(ParseError::Damaged(anyhow::anyhow!("no version"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::CpuLoad;
    use crate::frontend::config::{Launch, Standalone};
    use crate::frontend::emulator::Emulators;
    use std::fs;

    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let directory = std::env::temp_dir().join(format!("ghazal-emulators-{}-{}", std::process::id(), NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
            fs::create_dir(&directory).unwrap();
            Self(directory)
        }
        fn file(&self) -> PathBuf { self.0.join("emulators.toml") }
    }
    impl Drop for Temp { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }

    fn core(file: &str, cpu: CpuLoad) -> Emulator {
        let launch = Launch::Core(file.into());
        Emulator { label: launch.default_label(), launch, cpu }
    }

    fn standalone(name: &str) -> Emulator {
        let launch = Launch::Standalone(Standalone { name: name.into(), argv: vec![format!("/usr/bin/{name}"), "{rom}".into()], env: Default::default(), log: None });
        Emulator { label: launch.default_label(), launch, cpu: CpuLoad::Full }
    }

    fn snes() -> System {
        let emulators = Emulators::new(core("snes9x_libretro.so", CpuLoad::Moderate), vec![core("snes9x2010_libretro.so", CpuLoad::Light), standalone("bsnes")]).unwrap();
        System { name: "SNES".into(), art: Default::default(), emulators, games: vec![game("/ROMS/SNES/a.sfc"), game("/ROMS/SNES/b.sfc")] }
    }

    fn game(path: &str) -> Game { Game { title: path.into(), path: path.into() } }

    fn id(emulator: &Emulator) -> EmulatorId { emulator.id() }

    #[test]
    fn games_use_their_pin_then_the_system_default_then_the_configured_emulator() {
        let directory = Temp::new();
        let system = snes();
        let [stock, fast, bsnes] = [0, 1, 2].map(|index| system.emulators.iter().nth(index).unwrap());
        let (a, b) = (&system.games[0], &system.games[1]);
        let mut choices = Choices::load(&directory.file()).unwrap();
        assert_eq!(id(choices.resolve(&system, a)), id(stock));
        assert!(choices.pinned(&system, a).is_none());

        choices.set_default(&system, fast).unwrap();
        assert_eq!(id(choices.resolve(&system, a)), id(fast));
        assert_eq!(id(choices.default_for(&system)), id(fast));

        choices.pin(b, bsnes).unwrap();
        assert_eq!(id(choices.resolve(&system, b)), id(bsnes));
        assert_eq!(id(choices.resolve(&system, a)), id(fast), "a pin applies to its own game only");
        assert_eq!(id(choices.pinned(&system, b).unwrap()), id(bsnes));

        choices.unpin(b).unwrap();
        assert_eq!(id(choices.resolve(&system, b)), id(fast));
        choices.reset_default(&system).unwrap();
        assert_eq!(id(choices.resolve(&system, b)), id(stock));
    }

    #[test]
    fn choosing_the_configured_emulator_as_default_forgets_the_choice() {
        let directory = Temp::new();
        let system = snes();
        let mut choices = Choices::load(&directory.file()).unwrap();
        choices.set_default(&system, system.emulators.iter().nth(1).unwrap()).unwrap();
        choices.set_default(&system, system.emulators.default()).unwrap();
        assert!(Choices::load(&directory.file()).unwrap().data.systems.is_empty());
    }

    #[test]
    fn choices_survive_a_restart() {
        let directory = Temp::new();
        let system = snes();
        let [_, fast, bsnes] = [0, 1, 2].map(|index| system.emulators.iter().nth(index).unwrap());
        let mut choices = Choices::load(&directory.file()).unwrap();
        choices.set_default(&system, fast).unwrap();
        choices.pin(&system.games[1], bsnes).unwrap();
        let loaded = Choices::load(&directory.file()).unwrap();
        assert_eq!(loaded.data, choices.data);
        assert_eq!(id(loaded.resolve(&system, &system.games[0])), id(fast));
        assert_eq!(id(loaded.resolve(&system, &system.games[1])), id(bsnes));
        let text = fs::read_to_string(directory.file()).unwrap();
        assert!(text.starts_with("version = \"1\""), "{text}");
    }

    /// The version 1 file as shipped. Changing how it reads would silently
    /// drop players' choices after an update: add a version instead.
    #[test]
    fn version_1_files_keep_reading() {
        let directory = Temp::new();
        fs::write(directory.file(), r#"
version = "1"

[systems]
SNES = { core = "snes9x2010_libretro.so" }

[systems."Game Boy Advance"]
core = "vba_next_libretro.so"

[[games]]
path = "/ROMS/SNES/b.sfc"
emulator = { standalone = "bsnes" }

[[games]]
path = "/ROMS/PSP/Game.iso"
emulator = { core = "ppsspp_libretro.so" }
"#).unwrap();
        let choices = Choices::load(&directory.file()).unwrap();
        let system = snes();
        assert_eq!(id(choices.default_for(&system)), EmulatorId::Core("snes9x2010_libretro.so".into()));
        assert_eq!(id(choices.resolve(&system, &system.games[1])), EmulatorId::Standalone("bsnes".into()));
        assert_eq!(choices.data.systems.len(), 2);
        assert_eq!(choices.data.games.len(), 2);
    }

    #[test]
    fn a_choice_naming_a_removed_emulator_is_ignored_but_kept() {
        let directory = Temp::new();
        fs::write(directory.file(), "version = '1'\n[systems]\nSNES = { core = 'gone_libretro.so' }\n[[games]]\npath = '/ROMS/SNES/a.sfc'\nemulator = { standalone = 'gone' }\n[[games]]\npath = '/ROMS/SNES/b.sfc'\nemulator = { core = 'snes9x2010_libretro.so' }\n").unwrap();
        let system = snes();
        let mut choices = Choices::load(&directory.file()).unwrap();
        assert_eq!(id(choices.resolve(&system, &system.games[0])), id(system.emulators.default()));
        assert_eq!(id(choices.resolve(&system, &system.games[1])), EmulatorId::Core("snes9x2010_libretro.so".into()));
        // Saving another choice must not drop what this build cannot use.
        choices.pin(&system.games[1], system.emulators.default()).unwrap();
        let text = fs::read_to_string(directory.file()).unwrap();
        assert!(text.contains("gone_libretro.so") && text.contains("gone"), "{text}");
    }

    #[test]
    fn an_unknown_version_is_left_alone_and_choices_stay_off() {
        let directory = Temp::new();
        let future = "version = '7'\nsomething = { new = true }\n";
        fs::write(directory.file(), future).unwrap();
        assert!(matches!(Choices::load(&directory.file()), Err(LoadError::UnknownVersion(version)) if version == "7"));
        let (mut choices, warning) = Choices::open(&directory.file());
        assert!(warning.unwrap().contains("newer"));
        let system = snes();
        assert_eq!(id(choices.resolve(&system, &system.games[0])), id(system.emulators.default()));
        let refused = choices.pin(&system.games[0], system.emulators.iter().nth(1).unwrap()).unwrap_err();
        assert!(refused.to_string().contains("off"), "{refused}");
        assert_eq!(fs::read_to_string(directory.file()).unwrap(), future);
    }

    #[test]
    fn a_damaged_file_is_set_aside_and_choices_start_empty() {
        let directory = Temp::new();
        for damaged in ["version = [", "systems = {}", "version = '1'\ngames = 3", "version = '1'\n[systems]\nSNES = { core = 'a', standalone = 'b' }"] {
            fs::write(directory.file(), damaged).unwrap();
            assert!(matches!(Choices::load(&directory.file()), Err(LoadError::Damaged(_))), "{damaged}");
            let (mut choices, warning) = Choices::open(&directory.file());
            assert!(warning.unwrap().contains("kept them as"));
            let aside = fs::read_dir(&directory.0).unwrap().flatten().find(|entry| entry.file_name().to_string_lossy().contains(".invalid-")).unwrap().path();
            assert_eq!(fs::read_to_string(&aside).unwrap(), damaged);
            fs::remove_file(aside).unwrap();
            let system = snes();
            // The fresh store works.
            choices.pin(&system.games[0], system.emulators.iter().nth(1).unwrap()).unwrap();
            assert_eq!(id(Choices::load(&directory.file()).unwrap().resolve(&system, &system.games[0])), id(system.emulators.iter().nth(1).unwrap()));
            fs::remove_file(directory.file()).unwrap();
        }
    }

    #[test]
    fn an_unreadable_file_is_never_replaced() {
        let directory = Temp::new();
        // A directory where the file belongs: reading fails with something other than NotFound.
        fs::create_dir(directory.file()).unwrap();
        assert!(matches!(Choices::load(&directory.file()), Err(LoadError::Unreadable(_))));
        let (mut choices, warning) = Choices::open(&directory.file());
        assert!(warning.is_some());
        let system = snes();
        assert!(choices.set_default(&system, system.emulators.iter().nth(1).unwrap()).is_err());
        assert!(directory.file().is_dir());
    }

    #[test]
    fn a_staged_copy_is_read_when_the_rename_never_happened() {
        let directory = Temp::new();
        fs::write(crate::sys::atomic_pending(&directory.file()), "version = '1'\n[systems]\nSNES = { core = 'snes9x2010_libretro.so' }\n").unwrap();
        let system = snes();
        assert_eq!(id(Choices::load(&directory.file()).unwrap().default_for(&system)), EmulatorId::Core("snes9x2010_libretro.so".into()));
    }
}
