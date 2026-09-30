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

/// Loads a driver for every device the kernel has found (what `udevadm trigger` +
/// udev's kmod builtin do): each /sys/devices/**/modalias is matched against
/// modules.alias. Repeats while new modules appear, since loading a bus driver can
/// reveal more devices. Returns the modules loaded.
pub fn coldplug(exclude: &[String]) -> Result<Vec<String>> {
    let dir = PathBuf::from("/lib/modules").join(sys::kernel_release());
    let text = std::fs::read_to_string(dir.join("modules.alias")).with_context(|| format!("reading {}/modules.alias", dir.display()))?;
    let aliases = parse_aliases(&text);
    let exclude: Vec<String> = exclude.iter().map(|e| normalize(e)).collect();
    let mut loaded: Vec<String> = Vec::new();
    for _pass in 0..4 {
        let mut modaliases = Vec::new();
        collect_modaliases(std::path::Path::new("/sys/devices"), 0, &mut modaliases);
        modaliases.sort();
        modaliases.dedup();
        let mut new = 0;
        for alias in &modaliases {
            for (pattern, module) in &aliases {
                if loaded.contains(module) || exclude.contains(module) || !glob_match(pattern.as_bytes(), alias.as_bytes()) {
                    continue;
                }
                loaded.push(module.clone());
                match load(module, "") {
                    Ok(()) => new += 1,
                    Err(e) => eprintln!("oxmux-init: coldplug {module}: {e:#}"),
                }
            }
        }
        if new == 0 {
            break;
        }
    }
    Ok(loaded)
}

fn parse_aliases(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            (it.next() == Some("alias")).then_some(())?;
            Some((it.next()?.to_string(), normalize(it.next()?)))
        })
        .collect()
}

fn collect_modaliases(dir: &std::path::Path, depth: usize, out: &mut Vec<String>) {
    if depth > 16 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else { continue };
        // Symlinks in sysfs point back into the tree; following them would loop.
        if kind.is_dir() && !kind.is_symlink() {
            collect_modaliases(&entry.path(), depth + 1, out);
        } else if entry.file_name() == "modalias" {
            if let Ok(s) = std::fs::read_to_string(entry.path()) {
                let s = s.trim();
                if !s.is_empty() {
                    out.push(s.to_string());
                }
            }
        }
    }
}

/// fnmatch-style matching for modules.alias patterns: `*`, `?` and `[...]` sets.
fn glob_match(pat: &[u8], s: &[u8]) -> bool {
    let (mut p, mut i) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while i < s.len() {
        if p < pat.len() {
            match pat[p] {
                b'*' => {
                    star = Some((p, i));
                    p += 1;
                    continue;
                }
                b'?' => {
                    p += 1;
                    i += 1;
                    continue;
                }
                b'[' => {
                    if let Some((matched, len)) = match_set(&pat[p..], s[i]) {
                        if matched {
                            p += len;
                            i += 1;
                            continue;
                        }
                    }
                }
                c if c == s[i] => {
                    p += 1;
                    i += 1;
                    continue;
                }
                _ => {}
            }
        }
        match star {
            Some((sp, si)) => {
                p = sp + 1;
                i = si + 1;
                star = Some((sp, si + 1));
            }
            None => return false,
        }
    }
    pat[p..].iter().all(|&c| c == b'*')
}

/// Matches `c` against a `[...]` set at the start of `pat`. Returns (matched, set length).
fn match_set(pat: &[u8], c: u8) -> Option<(bool, usize)> {
    let end = pat.iter().skip(1).position(|&b| b == b']')? + 1;
    let body = &pat[1..end];
    let (negate, body) = match body.first() {
        Some(b'!') | Some(b'^') => (true, &body[1..]),
        _ => (false, body),
    };
    let mut matched = false;
    let mut k = 0;
    while k < body.len() {
        if k + 2 < body.len() && body[k + 1] == b'-' {
            matched |= (body[k]..=body[k + 2]).contains(&c);
            k += 3;
        } else {
            matched |= body[k] == c;
            k += 1;
        }
    }
    Some((matched != negate, end + 1))
}

#[cfg(test)]
mod glob_tests {
    use super::glob_match;

    #[test]
    fn globs() {
        let m = |p: &str, s: &str| glob_match(p.as_bytes(), s.as_bytes());
        assert!(m("virtio:d00000010v*", "virtio:d00000010v00001AF4"));
        assert!(!m("virtio:d00000010v*", "virtio:d00000012v00001AF4"));
        assert!(m("pci:v00001AF4d*sv*sd*bc03sc*i*", "pci:v00001AF4d00001050sv00001AF4sd00001100bc03sc00i00"));
        assert!(m("of:N*T*Callwinner,sun50i-h616-mmc", "of:NmmcT(null)Callwinner,sun50i-h616-mmc"));
        assert!(m("usb:v*p*d*dc*dsc*dp*ic03isc*ip*in*", "usb:v1234p5678d0100dc00dsc00dp00ic03isc01ip01in00"));
        assert!(m("a[0-9]c", "a5c") && !m("a[!0-9]c", "a5c") && m("a?c", "abc"));
    }
}
