//! Compressed swap in RAM (replaces muOS script/system/swap.sh + zramctl/mkswap/swapon).

use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::sys;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ZramStep {
    #[serde(default = "default_device")]
    pub device: String,
    pub size_mb: u64,
    #[serde(default = "default_algorithm")]
    pub algorithm: String,
    #[serde(default = "default_priority")]
    pub priority: i32,
}

fn default_device() -> String {
    "zram0".into()
}

fn default_algorithm() -> String {
    "lz4".into()
}

fn default_priority() -> i32 {
    100
}

pub fn setup(z: &ZramStep) -> Result<()> {
    let sysfs = PathBuf::from("/sys/block").join(&z.device);
    if !sysfs.exists() {
        super::modules::load("zram", "")?;
    }
    let dev = PathBuf::from("/dev").join(&z.device);
    // Only configurable while unused; skip if a previous boot stage already set it up.
    if std::fs::read_to_string(sysfs.join("disksize")).is_ok_and(|s| s.trim() != "0") {
        return Ok(());
    }
    // Not every kernel offers every algorithm; fall back to the default one.
    if let Err(e) = sys::write_file(&sysfs.join("comp_algorithm"), &z.algorithm) {
        eprintln!("oxmux-init: zram {}: {e:#}", z.algorithm);
    }
    let bytes = z.size_mb * 1024 * 1024;
    sys::write_file(&sysfs.join("disksize"), &bytes.to_string())?;
    mkswap(&dev, bytes)?;
    sys::swapon(&dev, z.priority)
}

/// Writes a Linux swap v1 header (what mkswap does).
fn mkswap(dev: &std::path::Path, bytes: u64) -> Result<()> {
    let page = 4096usize;
    let pages = bytes / page as u64;
    let mut header = vec![0u8; page];
    // struct swap_header_v1_2 starts at offset 1024: version, last_page, nr_badpages.
    header[1024..1028].copy_from_slice(&1u32.to_ne_bytes());
    header[1028..1032].copy_from_slice(&((pages - 1) as u32).to_ne_bytes());
    header[page - 10..].copy_from_slice(b"SWAPSPACE2");
    let mut f = std::fs::OpenOptions::new().write(true).open(dev).with_context(|| format!("opening {}", dev.display()))?;
    f.write_all(&header)?;
    f.sync_all()?;
    Ok(())
}
