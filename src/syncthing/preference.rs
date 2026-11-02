//! The player's Syncthing choices, kept on the data card beside Syncthing's own
//! files so they move with the card and its identity.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::model::{Exposure, Password, Switch, WebInterface};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preference {
    pub service: Switch,
    pub web: WebInterface,
}

impl Default for Preference {
    fn default() -> Self { Self { service: Switch::default(), web: WebInterface::Local(None) } }
}

/// The file's form: `syncthing = "on"`, `web_interface = "network"`,
/// `web_password = "…"`. The password is kept so the screen can show it.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Stored {
    syncthing: Switch,
    web_interface: Exposure,
    web_password: Option<String>,
}

impl TryFrom<Stored> for Preference {
    type Error = String;

    fn try_from(stored: Stored) -> Result<Self, String> {
        let password = stored.web_password.as_deref().map(|text| Password::parse(text).ok_or("web_password has invalid characters")).transpose()?;
        let web = match (stored.web_interface, password) {
            (Exposure::Local, password) => WebInterface::Local(password),
            (Exposure::Network, Some(password)) => WebInterface::Network(password),
            (Exposure::Network, None) => return Err("web_interface is network without a web_password".into()),
        };
        Ok(Self { service: stored.syncthing, web })
    }
}

impl From<&Preference> for Stored {
    fn from(preference: &Preference) -> Self {
        let (web_interface, password) = match &preference.web {
            WebInterface::Local(password) => (Exposure::Local, password.as_ref()),
            WebInterface::Network(password) => (Exposure::Network, Some(password)),
        };
        Self { syncthing: preference.service, web_interface, web_password: password.map(|password| password.expose().to_owned()) }
    }
}

impl Preference {
    /// A missing file is the default; a damaged one too, with a note in the log.
    /// The default never exposes the web interface.
    pub fn load(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else { return Self::default() };
        toml::from_str::<Stored>(&text).map_err(|error| error.to_string()).and_then(Self::try_from).unwrap_or_else(|error| {
            eprintln!("ghazal: {}: {error}; using the defaults", path.display());
            Self::default()
        })
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = toml::to_string(&Stored::from(self))?;
        crate::sys::write_atomic_private(path, text.as_bytes()).with_context(|| format!("saving {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("ghazal-syncthing-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        path.join("ghazal.toml")
    }

    #[test]
    fn a_card_without_the_file_keeps_syncing_with_a_local_web_interface() {
        assert_eq!(Preference::load(&scratch("missing")), Preference { service: Switch::On, web: WebInterface::Local(None) });
    }

    #[test]
    fn choices_survive_a_round_trip() {
        let path = scratch("round-trip");
        let password = Password::parse("abcd-efgh-jkmn-pqrs").unwrap();
        for preference in [
            Preference { service: Switch::Off, web: WebInterface::Local(None) },
            Preference { service: Switch::On, web: WebInterface::Network(password.clone()) },
            Preference { service: Switch::On, web: WebInterface::Local(Some(password.clone())) },
        ] {
            preference.save(&path).unwrap();
            assert_eq!(Preference::load(&path), preference);
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "syncthing = \"on\"\nweb_interface = \"local\"\nweb_password = \"abcd-efgh-jkmn-pqrs\"\n");
    }

    #[test]
    fn a_damaged_file_never_exposes_the_web_interface() {
        let path = scratch("damaged");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        for text in ["web_interface = \"network\"\n", "syncthing = \"maybe\"\n", "web_interface = \"network\"\nweb_password = \"with space\"\n", "colour = \"blue\"\n"] {
            std::fs::write(&path, text).unwrap();
            assert_eq!(Preference::load(&path), Preference::default(), "{text}");
        }
    }
}
