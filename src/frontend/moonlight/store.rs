//! What Moonlight remembers between runs: known hosts and stream settings, in
//! `moonlight.toml` on the data card.

use std::net::Ipv4Addr;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::discover::Found;

/// GameStream's HTTP port, which Sunshine also uses by default.
pub const DEFAULT_PORT: u16 = 47989;

/// A host's IP address or name, as Moonlight takes it on its command line.
/// Parsed so that it can never be read as an option or contain whitespace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Address(String);

impl Address {
    pub fn as_str(&self) -> &str { &self.0 }
}

impl TryFrom<String> for Address {
    type Error = String;

    fn try_from(text: String) -> Result<Self, String> {
        let valid = !text.is_empty()
            && text.len() <= 253
            && !text.starts_with(['-', '.'])
            && text.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
        if valid { Ok(Self(text)) } else { Err(format!("{text:?} is not a host name or IPv4 address")) }
    }
}

impl From<Address> for String {
    fn from(address: Address) -> String { address.0 }
}

impl From<Ipv4Addr> for Address {
    fn from(address: Ipv4Addr) -> Self { Self(address.to_string()) }
}

/// Whether the host has accepted this device's key, as of the last time it was asked.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Pairing {
    #[default]
    Unpaired,
    Paired,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Host {
    pub name: String,
    pub address: Address,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub pairing: Pairing,
}

fn default_port() -> u16 { DEFAULT_PORT }

/// A setting with a fixed list of values, cycled with Left/Right.
pub trait Choice: Copy + PartialEq + 'static {
    const ALL: &'static [Self];
    fn label(self) -> &'static str;
}

/// The next (or previous) value, wrapping around.
pub fn cycle<T: Choice>(current: T, forward: bool) -> T {
    let len = T::ALL.len() as isize;
    let at = T::ALL.iter().position(|choice| *choice == current).map_or(0, |at| at as isize);
    T::ALL[(at + if forward { 1 } else { -1 }).rem_euclid(len) as usize]
}

/// The stream's size. The panel is 640x480, so anything else is scaled to fit
/// with black bars; 16:9 sizes keep the picture undistorted.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Resolution {
    #[default]
    #[serde(rename = "640x360")]
    R640x360,
    #[serde(rename = "960x540")]
    R960x540,
    #[serde(rename = "1280x720")]
    R1280x720,
    #[serde(rename = "640x480")]
    R640x480,
}

impl Resolution {
    pub fn size(self) -> (u32, u32) {
        match self {
            Self::R640x360 => (640, 360),
            Self::R960x540 => (960, 540),
            Self::R1280x720 => (1280, 720),
            Self::R640x480 => (640, 480),
        }
    }
}

impl Choice for Resolution {
    const ALL: &'static [Self] = &[Self::R640x360, Self::R960x540, Self::R1280x720, Self::R640x480];
    fn label(self) -> &'static str {
        match self {
            Self::R640x360 => "640x360",
            Self::R960x540 => "960x540",
            Self::R1280x720 => "1280x720",
            Self::R640x480 => "640x480",
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Fps {
    #[serde(rename = "30")]
    Thirty,
    #[default]
    #[serde(rename = "60")]
    Sixty,
}

impl Fps {
    pub fn value(self) -> u32 {
        match self { Self::Thirty => 30, Self::Sixty => 60 }
    }
}

impl Choice for Fps {
    const ALL: &'static [Self] = &[Self::Thirty, Self::Sixty];
    fn label(self) -> &'static str {
        match self { Self::Thirty => "30 fps", Self::Sixty => "60 fps" }
    }
}

/// The video bitrate. `Auto` leaves Moonlight's own table (2 Mbps at 640x360 and 60 fps).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Bitrate {
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "2")]
    Mbps2,
    #[default]
    #[serde(rename = "5")]
    Mbps5,
    #[serde(rename = "10")]
    Mbps10,
    #[serde(rename = "15")]
    Mbps15,
    #[serde(rename = "20")]
    Mbps20,
}

