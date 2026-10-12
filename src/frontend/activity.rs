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
        library.resolve_path(&self.path)
    }
}

#[derive(Clone, Copy)]
pub enum Collection { Favorites, Recent }

/// Own library and activity together so resolved collections cannot outlive
/// their library or become stale after a favorite, launch or rescan.
pub struct Catalog {
    library: Library,
    activity: Activity,
    favorites: Vec<GameId>,
    recent: Vec<GameId>,
}

impl Catalog {
    pub fn new(library: Library, activity: Activity) -> Self {
        let mut catalog = Self { library, activity, favorites: Vec::new(), recent: Vec::new() };
        catalog.refresh();
        catalog
    }
    pub fn library(&self) -> &Library { &self.library }
    pub fn replace_library(&mut self, library: Library) { self.library = library; self.refresh(); }
    pub fn is_favorite(&self, game: &GameId) -> bool { self.activity.is_favorite(game) }
    pub fn toggle_favorite(&mut self, game: GameId) -> Result<bool> {
        let added = self.activity.toggle_favorite(game)?;
        self.refresh();
        Ok(added)
    }
    pub fn launched(&mut self, game: GameId) -> Result<()> { self.activity.launched(game)?; self.refresh(); Ok(()) }
    fn collection(&self, collection: Collection) -> &[GameId] {
        match collection { Collection::Favorites => &self.favorites, Collection::Recent => &self.recent }
    }
    pub fn collection_len(&self, collection: Collection) -> usize { self.collection(collection).len() }
    pub fn collection_game(&self, collection: Collection, index: usize) -> Option<(&System, &Game)> {
        self.collection(collection).get(index)?.resolve(&self.library)
    }
    fn refresh(&mut self) {
        self.favorites = self.activity.favorites().filter(|id| id.resolve(&self.library).is_some()).cloned().collect();
        self.recent = self.activity.recent().filter(|id| id.resolve(&self.library).is_some()).cloned().collect();
    }
}

/// Exercise the former and current 48-row lookup paths with 100 recent games.
/// Only a private temporary activity fixture is written; user history is untouched.
pub fn benchmark(config: &super::config::FrontendConfig) -> Result<()> {
    use std::hint::black_box;
    use std::time::{Instant, SystemTime, UNIX_EPOCH};
    struct Fixture(PathBuf);
    impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
    let started = Instant::now();
    let library = Library::scan(config);
    println!("library_games={} scan_ms={:.3}", library.game_count(), started.elapsed().as_secs_f64() * 1000.0);
    let ids: Vec<_> = library.systems().iter().flat_map(|system| system.games.iter().map(move |game| GameId::new(system, game))).rev().take(100).collect();
    if ids.is_empty() { anyhow::bail!("benchmark requires games"); }
    let fixture = Fixture(std::env::temp_dir().join(format!("oxmux-recent-{}-{}", std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos())));
    fs::create_dir(&fixture.0)?;
    let mut activity = Activity::load(&fixture.0.join("activity.toml"))?;
    for id in &ids { activity.launched(id.clone())?; }
    let catalog = Catalog::new(library, activity);
    let linear = |id: &GameId| {
        let mut matches = catalog.library().systems().iter().flat_map(|system| system.games.iter().filter(|game| game.path == id.path));
        let game = matches.next()?;
        matches.next().is_none().then_some(game)
    };
    let rows = ids.len().min(48);
    let started = Instant::now();
    for _ in 0..3 {
        black_box(ids.iter().filter(|id| linear(id).is_some()).count());
        for index in 0..rows { black_box(ids.iter().filter_map(&linear).nth(index)); }
    }
    let linear_us = started.elapsed().as_secs_f64() * 1_000_000.0 / 3.0;
    let started = Instant::now();
    for _ in 0..2000 {
        black_box(catalog.collection_len(Collection::Recent));
        for index in 0..rows { black_box(catalog.collection_game(Collection::Recent, index)); }
    }
    let indexed_us = started.elapsed().as_secs_f64() * 1_000_000.0 / 2000.0;
    println!("recent_entries={} rows={rows} linear_window_us={linear_us:.3} indexed_window_us={indexed_us:.3}", ids.len());
    Ok(())
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
    fn catalog_updates_collections_after_activity_changes_and_library_replacement() {
        let directory = Temp::new();
        let library = |indices: &[usize]| Library::from_systems(vec![System { name: "GB".into(), core: "test.so".into(), games: indices.iter().map(|index| Game { title: index.to_string(), path: game(*index).path }).collect() }]);
        let mut catalog = Catalog::new(library(&[0, 1]), Activity::load(&directory.file()).unwrap());
        catalog.launched(game(0)).unwrap();
        catalog.launched(game(1)).unwrap();
        catalog.toggle_favorite(game(1)).unwrap();
        assert_eq!(catalog.collection_len(Collection::Recent), 2);
        assert_eq!(catalog.collection_game(Collection::Recent, 0).unwrap().1.title, "1");
        assert_eq!(catalog.collection_len(Collection::Favorites), 1);
        catalog.replace_library(library(&[0]));
        assert_eq!(catalog.collection_len(Collection::Recent), 1);
        assert_eq!(catalog.collection_len(Collection::Favorites), 0);
        catalog.replace_library(library(&[0, 1]));
        assert_eq!(catalog.collection_len(Collection::Favorites), 1);
        assert_eq!(catalog.collection_len(Collection::Recent), 2);
        catalog.toggle_favorite(game(1)).unwrap();
        assert_eq!(catalog.collection_len(Collection::Favorites), 0);
    }

    #[test]
    fn legacy_labels_migrate_without_losing_favorites_or_duplicating_history() {
        let directory = Temp::new();
        fs::write(directory.file(), "version = '1'\nfavorites = [{system = 'Arcade', path = '/ROMS/MAME/005.zip'}]\nrecent = [{system = 'Arcade', path = '/ROMS/MAME/005.zip'}, {system = 'MAME 2003 Plus', path = '/ROMS/MAME/005.zip'}]").unwrap();
        let loaded = Activity::load(&directory.file()).unwrap();
        let library = Library::from_systems(vec![System { name: "MAME 2003 Plus".into(), core: "mame2003_plus_libretro.so".into(), games: vec![Game { title: "005".into(), path: "/ROMS/MAME/005.zip".into() }] }]);
        let id = GameId::new(&library.systems()[0], &library.systems()[0].games[0]);
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
