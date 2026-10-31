//! Looks paths up in the runtime tree the way the device's root filesystem would.
//!
//! The runtime tree (`nix/runtime`, `tree`) holds `/usr` and `/etc` entries that
//! are symlinks to absolute store paths. On the device those paths exist in the
//! root filesystem; here they exist on this machine, so an absolute link is
//! followed inside the tree first and on the host otherwise.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

const MAX_LINKS: usize = 40;

pub struct Tree {
    root: PathBuf,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Resolved {
    Found(PathBuf),
    Missing(PathBuf),
    Loop,
}

fn parts(path: &Path) -> VecDeque<OsString> {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_owned()),
            Component::ParentDir => Some("..".into()),
            _ => None,
        })
        .collect()
}

impl Tree {
    pub fn new(root: &Path) -> Self {
        Self { root: root.to_owned() }
    }

    /// Follows `path` (absolute on the device) through every symlink.
    pub fn resolve(&self, path: &Path) -> Resolved {
        let mut remaining = parts(path);
        let mut current = self.root.clone();
        let mut links = 0;
        while let Some(part) = remaining.pop_front() {
            if part == ".." {
                // Never climb out of the tree's own root; the host root is a real root.
                if current != self.root {
                    current.pop();
                }
                continue;
            }
            let next = current.join(&part);
            match fs::symlink_metadata(&next) {
                Err(_) => return Resolved::Missing(next),
                Ok(meta) if meta.file_type().is_symlink() => {
                    links += 1;
                    if links > MAX_LINKS {
                        return Resolved::Loop;
                    }
                    let Ok(target) = fs::read_link(&next) else { return Resolved::Missing(next) };
                    if target.is_absolute() {
                        let inside = self.root.join(target.strip_prefix("/").unwrap_or(&target));
                        current = if fs::symlink_metadata(inside).is_ok() { self.root.clone() } else { PathBuf::from("/") };
                    }
                    let mut next_parts = parts(&target);
                    next_parts.append(&mut remaining);
                    remaining = next_parts;
                }
                Ok(_) => current = next,
            }
        }
        Resolved::Found(current)
    }

    /// The regular file or directory `path` leads to, or why it does not exist.
    pub fn find(&self, path: &Path) -> Result<PathBuf, String> {
        match self.resolve(path) {
            Resolved::Found(found) => Ok(found),
            Resolved::Missing(at) => Err(format!("{} is missing (stopped at {})", path.display(), at.display())),
            Resolved::Loop => Err(format!("{} loops through symlinks", path.display())),
        }
    }

    pub fn is_executable(&self, path: &Path) -> Result<PathBuf, String> {
        let file = self.find(path)?;
        let meta = fs::metadata(&file).map_err(|error| format!("{}: {error}", path.display()))?;
        if !meta.is_file() {
            return Err(format!("{} is not a file", path.display()));
        }
        if meta.permissions().mode() & 0o111 == 0 {
            return Err(format!("{} is not executable", path.display()));
        }
        Ok(file)
    }
}

/// Where a path on the device would sit inside `root`.
pub fn under(root: &Path, device_path: &Path) -> PathBuf {
    root.join(device_path.strip_prefix("/").unwrap_or(device_path))
}

#[derive(Debug, PartialEq, Eq)]
pub enum ElfKind {
    Executable,
    Shared,
    Other,
}

const EM_AARCH64: u16 = 183;

/// The header facts the gates need, from the first bytes of a file.
#[derive(Debug, PartialEq, Eq)]
pub struct Elf {
    pub kind: ElfKind,
    pub aarch64: bool,
}

pub fn parse_elf(header: &[u8]) -> Option<Elf> {
    // e_ident: magic, class (2 = 64-bit), data (1 = little endian); then e_type and e_machine.
    if header.len() < 20 || header[..4] != *b"\x7fELF" || header[4] != 2 || header[5] != 1 {
        return None;
    }
    let kind = match u16::from_le_bytes([header[16], header[17]]) {
        2 => ElfKind::Executable,
        3 => ElfKind::Shared,
        _ => ElfKind::Other,
    };
    Some(Elf { kind, aarch64: u16::from_le_bytes([header[18], header[19]]) == EM_AARCH64 })
}

pub fn read_elf(path: &Path) -> Result<Elf, String> {
    let mut header = [0u8; 20];
    let mut file = fs::File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    file.read_exact(&mut header).map_err(|_| format!("{} is too short to be an ELF file", path.display()))?;
    parse_elf(&header).ok_or_else(|| format!("{} is not a 64-bit little-endian ELF file", path.display()))
}

