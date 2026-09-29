//! `oxmux install <frontend|init>`, `oxmux uninstall`, `oxmux status`, `oxmux check`.
//! Run on the device (as root) to move a muOS install through the stages:
//!
//!   frontend  stage 2: muOS boots as usual, but S99muos.sh starts `oxmux supervise`
//!             (daemon + frontend) instead of muOS's frontend, hotkey and power scripts.
//!   init      stage 3: busybox init's sysinit runs `oxmux init`; no muOS script runs.
//!
//! Originals are kept next to what they replace as `*.oxmux-orig` and restored by
//! `oxmux uninstall`. Stage 4 (a whole SD image) is built on a PC: scripts/build-image.sh.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::profile::Profile;
use crate::system::SystemConfig;

pub const PREFIX: &str = "/opt/oxmux";
const ORIG: &str = "oxmux-orig";
const MARKER: &str = "# oxmux shim";
const S99: &str = "/opt/muos/script/init/S99muos.sh";

const SYSTEM_FRONTEND: &str = include_str!("../config/rg40xxv/system-muos-frontend.toml");
const SYSTEM_STANDALONE: &str = include_str!("../config/rg40xxv/system-standalone.toml");

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Stage {
    Frontend,
    Init,
}

/// Maps an on-device path to where it is on disk. `OXMUX_SYSROOT` points the installer at
/// a mounted or extracted rootfs instead of `/` (used by tests).
fn host(path: impl AsRef<Path>) -> PathBuf {
    let root = std::env::var_os("OXMUX_SYSROOT").map(PathBuf::from).unwrap_or_else(|| "/".into());
    root.join(path.as_ref().strip_prefix("/").unwrap_or(path.as_ref()))
}

fn etc() -> PathBuf {
    host(PREFIX).join("etc")
}

fn orig(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.{ORIG}", path.display()))
}

fn is_shim(path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|s| s.contains(MARKER))
}

pub fn install(stage: Stage, dry_run: bool) -> Result<()> {
    let sysinit = if stage == Stage::Init { Some(find_sysinit()?) } else { None };
    let system_text = match (stage, &sysinit) {
        (Stage::Frontend, _) => SYSTEM_FRONTEND.to_string(),
        (Stage::Init, Some(path)) => SYSTEM_STANDALONE.replace("@RESCUE_EXEC@", &orig(path).display().to_string()),
        (Stage::Init, None) => unreachable!(),
    };
    let system: SystemConfig = toml::from_str(&system_text).context("parsing the stage's system.toml")?;
    let problems = preflight(&system);
    for p in &problems {
        println!("{p}");
    }
    if problems.iter().any(|p| p.starts_with("error")) {
        bail!("preflight failed; nothing was changed");
    }
    if dry_run {
        println!("preflight ok (dry run, nothing changed)");
        return Ok(());
    }

    install_files(&system_text)?;
    match stage {
        Stage::Frontend => {
            shim(&host(S99), &s99_shim())?;
        }
        Stage::Init => {
            // In stage 3 muOS's own boot only runs as the rescue path; it must be pure muOS.
            restore(&host(S99))?;
            let sysinit = sysinit.expect("found above");
            shim(&host(&sysinit), &format!("#!/bin/sh\n{MARKER} (stage 3). Original: {}\nexec {PREFIX}/oxmux init\n", orig(&sysinit).display()))?;
        }
    }
    sync_fs();
    println!("installed stage {stage:?}. Reboot to use it; `{PREFIX}/oxmux uninstall` reverts.");
    Ok(())
}

pub fn uninstall() -> Result<()> {
    let mut restored = 0;
    if restore(&host(S99))? {
        restored += 1;
    }
    if let Ok(sysinit) = find_sysinit() {
        if restore(&host(&sysinit))? {
            restored += 1;
        }
    }
    sync_fs();
    println!("restored {restored} muOS file(s); {PREFIX} was left in place (delete it to remove oxmux entirely)");
    Ok(())
}

pub fn status() -> Result<()> {
    let s99 = if is_shim(&host(S99)) { "oxmux (stage 2)" } else { "muOS" };
    println!("S99muos.sh: {s99}");
    match find_sysinit() {
        Ok(p) => println!("sysinit {}: {}", p.display(), if is_shim(&host(&p)) { "oxmux (stage 3)" } else { "muOS" }),
        Err(e) => println!("sysinit: {e:#}"),
    }
    println!("init (PID 1): {}", std::fs::read_link("/proc/1/exe").map(|p| p.display().to_string()).unwrap_or("?".into()));
    let counter = host(PREFIX).join("state/boot-attempts");
    if let Ok(n) = std::fs::read_to_string(&counter) {
        println!("failed boot attempts: {}", n.trim());
    }
    Ok(())
}

/// `oxmux check`: validates the installed configs without changing anything.
pub fn check(profile: &Profile) -> Result<()> {
    profile.device().context("device.toml")?;
    profile.frontend().context("frontend.toml")?;
    let system = profile.system().context("system.toml")?;
    let problems = preflight(&system);
    for p in &problems {
        println!("{p}");
    }
    if problems.iter().any(|p| p.starts_with("error")) {
        bail!("check failed");
    }
    println!("configs ok");
    Ok(())
}

