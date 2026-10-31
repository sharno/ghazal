//! A DRM connector as sysfs shows it. Reading `status` makes the driver detect
//! (a register read on dw-hdmi). Writing `off` forces the connector to report
//! disconnected and `detect` releases it. RetroArch and SDL both use the first
//! connected connector, so forcing the other one off steers a game to a single
//! screen without touching either emulator's configuration.

use std::path::{Path, PathBuf};

use anyhow::Result;

use super::Connection;

pub const DRM_CLASS: &str = "/sys/class/drm";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Force {
    Off,
    Detect,
}

#[derive(Debug, Clone)]
pub struct Connector {
    dir: PathBuf,
}

impl Connector {
    /// The HDMI connector of `card` (any card when `None`) below `class_dir`.
    pub fn find_hdmi(class_dir: &Path, card: Option<&str>) -> Option<Self> {
        let mut names: Vec<String> = std::fs::read_dir(class_dir).ok()?.flatten().map(|entry| entry.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names.into_iter().find(|name| is_hdmi(name, card)).map(|name| Self { dir: class_dir.join(name) })
    }

    /// `connector` is DRM's own name, e.g. `DSI-1`.
    pub fn named(class_dir: &Path, card: &str, connector: &str) -> Self {
        Self { dir: class_dir.join(format!("{card}-{connector}")) }
    }

    pub fn name(&self) -> String {
        self.dir.file_name().map_or_else(String::new, |name| name.to_string_lossy().into_owned())
    }

    /// Unreadable status counts as disconnected: nothing can be shown there.
    pub fn connection(&self) -> Connection {
        parse_status(&std::fs::read_to_string(self.dir.join("status")).unwrap_or_default())
    }

    /// The sink's modes, one `WIDTHxHEIGHT` per line. Reading asks the driver to read the EDID.
    pub fn modes(&self) -> String {
        std::fs::read_to_string(self.dir.join("modes")).unwrap_or_default()
    }

    pub fn force(&self, force: Force) -> Result<()> {
        crate::sys::write_file(&self.dir.join("status"), match force { Force::Off => "off", Force::Detect => "detect" })
    }
}

/// A frontend that died during a game may have left the handheld's own screen
/// forced off, which would hide the only screen there is: release every
/// non-HDMI connector that reads as disconnected. One directory listing and no
/// writes in the normal case.
pub fn release_forced_panels(class_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(class_dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_connector(&name) && !is_hdmi(&name, None) {
            let connector = Connector { dir: entry.path() };
            if connector.connection() == Connection::Disconnected {
                let _ = connector.force(Force::Detect);
            }
        }
    }
}

/// `card1-DSI-1`, `card0-Virtual-1`: a card, then DRM's connector name.
fn is_connector(name: &str) -> bool {
    name.split_once('-').is_some_and(|(owner, connector)| {
        owner.strip_prefix("card").is_some_and(|number| !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())) && !connector.is_empty()
    })
}

fn parse_status(text: &str) -> Connection {
    if text.trim() == "connected" { Connection::Connected } else { Connection::Disconnected }
}

/// `card1-HDMI-A-1`
fn is_hdmi(name: &str, card: Option<&str>) -> bool {
    let Some((owner, connector)) = name.split_once('-') else { return false };
    owner.strip_prefix("card").is_some_and(|number| !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit()))
        && card.is_none_or(|card| card == owner)
        && connector.starts_with("HDMI-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_hdmi_connectors_of_the_right_card_match() {
        assert!(is_hdmi("card1-HDMI-A-1", None));
        assert!(is_hdmi("card1-HDMI-A-1", Some("card1")));
        assert!(!is_hdmi("card1-HDMI-A-1", Some("card0")));
        assert!(!is_hdmi("card1-DSI-1", None));
        assert!(!is_hdmi("card1", None));
        assert!(!is_hdmi("renderD128", None));
        assert!(!is_hdmi("cardx-HDMI-A-1", None));
    }

    #[test]
    fn a_panel_left_forced_off_is_released_and_a_disconnected_hdmi_is_not_touched() {
        let root = std::env::temp_dir().join(format!("ghazal-drm-stale-{}", std::process::id()));
        for (name, status) in [("card1-DSI-1", "disconnected\n"), ("card1-HDMI-A-1", "disconnected\n"), ("card1-Virtual-1", "connected\n")] {
            std::fs::create_dir_all(root.join(name)).unwrap();
            std::fs::write(root.join(name).join("status"), status).unwrap();
        }
        std::fs::create_dir_all(root.join("card1")).unwrap();
        release_forced_panels(&root);
        let status = |name: &str| std::fs::read_to_string(root.join(name).join("status")).unwrap();
        assert_eq!(status("card1-DSI-1"), "detect");
        assert_eq!(status("card1-HDMI-A-1"), "disconnected\n");
        assert_eq!(status("card1-Virtual-1"), "connected\n");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn status_reads_as_a_connection() {
        assert_eq!(parse_status("connected\n"), Connection::Connected);
        assert_eq!(parse_status("disconnected\n"), Connection::Disconnected);
        assert_eq!(parse_status("unknown\n"), Connection::Disconnected);
        assert_eq!(parse_status(""), Connection::Disconnected);
    }

    #[test]
    fn a_directory_of_connectors_is_searched_in_name_order() {
        let root = std::env::temp_dir().join(format!("ghazal-drm-class-{}", std::process::id()));
        for name in ["card0", "card1-DSI-1", "card1-HDMI-A-2", "card1-HDMI-A-1"] {
            std::fs::create_dir_all(root.join(name)).unwrap();
        }
        std::fs::write(root.join("card1-HDMI-A-1/status"), "connected\n").unwrap();
        let found = Connector::find_hdmi(&root, None).unwrap();
        assert_eq!(found.name(), "card1-HDMI-A-1");
        assert_eq!(found.connection(), Connection::Connected);
        assert_eq!(Connector::named(&root, "card1", "DSI-1").connection(), Connection::Disconnected);
        found.force(Force::Off).unwrap();
        assert_eq!(std::fs::read_to_string(root.join("card1-HDMI-A-1/status")).unwrap(), "off");
        assert!(Connector::find_hdmi(&root.join("missing"), None).is_none());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
