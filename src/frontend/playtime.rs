//! Time played per game: from an emulator's start until it exits, measured on
//! the monotonic clock so a network time step cannot bend it.
//!
//! Kept in its own file beside `activity.toml` rather than as a new activity
//! version: an older system on the other A/B slot refuses an activity version
//! it does not know, and would set the player's favorites aside. A version of
//! this file this build does not know is left alone, never replaced.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::activity::{Activity, GameId};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Played {
    pub seconds: u64,
    pub sessions: u32,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "version", deny_unknown_fields)]
enum Stored {
    #[serde(rename = "1")]
    V1 {
        #[serde(default)]
        games: Vec<StoredGame>,
    },
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredGame {
    path: PathBuf,
    seconds: u64,
    sessions: u32,
}

enum Backing {
    File(PathBuf),
    /// The file could not be used and must not be replaced: nothing is recorded.
    Unavailable,
}

pub struct PlayTime {
    backing: Backing,
    games: BTreeMap<PathBuf, Played>,
}

enum LoadError {
    Unreadable(anyhow::Error),
    Damaged(anyhow::Error),
    UnknownVersion(String),
}

impl PlayTime {
    /// Loads for the menu, which must start whatever the file holds: returns
    /// usable play time and a warning for the player when something was wrong.
    pub fn open(path: &Path) -> (Self, Option<String>) {
        let unavailable = || Self { backing: Backing::Unavailable, games: BTreeMap::new() };
        match load(path) {
            Ok(games) => (Self { backing: Backing::File(path.to_owned()), games }, None),
            Err(LoadError::Damaged(error)) => match Activity::set_aside(path) {
                Ok(aside) => {
                    eprintln!("ghazal: reading play time: {error:#}; moved it to {}", aside.display());
                    (Self { backing: Backing::File(path.to_owned()), games: BTreeMap::new() }, Some(format!("Play time was unreadable; kept it as {}", aside.display())))
                }
                Err(aside_error) => {
                    eprintln!("ghazal: reading play time: {error:#}; {aside_error:#}");
                    (unavailable(), Some("Play time is unreadable and will not be recorded".to_owned()))
                }
            },
            Err(LoadError::Unreadable(error)) => {
                eprintln!("ghazal: reading play time: {error:#}");
                (unavailable(), Some("Play time could not be read and will not be recorded".to_owned()))
            }
            Err(LoadError::UnknownVersion(version)) => {
                eprintln!("ghazal: play time file version {version} is newer than this system; leaving it alone");
                (unavailable(), None)
            }
        }
    }

    pub fn played(&self, game: &GameId) -> Played {
        self.games.get(game.path()).copied().unwrap_or_default()
    }

    /// Adds one session of `elapsed` to `game`.
    pub fn record(&mut self, game: &GameId, elapsed: Duration) -> Result<()> {
        let Backing::File(path) = &self.backing else { return Ok(()) };
        let mut games = self.games.clone();
        let played = games.entry(game.path().to_owned()).or_default();
        *played = Played { seconds: played.seconds + elapsed.as_secs(), sessions: played.sessions + 1 };
        let stored = Stored::V1 {
            games: games.iter().map(|(path, played)| StoredGame { path: path.clone(), seconds: played.seconds, sessions: played.sessions }).collect(),
        };
        crate::sys::write_atomic(path, toml::to_string(&stored)?.as_bytes())?;
        self.games = games;
        Ok(())
    }
}

fn load(path: &Path) -> Result<BTreeMap<PathBuf, Played>, LoadError> {
    let read = |path: &Path| match std::fs::read_to_string(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(LoadError::Unreadable(error.into())),
    };
    // exFAT has no journal: a power cut during the rename can leave only the staged copy.
    let Some(text) = (match read(path)? { Some(text) => Some(text), None => read(&crate::sys::atomic_pending(path))? }) else {
        return Ok(BTreeMap::new());
    };
    /// Read first, on its own, so that a future version's other fields cannot
    /// make it look damaged.
    #[derive(Deserialize)]
    struct Header {
        version: Option<String>,
    }
    let header: Header = toml::from_str(&text).map_err(|error| LoadError::Damaged(error.into()))?;
    match header.version.as_deref() {
        Some("1") => match toml::from_str::<Stored>(&text).map_err(|error| LoadError::Damaged(error.into()))? {
            Stored::V1 { games } => Ok(games.into_iter().map(|game| (game.path, Played { seconds: game.seconds, sessions: game.sessions })).collect()),
        },
        Some(other) => Err(LoadError::UnknownVersion(other.to_owned())),
        None => Err(LoadError::Damaged(anyhow::anyhow!("no version"))),
    }
}

/// How long a game was played, as the menu shows it: minutes under an hour,
/// then hours and minutes. Nothing for a game never played for a minute.
pub fn describe(played: Played) -> Option<String> {
    let minutes = played.seconds / 60;
    match (minutes / 60, minutes % 60) {
        (0, 0) => None,
        (0, minutes) => Some(format!("{minutes} min")),
        (hours, 0) => Some(format!("{hours} h")),
        (hours, minutes) => Some(format!("{hours} h {minutes} min")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let directory = std::env::temp_dir().join(format!("ghazal-playtime-{}-{}", std::process::id(), NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
            std::fs::create_dir(&directory).unwrap();
            Self(directory)
        }
        fn file(&self) -> PathBuf { self.0.join("playtime.toml") }
    }
    impl Drop for Temp { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }

    fn game(name: &str) -> GameId { GameId::from_path(format!("/ROMS/GB/{name}.gb").into()) }

    #[test]
    fn sessions_add_up_and_survive_a_reload() {
        let directory = Temp::new();
        let (mut playtime, warning) = PlayTime::open(&directory.file());
        assert!(warning.is_none());
        playtime.record(&game("a"), Duration::from_secs(90)).unwrap();
        playtime.record(&game("a"), Duration::from_secs(3_600)).unwrap();
        let (loaded, _) = PlayTime::open(&directory.file());
        assert_eq!(loaded.played(&game("a")), Played { seconds: 3_690, sessions: 2 });
        assert_eq!(loaded.played(&game("b")), Played::default());
    }

    #[test]
    fn a_newer_version_is_left_alone_and_damage_is_set_aside() {
        let directory = Temp::new();
        std::fs::write(directory.file(), "version = '2'\nsomething = 1\n").unwrap();
        let (mut playtime, warning) = PlayTime::open(&directory.file());
        assert!(warning.is_none());
        playtime.record(&game("a"), Duration::from_secs(60)).unwrap();
        assert_eq!(std::fs::read_to_string(directory.file()).unwrap(), "version = '2'\nsomething = 1\n");

        std::fs::write(directory.file(), "version = '1'\ngames = 3\n").unwrap();
        let (mut playtime, warning) = PlayTime::open(&directory.file());
        assert!(warning.is_some_and(|warning| warning.contains("unreadable")));
        playtime.record(&game("a"), Duration::from_secs(60)).unwrap();
        assert_eq!(PlayTime::open(&directory.file()).0.played(&game("a")).sessions, 1);
    }

    #[test]
    fn durations_read_as_hours_and_minutes() {
        let played = |seconds| Played { seconds, sessions: 1 };
        assert_eq!(describe(played(59)), None);
        assert_eq!(describe(played(60 * 45)), Some("45 min".to_owned()));
        assert_eq!(describe(played(3_600 * 2)), Some("2 h".to_owned()));
        assert_eq!(describe(played(3_600 * 3 + 60 * 20 + 59)), Some("3 h 20 min".to_owned()));
    }
}
