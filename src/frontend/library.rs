use std::path::{Path, PathBuf};
use std::collections::BTreeMap;

use super::config::{FrontendConfig, Launch, SystemEntry};

const MAX_DEPTH: usize = 3;

pub struct Library {
    /// Only systems that have at least one game, in config order.
    systems: Vec<System>,
    paths: BTreeMap<PathBuf, PathMatch>,
}

enum PathMatch { Unique { system: usize, game: usize }, Ambiguous }

pub struct System {
    pub name: String,
    pub launch: Launch,
    pub games: Vec<Game>,
}

pub struct Game {
    pub title: String,
    pub path: PathBuf,
}

impl Library {
    pub fn scan(config: &FrontendConfig) -> Self {
        let systems = config
            .systems
            .iter()
            .filter(|sys| {
                let available = sys.requires_any.is_empty() || sys.requires_any.iter().any(|bios| config.bios_dir.join(bios).exists());
                if !available {
                    eprintln!("oxmux: hiding {}: needs one of {:?} in {}", sys.name, sys.requires_any, config.bios_dir.display());
                }
                available
            })
            .filter_map(|sys| {
                let mut games = Vec::new();
                for root in &config.rom_roots {
                    for dir in matching_dirs(root, sys) {
                        collect_games(&dir, sys, 0, &mut games);
                    }
                }
                games.sort_by_cached_key(|g| g.title.to_lowercase());
                games.dedup_by(|a, b| a.path == b.path);
                (!games.is_empty()).then(|| System {
                    name: sys.name.clone(),
                    launch: sys.launch.clone(),
                    games,
                })
            })
            .collect();
        Self::from_systems(systems)
    }

    pub fn from_systems(systems: Vec<System>) -> Self {
        let mut paths = BTreeMap::new();
        for (system_index, system) in systems.iter().enumerate() {
            for (game_index, game) in system.games.iter().enumerate() {
                paths.entry(game.path.clone()).and_modify(|entry| *entry = PathMatch::Ambiguous)
                    .or_insert(PathMatch::Unique { system: system_index, game: game_index });
            }
        }
        Self { systems, paths }
    }

    pub fn systems(&self) -> &[System] { &self.systems }

    pub fn resolve_path(&self, path: &Path) -> Option<(&System, &Game)> {
        match self.paths.get(path)? {
            PathMatch::Unique { system, game } => Some((&self.systems[*system], &self.systems[*system].games[*game])),
            PathMatch::Ambiguous => None,
        }
    }

    pub fn game_count(&self) -> usize {
        self.systems.iter().map(|s| s.games.len()).sum()
    }
}

/// Sub-folders of `root` whose name matches one of the system's `dirs`, ignoring case.
fn matching_dirs(root: &Path, sys: &SystemEntry) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            sys.dirs.iter().any(|d| d.eq_ignore_ascii_case(&name))
        })
        .map(|e| e.path())
        .collect()
}

fn collect_games(dir: &Path, sys: &SystemEntry, depth: usize, out: &mut Vec<Game>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            if depth < MAX_DEPTH {
                collect_games(&path, sys, depth + 1, out);
            }
            continue;
        }
        let ext = path.extension().map(|e| e.to_string_lossy()).unwrap_or_default();
        if sys.extensions.iter().any(|x| x.eq_ignore_ascii_case(&ext)) {
            let stem = path.file_stem().map(|s| s.to_string_lossy()).unwrap_or_default();
            out.push(Game { title: clean_title(&stem), path });
        }
    }
}

/// Drops No-Intro style tags: "Game (USA) [!]" -> "Game".
fn clean_title(stem: &str) -> String {
    let mut out = String::with_capacity(stem.len());
    let mut depth = 0u32;
    for ch in stem.chars() {
        match ch {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    let cleaned = out.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.is_empty() {
        stem.to_string()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn systems_needing_bios_stay_hidden_until_it_exists() {
        let root = std::env::temp_dir().join(format!("oxmux-bios-{}", std::process::id()));
        std::fs::create_dir_all(root.join("ROMS/PCECD")).unwrap();
        std::fs::create_dir_all(root.join("BIOS")).unwrap();
        std::fs::write(root.join("ROMS/PCECD/Game.chd"), b"").unwrap();
        let config: FrontendConfig = toml::from_str(&format!(r#"
            rom_roots = ["{root}/ROMS"]
            bios_dir = "{root}/BIOS"
            [retroarch]
            bin = "retroarch"
            core_dirs = []
            [[system]]
            name = "PC Engine CD"
            dirs = ["PCECD"]
            extensions = ["chd"]
            core = "pce.so"
            requires_any = ["syscard3.pce"]
        "#, root = root.display())).unwrap();
        assert!(Library::scan(&config).systems().is_empty());
        std::fs::write(root.join("BIOS/syscard3.pce"), b"").unwrap();
        assert_eq!(Library::scan(&config).systems().len(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn path_index_resolves_unique_paths_and_rejects_ambiguous_assignments() {
        let system = |name: &str, paths: &[&str]| System { name: name.into(), launch: Launch::Core("test.so".into()), games: paths.iter().map(|path| Game { title: (*path).into(), path: (*path).into() }).collect() };
        let library = Library::from_systems(vec![system("A", &["/unique", "/shared"]), system("B", &["/shared"])]);
        assert_eq!(library.resolve_path(Path::new("/unique")).unwrap().0.name, "A");
        assert!(library.resolve_path(Path::new("/shared")).is_none());
        assert!(library.resolve_path(Path::new("/missing")).is_none());
    }

    #[test]
    fn strips_tags() {
        assert_eq!(clean_title("Tiny Dungeon (Europe) (Rev 1)"), "Tiny Dungeon");
        assert_eq!(clean_title("Racer (USA) [!]"), "Racer");
        assert_eq!(clean_title("(Unnamed)"), "(Unnamed)");
        assert_eq!(clean_title("Plain"), "Plain");
    }
}
