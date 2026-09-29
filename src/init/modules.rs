//! Kernel module loading (what `modprobe` does): resolve dependencies from
//! /lib/modules/<release>/modules.dep and load each with finit_module.

use std::collections::HashMap;
use std::fs::File;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};

use crate::sys;

/// Module names treat '-' and '_' as the same character.
fn normalize(name: &str) -> String {
    name.replace('-', "_")
}

fn module_name(path: &str) -> String {
    let file = path.rsplit('/').next().unwrap_or(path);
    normalize(file.split(".ko").next().unwrap_or(file))
}

struct DepDb {
    dir: PathBuf,
    /// name -> (relative path, dependency paths in load-last-first order)
    entries: HashMap<String, (String, Vec<String>)>,
}

impl DepDb {
    fn load() -> Result<Self> {
        let dir = PathBuf::from("/lib/modules").join(sys::kernel_release());
        let text = std::fs::read_to_string(dir.join("modules.dep"))
            .with_context(|| format!("reading {}/modules.dep", dir.display()))?;
        Ok(Self { entries: parse_deps(&text), dir })
    }
}

fn parse_deps(text: &str) -> HashMap<String, (String, Vec<String>)> {
    text.lines()
        .filter_map(|line| {
            let (module, deps) = line.split_once(':')?;
            let deps = deps.split_whitespace().map(str::to_string).collect();
            Some((module_name(module), (module.to_string(), deps)))
        })
        .collect()
}

/// Loads `name` and its dependencies. Already-loaded modules (and built-ins) are fine.
pub fn load(name: &str, params: &str) -> Result<()> {
    let name = normalize(name);
    if is_loaded_or_builtin(&name) {
        return Ok(());
    }
    let db = DepDb::load()?;
    let Some((path, deps)) = db.entries.get(&name) else { bail!("module {name} not found in modules.dep") };
    // modules.dep lists dependencies so that the last one must be loaded first.
    for dep in deps.iter().rev() {
        if !is_loaded_or_builtin(&module_name(dep)) {
            insmod(&db.dir.join(dep), "")?;
        }
    }
    insmod(&db.dir.join(path), params)
}

fn insmod(path: &std::path::Path, params: &str) -> Result<()> {
    let s = path.to_string_lossy();
    if s.ends_with(".xz") || s.ends_with(".zst") || s.ends_with(".gz") {
        bail!("compressed module {} not supported", path.display());
    }
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    sys::finit_module(&file, params).with_context(|| format!("loading {}", path.display()))?;
    Ok(())
}

fn is_loaded_or_builtin(name: &str) -> bool {
    PathBuf::from("/sys/module").join(name).join("initstate").exists()
        || std::fs::read_to_string(PathBuf::from("/lib/modules").join(sys::kernel_release()).join("modules.builtin"))
            .map(|b| b.lines().any(|l| module_name(l) == name))
            .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modules_dep() {
        let db = parse_deps("kernel/net/wireless/8821cs.ko: kernel/net/cfg80211.ko kernel/lib/rfkill.ko\nkernel/fs/exfat/exfat.ko:\n");
        let (path, deps) = &db["8821cs"];
        assert_eq!(path, "kernel/net/wireless/8821cs.ko");
        assert_eq!(deps.len(), 2);
        assert!(db.contains_key("exfat"));
        assert_eq!(module_name("kernel/drivers/foo-bar.ko"), "foo_bar");
    }
}
