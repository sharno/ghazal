//! Small independent-system helpers, invoked by the service supervisor and iwd.
use std::io::Read;
use std::path::Path;

use anyhow::{bail, Result};

/// Read complete kernel records, including the boot backlog, without depending
/// on optional BusyBox dmesg features. The supervisor persists stdout.
pub fn kernel_log() -> Result<()> {
    use std::io::Write;
    let mut source = std::fs::File::open("/dev/kmsg")?;
    let mut output = std::io::stdout().lock();
    let mut record = [0u8; 65536];
    loop {
        match source.read(&mut record) {
            Ok(0) => bail!("kernel log closed"),
            Ok(size) => {
                output.write_all(&record[..size])?;
                output.flush()?;
                if record[..size].windows(4).any(|bytes| bytes == b"PM: ") {
                    let log = std::fs::File::open("/proc/self/fd/1")?;
                    if log.metadata()?.is_file() { log.sync_all()?; }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted || error.raw_os_error() == Some(libc::EPIPE) => continue,
            Err(error) => return Err(error.into()),
        }
    }
}

pub fn ssh_key() -> Result<()> {
    let key = Path::new("/etc/ssh/ssh_host_ed25519_key");
    if key.is_file() {
        return Ok(());
    }
    let status = std::process::Command::new("/usr/bin/ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(key)
        .status()?;
    if !status.success() { bail!("SSH host key generation failed: {status}"); }
    Ok(())
}

/// iwd's resolvconf backend passes DNS settings on stdin with `-a INTERFACE`.
pub fn resolvconf() -> Result<()> {
    if std::env::args().any(|arg| arg == "-a") {
        let mut input = String::new();
        std::io::stdin().read_to_string(&mut input)?;
        if input.len() > 65536 { bail!("resolver configuration too large"); }
        for line in input.lines() {
            if !(line.starts_with("nameserver ") || line.starts_with("search ") || line.starts_with('#') || line.is_empty()) {
                bail!("unexpected resolver directive");
            }
        }
        std::fs::write("/etc/resolv.conf.new", input)?;
        std::fs::rename("/etc/resolv.conf.new", "/etc/resolv.conf")?;
    }
    Ok(())
}
