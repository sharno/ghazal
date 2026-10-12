//! First-boot preparation touches only the image's reserved TF1 data partition.
//! Existing TF2 filesystems are mounted by the boot plan and are never formatted.
use std::{fs::{self, OpenOptions}, io::{Read, Seek, SeekFrom, Write}, os::fd::AsRawFd, path::Path, process::Command};
use anyhow::{bail, Context, Result};
use serde::Deserialize;

const DATA_START: u32 = 2_392_064;
const FACTORY: &str = "/var/lib/oxmux/factory-data";
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Prepare {}

/// The validated layout owns its MBR, so a resize cannot select another partition.
struct FactoryLayout { mbr: [u8; 512] }
impl FactoryLayout {
    fn parse(mbr: [u8; 512]) -> Result<Self> {
        if mbr[510..] != [0x55, 0xaa] { bail!("missing MBR signature"); }
        for (index, start, size, kind) in [(0, 32768, Some(262144), 0x0c), (1, 294912, Some(2097152), 0x83), (2, DATA_START, None, 0x07)] {
            let entry = &mbr[446 + index * 16..][..16];
            let actual_start = u32::from_le_bytes(entry[8..12].try_into()?);
            let actual_size = u32::from_le_bytes(entry[12..16].try_into()?);
            if entry[4] != kind || actual_start != start || actual_size == 0 || size.is_some_and(|size| size != actual_size) {
                bail!("TF1 is not an oxmux factory partition layout");
            }
        }
        if mbr[494..510].iter().any(|&byte| byte != 0) { bail!("unexpected fourth partition"); }
        Ok(Self { mbr })
    }
    fn grow(mut self, disk_sectors: u64) -> Result<[u8; 512]> {
        let sectors: u32 = disk_sectors.checked_sub(u64::from(DATA_START)).context("card is too small")?.try_into().context("MBR cards must be smaller than 2 TiB")?;
        let old = u32::from_le_bytes(self.mbr[490..494].try_into()?);
        if sectors < old { bail!("refusing to shrink the data partition"); }
        self.mbr[490..494].copy_from_slice(&sectors.to_le_bytes());
        Ok(self.mbr)
    }
}

