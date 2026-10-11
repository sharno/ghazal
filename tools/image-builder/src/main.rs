//! Assemble an independent rootfs from a checksum-pinned hardware bundle.
//! Never opens a block device; the output is a regular image file.
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};
type Result<T> = std::result::Result<T, Box<dyn Error>>;

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
fn splice(src: &Path, dst: &Path, offset: u64) -> Result<()> {
    let mut input = File::open(src)?;
    let mut out = fs::OpenOptions::new().write(true).open(dst)?;
    out.seek(SeekFrom::Start(offset))?;
    std::io::copy(&mut input, &mut out)?;
    Ok(())
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 3 {
        return Err(
            "usage: oxmux-image-builder SOURCE_DIRECTORY SECRETS_DIRECTORY OUTPUT.img".into(),
        );
    }
    let repo = fs::canonicalize(&args[0])?;
    let secrets = fs::canonicalize(&args[1])?;
    let image = std::env::current_dir()?.join(&args[2]);
    if image.exists() {
        return Err("output exists; choose a new filename".into());
    }
    let binary = PathBuf::from(std::env::var("OXMUX_BINARY")?);
    let busybox = PathBuf::from(std::env::var("OXMUX_BUSYBOX")?);
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
    for (name, dest) in [
        ("SYSTEM", work.join("SYSTEM")),
        ("KERNEL", work.join("KERNEL")),
    ] {
        run(Command::new("mcopy")
            .args(["-i", &spec, &format!("::/{name}")])
            .arg(dest))?;
    }
    run(Command::new("mcopy")
        .args([
            "-i",
            &spec,
            &format!("::/device_trees/{}", value("device_tree")?),
        ])
        .arg(work.join("dtb.img")))?;
    run(Command::new("unsquashfs")
        .args(["-no-progress", "-d"])
        .arg(&upstream)
        .arg(work.join("SYSTEM")))?;
    let release = fs::read_to_string(upstream.join("etc/os-release"))?;
    let expected_revision = format!("BUILD_ID=\"{}\"", value("source_revision")?);
    if !release.lines().any(|line| line == expected_revision) {
        return Err("hardware source revision disagrees with hardware.lock".into());
    }
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
        "root/.ssh",
        "etc/ssh",
        "var/lib/oxmux",
        "var/lib/iwd",
        "var/log/oxmux",
        "var/empty",
        "etc/oxmux",
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
    copy(&binary, &root.join("opt/oxmux/oxmux"))?;
    copy(&busybox, &root.join("usr/bin/busybox"))?;
    for applet in [
        "sh", "ls", "cat", "ps", "mount", "umount", "dmesg", "grep", "top", "kill", "mkdir", "rm",
        "cp", "mv", "echo", "vi", "free", "df", "uname", "sleep", "ip", "ping", "sync", "date",
        "tail", "head", "find", "chmod", "chown", "modprobe", "lsmod", "insmod", "rmmod", "stat",
        "du", "wc", "tee", "reboot", "poweroff",
    ] {
        symlink("busybox", root.join("usr/bin").join(applet))?;
    }
    symlink("/opt/oxmux/oxmux", root.join("usr/sbin/resolvconf"))?;
    symlink("../bin/busybox", root.join("usr/sbin/modprobe"))?;
    copy_tree(
        &repo.join("config/rg40xxv-rocknix"),
        &root.join("etc/oxmux"),
    )?;
    copy_tree(
        &upstream.join("usr/lib/kernel-overlays/base/lib/modules"),
        &root.join("usr/lib/modules"),
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
    let frontend = fs::read_to_string(root.join("etc/oxmux/frontend.toml"))?;
    for line in frontend.lines().filter(|l| l.starts_with("core = ")) {
        let core = line.split('"').nth(1).ok_or("invalid core profile")?;
        closure.add(&format!("/usr/lib/libretro/{core}"))?;
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
        "127.0.0.1 localhost oxmux\n::1 localhost\n",
    )?;
    write(
        &root,
        "etc/os-release",
        "NAME=Oxmux\nID=oxmux\nVERSION_ID=0.1.0\n",
    )?;
    write(&root, "etc/machine-id", format!("{stamp:032x}\n"))?;
    write(&root, "etc/ssh/sshd_config", "Port 22\nHostKey /etc/ssh/ssh_host_ed25519_key\nPermitRootLogin prohibit-password\nPasswordAuthentication no\nKbdInteractiveAuthentication no\nAuthorizedKeysFile .ssh/authorized_keys\nSubsystem sftp internal-sftp\n")?;
    copy(
        &secrets.join("authorized_keys"),
        &root.join("root/.ssh/authorized_keys"),
    )?;
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
    write(
        &root,
        "etc/resolv.conf",
        "# populated by iwd via oxmux resolvconf\n",
    )?;
    write(
        &root,
        "etc/asound.conf",
        "pcm.!default { type plug slave.pcm \"hw:0,0\" }\nctl.!default { type hw card 0 }\n",
    )?;
    write(
        &root,
        "etc/oxmux/retroarch.cfg",
        include_str!("../../../config/rg40xxv-rocknix/retroarch.cfg"),
    )?;
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
        &root.join("etc/oxmux/hardware.lock"),
    )?;
    let initrd = work.join("initrd");
    for dir in ["bin", "dev", "proc", "sys", "sysroot"] {
        fs::create_dir_all(initrd.join(dir))?;
    }
    copy(&binary, &initrd.join("oxmux-bootstrap"))?;
    copy(&busybox, &initrd.join("bin/busybox"))?;
    symlink("busybox", initrd.join("bin/sh"))?;
    let archive = work.join("initramfs.cpio");
    // Fixed shell program, paths passed as positional arguments, no interpolation.
    run(Command::new("sh").args(["-c", "cd \"$1\"; find . -print0 | LC_ALL=C sort -z | cpio --null -o -H newc --owner=0:0 > \"$2\"", "image-builder"]).arg(&initrd).arg(&archive))?;
    run(Command::new("gzip")
        .args(["-n", "-c"])
        .arg(&archive)
        .stdout(File::create(work.join("initramfs.gz"))?))?;
    let root_image = work.join("root.ext4");
    File::create(&root_image)?.set_len(1024 * 1024 * 1024)?;
    run(Command::new("fakeroot")
        .args([
            "sh",
            "-ec",
            "chown -R 0:0 \"$1\"; mkfs.ext4 -q -F -L OXMUXROOT -d \"$1\" \"$2\"",
            "image-builder",
        ])
        .arg(&root)
        .arg(&root_image))?;
    let boot = work.join("boot");
    fs::create_dir_all(boot.join("extlinux"))?;
    for name in ["KERNEL", "dtb.img", "initramfs.gz"] {
        copy(&work.join(name), &boot.join(name))?;
    }
    write(&boot, "extlinux/extlinux.conf", "DEFAULT oxmux\nTIMEOUT 0\nLABEL oxmux\n  LINUX /KERNEL\n  INITRD /initramfs.gz\n  FDT /dtb.img\n  APPEND rdinit=/oxmux-bootstrap oxmux.root=/dev/mmcblk0p2 console=ttyS0,115200 console=tty0 rootwait panic=0\n")?;
    let boot_image = work.join("boot.fat");
    File::create(&boot_image)?.set_len(128 * 1024 * 1024)?;
    run(Command::new("mkfs.vfat")
        .args(["-F", "32", "-n", "OXMUXBOOT"])
        .arg(&boot_image))?;
    for name in ["KERNEL", "dtb.img", "initramfs.gz", "extlinux"] {
        run(Command::new("mcopy")
            .args(["-s", "-i"])
            .arg(&boot_image)
            .arg(boot.join(name))
            .arg("::/"))?;
    }
    fs::create_dir_all(image.parent().ok_or("missing output directory")?)?;
    let mut out = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&image)?;
    fs::set_permissions(&image, fs::Permissions::from_mode(0o600))?;
    out.set_len((16 + 128 + 1024) * 1024 * 1024)?;
    let mut vendor = File::open(&raw)?.take(16 * 1024 * 1024);
    std::io::copy(&mut vendor, &mut out)?;
    let mut disk = Command::new("sfdisk")
        .args(["--wipe", "never"])
        .arg(&image)
        .stdin(Stdio::piped())
        .spawn()?;
    disk.stdin.take().ok_or("sfdisk stdin")?.write_all(b"label: dos\nunit: sectors\n\nstart=32768,size=262144,type=c,bootable\nstart=294912,size=2097152,type=83\n")?;
    if !disk.wait()?.success() {
        return Err("partition creation failed".into());
    }
    splice(&boot_image, &image, 16 * 1024 * 1024)?;
    splice(&root_image, &image, 144 * 1024 * 1024)?;
    out.sync_all()?;
    let hash = output(Command::new("sha256sum").arg(&image))?;
    fs::write(image.with_extension("img.sha256"), hash)?;
    println!(
        "Built {}. Private image contains Wi-Fi provisioning. Work tree: {}",
        image.display(),
        work.display()
    );
    Ok(())
}
