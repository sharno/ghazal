//! Bringing the radio and its daemon into existence, and remembering whether the
//! user wants Bluetooth on.
//!
//! Nothing Bluetooth runs until it is asked for. The kernel leaves the UART
//! controller unpowered (`nix/hci-h5-start-attribute.patch`) and the supervisor
//! holds `bluetoothd` back until a marker file exists, so a user who never turns
//! Bluetooth on pays for neither the boot time nor the battery.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// `bluetoothd` waits for this (`wait_for` in system.toml).
pub const MARKER: &str = "/run/ghazal/bluetooth-requested";
/// Bound serdev devices of the three-wire UART driver, each with a `start` attribute.
const DRIVER: &str = "/sys/bus/serial/drivers/hci_uart_h5";
/// Registered HCI controllers.
const CLASS: &str = "/sys/class/bluetooth";
const PREFERENCE: &str = "/var/lib/ghazal/bluetooth.toml";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Controller {
    /// This board or kernel has no Bluetooth controller.
    Absent,
    /// A controller whose `start` attribute has not been written yet.
    Dormant(Vec<PathBuf>),
    /// A controller is registered with the kernel.
    Registered,
}

/// What `root`-relative sysfs directories say about the controller.
fn controller_in(driver: &Path, class: &Path) -> Controller {
    if std::fs::read_dir(class).is_ok_and(|entries| entries.flatten().any(|entry| entry.file_name().to_string_lossy().starts_with("hci"))) {
        return Controller::Registered;
    }
    let attributes: Vec<PathBuf> = std::fs::read_dir(driver)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path().join("start"))
        .filter(|path| path.exists())
        .collect();
    if attributes.is_empty() { Controller::Absent } else { Controller::Dormant(attributes) }
}

pub fn controller() -> Controller {
    controller_in(Path::new(DRIVER), Path::new(CLASS))
}

/// Whether a device has a link: the kernel lists each connection as
/// `hci0:<handle>` next to its controller.
pub fn linked() -> bool {
    std::fs::read_dir(CLASS).is_ok_and(|entries| entries.flatten().any(|entry| entry.file_name().to_string_lossy().contains(':')))
}

/// Whether this device can do Bluetooth at all.
pub fn present() -> bool {
    controller() != Controller::Absent
}

/// Powers the controller and releases `bluetoothd`. Safe to repeat.
pub fn start() -> Result<()> {
    let attributes = match controller() {
        Controller::Absent => bail!("this device has no Bluetooth controller"),
        Controller::Registered => Vec::new(),
        Controller::Dormant(attributes) => attributes,
    };
    crate::sys::write_atomic(Path::new(MARKER), b"1").context("asking the supervisor to start bluetoothd")?;
    for attribute in attributes {
        // The kernel sleeps while the controller wakes up, so this takes about a second.
        std::fs::write(&attribute, "1").with_context(|| format!("starting the controller through {}", attribute.display()))?;
    }
    Ok(())
}

/// Kept on the state partition: whether Bluetooth was on when the device last ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Preference {
    pub enabled: bool,
}

impl Preference {
    pub fn load() -> Self {
        Self::load_from(Path::new(PREFERENCE))
    }

    fn load_from(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|error| {
                eprintln!("ghazal: {}: {error}; Bluetooth stays off", path.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(self) -> Result<()> {
        self.save_to(Path::new(PREFERENCE))
    }

    fn save_to(self, path: &Path) -> Result<()> {
        crate::sys::write_atomic(path, toml::to_string(&self)?.as_bytes()).with_context(|| format!("saving {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("ghazal-bluetooth-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn a_board_without_the_driver_has_no_radio() {
        let root = scratch("absent");
        assert_eq!(controller_in(&root.join("driver"), &root.join("class")), Controller::Absent);
        // A bound device without the attribute is not ours to start.
        std::fs::create_dir_all(root.join("driver/serial0-0")).unwrap();
        assert_eq!(controller_in(&root.join("driver"), &root.join("class")), Controller::Absent);
    }

    #[test]
    fn a_bound_controller_waits_for_its_start_attribute() {
        let root = scratch("dormant");
        std::fs::create_dir_all(root.join("driver/serial0-0")).unwrap();
        std::fs::write(root.join("driver/serial0-0/start"), "0\n").unwrap();
        assert_eq!(controller_in(&root.join("driver"), &root.join("class")), Controller::Dormant(vec![root.join("driver/serial0-0/start")]));
    }

    #[test]
    fn a_registered_controller_needs_no_start() {
        let root = scratch("registered");
        std::fs::create_dir_all(root.join("driver/serial0-0")).unwrap();
        std::fs::write(root.join("driver/serial0-0/start"), "1\n").unwrap();
        std::fs::create_dir_all(root.join("class/hci0")).unwrap();
        assert_eq!(controller_in(&root.join("driver"), &root.join("class")), Controller::Registered);
    }

    #[test]
    fn the_preference_survives_a_round_trip_and_defaults_to_off() {
        let root = scratch("preference");
        let path = root.join("var/bluetooth.toml");
        assert_eq!(Preference::load_from(&path), Preference { enabled: false });
        Preference { enabled: true }.save_to(&path).unwrap();
        assert_eq!(Preference::load_from(&path), Preference { enabled: true });
        std::fs::write(&path, "enabled = \"maybe\"").unwrap();
        assert_eq!(Preference::load_from(&path), Preference { enabled: false });
    }
}
