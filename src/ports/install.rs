//! Installing PortMaster: download its release zip and unpack it in the ports folder.
//!
//! Done on the handheld, over its own network, from PortMaster's GitHub release;
//! nothing of PortMaster ships in the image.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{channel, Receiver};

use anyhow::{bail, Context, Result};

use super::{Config, LAUNCHER};

pub enum Progress {
    Step(&'static str),
    Done,
    Failed(String),
}

/// Runs `install` in a worker so the menu keeps drawing.
pub fn spawn(ports: Config, ports_dir: PathBuf) -> Result<Receiver<Progress>> {
    let (sender, receiver) = channel();
    std::thread::Builder::new().name("ghazal-portmaster".into()).spawn(move || {
        let result = install(&ports, &ports_dir, &mut |step| { let _ = sender.send(Progress::Step(step)); });
        let _ = sender.send(match result {
            Ok(()) => Progress::Done,
            Err(error) => Progress::Failed(format!("{error:#}")),
        });
    })?;
    Ok(receiver)
}

pub fn install(ports: &Config, ports_dir: &Path, step: &mut dyn FnMut(&'static str)) -> Result<()> {
    let target = ports_dir.join("PortMaster");
    if ports_dir.join(LAUNCHER).is_file() { bail!("PortMaster is already installed"); }
    std::fs::create_dir_all(ports_dir).with_context(|| format!("creating {}", ports_dir.display()))?;
    // Hidden names: the library scan skips them, so an interrupted install shows nothing.
    let archive = ports_dir.join(".PortMaster.zip.part");
    let copy = ports_dir.join(".PortMaster.new");
    // Unpacked in RAM: unzip sets each file's mode, and an exFAT data card refuses that.
    static ATTEMPTS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let attempt = ATTEMPTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let staging = std::env::temp_dir().join(format!("ghazal-portmaster-{}-{attempt}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    let _ = std::fs::remove_dir_all(&copy);

    step("Downloading PortMaster…");
    run(Command::new(ports.bin.join("curl"))
        .args(["--fail", "--location", "--silent", "--show-error", "--retry", "3", "--connect-timeout", "15", "--max-time", "900"])
        .arg("--output").arg(&archive).arg(&ports.release_url), "download")?;

    step("Unpacking PortMaster…");
    std::fs::create_dir_all(&staging)?;
    let result = unpack(ports, &archive, &staging, &copy);
    let _ = std::fs::remove_dir_all(&staging);
    result.inspect_err(|_| { let _ = std::fs::remove_dir_all(&copy); })?;
    // An earlier attempt may have left a folder without its launcher.
    let _ = std::fs::remove_dir_all(&target);
    std::fs::rename(&copy, &target).with_context(|| format!("moving PortMaster into {}", ports_dir.display()))?;
    let _ = std::fs::remove_file(&archive);

    step("Adding Ghazal's control files…");
    super::run::refresh_portmaster(ports, &target)
}

fn unpack(ports: &Config, archive: &Path, staging: &Path, copy: &Path) -> Result<()> {
    run(Command::new(ports.bin.join("unzip")).args(["-q", "-o"]).arg(archive).arg("-d").arg(staging), "unzip")?;
    let unpacked = staging.join("PortMaster");
    if !unpacked.join("PortMaster.sh").is_file() { bail!("the download is not PortMaster.zip"); }
    copy_tree(&unpacked, copy).with_context(|| format!("copying PortMaster to {}", copy.display()))
}

/// Copies files and folders, every file executable: PortMaster's zip stores its
/// input helpers 0644 and its launcher only runs `chmod -R +x` after the Love2D
/// splash has already failed to start. Modes are applied afterwards and may be
/// refused (exFAT has none to set); the card's own defaults then apply.
fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)?.flatten() {
        let (source, target) = (entry.path(), to.join(entry.file_name()));
        let metadata = std::fs::metadata(&source)?;
        if metadata.is_dir() {
            copy_tree(&source, &target)?;
        } else {
            std::io::copy(&mut std::fs::File::open(&source)?, &mut std::fs::File::create(&target)?)?;
            let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(metadata.permissions().mode() | 0o111));
        }
    }
    Ok(())
}

fn run(command: &mut Command, what: &str) -> Result<()> {
    let output = command.output().with_context(|| format!("starting {what}"))?;
    if output.status.success() { return Ok(()); }
    let detail = String::from_utf8_lossy(&output.stderr);
    bail!("{what} failed ({}): {}", output.status, detail.lines().last().unwrap_or("no output"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn script(path: &Path, body: &str) {
        std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// A curl that "downloads" a marker and an unzip that unpacks a PortMaster folder.
    fn fixture(name: &str) -> (PathBuf, Config) {
        let root = std::env::temp_dir().join(format!("ghazal-ports-install-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (bin, files) = (root.join("bin"), root.join("files"));
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&files).unwrap();
        std::fs::write(files.join("control.txt"), b"ours").unwrap();
        script(&bin.join("curl"), "while [ $# -gt 0 ]; do [ \"$1\" = --output ] && echo zip > \"$2\"; shift; done");
        script(&bin.join("unzip"), "while [ $# -gt 0 ]; do [ \"$1\" = -d ] && d=\"$2\"; shift; done\nmkdir -p \"$d/PortMaster\" && echo '#!/bin/bash' > \"$d/PortMaster/PortMaster.sh\" && echo stock > \"$d/PortMaster/control.txt\"");
        (root.clone(), Config { bin, files, ..Config::default() })
    }

    #[test]
    fn installs_portmaster_and_replaces_its_control_file() {
        let (root, ports) = fixture("ok");
        let ports_dir = root.join("PORTS");
        let mut steps = Vec::new();
        install(&ports, &ports_dir, &mut |step| steps.push(step)).unwrap();
        assert_eq!(steps.len(), 3);
        assert!(ports_dir.join("PortMaster/PortMaster.sh").is_file());
        assert_eq!(std::fs::read(ports_dir.join("PortMaster/control.txt")).unwrap(), b"ours");
        assert!(!ports_dir.join(".PortMaster.zip.part").exists() && !ports_dir.join(".PortMaster.new").exists());
        assert!(install(&ports, &ports_dir, &mut |_| {}).unwrap_err().to_string().contains("already installed"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_failed_download_leaves_no_portmaster_folder_behind() {
        let (root, ports) = fixture("fail");
        script(&ports.bin.join("curl"), "echo 'curl: (22) The requested URL returned error: 404' >&2; exit 22");
        let ports_dir = root.join("PORTS");
        let error = install(&ports, &ports_dir, &mut |_| {}).unwrap_err().to_string();
        assert!(error.contains("download failed") && error.contains("404"), "{error}");
        assert!(!ports_dir.join("PortMaster").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn nested_files_are_copied_and_made_executable() {
        let (root, ports) = fixture("nested");
        script(&ports.bin.join("unzip"), "while [ $# -gt 0 ]; do [ \"$1\" = -d ] && d=\"$2\"; shift; done\nmkdir -p \"$d/PortMaster/libs\" && echo '#!/bin/bash' > \"$d/PortMaster/PortMaster.sh\" && echo lib > \"$d/PortMaster/libs/runtime.squashfs\" && chmod 755 \"$d/PortMaster/PortMaster.sh\" && chmod 644 \"$d/PortMaster/libs/runtime.squashfs\"");
        let ports_dir = root.join("PORTS");
        install(&ports, &ports_dir, &mut |_| {}).unwrap();
        assert_eq!(std::fs::read(ports_dir.join("PortMaster/libs/runtime.squashfs")).unwrap(), b"lib\n");
        assert_eq!(std::fs::metadata(ports_dir.join("PortMaster/libs/runtime.squashfs")).unwrap().permissions().mode() & 0o111, 0o111);
        assert_eq!(std::fs::metadata(ports_dir.join("PortMaster/PortMaster.sh")).unwrap().permissions().mode() & 0o111, 0o111);
        assert!(!ports_dir.join(".PortMaster.new").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_archive_without_portmaster_is_rejected() {
        let (root, ports) = fixture("wrong");
        script(&ports.bin.join("unzip"), "while [ $# -gt 0 ]; do [ \"$1\" = -d ] && d=\"$2\"; shift; done\nmkdir -p \"$d/Other\"");
        let ports_dir = root.join("PORTS");
        assert!(install(&ports, &ports_dir, &mut |_| {}).unwrap_err().to_string().contains("not PortMaster.zip"));
        assert!(!ports_dir.join("PortMaster").exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
