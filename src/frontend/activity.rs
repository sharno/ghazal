use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::library::{Game, Library, System};

const RECENT_LIMIT: usize = 100;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GameId {
    path: PathBuf,
}

impl GameId {
    pub fn new(_system: &System, game: &Game) -> Self {
        Self { path: game.path.clone() }
    }

    pub fn resolve<'a>(&self, library: &'a Library) -> Option<(&'a System, &'a Game)> {
        let mut matches = library.systems.iter().flat_map(|system| {
            system.games.iter().filter(|game| game.path == self.path).map(move |game| (system, game))
        });
        let found = matches.next()?;
        matches.next().is_none().then_some(found)
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "version", deny_unknown_fields)]
enum StoredActivity {
    #[serde(rename = "1")]
    V1 { favorites: BTreeSet<LegacyGameId>, recent: Vec<LegacyGameId> },
    #[serde(rename = "2")]
    V2 { favorites: BTreeSet<GameId>, recent: Vec<GameId> },
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LegacyGameId { system: String, path: PathBuf }

#[derive(Clone)]
struct ActivityData { favorites: BTreeSet<GameId>, recent: Vec<GameId> }

pub struct Activity {
    path: PathBuf,
    stored: ActivityData,
}

impl Activity {
    pub fn load(path: &Path) -> Result<Self> {
        let stored = match fs::read_to_string(path) {
            Ok(contents) => toml::from_str(&contents)
                .with_context(|| format!("reading game activity {}", path.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound =>
                StoredActivity::V2 { favorites: BTreeSet::new(), recent: Vec::new() },
            Err(error) => return Err(error.into()),
        };
        let (stored, migrated) = match stored {
            StoredActivity::V1 { favorites, recent } => {
                let mut seen = BTreeSet::new();
                let recent = recent.into_iter().map(|id| GameId { path: id.path })
                    .filter(|id| seen.insert(id.clone())).take(RECENT_LIMIT).collect();
                (ActivityData { favorites: favorites.into_iter().map(|id| GameId { path: id.path }).collect(), recent }, true)
            }
            StoredActivity::V2 { favorites, recent } => (ActivityData { favorites, recent }, false),
        };
        let mut activity = Self { path: path.to_owned(), stored };
        if migrated { activity.commit(activity.stored.clone())?; }
        Ok(activity)
    }

    pub fn is_favorite(&self, game: &GameId) -> bool {
        self.stored.favorites.contains(game)
    }

    pub fn favorites(&self) -> impl Iterator<Item = &GameId> {
        self.stored.favorites.iter()
    }

    pub fn recent(&self) -> impl Iterator<Item = &GameId> {
        self.stored.recent.iter()
    }

    pub fn toggle_favorite(&mut self, game: GameId) -> Result<bool> {
        let mut candidate = self.stored.clone();
        let favorites = &mut candidate.favorites;
        let added = if favorites.remove(&game) { false } else { favorites.insert(game); true };
        self.commit(candidate)?;
        Ok(added)
    }

    pub fn launched(&mut self, game: GameId) -> Result<()> {
        let mut candidate = self.stored.clone();
        let recent = &mut candidate.recent;
        recent.retain(|item| item != &game);
        recent.insert(0, game);
        recent.truncate(RECENT_LIMIT);
        self.commit(candidate)
    }

    fn commit(&mut self, candidate: ActivityData) -> Result<()> {
        let serialized = StoredActivity::V2 { favorites: candidate.favorites.clone(), recent: candidate.recent.clone() };
        crate::sys::write_atomic(&self.path, toml::to_string(&serialized)?.as_bytes())?;
        self.stored = candidate;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Temp(std::path::PathBuf);
    impl Temp {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let directory = std::env::temp_dir().join(format!("oxmux-activity-{}-{}", std::process::id(), NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
            fs::create_dir(&directory).unwrap();
            Self(directory)
        }
        fn file(&self) -> PathBuf { self.0.join("activity.toml") }
    }
    impl Drop for Temp { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }

    fn game(index: usize) -> GameId { GameId { path: format!("/ROMS/GB/{index}.gb").into() } }

    #[test]
    fn legacy_labels_migrate_without_losing_favorites_or_duplicating_history() {
        let directory = Temp::new();
        fs::write(directory.file(), "version = '1'\nfavorites = [{system = 'Arcade', path = '/ROMS/MAME/005.zip'}]\nrecent = [{system = 'Arcade', path = '/ROMS/MAME/005.zip'}, {system = 'MAME 2003 Plus', path = '/ROMS/MAME/005.zip'}]").unwrap();
        let loaded = Activity::load(&directory.file()).unwrap();
        let library = Library { systems: vec![System { name: "MAME 2003 Plus".into(), core: "mame2003_plus_libretro.so".into(), games: vec![Game { title: "005".into(), path: "/ROMS/MAME/005.zip".into() }] }] };
        let id = GameId::new(&library.systems[0], &library.systems[0].games[0]);
        assert!(loaded.is_favorite(&id));
        assert_eq!(loaded.recent().count(), 1);
        assert_eq!(id.resolve(&library).unwrap().0.name, "MAME 2003 Plus");
        assert!(fs::read_to_string(directory.file()).unwrap().contains("version = \"2\""));
    }

    #[test]
    fn favorites_round_trip_and_toggle() {
        let directory = Temp::new();
        let mut activity = Activity::load(&directory.file()).unwrap();
        assert!(activity.toggle_favorite(game(1)).unwrap());
        let mut loaded = Activity::load(&directory.file()).unwrap();
        assert!(loaded.is_favorite(&game(1)));
        assert!(!loaded.toggle_favorite(game(1)).unwrap());
        assert!(!Activity::load(&directory.file()).unwrap().is_favorite(&game(1)));
    }

    #[test]
    fn history_is_unique_recent_and_bounded() {
        let directory = Temp::new();
        let mut activity = Activity::load(&directory.file()).unwrap();
        for index in 0..105 { activity.launched(game(index)).unwrap(); }
        activity.launched(game(50)).unwrap();
        let loaded = Activity::load(&directory.file()).unwrap();
        let recent: Vec<_> = loaded.recent().collect();
        assert_eq!(recent.len(), RECENT_LIMIT);
        assert_eq!(recent[0], &game(50));
        assert_eq!(recent.iter().filter(|id| **id == &game(50)).count(), 1);
        assert!(!recent.contains(&&game(0)));
    }

    #[test]
    fn invalid_data_is_reported_without_overwriting() {
        let directory = Temp::new();
        fs::write(directory.file(), "version = 'unknown'").unwrap();
        assert!(Activity::load(&directory.file()).is_err());
        assert_eq!(fs::read_to_string(directory.file()).unwrap(), "version = 'unknown'");
    }
}
