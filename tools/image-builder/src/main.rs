//! Assemble an independent rootfs from a checksum-pinned hardware bundle.
//! Never opens a block device; the output is a regular image file.
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs::{self, File};
use std::io::{Seek, SeekFrom, Write};
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};
type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Clone, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct Device {
    id: String,
    name: String,
    dtb: String,
    rotation: String,
    wifi: bool,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    devices: Vec<Device>,
}
fn catalog() -> Result<Catalog> {
    let catalog: Catalog = toml::from_str(include_str!("../../../config/h700/devices.toml"))?;
    let mut ids = BTreeSet::new();
    for device in &catalog.devices {
        if !ids.insert(&device.id)
            || device.id.is_empty()
            || !device
                .id
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            || !device.dtb.starts_with("sun50i-h700-anbernic-")
            || !device
                .dtb
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            || !matches!(
                device.rotation.as_str(),
                "normal" | "left" | "right" | "inverted"
            )
        {
            return Err("invalid or duplicate device catalog entry".into());
        }
    }
    Ok(catalog)
}

struct Options {
    args: Vec<String>,
    device: String,
    memory: String,
    public: bool,
    all: bool,
    version: String,
}
impl Options {
    fn parse() -> Result<Self> {
        let mut options = Self {
            args: Vec::new(),
            device: "rg40xx-v-v2".into(),
            memory: "ddr4".into(),
            public: false,
            all: false,
            version: "v0.1.0-beta.1".into(),
        };
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--check-mountpoints" => {
                    check_mountpoints(Path::new(&args.next().ok_or("missing root tree")?))?;
                    println!("Read-only root mountpoints are complete");
                    std::process::exit(0);
                }
                "--device" => options.device = args.next().ok_or("missing device")?,
                "--memory" => options.memory = args.next().ok_or("missing memory")?,
                "--version" => options.version = args.next().ok_or("missing version")?,
                "--public" => options.public = true,
                "--all" => {
                    options.all = true;
                    options.public = true;
                }
                "--matrix" => {
                    let matrix: Vec<_> = catalog()?.devices.into_iter().flat_map(|device| ["ddr3", "ddr4"].map(move |memory| serde_json::json!({"device": device.id, "memory": memory}))).collect();
                    println!("{}", serde_json::json!({"include":matrix}));
                    std::process::exit(0);
                }
                flag if flag.starts_with('-') => {
                    return Err(format!("unknown option {flag}").into())
                }
                _ => options.args.push(arg),
            }
        }
        if !matches!(options.memory.as_str(), "ddr3" | "ddr4") {
            return Err("memory must be ddr3 or ddr4".into());
        }
        if options.version.is_empty()
            || !options
                .version
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b".-".contains(&byte))
        {
            return Err("invalid release version".into());
        }
        if options.args.len() != if options.public { 2 } else { 3 } {
            return Err("usage: ghazal-image-builder [--device ID] [--memory ddr3|ddr4] SOURCE_DIRECTORY SECRETS_DIRECTORY OUTPUT.img; public builds: --public SOURCE_DIRECTORY OUTPUT.img; release batch: --all --memory ddr3|ddr4 --version TAG SOURCE_DIRECTORY OUTPUT_DIRECTORY".into());
        }
        Ok(options)
    }
}

