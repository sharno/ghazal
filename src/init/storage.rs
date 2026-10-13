//! First-boot preparation touches only the image's reserved TF1 data partition.
//! Existing TF2 filesystems are mounted by the boot plan and are never formatted.
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::{
    fs::{self, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::fd::AsRawFd,
    path::Path,
    process::Command,
};

const DATA_START: u32 = 2_392_064;
const FACTORY: &str = "/var/lib/ghazal/factory-data";
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Prepare {}

#[derive(Clone, Copy, PartialEq)]
enum FactoryPhase {
    Fresh,
    Formatting,
}
struct FactoryTransaction {
    marker: std::path::PathBuf,
    phase: FactoryPhase,
}
impl FactoryTransaction {
    fn read(marker: &Path) -> Result<Self> {
        let phase = match fs::read_to_string(marker)?.trim() {
            "ghazal-factory-v1" => FactoryPhase::Fresh,
            "ghazal-formatting-v1" => FactoryPhase::Formatting,
            _ => bail!("unknown factory storage marker"),
        };
        Ok(Self {
            marker: marker.into(),
            phase,
        })
    }
    fn begin_format(&mut self) -> Result<()> {
        crate::sys::write_atomic(&self.marker, b"ghazal-formatting-v1\n")?;
        self.phase = FactoryPhase::Formatting;
        Ok(())
    }
}
struct FilesystemTools<'a> {
    formatter: &'a Path,
    labeler: &'a Path,
    checker: &'a Path,
}

/// The validated layout owns its MBR, so a resize cannot select another partition.
struct FactoryLayout {
    mbr: [u8; 512],
}
impl FactoryLayout {
    fn parse(mbr: [u8; 512]) -> Result<Self> {
        if mbr[510..] != [0x55, 0xaa] {
            bail!("missing MBR signature");
        }
        for (index, start, size, kind) in [
            (0, 32768, Some(262144), 0x0c),
            (1, 294912, Some(2097152), 0x83),
            (2, DATA_START, None, 0x07),
        ] {
            let entry = &mbr[446 + index * 16..][..16];
            let actual_start = u32::from_le_bytes(entry[8..12].try_into()?);
            let actual_size = u32::from_le_bytes(entry[12..16].try_into()?);
            if entry[4] != kind
                || actual_start != start
                || actual_size == 0
                || size.is_some_and(|size| size != actual_size)
            {
                bail!("TF1 is not an ghazal factory partition layout");
            }
        }
        if mbr[494..510].iter().any(|&byte| byte != 0) {
            bail!("unexpected fourth partition");
        }
        Ok(Self { mbr })
    }
    fn grow(mut self, disk_sectors: u64) -> Result<[u8; 512]> {
        let sectors: u32 = disk_sectors
            .checked_sub(u64::from(DATA_START))
            .context("card is too small")?
            .try_into()
            .context("MBR cards must be smaller than 2 TiB")?;
        let old = u32::from_le_bytes(self.mbr[490..494].try_into()?);
        if sectors < old {
            bail!("refusing to shrink the data partition");
        }
        self.mbr[490..494].copy_from_slice(&sectors.to_le_bytes());
        Ok(self.mbr)
    }
}

pub fn prepare() -> Result<()> {
    if !Path::new(FACTORY).exists() {
        return Ok(());
    }
    let mut transaction = FactoryTransaction::read(Path::new(FACTORY))?;
    if crate::sys::is_mounted(Path::new("/mnt/games")) {
        bail!("factory data is already mounted");
    }
    let sectors = fs::read_to_string("/sys/class/block/mmcblk0/size")?
        .trim()
        .parse()?;
    let tools = FilesystemTools {
        formatter: Path::new("/usr/sbin/mkfs.exfat"),
        labeler: Path::new("/usr/sbin/blkid"),
        checker: Path::new("/usr/sbin/fsck.exfat"),
    };
    prepare_partition(
        Path::new("/dev/mmcblk0"),
        Path::new("/dev/mmcblk0p3"),
        sectors,
        &tools,
        &mut transaction,
    )?;
    fs::remove_file(FACTORY)?;
    std::fs::File::open("/var/lib/ghazal")?.sync_all()?;
    Ok(())
}