pub fn prepare() -> Result<()> {
    if !Path::new(FACTORY).exists() { return Ok(()); }
    if fs::read_to_string(FACTORY)?.trim() != "oxmux-factory-v1" { bail!("unknown factory storage marker"); }
    if crate::sys::is_mounted(Path::new("/mnt/games")) { bail!("factory data is already mounted"); }
    let mut disk = OpenOptions::new().read(true).write(true).open("/dev/mmcblk0")?;
    let mut mbr = [0; 512];
    disk.read_exact(&mut mbr)?;
    let layout = FactoryLayout::parse(mbr)?;
    let sectors = fs::read_to_string("/sys/class/block/mmcblk0/size")?.trim().parse()?;
    let expanded = layout.grow(sectors)?;
    // Refuse to overwrite a filesystem even if a factory marker was restored.
    let mut partition = std::fs::File::open("/dev/mmcblk0p3")?;
    let mut boot = [0; 512];
    partition.read_exact(&mut boot)?;
    let already_formatted = boot[3..11] == *b"EXFAT   ";
    if already_formatted {
        let label = Command::new("/usr/sbin/blkid").args(["-s", "LABEL", "-o", "value", "/dev/mmcblk0p3"]).output()?;
        if !label.status.success() || String::from_utf8(label.stdout)?.trim() != "OXMUXDATA" {
            bail!("refusing to alter an existing data filesystem");
        }
        // The factory transaction formatted the expanded partition on an earlier boot.
        if expanded != mbr { bail!("existing exFAT filesystem does not fill the current card; refusing to resize it"); }
        if !Command::new("/usr/sbin/fsck.exfat").args(["-n", "/dev/mmcblk0p3"]).status()?.success() {
            bail!("factory exFAT filesystem is incomplete; repair it before retrying");
        }
    } else if boot.iter().any(|&byte| byte != 0) {
        bail!("factory data contains an unknown filesystem; refusing to format it");
    }
    if !already_formatted {
        disk.seek(SeekFrom::Start(0))?;
        disk.write_all(&expanded)?;
        disk.sync_all()?;
        #[repr(C)]
        struct Partition { start: i64, length: i64, number: i32, name: [u8; 64], volume: [u8; 64] }
        #[repr(C)]
        struct Argument { op: i32, flags: i32, length: i32, data: *mut libc::c_void }
        let data_size = u32::from_le_bytes(expanded[490..494].try_into()?);
        let mut partition = Partition { start: i64::from(DATA_START) * 512, length: i64::from(data_size) * 512, number: 3, name: [0; 64], volume: [0; 64] };
        let mut argument = Argument { op: 3, flags: 0, length: std::mem::size_of::<Partition>() as i32, data: (&mut partition as *mut Partition).cast() };
        // SAFETY: BLKPG_RESIZE_PARTITION takes these Linux UAPI structures; the
        // descriptor is the validated TF1 disk and partition 3 is unmounted.
        if unsafe { libc::ioctl(disk.as_raw_fd(), 0x1269, &mut argument) } < 0 {
            return Err(std::io::Error::last_os_error()).context("updating the TF1 data partition size");
        }
        let status = Command::new("/usr/sbin/mkfs.exfat").args(["-L", "OXMUXDATA", "/dev/mmcblk0p3"]).status()?;
        if !status.success() { bail!("creating the TF1 data filesystem failed"); }
    }
    fs::remove_file(FACTORY)?;
    std::fs::File::open("/var/lib/oxmux")?.sync_all()?;
    Ok(())
}

pub fn directories() -> Result<()> {
    if !crate::sys::is_mounted(Path::new("/mnt/games")) { bail!("data directory is not mounted"); }
    for name in ["ROMS", "BIOS", "SAVES/file", "SAVES/state", "SAVES/screenshots", ".oxmux"] {
        fs::create_dir_all(Path::new("/mnt/games").join(name))?;
    }
    let frontend = crate::profile::Profile::locate(None).frontend()?;
    for system in frontend.systems {
        if let Some(directory) = system.dirs.first() {
            let directory = Path::new(directory);
            if directory.components().any(|part| !matches!(part, std::path::Component::Normal(_))) { bail!("invalid ROM folder"); }
            fs::create_dir_all(Path::new("/mnt/games/ROMS").join(directory))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn factory() -> [u8; 512] {
        let mut mbr = [0; 512]; mbr[510..].copy_from_slice(&[0x55, 0xaa]);
        for (index, start, size, kind) in [(0, 32768u32, 262144u32, 0x0c), (1, 294912, 2097152, 0x83), (2, DATA_START, 262144, 0x07)] {
            let entry = &mut mbr[446 + index * 16..][..16]; entry[4] = kind;
            entry[8..12].copy_from_slice(&start.to_le_bytes()); entry[12..16].copy_from_slice(&size.to_le_bytes());
        } mbr
    }
    #[test] fn only_factory_data_can_grow() {
        let original = factory();
        let grown = FactoryLayout::parse(original).unwrap().grow(32_000_000).unwrap();
        assert_eq!(&grown[..490], &original[..490]); assert_eq!(&grown[494..], &original[494..]);
        assert_eq!(u32::from_le_bytes(grown[490..494].try_into().unwrap()), 32_000_000 - DATA_START);
        let mut wrong = original; wrong[482] = 0x83; assert!(FactoryLayout::parse(wrong).is_err());
        assert!(FactoryLayout::parse(original).unwrap().grow(1).is_err());
        assert!(FactoryLayout::parse(original).unwrap().grow(u64::MAX).is_err());
    }
}