fn run(cmd: &mut Command) -> Result<()> {
    let status = cmd.status()?;
    if !status.success() {
        return Err(format!("command failed ({status}): {cmd:?}").into());
    }
    Ok(())
}
fn output(cmd: &mut Command) -> Result<String> {
    let out = cmd.output()?;
    if !out.status.success() {
        return Err(format!(
            "command failed: {cmd:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        )
        .into());
    }
    Ok(String::from_utf8(out.stdout)?)
}
fn write(root: &Path, name: &str, contents: impl AsRef<[u8]>) -> Result<()> {
    let path = root.join(name.trim_start_matches('/'));
    fs::create_dir_all(path.parent().ok_or("missing parent")?)?;
    fs::write(path, contents)?;
    Ok(())
}
fn copy(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst.parent().ok_or("missing parent")?)?;
    fs::copy(src, dst)?;
    Ok(())
}
fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    run(Command::new("cp").args(["-a"]).arg(src.join(".")).arg(dst))
}
fn resolve(root: &Path, path: &Path) -> Result<PathBuf> {
    // Interpret absolute symlinks inside the source image, never against the host.
    let mut pending = path.to_path_buf();
    for _ in 0..40 {
        let mut cur = root.to_path_buf();
        let previous = pending.clone();
        let components: Vec<_> = previous.components().collect();
        let mut changed = false;
        for (i, component) in components.iter().enumerate() {
            match component {
                Component::RootDir => continue,
                Component::Normal(p) => cur.push(p),
                Component::CurDir => continue,
                Component::ParentDir => {
                    cur.pop();
                    if !cur.starts_with(root) {
                        return Err("symlink escaped root".into());
                    }
                }
                _ => return Err("invalid path".into()),
            }
            if fs::symlink_metadata(&cur)
                .map_err(|error| format!("reading {}: {error}", cur.display()))?
                .file_type()
                .is_symlink()
            {
                let link = fs::read_link(&cur)?;
                pending = if link.is_absolute() {
                    link
                } else {
                    cur.parent()
                        .ok_or("missing parent")?
                        .strip_prefix(root)?
                        .join(link)
                };
                for rest in &components[i + 1..] {
                    pending.push(rest.as_os_str());
                }
                changed = true;
                break;
            }
        }
        if !changed {
            return Ok(cur);
        }
    }
    Err("symlink cycle".into())
}
struct Closure<'a> {
    source: &'a Path,
    root: &'a Path,
    copied: BTreeSet<String>,
}
impl Closure<'_> {
    fn add(&mut self, name: &str) -> Result<()> {
        if !self.copied.insert(name.to_string()) {
            return Ok(());
        }
        let src = resolve(self.source, Path::new(name))?;
        let dst = self.root.join(name.trim_start_matches('/'));
        copy(&src, &dst)?;
        let bytes = fs::read(&src)?;
        if !bytes.starts_with(b"\x7fELF") {
            return Ok(());
        }
        let header = output(Command::new("readelf").arg("-h").arg(&src))?;
        if !header.contains("AArch64") {
            return Err(format!("wrong architecture: {name}").into());
        }
        let info = output(Command::new("readelf").args(["-d", "-l"]).arg(&src))?;
        let mut search = Vec::new();
        for line in info
            .lines()
            .filter(|line| line.contains("(RPATH)") || line.contains("(RUNPATH)"))
        {
            let paths = line
                .split('[')
                .nth(1)
                .ok_or("invalid library search path")?
                .split(']')
                .next()
                .ok_or("invalid library search path")?;
            let origin = Path::new(name)
                .parent()
                .ok_or("missing ELF parent")?
                .to_string_lossy();
            for dir in paths.split(':') {
                let dir = dir
                    .replace("${ORIGIN}", &origin)
                    .replace("$ORIGIN", &origin);
                if !dir.starts_with('/') {
                    return Err("relative library search path is unsupported".into());
                }
                search.push(dir);
            }
        }
        search.extend(["/usr/lib".to_string(), "/lib".to_string()]);
        for line in info.lines() {
            if line.contains("(NEEDED)") {
                let lib = line
                    .split('[')
                    .nth(1)
                    .ok_or("invalid NEEDED")?
                    .split(']')
                    .next()
                    .ok_or("invalid NEEDED")?;
                let candidate = search
                    .iter()
                    .map(|dir| format!("{dir}/{lib}"))
                    .find(|path| resolve(self.source, Path::new(path)).is_ok())
                    .ok_or_else(|| format!("unresolved ELF dependency {lib} required by {name}"))?;
                self.add(&candidate)?;
            }
            if let Some(interp) = line.split("Requesting program interpreter: ").nth(1) {
                self.add(interp.trim_end_matches(']').trim())?;
            }
        }
        Ok(())
    }
}
/// Sets `key = value` pairs from `overrides` in the matching `[section]`s of
/// `base`, appending a key to its section when the base lacks it.
fn merge_ini(base: &str, overrides: &str) -> Result<String> {
    let mut lines: Vec<String> = base.lines().map(str::to_owned).collect();
    let mut section = String::new();
    for line in overrides
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with(';'))
    {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.to_owned();
            continue;
        }
        let (key, _) = line
            .split_once('=')
            .ok_or_else(|| format!("invalid override {line:?}"))?;
        let key = key.trim();
        let header = format!("[{section}]");
        let start = lines
            .iter()
            .position(|l| l.trim_start_matches('\u{feff}').trim() == header)
            .ok_or_else(|| format!("section {header} missing"))?;
        let end = lines[start + 1..]
            .iter()
            .position(|l| l.starts_with('['))
            .map_or(lines.len(), |i| start + 1 + i);
        match lines[start + 1..end]
            .iter()
            .position(|l| l.split_once('=').is_some_and(|(k, _)| k.trim() == key))
        {
            Some(i) => lines[start + 1 + i] = line.to_owned(),
            None => lines.insert(end, line.to_owned()),
        }
    }
    Ok(lines.join("\n") + "\n")
}
fn splice(src: &Path, dst: &Path, offset: u64) -> Result<()> {
    let mut input = File::open(src)?;
    let mut out = fs::OpenOptions::new().write(true).open(dst)?;
    out.seek(SeekFrom::Start(offset))?;
    std::io::copy(&mut input, &mut out)?;
    Ok(())
}
/// Mount creates its target before mounting. On a read-only root, static
/// mountpoints must already exist; writable mounts can create their children.
fn check_mountpoints(root: &Path) -> Result<()> {
    let config: toml::Value =
        toml::from_str(&fs::read_to_string(root.join("etc/ghazal/system.toml"))?)?;
    let steps = config["init"]["step"]
        .as_array()
        .ok_or("missing boot plan")?;
    let mut writable = Vec::<PathBuf>::new();
    for step in steps {
        if let Some(target) = step.get("mkdir").and_then(toml::Value::as_str) {
            let target = Path::new(target);
            if !writable.iter().any(|parent| target.starts_with(parent))
                && !root.join(target.strip_prefix("/")?).is_dir()
            {
                return Err(
                    format!("mkdir cannot write read-only root: {}", target.display()).into(),
                );
            }
        }
        if let Some(mount) = step.get("mount") {
            let target = Path::new(mount["target"].as_str().ok_or("missing mount target")?);
            if !writable.iter().any(|parent| target.starts_with(parent))
                && !root.join(target.strip_prefix("/")?).is_dir()
            {
                return Err(
                    format!("missing mountpoint on read-only root: {}", target.display()).into(),
                );
            }
            if mount["options"]
                .as_str()
                .is_some_and(|options| options.split(',').any(|option| option == "rw"))
                || matches!(mount["fstype"].as_str(), Some("tmpfs" | "devtmpfs"))
            {
                writable.push(target.into());
            }
        }
    }
    Ok(())
}
fn main() -> Result<()> {
    let options = Options::parse()?;
    let mut args = options.args.clone();
    if options.public {
        args.insert(1, String::new());
    }
    let devices = catalog()?.devices;
    let devices: Vec<_> = if options.all {
        devices
    } else {
        vec![devices
            .into_iter()
            .find(|device| device.id == options.device)
            .ok_or("unknown H700 device")?]
    };
    let repo = fs::canonicalize(&args[0])?;
    let secrets = if options.public {
        None
    } else {
        Some(fs::canonicalize(&args[1])?)
    };
    let image = std::env::current_dir()?.join(&args[2]);
    if image.exists() && !options.all {
        return Err("output exists; choose a new filename".into());
    }
    let binary = PathBuf::from(std::env::var("GHAZAL_BINARY")?);
    let busybox = PathBuf::from(std::env::var("GHAZAL_BUSYBOX")?);
    // Repository-built kernel and device tree (nix/h700-kernel.nix): all drivers
    // built in, so the bundle's kernel, modules and an initramfs are not used.
    let kernel = PathBuf::from(std::env::var("GHAZAL_KERNEL")?);
    let uboot = PathBuf::from(std::env::var(if options.memory == "ddr3" {
        "GHAZAL_UBOOT_DDR3"
    } else {
        "GHAZAL_UBOOT"
    })?);
    let boot_config = fs::read_to_string(
        uboot
            .parent()
            .ok_or("missing bootloader directory")?
            .join(".config"),
    )?;
    for setting in [
        "ENV_IS_IN_MMC=y",
        "ENV_REDUNDANT=y",
        "ENV_SIZE=0x20000",
        "ENV_OFFSET=0x400000",
        "ENV_OFFSET_REDUND=0x420000",
        "ENV_MMC_DEVICE_INDEX=0",
        "ENV_MMC_EMMC_HW_PARTITION=0",
    ] {
        if !boot_config
            .lines()
            .any(|line| line == format!("CONFIG_{setting}"))
        {
            return Err(
                format!("bootloader does not implement the A/B environment: {setting}").into(),
            );
        }
    }
    let builder_shell = std::env::var("GHAZAL_BUILDER_SHELL")?;
    let lock: BTreeMap<_, _> = fs::read_to_string(repo.join("hardware.lock"))?
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .map(|l| l.split_once('=').ok_or("invalid hardware.lock"))
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let value = |key: &str| {
        lock.get(key)
            .ok_or_else(|| format!("missing lock entry {key}"))
    };
    let cache = repo.join(".cache/hardware");
    fs::create_dir_all(&cache)?;
    let archive = cache.join("rocknix-ddr4.img.gz");
    if !archive.exists() {
        run(Command::new("curl")
            .args(["-fL", "--retry", "3", value("url")?])
            .arg("-o")
            .arg(&archive))?;
    }
    let digest = output(Command::new("sha256sum").arg(&archive))?;
    if digest.split_whitespace().next() != Some(value("sha256")?.as_str()) {
        return Err("hardware archive checksum mismatch".into());
    }
    let raw = cache.join("rocknix.img");
    // Regenerate raw and extracted files every time from the verified archive.
    run(Command::new("gzip")
        .args(["-dc"])
        .arg(&archive)
        .stdout(File::create(&raw)?))?;
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let work = repo.join(format!("build/{stamp}"));
    let upstream = work.join("upstream");
    fs::create_dir_all(&work)?;
    let partition = output(Command::new("sfdisk").arg("--json").arg(&raw))?;
    let mut jq = Command::new("jq")
        .args([
            "-r",
            ".partitiontable.partitions[0].start * .partitiontable.sectorsize",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    jq.stdin
        .take()
        .ok_or("jq stdin")?
        .write_all(partition.as_bytes())?;
    let start = jq.wait_with_output()?;
    if !start.status.success() {
        return Err("cannot inspect hardware partition table".into());
    }
    let start: u64 = String::from_utf8(start.stdout)?.trim().parse()?;
    if start != 16 * 1024 * 1024 {
        return Err("unexpected bootloader layout".into());
    }
    let spec = format!("{}@@{start}", raw.display());
    run(Command::new("mcopy")
        .args(["-i", &spec, "::/SYSTEM"])
        .arg(work.join("SYSTEM")))?;
    run(Command::new("unsquashfs")
        .args(["-no-progress", "-d"])
        .arg(&upstream)
        .arg(work.join("SYSTEM")))?;
    let release = fs::read_to_string(upstream.join("etc/os-release"))?;
    let expected_revision = format!("BUILD_ID=\"{}\"", value("source_revision")?);
    if !release.lines().any(|line| line == expected_revision) {
        return Err("hardware source revision disagrees with hardware.lock".into());
    }
    fs::remove_file(&raw)?;
    fs::remove_file(work.join("SYSTEM"))?;
    let root = work.join("root");
    fs::create_dir_all(&root)?;
    for dir in [
        "usr/bin",
        "usr/sbin",
        "usr/lib",
        "dev",
        "proc",
        "sys",
        "run",
        "tmp",
        "mnt/games",
        "mnt/boot",
        "root/.ssh",
        "etc/ssh",
        "var/lib/ghazal",
        "var/lib/iwd",
        "var/log/ghazal",
        "var/empty",
        "etc/ghazal",
        "etc/dbus-1/system.d",
    ] {
        fs::create_dir_all(root.join(dir))?;
    }
    for (link, target) in [
        ("bin", "usr/bin"),
        ("sbin", "usr/sbin"),
        ("lib", "usr/lib"),
        ("var/run", "/run"),
    ] {
        symlink(target, root.join(link))?;
    }
    fs::set_permissions(root.join("tmp"), fs::Permissions::from_mode(0o1777))?;
    copy(&binary, &root.join("opt/ghazal/ghazal"))?;
    copy(&busybox, &root.join("usr/bin/busybox"))?;
    for applet in [
        "sh",
        "ls",
        "cat",
        "ps",
        "mount",
        "umount",
        "dmesg",
        "grep",
        "top",
        "kill",
        "mkdir",
        "rm",
        "cp",
        "mv",
        "echo",
        "vi",
        "free",
        "df",
        "uname",
        "sleep",
        "ip",
        "ping",
        "sync",
        "date",
        "tail",
        "head",
        "find",
        "chmod",
        "chown",
        "modprobe",
        "lsmod",
        "insmod",
        "rmmod",
        "stat",
        "du",
        "wc",
        "tee",
        "reboot",
        "poweroff",
        "sed",
        "awk",
        "cut",
        "sort",
        "uniq",
        "tr",
        "diff",
        "strings",
        "dd",
        "time",
        "less",
        "xargs",
        "md5sum",
        "sha256sum",
        "ln",
        "hexdump",
    ] {
        symlink("busybox", root.join("usr/bin").join(applet))?;
    }
    symlink("/opt/ghazal/ghazal", root.join("usr/sbin/resolvconf"))?;
    // The kernel mounts the root filesystem and runs this; ghazal dispatches on argv[0].
    symlink("/opt/ghazal/ghazal", root.join("usr/sbin/init"))?;
    copy_tree(
        &repo.join("config/rg40xxv-rocknix"),
        &root.join("etc/ghazal"),
    )?;
    copy_tree(
        &upstream.join("usr/lib/kernel-overlays/base/lib/firmware"),
        &root.join("usr/lib/firmware"),
    )?;
    copy_tree(
        &upstream.join("usr/share/alsa"),
        &root.join("usr/share/alsa"),
    )?;
    copy_tree(
        &upstream.join("usr/share/licenses"),
        &root.join("usr/share/licenses"),
    )?;
    copy_tree(
        &upstream.join("usr/share/libretro/autoconfig"),
        &root.join("usr/share/libretro/autoconfig"),
    )?;
    // The H700 pad sends BTN_WEST for its X button and BTN_NORTH for Y.
    // Own this board mapping rather than inheriting the generic Xbox ordering.
    copy(
        &repo.join("config/rg40xxv-rocknix/gamepad.cfg"),
        &root.join("usr/share/libretro/autoconfig/H700 Gamepad.cfg"),
    )?;
    // Standalone PPSSPP: assets (including the PSP flash0 fonts it looks up next
    // to its executable), ROCKNIX's H700 defaults and the SDL controller database.
    copy_tree(
        &upstream.join("usr/config/ppsspp/assets"),
        &root.join("usr/share/ppsspp/assets"),
    )?;
    copy_tree(
        &upstream.join("usr/config/ppsspp/PSP"),
        &root.join("var/lib/ghazal/ppsspp/PSP"),
    )?;
    let ppsspp = repo.join("config/rg40xxv-rocknix/ppsspp");
    let system = root.join("var/lib/ghazal/ppsspp/PSP/SYSTEM");
    copy(&ppsspp.join("controls.ini"), &system.join("controls.ini"))?;
    let defaults = fs::read_to_string(system.join("ppsspp.ini"))?;
    let settings = merge_ini(
        &defaults,
        &fs::read_to_string(ppsspp.join("ppsspp-overrides.ini"))?,
    )?;
    fs::write(system.join("ppsspp.ini"), &settings)?;
    copy_tree(
        &ppsspp.join("Cheats"),
        &root.join("var/lib/ghazal/ppsspp/PSP/Cheats"),
    )?;
    // PPSSPP reads every per-game setting from a game's own ini, so each one
    // starts from the full settings and changes only that game's overrides.
    for entry in fs::read_dir(ppsspp.join("games"))? {
        let path = entry?.path();
        let id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or("invalid game override name")?;
        fs::write(
            system.join(format!("{id}_ppsspp.ini")),
            merge_ini(&settings, &fs::read_to_string(&path)?)?,
        )?;
    }
    copy_tree(
        &upstream.join("usr/config/SDL-GameControllerDB"),
        &root.join("usr/share/SDL-GameControllerDB"),
    )?;
    // ROCKNIX's PPSSPP has its /storage paths compiled in. Point them at our
    // layout: settings on SD1, game saves shared with the libretro core on SD2.
    for (link, target) in [
        ("usr/bin/assets", "/usr/share/ppsspp/assets"),
        ("var/lib/ghazal/ppsspp/assets", "/usr/share/ppsspp/assets"),
        ("storage/.config/ppsspp", "/var/lib/ghazal/ppsspp"),
        (
            "storage/.config/SDL-GameControllerDB",
            "/usr/share/SDL-GameControllerDB",
        ),
        (
            "storage/roms/psp",
            "/mnt/games/SAVES/file/PPSSPP/PSP/SAVEDATA",
        ),
        (
            "storage/roms/savestates/psp/ppsspp-sa",
            "/mnt/games/SAVES/state/PPSSPP-standalone",
        ),
        ("storage/roms/screenshots", "/mnt/games/SAVES/screenshots"),
    ] {
        fs::create_dir_all(root.join(link).parent().ok_or("missing parent")?)?;
        symlink(target, root.join(link))?;
    }
    for path in [
        "usr/share/xkeyboard-config-2",
        "usr/share/fontconfig",
        "usr/share/fonts",
        "etc/fonts",
    ] {
        copy_tree(&upstream.join(path), &root.join(path))?;
    }
    fs::create_dir_all(root.join("usr/share/X11"))?;
    symlink(
        "/usr/share/xkeyboard-config-2",
        root.join("usr/share/X11/xkb"),
    )?;
    // Rules are explicit: do not pull in upstream's systemd service hooks.
    write(&root, "etc/udev/rules.d/70-gamepad.rules", "SUBSYSTEM==\"input\", KERNEL==\"event*\", ATTRS{name}==\"H700 Gamepad\", ENV{ID_INPUT}=\"1\", ENV{ID_INPUT_JOYSTICK}=\"1\"\n")?;
    let mut closure = Closure {
        source: &upstream,
        root: &root,
        copied: BTreeSet::new(),
    };
    for name in [
        "/usr/bin/retroarch",
        "/usr/bin/syncthing",
        "/usr/bin/amixer",
        "/usr/sbin/alsactl",
        "/usr/bin/alsaucm",
        "/usr/bin/udevadm",
        "/usr/lib/systemd/systemd-udevd",
        "/usr/bin/dbus-daemon",
        "/usr/lib/iwd",
        "/usr/bin/iwctl",
        "/usr/sbin/iw",
        "/usr/sbin/blkid",
        "/usr/sbin/mkfs.exfat",
        "/usr/sbin/fsck.exfat",
        "/usr/sbin/sshd",
        "/usr/bin/ssh-keygen",
        "/usr/lib/openssh/sshd-session",
        "/usr/lib/libEGL.so.1",
        "/usr/lib/libGLESv2.so.2",
        "/usr/lib/gbm/dri_gbm.so",
        "/usr/lib/dri/panfrost_dri.so",
    ] {
        closure.add(name)?;
    }
    let frontend = fs::read_to_string(root.join("etc/ghazal/frontend.toml"))?;
    for line in frontend.lines().filter(|l| l.starts_with("core = ")) {
        let core = line.split('"').nth(1).ok_or("invalid core profile")?;
        closure.add(&format!("/usr/lib/libretro/{core}"))?;
    }
    // Standalone emulators: the first element of each `command`.
    for line in frontend.lines().filter(|l| l.starts_with("command = [")) {
        let program = line.split('"').nth(1).ok_or("invalid emulator command")?;
        closure.add(program)?;
    }
    for name in [
        "/usr/lib/libEGL_mesa.so.0",
        "/usr/lib/libGL.so.1",
        "/usr/lib/libGLX_mesa.so.0",
        "/usr/lib/libnss_files.so.2",
        "/usr/lib/libnss_dns.so.2",
    ] {
        closure.add(name)?;
    }
    copy_tree(
        &upstream.join("usr/share/glvnd"),
        &root.join("usr/share/glvnd"),
    )?;
    write(&root, "etc/passwd", "root:x:0:0:root:/root:/bin/sh\ndbus:x:81:81:dbus:/:/bin/false\nsshd:x:22:22:sshd:/var/empty:/bin/false\nnobody:x:65534:65534:nobody:/:/bin/false\n")?;
    // An unlocked, unusable password permits public-key SSH; password auth is disabled.
    write(
        &root,
        "etc/shadow",
        "root::0:0:99999:7:::\ndbus:!:0:0:99999:7:::\nsshd:!:0:0:99999:7:::\n",
    )?;
    write(&root, "etc/group", "root:x:0:\ntty:x:5:\ninput:x:24:\naudio:x:29:\nvideo:x:44:\nrender:x:107:\ndbus:x:81:\nsshd:x:22:\n")?;
    write(
        &root,
        "etc/nsswitch.conf",
        "passwd: files\ngroup: files\nshadow: files\nhosts: files dns\n",
    )?;
    write(
        &root,
        "etc/hosts",
        "127.0.0.1 localhost ghazal\n::1 localhost\n",
    )?;
    write(
        &root,
        "etc/os-release",
        "NAME=Ghazal\nID=ghazal\nVERSION_ID=0.1.0\n",
    )?;
    write(&root, "etc/machine-id", "")?;
    write(&root, "var/lib/ghazal/factory-data", "ghazal-factory-v1\n")?;
    write(&root, "etc/ssh/sshd_config", "Port 22\nHostKey /var/lib/ghazal/ssh/ssh_host_ed25519_key\nPermitRootLogin prohibit-password\nPasswordAuthentication no\nKbdInteractiveAuthentication no\nAuthorizedKeysFile /var/lib/ghazal/ssh/authorized_keys\nSubsystem sftp internal-sftp\n")?;
    if let Some(secrets) = &secrets {
        copy(
            &secrets.join("authorized_keys"),
            &root.join("root/.ssh/authorized_keys"),
        )?;
    } else {
        write(&root, "root/.ssh/authorized_keys", "")?;
    }
    fs::set_permissions(root.join("root/.ssh"), fs::Permissions::from_mode(0o700))?;
    fs::set_permissions(
        root.join("root/.ssh/authorized_keys"),
        fs::Permissions::from_mode(0o600),
    )?;
    fs::set_permissions(root.join("etc/shadow"), fs::Permissions::from_mode(0o600))?;
    copy(
        &upstream.join("usr/share/dbus-1/system.d/iwd-dbus.conf"),
        &root.join("etc/dbus-1/system.d/iwd.conf"),
    )?;
    write(&root, "etc/dbus-1/system.conf", "<!DOCTYPE busconfig PUBLIC \"-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN\" \"http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd\">\n<busconfig><type>system</type><user>root</user><listen>unix:path=/run/dbus/system_bus_socket</listen><auth>EXTERNAL</auth><policy context=\"default\"><allow user=\"root\"/><allow own=\"*\"/><allow send_destination=\"*\"/><allow receive_sender=\"*\"/></policy><includedir>system.d</includedir></busconfig>\n")?;
    write(
        &root,
        "etc/iwd/main.conf",
        "[General]\nEnableNetworkConfiguration=true\n[Network]\nNameResolvingService=resolvconf\n",
    )?;
    if let Some(secrets) = &secrets {
        let ssid = fs::read_to_string(secrets.join("ssid"))?
            .trim_end_matches('\n')
            .to_string();
        let pass = fs::read_to_string(secrets.join("passphrase"))?
            .trim_end_matches('\n')
            .to_string();
        if ssid.is_empty()
            || ssid.len() > 32
            || ssid.contains('\n')
            || pass.contains('\n')
            || pass.len() < 8
        {
            return Err("invalid Wi-Fi build inputs".into());
        }
        let filename = if ssid
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b' ')
        {
            ssid.clone()
        } else {
            format!(
                "={}",
                ssid.bytes().map(|b| format!("{b:02x}")).collect::<String>()
            )
        };
        write(
            &root,
            &format!("var/lib/iwd/{filename}.psk"),
            format!("[Security]\nPassphrase={pass}\n[Settings]\nAutoConnect=true\n"),
        )?;
        fs::set_permissions(
            root.join(format!("var/lib/iwd/{filename}.psk")),
            fs::Permissions::from_mode(0o600),
        )?;
    }
    write(
        &root,
        "etc/resolv.conf",
        "# populated by iwd via ghazal resolvconf\n",
    )?;
    write(
        &root,
        "etc/asound.conf",
        "pcm.!default { type plug slave.pcm \"hw:0,0\" }\nctl.!default { type hw card 0 }\n",
    )?;
    write(
        &root,
        "etc/ghazal/retroarch.cfg",
        include_str!("../../../config/rg40xxv-rocknix/retroarch.cfg"),
    )?;
    if devices.iter().any(|device| device.rotation == "left") {
        closure.add("/usr/lib/libretro/ppsspp_libretro.so")?;
    }
    fs::write(
        work.join("closure.txt"),
        closure
            .copied
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n"),
    )?;
    copy(
        &repo.join("hardware.lock"),
        &root.join("etc/ghazal/hardware.lock"),
    )?;
    if options.public
        && (!fs::read(root.join("root/.ssh/authorized_keys"))?.is_empty()
            || fs::read_dir(root.join("var/lib/iwd"))?.next().is_some()
            || !fs::read(root.join("etc/machine-id"))?.is_empty())
    {
        return Err("public image contains credentials or a shared machine identity".into());
    }
    // Preserve the complete Nix closure: its ELF interpreters and RPATHs must not
    // accidentally bind to the emulator runtime's different glibc.
    for store in fs::read_to_string(std::env::var("GHAZAL_UPDATE_CLOSURE")?)?.lines() {
        if !store.starts_with("/nix/store/") {
            return Err("invalid update closure".into());
        }
        copy_tree(Path::new(store), &root.join(store.trim_start_matches('/')))?;
    }
    for (name, variable) in [
        ("rauc", "GHAZAL_RAUC"),
        ("curl", "GHAZAL_CURL"),
        ("fw_printenv", "GHAZAL_FW_PRINTENV"),
        ("fw_setenv", "GHAZAL_FW_SETENV"),
    ] {
        symlink(std::env::var(variable)?, root.join("usr/bin").join(name))?;
    }
    copy(
        Path::new(&std::env::var("GHAZAL_CA_CERT")?),
        &root.join("etc/ssl/certs/ca-certificates.crt"),
    )?;
    copy(
        &repo.join("config/updates/release.pem"),
        &root.join("etc/rauc/release.pem"),
    )?;
    copy(
        &repo.join("config/updates/fw_env.config"),
        &root.join("etc/fw_env.config"),
    )?;
    let state = work.join("state");
    fs::rename(root.join("var"), &state)?;
    fs::create_dir(root.join("var"))?;
    check_mountpoints(&root)?;
    fs::create_dir_all(state.join("lib/ghazal/ssh"))?;
    fs::rename(
        root.join("root/.ssh/authorized_keys"),
        state.join("lib/ghazal/ssh/authorized_keys"),
    )?;
    fs::rename(
        root.join("etc/machine-id"),
        state.join("lib/ghazal/machine-id"),
    )?;
    symlink("/var/lib/ghazal/machine-id", root.join("etc/machine-id"))?;
    fs::remove_file(root.join("etc/resolv.conf"))?;
    symlink("/run/ghazal/resolv.conf", root.join("etc/resolv.conf"))?;
    let state_image = work.join("state.ext4");
    File::create(&state_image)?.set_len(256 * 1024 * 1024)?;
    run(Command::new("fakeroot")
        .args([
            builder_shell.as_str(),
            "-ec",
            "chown -R 0:0 \"$1\"; mkfs.ext4 -q -F -L GHAZALSTATE -d \"$1\" \"$2\"",
            "state-builder",
        ])
        .arg(&state)
        .arg(&state_image))?;
    let device_template = fs::read_to_string(root.join("etc/ghazal/device.toml"))?;
    let frontend_template = fs::read_to_string(root.join("etc/ghazal/frontend.toml"))?;
    fs::remove_dir_all(&upstream)?;
    copy(&kernel.join("boot/KERNEL"), &root.join("boot/KERNEL"))?;
    for device in devices {
        let image = if options.all {
            image.join(format!(
                "ghazal-{}-{}-{}.img",
                options.version, device.id, options.memory
            ))
        } else {
            image.clone()
        };
        let mut profile: toml::Value = toml::from_str(&device_template)?;
        profile["name"] = toml::Value::String(format!("Anbernic {}", device.name));
        profile["display"]
            .as_table_mut()
            .ok_or("missing display profile")?
            .insert(
                "rotation".into(),
                toml::Value::String(device.rotation.clone()),
            );
        write(&root, "etc/ghazal/device.toml", toml::to_string(&profile)?)?;
        let mut frontend: toml::Value = toml::from_str(&frontend_template)?;
        if device.rotation == "left" {
            for system in frontend["system"].as_array_mut().ok_or("missing systems")? {
                if system["name"].as_str() == Some("PSP") {
                    let system = system.as_table_mut().ok_or("invalid PSP profile")?;
                    for key in ["command", "env", "log"] {
                        system.remove(key);
                    }
                    system.insert(
                        "core".into(),
                        toml::Value::String("ppsspp_libretro.so".into()),
                    );
                }
            }
        }
        write(
            &root,
            "etc/ghazal/frontend.toml",
            toml::to_string(&frontend)?,
        )?;
        write(
            &root,
            "etc/ghazal/retroarch.cfg",
            format!(
                "{}\nvideo_rotation = \"{}\"\n",
                include_str!("../../../config/rg40xxv-rocknix/retroarch.cfg"),
                if device.rotation == "left" { 3 } else { 0 }
            ),
        )?;
        write(
            &root,
            "etc/ghazal/image.json",
            serde_json::to_vec_pretty(&serde_json::json!({
                "device":device, "memory":options.memory, "version":options.version, "public":options.public,
                "data_policy":"TF2 if present, otherwise TF1 data partition", "hardware_revision":value("source_revision")?
            }))?,
        )?;
        // FAT is a provisioning volume; boot-critical files belong to each OS slot.
        let boot = work.join(format!("boot-{}", device.id));
        write(&boot, "provision.toml.example", "# Optional: copy to provision.toml before first boot. Never share the edited file.\n# auto selects TF2 when inserted; tf1 and tf2 require that chosen card.\ndata_card = \"auto\"\nssh_authorized_keys = []\n# [wifi]\n# ssid = \"Your Wi-Fi name\"\n# password = \"Your Wi-Fi password\"\n")?;
        // `quiet`: printing the boot log over the 115200-baud serial console alone
        // took ~2.5 s. Errors still reach both consoles; dmesg keeps everything.
        // rtw88: the pinned firmware fails to acknowledge SDIO deep power-save transitions.
        if root.join("boot/dtb.img").exists() {
            fs::remove_file(root.join("boot/dtb.img"))?;
        }
        copy(
            &kernel
                .join("boot/device_trees")
                .join(format!("{}.dtb", device.dtb)),
            &root.join("boot/dtb.img"),
        )?;
        let compatible = format!("ghazal-{}-{}-ab1", device.id, options.memory);
        write(
            &root,
            "etc/rauc/system.conf",
            include_str!("../../../config/updates/system.conf.in")
                .replace("@COMPATIBLE@", &compatible),
        )?;
        let root_image = work.join(format!("root-{}.ext4", device.id));
        File::create(&root_image)?.set_len(1024 * 1024 * 1024)?;
        run(Command::new("fakeroot")
            .args([
                builder_shell.as_str(),
                "-ec",
                "chown -R 0:0 \"$1\"; mkfs.ext4 -q -F -L GHAZALROOT -d \"$1\" \"$2\"",
                "root-builder",
            ])
            .arg(&root)
            .arg(&root_image))?;
        if let Ok(key) = std::env::var("GHAZAL_UPDATE_KEY") {
            let bundle = work.join(format!("bundle-{}", device.id));
            fs::create_dir(&bundle)?;
            fs::hard_link(&root_image, bundle.join("root.ext4"))?;
            write(&bundle, "manifest.raucm", format!("[update]\ncompatible={compatible}\nversion={}\n[bundle]\nformat=verity\n[image.rootfs]\nfilename=root.ext4\n", options.version))?;
            let destination = image
                .parent()
                .ok_or("missing image parent")?
                .join(format!("ghazal-{}-{}.raucb", device.id, options.memory));
            fs::create_dir_all(destination.parent().ok_or("missing bundle parent")?)?;
            run(Command::new("rauc")
                .args(["bundle", "--cert"])
                .arg(repo.join("config/updates/release.pem"))
                .arg("--key")
                .arg(key)
                .arg(&bundle)
                .arg(&destination)
                .env_remove("LD_LIBRARY_PATH"))?;
            fs::remove_dir_all(&bundle)?;
            run(Command::new("rauc")
                .args(["info", "--keyring"])
                .arg(repo.join("config/updates/release.pem"))
                .arg(&destination)
                .env_remove("LD_LIBRARY_PATH"))?;
        }
        let boot_image = work.join("boot.fat");
        File::create(&boot_image)?.set_len(128 * 1024 * 1024)?;
        run(Command::new("mkfs.vfat")
            .args(["-F", "32", "-n", "GHAZALBOOT"])
            .arg(&boot_image))?;
        {
            let name = "provision.toml.example";
            run(Command::new("mcopy")
                .args(["-s", "-i"])
                .arg(&boot_image)
                .arg(boot.join(name))
                .arg("::/"))?;
        }
        fs::create_dir_all(image.parent().ok_or("missing output directory")?)?;
        let out = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&image)?;
        fs::set_permissions(&image, fs::Permissions::from_mode(0o600))?;
        out.set_len((2450 + 128) * 1024 * 1024)?;
        if fs::metadata(&uboot)?.len() + 8192 >= 0x400000 {
            return Err("U-Boot overlaps redundant environment".into());
        }
        // The Allwinner boot ROM loads the SPL from 8 KiB; the first partition starts at 16 MiB.
        splice(&uboot, &image, 8 * 1024)?;
        let mut disk = Command::new("sfdisk")
            .args(["--wipe", "never"])
            .arg(&image)
            .stdin(Stdio::piped())
            .spawn()?;
        disk.stdin.take().ok_or("sfdisk stdin")?.write_all(b"label: dos\nunit: sectors\n\nstart=32768,size=262144,type=c,bootable\nstart=294912,size=2097152,type=83\nstart=5017600,size=262144,type=7\nstart=2392064,size=2625536,type=5\nstart=2394112,size=2097152,type=83\nstart=4493312,size=524288,type=83\n")?;
        if !disk.wait()?.success() {
            return Err("partition creation failed".into());
        }
        splice(&boot_image, &image, 16 * 1024 * 1024)?;
        splice(&root_image, &image, 144 * 1024 * 1024)?;
        splice(&root_image, &image, 1169 * 1024 * 1024)?;
        splice(&state_image, &image, 2194 * 1024 * 1024)?;
        out.sync_all()?;
        run(Command::new("e2fsck").args(["-fn"]).arg(&root_image))?;
        run(Command::new("fsck.vfat").args(["-n"]).arg(&boot_image))?;
        fs::remove_file(&root_image)?;
        let artifact = if options.all {
            run(Command::new("gzip").args(["-n", "-1"]).arg(&image))?;
            image.with_extension("img.gz")
        } else {
            image.clone()
        };
        let digest = output(Command::new("sha256sum").arg(&artifact))?;
        let hash = format!(
            "{}  {}\n",
            digest.split_whitespace().next().ok_or("missing checksum")?,
            artifact
                .file_name()
                .ok_or("missing filename")?
                .to_string_lossy()
        );
        fs::write(
            artifact.with_extension(if options.all {
                "gz.sha256"
            } else {
                "img.sha256"
            }),
            hash,
        )?;
        println!(
            "Built {} ({}). Work tree: {}",
            artifact.display(),
            if options.public {
                "public: no credentials"
            } else {
                "private: contains credentials"
            },
            work.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::merge_ini;

    #[test]
    fn overrides_replace_or_append_within_their_section() {
        let base =
            "\u{feff}[General]\nUIScaleFactor = -1\nOther = 1\n[Graphics]\nAnisotropyLevel = 4\n";
        let merged = merge_ini(base, "; note\n[General]\nUIScaleFactor = -6\n[Graphics]\nAnisotropyLevel = 0\nDepthRasterMode = 2\n").unwrap();
        assert_eq!(merged, "\u{feff}[General]\nUIScaleFactor = -6\nOther = 1\n[Graphics]\nAnisotropyLevel = 0\nDepthRasterMode = 2\n");
        assert!(merge_ini(base, "[Missing]\nKey = 1\n").is_err());
    }
}
