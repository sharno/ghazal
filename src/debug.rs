//! Small independent-system helpers, invoked by the service supervisor and iwd.
use std::io::Read;
use std::path::Path;

use anyhow::{bail, Result};

/// Read complete kernel records, including the boot backlog, without depending
/// on optional BusyBox dmesg features. The supervisor persists stdout.
pub fn kernel_log() -> Result<()> {
    use std::io::Write;
    let mut source = std::fs::File::open("/dev/kmsg")?;
    std::fs::create_dir_all("/run/ghazal")?;
    match std::fs::OpenOptions::new().write(true).create_new(true).open("/run/ghazal/kernel-log.started") {
        Ok(_) => {},
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            use std::io::Seek;
            source.seek(std::io::SeekFrom::End(0))?;
        }
        Err(error) => return Err(error.into()),
    }
    let mut output = std::io::stdout().lock();
    let mut record = [0u8; 65536];
    loop {
        match source.read(&mut record) {
            Ok(0) => bail!("kernel log closed"),
            Ok(size) => {
                output.write_all(&record[..size])?;
                output.flush()?;
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

/// Records iwd has set, one file per name, combined into /etc/resolv.conf.
const RESOLVCONF_RECORDS: &str = "/run/ghazal/resolvconf";

/// iwd's resolvconf backend: `-a NAME` sets a record from stdin, `-d NAME`
/// removes it. iwd sends nameservers and search domains as separate records
/// (`wlan0.dns`, `wlan0.domain`), so each replaces only its own record.
pub fn resolvconf() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let records = Path::new(RESOLVCONF_RECORDS);
    std::fs::create_dir_all(records)?;
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["-a", name, ..] => {
            let mut input = String::new();
            std::io::stdin().take(65537).read_to_string(&mut input)?;
            if input.len() > 65536 { bail!("resolver configuration too large"); }
            if input.lines().any(|line| !(line.starts_with("nameserver ") || line.starts_with("search ") || line.starts_with('#') || line.is_empty())) {
                bail!("unexpected resolver directive");
            }
            crate::sys::write_atomic(&records.join(record_name(name)?), input.as_bytes())?;
        }
        ["-d", name, ..] => match std::fs::remove_file(records.join(record_name(name)?)) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
            _ => {}
        },
        _ => return Ok(()),
    }
    let mut entries: Vec<_> = std::fs::read_dir(records)?.collect::<std::io::Result<_>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    let contents = entries.iter().map(|entry| std::fs::read_to_string(entry.path())).collect::<std::io::Result<Vec<_>>>()?;
    crate::sys::write_atomic(Path::new("/etc/resolv.conf"), resolv_conf(&contents).as_bytes())
}

fn record_name(name: &str) -> Result<&str> {
    if name.is_empty() || name.contains('/') || name.starts_with('.') { bail!("invalid resolvconf record {name:?}"); }
    Ok(name)
}

/// Nameservers first, then one combined search line, without repeats.
fn resolv_conf(records: &[String]) -> String {
    fn unique<'a>(items: impl Iterator<Item = &'a str>) -> Vec<&'a str> {
        let mut seen = Vec::new();
        for item in items { if !seen.contains(&item) { seen.push(item); } }
        seen
    }
    let lines = || records.iter().flat_map(|record| record.lines());
    let mut out: String = unique(lines().filter(|l| l.starts_with("nameserver "))).iter().map(|l| format!("{l}\n")).collect();
    let search = unique(lines().filter_map(|l| l.strip_prefix("search ")).flat_map(str::split_whitespace));
    if !search.is_empty() { out.push_str(&format!("search {}\n", search.join(" "))); }
    out
}

#[cfg(test)]
mod tests {
    use super::resolv_conf;

    #[test]
    fn nameservers_survive_a_separate_search_record() {
        let records = ["nameserver 192.0.2.1\nnameserver 192.0.2.1\n".to_owned(), "search example.test\nsearch example.test\n".to_owned()];
        assert_eq!(resolv_conf(&records), "nameserver 192.0.2.1\nsearch example.test\n");
    }
}