impl Bitrate {
    pub fn kbps(self) -> Option<u32> {
        match self {
            Self::Auto => None,
            Self::Mbps2 => Some(2_000),
            Self::Mbps5 => Some(5_000),
            Self::Mbps10 => Some(10_000),
            Self::Mbps15 => Some(15_000),
            Self::Mbps20 => Some(20_000),
        }
    }
}

impl Choice for Bitrate {
    const ALL: &'static [Self] = &[Self::Auto, Self::Mbps2, Self::Mbps5, Self::Mbps10, Self::Mbps15, Self::Mbps20];
    fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Mbps2 => "2 Mbps",
            Self::Mbps5 => "5 Mbps",
            Self::Mbps10 => "10 Mbps",
            Self::Mbps15 => "15 Mbps",
            Self::Mbps20 => "20 Mbps",
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StreamSettings {
    pub resolution: Resolution,
    pub fps: Fps,
    pub bitrate: Bitrate,
    /// Close the game on the host when the stream ends. Off keeps it running,
    /// so the next stream resumes where it was left.
    pub quit_app_after: bool,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Store {
    pub stream: StreamSettings,
    #[serde(rename = "host")]
    pub hosts: Vec<Host>,
}

impl Store {
    /// Missing settings are the defaults. Unreadable ones are set aside rather
    /// than overwritten by the next save, and reported.
    pub fn load(path: &Path) -> (Self, Option<String>) {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return (Self::default(), None),
            Err(error) => return (Self::default(), Some(format!("Reading Moonlight settings: {error}"))),
        };
        match toml::from_str(&text) {
            Ok(store) => (store, None),
            Err(error) => {
                let aside = path.with_extension("toml.damaged");
                let moved = std::fs::rename(path, &aside).map_or_else(|e| format!("could not move it aside: {e}"), |()| format!("kept as {}", aside.display()));
                (Self::default(), Some(format!("Moonlight settings were unreadable ({}); {moved}", error.message())))
            }
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        crate::sys::write_atomic(path, toml::to_string_pretty(self)?.as_bytes()).with_context(|| format!("saving {}", path.display()))
    }

    /// Adds the host unless its address is known; returns its index.
    pub fn add(&mut self, host: Host) -> usize {
        match self.hosts.iter().position(|known| known.address == host.address) {
            Some(index) => index,
            None => {
                self.hosts.push(host);
                self.hosts.len() - 1
            }
        }
    }

    /// Adds hosts found on the network; a known host keeps its pairing and
    /// takes the advertised name if it was only named by its address. Returns
    /// how many were new.
    pub fn merge(&mut self, found: &[Found]) -> usize {
        found.iter().filter(|found| {
            let Ok(address) = Address::try_from(found.address.to_string()) else { return false };
            let before = self.hosts.len();
            let index = self.add(Host { name: found.name.clone(), address: address.clone(), port: found.port, pairing: Pairing::Unpaired });
            let host = &mut self.hosts[index];
            if host.name == address.as_str() { host.name = found.name.clone(); }
            host.port = found.port;
            self.hosts.len() > before
        }).count()
    }

    pub fn remove(&mut self, index: usize) {
        if index < self.hosts.len() { self.hosts.remove(index); }
    }

    pub fn set_pairing(&mut self, index: usize, pairing: Pairing) {
        if let Some(host) = self.hosts.get_mut(index) { host.pairing = pairing; }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(name: &str, last: u8) -> Found {
        Found { name: name.into(), address: Ipv4Addr::new(192, 168, 1, last), port: DEFAULT_PORT }
    }

    #[test]
    fn addresses_cannot_be_read_as_options_or_hide_whitespace() {
        assert!(Address::try_from("192.168.1.20".to_owned()).is_ok());
        assert!(Address::try_from("gaming-pc.local".to_owned()).is_ok());
        for bad in ["", "-app", "-", ".hidden", "a b", "a\nb", "host;rm", "[::1]", "é"] {
            assert!(Address::try_from(bad.to_owned()).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn choices_wrap_in_both_directions() {
        assert_eq!(cycle(Fps::Sixty, true), Fps::Thirty);
        assert_eq!(cycle(Fps::Thirty, false), Fps::Sixty);
        assert_eq!(cycle(Resolution::R640x480, true), Resolution::R640x360);
        assert_eq!(cycle(Bitrate::Auto, false), Bitrate::Mbps20);
    }

    #[test]
    fn the_store_round_trips_and_defaults_are_small_and_16_by_9() {
        let mut store = Store::default();
        assert_eq!(store.stream, StreamSettings { resolution: Resolution::R640x360, fps: Fps::Sixty, bitrate: Bitrate::Mbps5, quit_app_after: false });
        store.stream.resolution = Resolution::R1280x720;
        store.stream.bitrate = Bitrate::Auto;
        store.add(Host { name: "PC".into(), address: Address::try_from("10.0.0.2".to_owned()).unwrap(), port: 48000, pairing: Pairing::Paired });
        let text = toml::to_string_pretty(&store).unwrap();
        assert_eq!(toml::from_str::<Store>(&text).unwrap(), store);
        assert!(text.contains("resolution = \"1280x720\"") && text.contains("[[host]]") && text.contains("pairing = \"paired\""));
    }

    #[test]
    fn a_minimal_file_fills_in_the_defaults() {
        let store: Store = toml::from_str("[[host]]\nname = \"PC\"\naddress = \"10.0.0.2\"\n").unwrap();
        assert_eq!(store.hosts[0].port, DEFAULT_PORT);
        assert_eq!(store.hosts[0].pairing, Pairing::Unpaired);
        assert_eq!(store.stream, StreamSettings::default());
        assert!(toml::from_str::<Store>("[[host]]\nname = \"PC\"\naddress = \"-bad\"\n").is_err());
    }

    #[test]
    fn found_hosts_are_merged_without_losing_pairings() {
        let mut store = Store::default();
        assert_eq!(store.merge(&[found("DESKTOP", 20), found("LAPTOP", 21)]), 2);
        store.set_pairing(0, Pairing::Paired);
        assert_eq!(store.merge(&[found("DESKTOP", 20), found("NEW", 22)]), 1);
        assert_eq!(store.hosts.iter().map(|host| host.name.as_str()).collect::<Vec<_>>(), ["DESKTOP", "LAPTOP", "NEW"]);
        assert_eq!(store.hosts[0].pairing, Pairing::Paired);
        // A host added by hand is named by its address until it is seen.
        let mut store = Store::default();
        let index = store.add(Host { name: "192.168.1.20".into(), address: Ipv4Addr::new(192, 168, 1, 20).into(), port: DEFAULT_PORT, pairing: Pairing::Unpaired });
        assert_eq!(store.merge(&[found("DESKTOP", 20)]), 0);
        assert_eq!(store.hosts[index].name, "DESKTOP");
    }

    #[test]
    fn a_damaged_file_is_set_aside_not_overwritten() {
        let dir = std::env::temp_dir().join(format!("ghazal-moonlight-store-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("moonlight.toml");
        assert_eq!(Store::load(&path), (Store::default(), None));
        std::fs::write(&path, "this is [not toml").unwrap();
        let (store, warning) = Store::load(&path);
        assert_eq!(store, Store::default());
        assert!(warning.is_some_and(|text| text.contains("moonlight.toml.damaged")));
        assert!(!path.exists() && dir.join("moonlight.toml.damaged").exists());
        let mut store = Store::default();
        store.stream.fps = Fps::Thirty;
        store.save(&path).unwrap();
        assert_eq!(Store::load(&path).0, store);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