#[cfg(test)]
pub mod testing {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A directory removed on drop, unique per test.
    pub struct TempDir(pub PathBuf);

    impl TempDir {
        pub fn new(label: &str) -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let path = std::env::temp_dir().join(format!("ghazal-release-check-{label}-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        pub fn path(&self) -> &Path {
            &self.0
        }

        pub fn write(&self, relative: &str, contents: &[u8]) -> PathBuf {
            let path = self.0.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, contents).unwrap();
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A minimal 64-bit little-endian ELF header with the given type and machine.
    pub fn elf(kind: u16, machine: u16) -> Vec<u8> {
        let mut header = vec![0u8; 64];
        header[..4].copy_from_slice(b"\x7fELF");
        header[4] = 2;
        header[5] = 1;
        header[16..18].copy_from_slice(&kind.to_le_bytes());
        header[18..20].copy_from_slice(&machine.to_le_bytes());
        header
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::testing::{elf, TempDir};
    use super::*;

    #[test]
    fn follows_absolute_links_into_the_tree_before_the_host() {
        let dir = TempDir::new("tree");
        let store = dir.write("store/pkg/bin/tool", b"x");
        let tree = dir.path().join("tree");
        fs::create_dir_all(tree.join("usr/bin")).unwrap();
        // A link to a path that only exists on the host (a store path).
        symlink(&store, tree.join("usr/bin/tool")).unwrap();
        assert_eq!(Tree::new(&tree).resolve(Path::new("/usr/bin/tool")), Resolved::Found(store.clone()));
        // The same absolute link, but its target also exists inside the tree: the tree wins.
        let inside = dir.write(&format!("tree/{}", store.strip_prefix("/").unwrap().display()), b"y");
        assert_eq!(Tree::new(&tree).resolve(Path::new("/usr/bin/tool")), Resolved::Found(inside));
    }

    #[test]
    fn reports_where_a_path_breaks() {
        let dir = TempDir::new("missing");
        fs::create_dir_all(dir.path().join("usr/bin")).unwrap();
        symlink("/nowhere/gone", dir.path().join("usr/bin/dangling")).unwrap();
        let tree = Tree::new(dir.path());
        assert!(matches!(tree.resolve(Path::new("/usr/bin/dangling")), Resolved::Missing(_)));
        assert!(matches!(tree.resolve(Path::new("/usr/bin/absent")), Resolved::Missing(_)));
        symlink("again", dir.path().join("usr/bin/again")).unwrap();
        assert_eq!(tree.resolve(Path::new("/usr/bin/again")), Resolved::Loop);
    }

    #[test]
    fn relative_links_and_parent_components_resolve() {
        let dir = TempDir::new("relative");
        dir.write("usr/lib/real.so", b"x");
        fs::create_dir_all(dir.path().join("usr/bin")).unwrap();
        symlink("../lib/real.so", dir.path().join("usr/bin/link")).unwrap();
        let tree = Tree::new(dir.path());
        assert_eq!(tree.resolve(Path::new("/usr/bin/link")), Resolved::Found(dir.path().join("usr/lib/real.so")));
        // ".." in a device path cannot leave the tree.
        assert_eq!(tree.resolve(Path::new("/../usr/lib/real.so")), Resolved::Found(dir.path().join("usr/lib/real.so")));
    }

    #[test]
    fn executables_need_the_execute_bit() {
        let dir = TempDir::new("exec");
        let file = dir.write("usr/bin/tool", b"#!/bin/sh\n");
        let tree = Tree::new(dir.path());
        assert!(tree.is_executable(Path::new("/usr/bin/tool")).unwrap_err().contains("not executable"));
        fs::set_permissions(file, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(tree.is_executable(Path::new("/usr/bin/tool")).is_ok());
        assert!(tree.is_executable(Path::new("/usr/bin")).unwrap_err().contains("not a file"));
    }

    #[test]
    fn elf_headers_are_classified() {
        assert_eq!(parse_elf(&elf(3, 183)), Some(Elf { kind: ElfKind::Shared, aarch64: true }));
        assert_eq!(parse_elf(&elf(2, 62)), Some(Elf { kind: ElfKind::Executable, aarch64: false }));
        assert_eq!(parse_elf(b"#!/bin/sh\n and some more text"), None);
        assert_eq!(parse_elf(&elf(1, 183)).unwrap().kind, ElfKind::Other);
        assert_eq!(parse_elf(b"\x7fELF"), None);
    }
}
