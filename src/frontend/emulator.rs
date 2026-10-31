//! The emulators a system can run its games with.
//!
//! A system always has a default (the first one in `frontend.toml`) and may
//! list alternatives. Which one a game uses is decided at launch time by
//! `emulator_choice`, from the player's choices.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::config::{Launch, Standalone};
use crate::device::CpuLoad;

/// A libretro core's file name, e.g. `snes9x_libretro.so`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(transparent)]
pub struct CoreName(String);

impl CoreName {
    pub fn as_str(&self) -> &str { &self.0 }

    /// The name without RetroArch's file decoration: `snes9x_libretro.so` -> `snes9x`.
    fn stem(&self) -> &str {
        let name = self.0.strip_suffix(".so").unwrap_or(&self.0);
        name.strip_suffix("_libretro").unwrap_or(name)
    }
}

impl From<&str> for CoreName { fn from(name: &str) -> Self { Self(name.to_owned()) } }
impl From<String> for CoreName { fn from(name: String) -> Self { Self(name) } }
impl fmt::Display for CoreName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.0) }
}

/// Names a standalone emulator within its system, e.g. `ppsspp`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(transparent)]
pub struct StandaloneName(String);

impl From<&str> for StandaloneName { fn from(name: &str) -> Self { Self(name.to_owned()) } }
impl From<String> for StandaloneName { fn from(name: String) -> Self { Self(name) } }
impl fmt::Display for StandaloneName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.0) }
}

/// What a player's choice refers to, as stored on the data card. It names the
/// emulator by its core file or standalone name, never by position, so adding
/// or reordering alternatives in `frontend.toml` keeps every choice valid.
/// In TOML: `{ core = "snes9x_libretro.so" }` or `{ standalone = "ppsspp" }`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum EmulatorId {
    Core(CoreName),
    Standalone(StandaloneName),
}

impl Launch {
    pub fn id(&self) -> EmulatorId {
        match self {
            Self::Core(core) => EmulatorId::Core(core.clone()),
            Self::Standalone(Standalone { name, .. }) => EmulatorId::Standalone(name.clone()),
        }
    }

    /// What the menu calls this emulator unless `frontend.toml` gives a label.
    pub fn default_label(&self) -> String {
        match self {
            Self::Core(core) => core.stem().to_owned(),
            Self::Standalone(standalone) => standalone.name.to_string(),
        }
    }
}

/// One way to run a system's games.
#[derive(Debug, Clone, PartialEq)]
pub struct Emulator {
    /// Shown in the emulator picker and on game rows.
    pub label: String,
    pub launch: Launch,
    /// The clock policy this emulator needs; each emulator is rated separately.
    pub cpu: CpuLoad,
}

impl Emulator {
    pub fn id(&self) -> EmulatorId { self.launch.id() }
}

/// A system's emulators: the default first, then the alternatives. Never
/// empty and without duplicates, by construction.
#[derive(Debug, Clone, PartialEq)]
pub struct Emulators {
    default: Emulator,
    alternatives: Vec<Emulator>,
}

/// Two emulators of one system share an identity, so a stored choice could
/// not tell them apart.
#[derive(Debug, PartialEq, Eq)]
pub struct Duplicate(pub EmulatorId);

impl fmt::Display for Duplicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            EmulatorId::Core(core) => write!(f, "core {core} is listed twice"),
            EmulatorId::Standalone(name) => write!(f, "standalone emulator {name:?} is listed twice; give one a different `name`"),
        }
    }
}

impl Emulators {
    pub fn new(default: Emulator, alternatives: Vec<Emulator>) -> Result<Self, Duplicate> {
        let mut seen = vec![default.id()];
        for emulator in &alternatives {
            let id = emulator.id();
            if seen.contains(&id) { return Err(Duplicate(id)); }
            seen.push(id);
        }
        Ok(Self { default, alternatives })
    }

    /// A system with no alternatives.
    #[cfg(test)]
    pub fn only(launch: Launch, cpu: CpuLoad) -> Self {
        Self { default: Emulator { label: launch.default_label(), launch, cpu }, alternatives: Vec::new() }
    }

    /// The emulator `frontend.toml` assigns when nothing else is chosen.
    pub fn default(&self) -> &Emulator { &self.default }

    /// Every emulator, the default first.
    pub fn iter(&self) -> impl Iterator<Item = &Emulator> {
        std::iter::once(&self.default).chain(&self.alternatives)
    }

    pub fn get(&self, id: &EmulatorId) -> Option<&Emulator> {
        self.iter().find(|emulator| &emulator.id() == id)
    }

    pub fn len(&self) -> usize { 1 + self.alternatives.len() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn core(file: &str) -> Emulator {
        let launch = Launch::Core(file.into());
        Emulator { label: launch.default_label(), launch, cpu: CpuLoad::Full }
    }

    #[test]
    fn labels_default_to_the_core_without_its_file_decoration() {
        assert_eq!(core("snes9x2010_libretro.so").label, "snes9x2010");
        assert_eq!(core("mednafen_pce_fast.so").label, "mednafen_pce_fast");
        assert_eq!(core("odd").label, "odd");
    }

    #[test]
    fn identities_come_from_the_core_or_standalone_name() {
        assert_eq!(core("a_libretro.so").id(), EmulatorId::Core("a_libretro.so".into()));
        let launch = Launch::Standalone(Standalone { name: "ppsspp".into(), argv: vec!["/usr/bin/ppsspp".into()], env: Default::default(), log: None });
        assert_eq!(launch.id(), EmulatorId::Standalone("ppsspp".into()));
        assert_eq!(launch.default_label(), "ppsspp");
    }

    #[test]
    fn a_system_lists_its_default_first_and_rejects_duplicates() {
        let emulators = Emulators::new(core("a_libretro.so"), vec![core("b_libretro.so")]).unwrap();
        assert_eq!(emulators.iter().map(|e| e.label.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(emulators.len(), 2);
        assert_eq!(emulators.default().label, "a");
        assert_eq!(emulators.get(&EmulatorId::Core("b_libretro.so".into())).unwrap().label, "b");
        assert!(emulators.get(&EmulatorId::Core("c_libretro.so".into())).is_none());
        // Same file, different kind of emulator: distinct.
        assert!(emulators.get(&EmulatorId::Standalone("a_libretro.so".into())).is_none());
        assert_eq!(Emulators::new(core("a_libretro.so"), vec![core("b_libretro.so"), core("a_libretro.so")]).unwrap_err(), Duplicate(EmulatorId::Core("a_libretro.so".into())));
    }
}