fn prepare_partition(
    disk_path: &Path,
    data_path: &Path,
    sectors: u64,
    tools: &FilesystemTools<'_>,
    transaction: &mut FactoryTransaction,
) -> Result<()> {
    let mut disk = OpenOptions::new().read(true).write(true).open(disk_path)?;
    let mut mbr = [0; 512];
    disk.read_exact(&mut mbr)?;
    let layout = FactoryLayout::parse(mbr)?;
    let expanded = layout.grow(sectors)?;
    // Refuse to overwrite a filesystem even if a factory marker was restored.
    let mut partition = std::fs::File::open(data_path)?;
    let mut boot = [0; 512];
    partition.read_exact(&mut boot)?;
    drop(partition);
    let already_formatted = if boot[3..11] == *b"EXFAT   " {
        let label = Command::new(tools.labeler)
            .args(["-s", "LABEL", "-o", "value"])
            .arg(data_path)
            .output()?;
        let ours = label.status.success() && String::from_utf8(label.stdout)?.trim() == "GHAZALDATA";
        let valid = ours
            && Command::new(tools.checker)
                .arg("-n")
                .arg(data_path)
                .status()?
                .success();
        if valid && expanded != mbr {
            bail!(
                "existing exFAT filesystem does not fill the current card; refusing to resize it"
            );
        }
        if !valid && transaction.phase == FactoryPhase::Fresh {
            bail!("refusing to alter an existing data filesystem");
        }
        valid
    } else {
        if boot.iter().any(|&byte| byte != 0) && transaction.phase == FactoryPhase::Fresh {
            bail!("factory data contains an unknown filesystem; refusing to format it");
        }
        false
    };
    if !already_formatted {
        disk.seek(SeekFrom::Start(0))?;
        disk.write_all(&expanded)?;
        disk.sync_all()?;
        #[repr(C)]
        struct Partition {
            start: i64,
            length: i64,
            number: i32,
            name: [u8; 64],
            volume: [u8; 64],
        }
        #[repr(C)]
        struct Argument {
            op: i32,
            flags: i32,
            length: i32,
            data: *mut libc::c_void,
        }
        const _: () = assert!(std::mem::size_of::<Partition>() == 152);
        const _: () = assert!(std::mem::size_of::<Argument>() == 24);
        let data_size = u32::from_le_bytes(expanded[490..494].try_into()?);
        let mut partition = Partition {
            start: i64::from(DATA_START) * 512,
            length: i64::from(data_size) * 512,
            number: 3,
            name: [0; 64],
            volume: [0; 64],
        };
        let mut argument = Argument {
            op: 3,
            flags: 0,
            length: std::mem::size_of::<Partition>() as i32,
            data: (&mut partition as *mut Partition).cast(),
        };
        // SAFETY: BLKPG_RESIZE_PARTITION takes these Linux UAPI structures; the
        // descriptor is the validated TF1 disk and partition 3 is unmounted.
        if unsafe { libc::ioctl(disk.as_raw_fd(), 0x1269, &mut argument) } < 0 {
            return Err(std::io::Error::last_os_error())
                .context("updating the TF1 data partition size");
        }
        // Ownership is durable before mkfs can write a partial boot record.
        // Data is never mounted or handed to the user until this transaction ends.
        transaction.begin_format()?;
        let status = Command::new(tools.formatter)
            .args(["-K", "-L", "GHAZALDATA"])
            .arg(data_path)
            .status()?;
        if !status.success() {
            bail!("creating the TF1 data filesystem failed");
        }
        std::fs::File::open(data_path)?.sync_all()?;
    }
    Ok(())
}