/// Things that would break the next boot. "error: ..." blocks the install.
fn preflight(system: &SystemConfig) -> Vec<String> {
    let mut out = Vec::new();
    for svc in &system.supervisor.services {
        let prog = &svc.exec[0];
        // Our own binary is copied into place by the install itself.
        if prog.starts_with(PREFIX) {
            continue;
        }
        if resolve(prog).is_none() {
            let level = if svc.optional { "warning" } else { "error" };
            out.push(format!("{level}: service {:?}: {prog:?} not found", svc.name));
        }
    }
    if let Some(r) = &system.init.rescue {
        if let Some(p) = r.exec.first().filter(|p| !p.starts_with('@')) {
            // The rescue target is created by the install (the backup), so only warn.
            if !Path::new(p).exists() && !p.ends_with(ORIG) {
                out.push(format!("warning: rescue {p:?} not found"));
            }
        }
    }
    out
}

/// Finds `prog` the way the supervisor will: absolute path, or on PATH.
fn resolve(prog: &str) -> Option<PathBuf> {
    let is_exec = |p: &Path| p.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0);
    if prog.contains('/') {
        return Some(host(prog)).filter(|p| is_exec(p));
    }
    let path = std::env::var("PATH").unwrap_or_default();
    let found = path.split(':').chain(["/usr/sbin", "/usr/bin", "/sbin", "/bin"]).map(|d| host(d).join(prog)).find(|p| is_exec(p));
    found
}

/// The script busybox init runs as `::sysinit:` (from /etc/inittab).
fn find_sysinit() -> Result<PathBuf> {
    let inittab = std::fs::read_to_string(host("/etc/inittab")).context("reading /etc/inittab")?;
    for line in inittab.lines().map(str::trim) {
        if line.starts_with('#') {
            continue;
        }
        // id:runlevels:action:process
        let mut fields = line.splitn(4, ':');
        let (_, _, action, process) = (fields.next(), fields.next(), fields.next(), fields.next());
        if action == Some("sysinit") {
            let prog = process.unwrap_or("").split_whitespace().next().unwrap_or("");
            if prog.starts_with('/') {
                return Ok(PathBuf::from(prog));
            }
        }
    }
    bail!("no ::sysinit: script in /etc/inittab; stage 3 can't be installed on this rootfs (use the stage 4 image instead)")
}

fn install_files(system_text: &str) -> Result<()> {
    let etc = etc();
    std::fs::create_dir_all(&etc)?;
    std::fs::create_dir_all(host(PREFIX).join("state"))?;

    // Copy ourselves into place (rename makes replacing a running binary safe).
    let me = std::env::current_exe()?;
    let target = host(PREFIX).join("oxmux");
    if me != target {
        let tmp = target.with_extension("new");
        std::fs::copy(&me, &tmp).with_context(|| format!("copying to {}", tmp.display()))?;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
        std::fs::rename(&tmp, &target)?;
    }

    // device.toml and frontend.toml hold user edits: only write them if missing.
    for (name, text) in [("device.toml", crate::profile::DEFAULT_DEVICE), ("frontend.toml", crate::profile::DEFAULT_FRONTEND)] {
        let path = etc.join(name);
        if !path.exists() {
            std::fs::write(&path, text)?;
        }
    }
    // system.toml defines the stage; keep the previous one if it differs.
    let path = etc.join("system.toml");
    if std::fs::read_to_string(&path).is_ok_and(|old| old != system_text) {
        std::fs::rename(&path, etc.join("system.toml.bak"))?;
    }
    std::fs::write(&path, system_text)?;
    Ok(())
}

/// Replaces `path` with a shim, keeping the original as `<path>.oxmux-orig`.
fn shim(path: &Path, contents: &str) -> Result<()> {
    if !path.exists() {
        bail!("{} not found", path.display());
    }
    let backup = orig(path);
    if !is_shim(path) {
        std::fs::rename(path, &backup).with_context(|| format!("backing up {}", path.display()))?;
    } else if !backup.exists() {
        bail!("{} is already a shim but its backup is missing", path.display());
    }
    let tmp = path.with_extension("oxmux-new");
    std::fs::write(&tmp, contents)?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    std::fs::rename(&tmp, path)?;
    println!("replaced {} (original kept as {})", path.display(), backup.display());
    Ok(())
}

/// Puts the original back if `path` is a shim. Returns whether anything changed.
fn restore(path: &Path) -> Result<bool> {
    let backup = orig(path);
    if !backup.exists() {
        return Ok(false);
    }
    std::fs::rename(&backup, path).with_context(|| format!("restoring {}", path.display()))?;
    println!("restored {}", path.display());
    Ok(true)
}

fn s99_shim() -> String {
    format!(
        r#"#!/bin/sh
{MARKER} (stage 2). muOS's original is S99muos.sh.{ORIG}; `{PREFIX}/oxmux uninstall` restores it.
case "$1" in
    stop) kill "$(cat /run/oxmux/supervise.pid 2>/dev/null)" 2>/dev/null ;;
    *) OXMUX_CONFIG_DIR={PREFIX}/etc setsid {PREFIX}/oxmux supervise --pidfile /run/oxmux/supervise.pid \
           </dev/null >>/run/oxmux-supervise.log 2>&1 & ;;
esac
"#
    )
}

fn sync_fs() {
    crate::sys::sync();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_configs_parse() {
        toml::from_str::<SystemConfig>(SYSTEM_FRONTEND).unwrap();
        let s: SystemConfig = toml::from_str(&SYSTEM_STANDALONE.replace("@RESCUE_EXEC@", "/x.orig")).unwrap();
        assert_eq!(s.init.rescue.unwrap().exec, vec!["/x.orig".to_string()]);
        assert!(s.init.steps.len() > 20);
        assert!(s99_shim().contains("supervise"));
    }
}
