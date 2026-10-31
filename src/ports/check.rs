//! `ghazal ports check`: what a port needs from the system, one line each.
//! The device-side counterpart of the checks nix/runtime/ports.nix makes at build time.

use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};

use super::controller::Pad;
use super::run::{bind, display_size, BASH, LOADER};
use super::{portmaster_state, Config, PortMaster};
use crate::device::DeviceConfig;

/// Libraries `nix/runtime/ports.nix` links into `[ports] libs`; keep the lists equal.
const SONAMES: [&str; 21] = [
    "libSDL2-2.0.so.0", "libSDL2_image-2.0.so.0", "libSDL2_mixer-2.0.so.0", "libSDL2_ttf-2.0.so.0",
    "libEGL.so.1", "libGLESv2.so.2", "libgbm.so.1", "libdrm.so.2", "libasound.so.2", "libudev.so.1",
    "libz.so.1", "libpng16.so.16", "libstdc++.so.6", "libgcc_s.so.1",
    "libfreetype.so.6", "libopenal.so.1", "libmpg123.so.0", "libogg.so.0", "libvorbis.so.0", "libvorbisfile.so.3", "libtheoradec.so.1",
];
/// PortMaster's launcher points pysdl2 at /usr/lib for these (nix/runtime/default.nix).
const PYSDL2_LIBS: [&str; 4] = ["libSDL2-2.0.so.0", "libSDL2_image-2.0.so.0", "libSDL2_mixer-2.0.so.0", "libSDL2_ttf-2.0.so.0"];
const TOOLS: [&str; 7] = ["python3", "unzip", "curl", "mount", "umount", "pkill", "unsquashfs"];

type Finding = Result<String>;

fn present(path: &Path) -> Finding {
    // `exists` follows symlinks: a dangling link is a missing file.
    if path.exists() { Ok(path.display().to_string()) } else { bail!("{} is missing", path.display()) }
}

pub fn check(ports: &Config, device: &DeviceConfig, ports_dir: &Path) -> Result<()> {
    let mut findings: Vec<(String, Finding)> = vec![
        ("loader".into(), present(Path::new(LOADER))),
        ("bash".into(), present(Path::new(BASH))),
        ("env".into(), present(Path::new("/usr/bin/env"))),
    ];
    findings.extend(TOOLS.iter().map(|tool| (format!("tool {tool}"), present(&ports.bin.join(tool)))));
    findings.extend([
        ("libraries".into(), libraries(&ports.libs)),
        ("python".into(), python(&ports.bin)),
        ("pysdl2".into(), libraries_in(Path::new("/usr/lib"), &PYSDL2_LIBS)),
        ("uinput".into(), present(Path::new("/dev/uinput"))),
        ("squashfs".into(), squashfs()),
        ("loop".into(), present(Path::new("/dev/loop-control"))),
        ("controller".into(), controller(device)),
        ("display".into(), display_size().map(|(width, height)| format!("{width}x{height}")).context("no connected display mode in /sys/class/drm")),
        ("portmaster".into(), portmaster(ports, ports_dir)),
        // Last: it moves this process into its own mount namespace.
        ("namespace".into(), namespace(ports, ports_dir)),
    ]);
    let failed = findings.iter().filter(|(_, finding)| finding.is_err()).count();
    for (name, finding) in &findings {
        match finding {
            Ok(detail) => println!("ok    {name}: {detail}"),
            Err(error) => println!("FAIL  {name}: {error:#}"),
        }
    }
    if failed > 0 { bail!("{failed} of {} checks failed", findings.len()); }
    Ok(())
}

fn libraries(libs: &Path) -> Finding {
    libraries_in(libs, &SONAMES)
}

fn libraries_in(dir: &Path, sonames: &[&str]) -> Finding {
    let missing: Vec<&str> = sonames.iter().copied().filter(|soname| !dir.join(soname).exists()).collect();
    if missing.is_empty() { Ok(format!("{} sonames in {}", sonames.len(), dir.display())) } else { bail!("missing from {}: {}", dir.display(), missing.join(" ")) }
}

/// PortMaster's GUI needs these standard modules (ctypes for pysdl2, ssl for downloads).
fn python(bin: &Path) -> Finding {
    let output = Command::new(bin.join("python3")).args(["-c", "import ctypes, ssl, zlib, lzma, bz2, json, zipfile, tarfile; print(ctypes.__name__)"]).output().context("running python3")?;
    if output.status.success() { Ok("ctypes ssl zlib lzma bz2 import".into()) } else { bail!("{}", String::from_utf8_lossy(&output.stderr).lines().last().unwrap_or("python3 failed")) }
}

fn squashfs() -> Finding {
    let filesystems = std::fs::read_to_string("/proc/filesystems").context("reading /proc/filesystems")?;
    if filesystems.lines().any(|line| line.trim_end().ends_with("squashfs")) { Ok("kernel supports squashfs (runtimes)".into()) } else { bail!("the kernel has no squashfs") }
}

fn controller(device: &DeviceConfig) -> Finding {
    let name = device.input.gamepad.as_deref().context("device.toml sets no [input] gamepad")?;
    let pad = Pad::find(Path::new("/sys/class/input"), name)?;
    Ok(format!("{} {}", pad.name, pad.guid()))
}

fn portmaster(ports: &Config, ports_dir: &Path) -> Finding {
    match portmaster_state(ports_dir) {
        PortMaster::Missing => Ok(format!("not installed in {} (Applications → Install PortMaster)", ports_dir.display())),
        PortMaster::Installed => {
            let ours = std::fs::read(ports.files.join("control.txt")).context("reading Ghazal's control.txt")?;
            let installed = std::fs::read(ports_dir.join("PortMaster/control.txt")).context("reading PortMaster's control.txt")?;
            // The next run replaces a stale one, so this is information, not a failure.
            Ok(format!("installed; control.txt {}", if ours == installed { "is Ghazal's" } else { "is stock and will be replaced on the next run" }))
        }
    }
}

fn namespace(ports: &Config, ports_dir: &Path) -> Finding {
    // SAFETY: plain syscall; no pointers. This process is single-threaded.
    if unsafe { libc::unshare(libc::CLONE_NEWNS) } < 0 { bail!("unshare(CLONE_NEWNS): {}", std::io::Error::last_os_error()); }
    crate::sys::mount("none", Path::new("/"), "none", libc::MS_REC | libc::MS_PRIVATE, "")?;
    std::fs::create_dir_all(ports_dir)?;
    bind(ports_dir, &ports.mount_point).with_context(|| format!("{} must exist in the image", ports.mount_point.display()))?;
    Ok(format!("{} is bound at {}", ports_dir.display(), ports.mount_point.display()))
}
