use std::path::{Path, PathBuf};

use super::config::{FrontendConfig, SystemEntry};

const MAX_DEPTH: usize = 3;

pub struct Library {
    /// Only systems that have at least one game, in config order.
    pub systems: Vec<System>,
}

pub struct System {
    pub name: String,
    pub core: String,
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
                    core: sys.core.clone(),
                    games,
                })
            })
            .collect();
        Self { systems }
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
    use super::clean_title;

    #[test]
    fn strips_tags() {
        assert_eq!(clean_title("Tiny Dungeon (Europe) (Rev 1)"), "Tiny Dungeon");
        assert_eq!(clean_title("Racer (USA) [!]"), "Racer");
        assert_eq!(clean_title("(Unnamed)"), "(Unnamed)");
        assert_eq!(clean_title("Plain"), "Plain");
    }
}
