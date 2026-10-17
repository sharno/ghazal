//! Optional first-boot provisioning from the Windows-readable boot partition.
use anyhow::{bail, Result};
use serde::Deserialize;
use std::{fs, io::Read, path::Path};

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Provision {
    wifi: Option<Wifi>,
    data_card: Option<DataCard>,
    #[serde(default)]
    ssh_authorized_keys: Vec<String>,
}
#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum DataCard {
    Auto,
    Tf1,
    Tf2,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wifi {
    ssid: String,
    password: String,
}
fn validate(config: &Provision) -> Result<()> {
    if let Some(wifi) = &config.wifi {
        if wifi.ssid.is_empty()
            || wifi.ssid.len() > 32
            || wifi.ssid.contains(['\n', '\r', '\0'])
            || !(8..=63).contains(&wifi.password.len())
            || wifi.password.contains(['\n', '\r', '\0'])
        {
            bail!("invalid Wi-Fi provisioning values");
        }
    }
    for key in &config.ssh_authorized_keys {
        let fields: Vec<_> = key.split_whitespace().collect();
        if key.contains(['\n', '\r', '\0'])
            || fields.len() < 2
            || !matches!(
                fields[0],
                "ssh-ed25519"
                    | "ssh-rsa"
                    | "ecdsa-sha2-nistp256"
                    | "ecdsa-sha2-nistp384"
                    | "ecdsa-sha2-nistp521"
                    | "sk-ssh-ed25519@openssh.com"
                    | "sk-ecdsa-sha2-nistp256@openssh.com"
            )
            || !fields[1]
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"+/=".contains(&byte))
        {
            bail!("provisioning requires complete public SSH keys");
        }
    }
    Ok(())
}
pub fn apply(path: &Path) -> Result<()> {
    if fs::read_to_string("/etc/machine-id")?.trim().is_empty() {
        let mut identity = [0; 16];
        fs::File::open("/dev/urandom")?.read_exact(&mut identity)?;
        let text = identity
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        crate::sys::write_atomic(Path::new("/var/lib/ghazal/machine-id"), format!("{text}\n").as_bytes())?;
    }
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let parsed = (|| -> Result<Provision> {
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| anyhow::anyhow!("Save provision.toml as UTF-8 text."))?;
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        // TOML's diagnostic includes source lines; those can contain the password.
        let config: Provision = toml::from_str(text).map_err(|_| {
            anyhow::anyhow!("Invalid TOML; check formatting against provision.toml.example.")
        })?;
        validate(&config)?;
        Ok(config)
    })();
    let config = match parsed {
        Ok(config) => config,
        Err(error) => return reject(path, &error.to_string()),
    };
    use std::os::unix::fs::PermissionsExt;
    if let Some(card) = config.data_card {
        crate::sys::write_atomic(
            Path::new("/var/lib/ghazal/data-card"),
            match card {
                DataCard::Auto => b"auto\n",
                DataCard::Tf1 => b"tf1\n",
                DataCard::Tf2 => b"tf2\n",
            },
        )?;
    }
    if let Some(wifi) = config.wifi {
        let name = format!(
            "={}",
            wifi.ssid
                .bytes()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        let destination = Path::new("/var/lib/iwd").join(format!("{name}.psk"));
        crate::sys::write_atomic(
            &destination,
            format!(
                "[Security]\nPassphrase={}\n[Settings]\nAutoConnect=true\n",
                wifi.password
            )
            .as_bytes(),
        )?;
        fs::set_permissions(destination, fs::Permissions::from_mode(0o600))?;
    }
    if !config.ssh_authorized_keys.is_empty() {
        let destination = Path::new("/var/lib/ghazal/ssh/authorized_keys");
        crate::sys::write_atomic(
            destination,
            format!("{}\n", config.ssh_authorized_keys.join("\n")).as_bytes(),
        )?;
        fs::set_permissions(destination, fs::Permissions::from_mode(0o600))?;
    }
    fs::remove_file(path)?;
    fs::File::open(
        path.parent()
            .ok_or_else(|| anyhow::anyhow!("provisioning has no parent"))?,
    )?
    .sync_all()?;
    eprintln!("ghazal: first-boot network and SSH provisioning applied");
    Ok(())
}

fn reject(path: &Path, reason: &str) -> Result<()> {
    let directory = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("provisioning has no parent"))?;
    let rejected = (0..100)
        .map(|index| directory.join(format!("provision.rejected-{index}.toml")))
        .find(|candidate| !candidate.exists())
        .ok_or_else(|| anyhow::anyhow!("too many rejected provisioning files"))?;
    fs::rename(path, rejected)?;
    crate::sys::write_atomic(
        &directory.join("provision.error.txt"),
        format!(
            "{reason}\nCorrect provision.toml and reboot; rejected input has been preserved.\n"
        )
        .as_bytes(),
    )?;
    crate::sys::write_atomic(
        Path::new("/run/ghazal/provision-warning"),
        b"Provisioning rejected; see GHAZALBOOT/provision.error.txt",
    )?;
    eprintln!("ghazal: provisioning rejected; continuing offline: {reason}");
    Ok(())
}

pub fn retry_boot(request: &Path) -> Result<()> {
    if !request.exists() {
        return Ok(());
    }
    crate::sys::write_atomic(Path::new("/var/lib/ghazal/boot-attempts"), b"0\n")?;
    fs::remove_file(request)?;
    let directory = request
        .parent()
        .ok_or_else(|| anyhow::anyhow!("boot retry has no parent"))?;
    fs::File::open(directory)?.sync_all()?;
    crate::sys::sync();
    crate::sys::umount(directory)?;
    crate::sys::mount(
        "none",
        Path::new("/"),
        "none",
        libc::MS_REMOUNT | libc::MS_RDONLY,
        "",
    )?;
    eprintln!("ghazal: boot counter reset by the boot-partition retry request");
    crate::sys::reboot(libc::RB_AUTOBOOT)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn provisioning_rejects_unknown_fields_and_ini_injection() {
        assert!(toml::from_str::<Provision>("password='nope'").is_err());
        assert!(validate(&Provision {
            wifi: Some(Wifi {
                ssid: "test".into(),
                password: "password\nfoo=bar".into()
            }),
            ..Default::default()
        })
        .is_err());
        assert!(validate(&Provision {
            ssh_authorized_keys: vec!["command=unsafe ssh-ed25519 AAAA".into()],
            ..Default::default()
        })
        .is_err());
        assert!(validate(&Provision {
            ssh_authorized_keys: vec!["ssh-ed25519 AAAA tester".into()],
            ..Default::default()
        })
        .is_ok());
    }
}