pub fn directories() -> Result<()> {
    if !crate::sys::is_mounted(Path::new("/mnt/games")) {
        bail!("data directory is not mounted");
    }
    for name in [
        "ROMS",
        "BIOS",
        "SAVES/file",
        "SAVES/state",
        "SAVES/screenshots",
        ".ghazal",
    ] {
        fs::create_dir_all(Path::new("/mnt/games").join(name))?;
    }
    let frontend = crate::profile::Profile::locate(None).frontend()?;
    for system in frontend.systems {
        if let Some(directory) = system.dirs.first() {
            let directory = Path::new(directory);
            if directory
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
            {
                bail!("invalid ROM folder");
            }
            fs::create_dir_all(Path::new("/mnt/games/ROMS").join(directory))?;
        }
    }
    Ok(())
}

pub fn mount_data() -> Result<()> {
    use std::time::{Duration, Instant};
    let mode = match fs::read_to_string("/var/lib/ghazal/data-card") {
        Ok(mode) => mode,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "auto".into(),
        Err(error) => return Err(error.into()),
    };
    let external = Path::new("/dev/mmcblk1p1");
    let use_external = match mode.trim() {
        "tf1" => false,
        "tf2" | "auto" => {
            // A bounded enumeration window precedes the decision, rather than
            // waiting only after a potentially wrong card has already been selected.
            let deadline = Instant::now() + Duration::from_secs(2);
            while !external.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(25));
            }
            let slot_present =
                fs::read_dir("/sys/class/mmc_host/mmc1")
                    .ok()
                    .is_some_and(|entries| {
                        entries
                            .flatten()
                            .any(|entry| entry.file_name().to_string_lossy().starts_with("mmc1:"))
                    });
            if !external.exists()
                && (mode.trim() == "tf2" || Path::new("/dev/mmcblk1").exists() || slot_present)
            {
                bail!("TF2 is missing its data partition; prepare it or select data_card='tf1'");
            }
            external.exists()
        }
        _ => bail!("invalid data-card preference"),
    };
    let source = if use_external {
        "/dev/mmcblk1p1"
    } else {
        "/dev/mmcblk0p3"
    };
    let mut boot = [0; 512];
    fs::File::open(source)?.read_exact(&mut boot)?;
    if boot[3..11] == *b"EXFAT   " && u16::from_le_bytes(boot[106..108].try_into()?) & 2 != 0 {
        let status = Command::new("/usr/sbin/fsck.exfat")
            .arg("-p")
            .arg(source)
            .status()?;
        if !matches!(status.code(), Some(0 | 1)) {
            bail!("data filesystem needs manual repair; refusing to mount it");
        }
    }
    super::mount(&super::MountStep {
        source: source.into(),
        prefer: None,
        target: "/mnt/games".into(),
        fstype: "exfat,vfat,ext4".into(),
        options: "rw,noatime".into(),
        wait: 3,
        optional: false,
    })?;
    crate::sys::write_atomic(
        Path::new("/run/ghazal/data-source"),
        if use_external { b"TF2" } else { b"TF1" },
    )?;
    eprintln!("ghazal: data mounted from {source}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn factory() -> [u8; 512] {
        let mut mbr = [0; 512];
        mbr[510..].copy_from_slice(&[0x55, 0xaa]);
        for (index, start, size, kind) in [
            (0, 32768u32, 262144u32, 0x0c),
            (1, 294912, 2097152, 0x83),
            (2, DATA_START, 262144, 0x07),
        ] {
            let entry = &mut mbr[446 + index * 16..][..16];
            entry[4] = kind;
            entry[8..12].copy_from_slice(&start.to_le_bytes());
            entry[12..16].copy_from_slice(&size.to_le_bytes());
        }
        mbr
    }
    #[test]
    fn only_factory_data_can_grow() {
        let original = factory();
        let grown = FactoryLayout::parse(original)
            .unwrap()
            .grow(32_000_000)
            .unwrap();
        assert_eq!(&grown[..490], &original[..490]);
        assert_eq!(&grown[494..], &original[494..]);
        assert_eq!(
            u32::from_le_bytes(grown[490..494].try_into().unwrap()),
            32_000_000 - DATA_START
        );
        let mut wrong = original;
        wrong[482] = 0x83;
        assert!(FactoryLayout::parse(wrong).is_err());
        assert!(FactoryLayout::parse(original).unwrap().grow(1).is_err());
        assert!(FactoryLayout::parse(original)
            .unwrap()
            .grow(u64::MAX)
            .is_err());
    }

    #[test]
    #[ignore = "requires root, losetup, mount and exfatprogs; CI runs this on a disposable loop device"]
    fn factory_loopback_preparation_preserves_existing_data() -> Result<()> {
        struct Fixture {
            directory: std::path::PathBuf,
            disk: std::path::PathBuf,
            mount: std::path::PathBuf,
            system_mounts: Vec<std::path::PathBuf>,
        }
        impl Drop for Fixture {
            fn drop(&mut self) {
                if crate::sys::is_mounted(&self.mount) {
                    let _ = Command::new("umount").arg(&self.mount).status();
                }
                for mount in &self.system_mounts {
                    if crate::sys::is_mounted(mount) {
                        let _ = Command::new("umount").arg(mount).status();
                    }
                }
                let _ = Command::new("losetup").arg("-d").arg(&self.disk).status();
                let _ = fs::remove_dir_all(&self.directory);
            }
        }
        let directory =
            std::env::temp_dir().join(format!("ghazal-factory-test-{}", std::process::id()));
        fs::create_dir(&directory)?;
        let image = directory.join("disk.img");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&image)?;
        let disk_size = 2u64 * 1024 * 1024 * 1024;
        file.set_len(disk_size)?;
        let original = factory();
        file.write_all(&original)?;
        file.seek(SeekFrom::Start(16 * 1024 * 1024))?;
        file.write_all(b"BOOT PRESERVED")?;
        file.seek(SeekFrom::Start(144 * 1024 * 1024))?;
        file.write_all(b"ROOT PRESERVED")?;
        file.sync_all()?;
        let attached = Command::new("losetup")
            .args(["--find", "--show", "--partscan"])
            .arg(&image)
            .output()?;
        if !attached.status.success() {
            bail!(
                "loop device creation failed: {}",
                String::from_utf8_lossy(&attached.stderr)
            );
        }
        let disk = std::path::PathBuf::from(String::from_utf8(attached.stdout)?.trim());
        let mount = directory.join("mounted");
        fs::create_dir(&mount)?;
        let mut fixture = Fixture {
            directory,
            disk,
            mount,
            system_mounts: Vec::new(),
        };
        let data = std::path::PathBuf::from(format!("{}p3", fixture.disk.display()));
        let formatter = std::env::var("GHAZAL_TEST_MKFS_EXFAT")
            .unwrap_or_else(|_| "/usr/sbin/mkfs.exfat".into());
        let checker = std::env::var("GHAZAL_TEST_FSCK_EXFAT")
            .unwrap_or_else(|_| "/usr/sbin/fsck.exfat".into());
        let labeler =
            std::env::var("GHAZAL_TEST_BLKID").unwrap_or_else(|_| "/usr/sbin/blkid".into());
        let mounter = std::env::var("GHAZAL_TEST_MOUNT_EXFAT").ok();
        let mount_data = |data: &Path, target: &Path| -> Result<()> {
            let mut command = if let Some(mounter) = &mounter {
                Command::new(mounter)
            } else {
                let mut command = Command::new("mount");
                command.args(["-t", "exfat"]);
                command
            };
            if !command.arg(data).arg(target).status()?.success() {
                bail!("test data mount failed");
            }
            Ok(())
        };
        let system_formatter =
            std::env::var("GHAZAL_TEST_MKFS_EXT4").unwrap_or_else(|_| "/usr/sbin/mkfs.ext4".into());
        for number in [1, 2] {
            let partition = format!("{}p{number}", fixture.disk.display());
            assert!(Command::new(&system_formatter)
                .args(["-q", "-F", &partition])
                .status()?
                .success());
            let mount = fixture.directory.join(format!("system{number}"));
            fs::create_dir(&mount)?;
            assert!(Command::new("mount")
                .args(["-t", "ext4"])
                .arg(&partition)
                .arg(&mount)
                .status()?
                .success());
            fs::write(
                mount.join("preserved"),
                format!("PARTITION {number} PRESERVED"),
            )?;
            fixture.system_mounts.push(mount);
        }
        let marker = fixture.system_mounts[1].join("factory-data");
        fs::write(&marker, "ghazal-factory-v1\n")?;
        let mut transaction = FactoryTransaction::read(&marker)?;
        let rustc = std::env::var("GHAZAL_TEST_RUSTC").unwrap_or_else(|_| "rustc".into());
        let helper_source = fixture.directory.join("interrupted-formatter.rs");
        fs::write(
            &helper_source,
            r#"
use std::{io::{Write,Seek,SeekFrom},fs::OpenOptions};
extern "C" { fn kill(pid: i32, signal: i32) -> i32; }
fn main() {
    let path = std::env::args().last().unwrap();
    let mut file = OpenOptions::new().write(true).open(path).unwrap();
    let mut sector = [0u8;512]; sector[3..11].copy_from_slice(b"EXFAT   ");
    file.seek(SeekFrom::Start(0)).unwrap(); file.write_all(&sector).unwrap(); file.sync_all().unwrap();
    unsafe { kill(std::process::id() as i32, 9); }
}
"#,
        )?;
        let helper = fixture.directory.join("interrupted-formatter");
        assert!(Command::new(rustc)
            .arg(&helper_source)
            .arg("-o")
            .arg(&helper)
            .status()?
            .success());
        let interrupted = FilesystemTools {
            formatter: &helper,
            labeler: Path::new(&labeler),
            checker: Path::new(&checker),
        };
        assert!(prepare_partition(
            &fixture.disk,
            &data,
            disk_size / 512,
            &interrupted,
            &mut transaction
        )
        .is_err());
        assert!(FactoryTransaction::read(&marker)?.phase == FactoryPhase::Formatting);
        let mut transaction = FactoryTransaction::read(&marker)?;
        let tools = FilesystemTools {
            formatter: Path::new(&formatter),
            labeler: Path::new(&labeler),
            checker: Path::new(&checker),
        };
        prepare_partition(
            &fixture.disk,
            &data,
            disk_size / 512,
            &tools,
            &mut transaction,
        )?;
        for (index, mount) in fixture.system_mounts.iter().enumerate() {
            assert_eq!(
                fs::read_to_string(mount.join("preserved"))?,
                format!("PARTITION {} PRESERVED", index + 1)
            );
        }
        mount_data(&data, &fixture.mount)?;
        fs::write(fixture.mount.join("save.srm"), b"USER SAVE")?;
        assert!(Command::new("umount")
            .arg(&fixture.mount)
            .status()?
            .success());
        // Simulate power loss after formatting but before removing the factory marker.
        prepare_partition(
            &fixture.disk,
            &data,
            disk_size / 512,
            &tools,
            &mut transaction,
        )?;
        mount_data(&data, &fixture.mount)?;
        assert_eq!(fs::read(fixture.mount.join("save.srm"))?, b"USER SAVE");
        assert!(Command::new("umount")
            .arg(&fixture.mount)
            .status()?
            .success());
        // Unknown filesystems must be rejected without changing their bytes.
        let mut partition = OpenOptions::new().read(true).write(true).open(&data)?;
        let mut changed = [0; 512];
        partition.read_exact(&mut changed)?;
        changed[3] = b'X';
        partition.seek(SeekFrom::Start(0))?;
        partition.write_all(&changed)?;
        partition.sync_all()?;
        drop(partition);
        // Once factory ownership is fresh, it cannot claim arbitrary existing data.
        fs::write(&marker, "ghazal-factory-v1\n")?;
        let mut transaction = FactoryTransaction::read(&marker)?;
        assert!(prepare_partition(
            &fixture.disk,
            &data,
            disk_size / 512,
            &tools,
            &mut transaction
        )
        .is_err());
        let mut actual = [0; 512];
        fs::File::open(&data)?.read_exact(&mut actual)?;
        assert_eq!(actual, changed);
        Ok(())
    }
}
