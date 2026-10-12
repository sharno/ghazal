//! Optional first-boot provisioning from the Windows-readable boot partition.
use std::{fs, io::Read, path::Path};
use anyhow::{bail, Result};
use serde::Deserialize;

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Provision {
    wifi: Option<Wifi>,
    #[serde(default)] ssh_authorized_keys: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wifi { ssid: String, password: String }
fn validate(config: &Provision) -> Result<()> {
    if let Some(wifi) = &config.wifi {
        if wifi.ssid.is_empty() || wifi.ssid.len() > 32 || wifi.ssid.contains(['\n', '\r', '\0'])
            || !(8..=63).contains(&wifi.password.len()) || wifi.password.contains(['\n', '\r', '\0']) {
            bail!("invalid Wi-Fi provisioning values");
        }
    }
    for key in &config.ssh_authorized_keys {
        let fields: Vec<_> = key.split_whitespace().collect();
        if key.contains(['\n', '\r', '\0']) || fields.len() < 2
            || !matches!(fields[0], "ssh-ed25519" | "ssh-rsa" | "ecdsa-sha2-nistp256" | "ecdsa-sha2-nistp384" | "ecdsa-sha2-nistp521" | "sk-ssh-ed25519@openssh.com" | "sk-ecdsa-sha2-nistp256@openssh.com")
            || !fields[1].bytes().all(|byte| byte.is_ascii_alphanumeric() || b"+/=".contains(&byte)) {
            bail!("provisioning requires complete public SSH keys");
        }
    }
    Ok(())
}
pub fn apply(path: &Path) -> Result<()> {
    if fs::read_to_string("/etc/machine-id")?.trim().is_empty() {
        let mut identity = [0; 16];
        fs::File::open("/dev/urandom")?.read_exact(&mut identity)?;
        let text = identity.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        crate::sys::write_atomic(Path::new("/etc/machine-id"), format!("{text}\n").as_bytes())?;
    }
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    // TOML's diagnostic includes source lines; those can contain the password.
    let config: Provision = toml::from_str(&text).map_err(|_| anyhow::anyhow!("invalid provision.toml; check its formatting against the example"))?;
    validate(&config)?;
    use std::os::unix::fs::PermissionsExt;
    if let Some(wifi) = config.wifi {
        let name = format!("={}", wifi.ssid.bytes().map(|byte| format!("{byte:02x}")).collect::<String>());
        let destination = Path::new("/var/lib/iwd").join(format!("{name}.psk"));
        crate::sys::write_atomic(&destination, format!("[Security]\nPassphrase={}\n[Settings]\nAutoConnect=true\n", wifi.password).as_bytes())?;
        fs::set_permissions(destination, fs::Permissions::from_mode(0o600))?;
    }
    if !config.ssh_authorized_keys.is_empty() {
        let destination = Path::new("/root/.ssh/authorized_keys");
        crate::sys::write_atomic(destination, format!("{}\n", config.ssh_authorized_keys.join("\n")).as_bytes())?;
        fs::set_permissions(destination, fs::Permissions::from_mode(0o600))?;
    }
    fs::remove_file(path)?;
    fs::File::open(path.parent().ok_or_else(|| anyhow::anyhow!("provisioning has no parent"))?)?.sync_all()?;
    eprintln!("oxmux: first-boot network and SSH provisioning applied");
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn provisioning_rejects_unknown_fields_and_ini_injection() {
        assert!(toml::from_str::<Provision>("password='nope'").is_err());
        assert!(validate(&Provision { wifi: Some(Wifi { ssid: "test".into(), password: "password\nfoo=bar".into() }), ..Default::default() }).is_err());
        assert!(validate(&Provision { ssh_authorized_keys: vec!["command=unsafe ssh-ed25519 AAAA".into()], ..Default::default() }).is_err());
        assert!(validate(&Provision { ssh_authorized_keys: vec!["ssh-ed25519 AAAA tester".into()], ..Default::default() }).is_ok());
    }
}
