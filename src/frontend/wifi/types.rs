//! Wi-Fi domain types. Raw iwctl text is turned into these once, in `parse`;
//! everything after that works with values that cannot be malformed.

use std::fmt;
use std::net::Ipv4Addr;

/// A network name: 1 to 32 bytes of UTF-8 without control characters.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Ssid(String);

#[derive(Debug, PartialEq, Eq)]
pub enum SsidError {
    Empty,
    TooLong,
    ControlCharacter,
}

impl Ssid {
    pub const MAX_BYTES: usize = 32;

    pub fn new(name: &str) -> Result<Self, SsidError> {
        match name {
            "" => Err(SsidError::Empty),
            name if name.len() > Self::MAX_BYTES => Err(SsidError::TooLong),
            name if name.chars().any(char::is_control) => Err(SsidError::ControlCharacter),
            name => Ok(Self(name.to_owned())),
        }
    }

    pub fn as_str(&self) -> &str { &self.0 }
}

impl fmt::Display for Ssid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.0) }
}

impl fmt::Display for SsidError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Empty => "Enter the network name",
            Self::TooLong => "Network names have at most 32 characters",
            Self::ControlCharacter => "Network name has invalid characters",
        })
    }
}

/// A WPA passphrase: 8 to 63 printable ASCII characters. Its `Debug` output is
/// redacted so it cannot reach a log by accident.
#[derive(Clone, PartialEq, Eq)]
pub struct Passphrase(String);

#[derive(Debug, PartialEq, Eq)]
pub enum PassphraseError {
    TooShort,
    TooLong,
    NotPrintable,
}

impl Passphrase {
    pub const MIN: usize = 8;
    pub const MAX: usize = 63;

    pub fn new(text: &str) -> Result<Self, PassphraseError> {
        match text {
            text if !text.chars().all(|c| c.is_ascii_graphic() || c == ' ') => Err(PassphraseError::NotPrintable),
            text if text.len() < Self::MIN => Err(PassphraseError::TooShort),
            text if text.len() > Self::MAX => Err(PassphraseError::TooLong),
            text => Ok(Self(text.to_owned())),
        }
    }

    /// The secret itself; only for handing to iwd.
    pub fn expose(&self) -> &str { &self.0 }
}

impl fmt::Debug for Passphrase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("Passphrase(..)") }
}

impl fmt::Display for PassphraseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::TooShort => "Passwords have at least 8 characters",
            Self::TooLong => "Passwords have at most 63 characters",
            Self::NotPrintable => "Password has invalid characters",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Security {
    Open,
    /// WPA2 or WPA3 with a passphrase: iwd reports both as `psk`.
    Psk,
    /// 802.1X: needs a username and certificates, which this screen cannot take.
    Enterprise,
    Other,
}

impl Security {
    /// The word iwctl prints and accepts in its `connect` command.
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "open" => Some(Self::Open),
            "psk" => Some(Self::Psk),
            "8021x" => Some(Self::Enterprise),
            "wep" | "wpa" => Some(Self::Other),
            _ => None,
        }
    }

    /// iwd's description of a connection's security, e.g. `WPA2-Personal`.
    pub fn from_description(text: &str) -> Self {
        match text {
            "Open" | "open" => Self::Open,
            text if text.contains("Enterprise") => Self::Enterprise,
            text if text.contains("Personal") => Self::Psk,
            _ => Self::Other,
        }
    }

    /// The argument iwctl's `connect` takes to pick between networks of one name.
    pub fn iwctl(self) -> Option<&'static str> {
        match self {
            Self::Open => Some("open"),
            Self::Psk => Some("psk"),
            Self::Enterprise => Some("8021x"),
            Self::Other => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Open => "Open",
            Self::Psk => "WPA",
            Self::Enterprise => "802.1X",
            Self::Other => "Other",
        }
    }

    /// Whether the Wi-Fi screen can join it.
    pub fn joinable(self) -> bool { matches!(self, Self::Open | Self::Psk) }
}

/// Signal strength in hundredths of a dBm, the unit of iwctl's `rssi-dbms`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Signal(i32);

impl Signal {
    pub fn from_centi_dbm(value: i32) -> Self { Self(value) }
    pub fn from_dbm(value: i32) -> Self { Self(value * 100) }
    pub fn dbm(self) -> i32 { self.0 / 100 }

    /// One to four bars, with iwd's own thresholds for its `rssi-bars` output.
    pub fn bars(self) -> u8 {
        match self.0 {
            s if s > -6000 => 4,
            s if s > -6700 => 3,
            s if s > -7500 => 2,
            _ => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Band {
    Ghz2,
    Ghz5,
    Ghz6,
}

impl Band {
    pub fn from_mhz(frequency: u32) -> Self {
        match frequency {
            ..=2999 => Self::Ghz2,
            3000..=5924 => Self::Ghz5,
            _ => Self::Ghz6,
        }
    }

    pub fn label(self) -> &'static str {
        match self { Self::Ghz2 => "2.4 GHz", Self::Ghz5 => "5 GHz", Self::Ghz6 => "6 GHz" }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Connection {
    pub ssid: Ssid,
    /// Unknown until DHCP finishes.
    pub address: Option<Ipv4Addr>,
    pub signal: Option<Signal>,
    pub band: Option<Band>,
    pub security: Security,
}

/// What the station is doing right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Link {
    Disconnected,
    Connecting,
    Connected(Connection),
    Disconnecting,
}

/// Whether a network shows up in scan results. Hidden ones must be asked for
/// by name, so joining them is a different iwctl command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Visibility {
    Listed,
    Hidden,
}

/// A network the screen can list. `signal` is None for a saved network that is
/// not in range.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Network {
    pub ssid: Ssid,
    pub security: Security,
    pub signal: Option<Signal>,
    pub known: bool,
    /// A saved hidden network never shows in scan results, so joining it again
    /// needs the same probe as the first time.
    pub visibility: Visibility,
}

/// Everything one refresh learns about the radio.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub link: Link,
    pub scanning: bool,
    /// Connected network first, then saved ones in range, then the rest by
    /// signal, then saved networks out of range.
    pub networks: Vec<Network>,
}

impl Snapshot {
    pub fn connected(&self) -> Option<&Connection> {
        match &self.link { Link::Connected(connection) => Some(connection), _ => None }
    }

    pub fn is_connected(&self, ssid: &Ssid) -> bool {
        self.connected().is_some_and(|connection| &connection.ssid == ssid)
    }

    pub fn network(&self, ssid: &Ssid) -> Option<&Network> {
        self.networks.iter().find(|network| &network.ssid == ssid)
    }
}

/// Why an operation did not happen. `message` is what the menu shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// iwd or D-Bus is not running (yet).
    Unavailable,
    NoAdapter,
    /// iwd's plain "Operation failed": a wrong passphrase or a refusal.
    Rejected,
    /// The network is neither in range nor saved.
    Missing,
    /// iwd is already doing it.
    Busy,
    Cancelled,
    TimedOut,
    NotConnected,
    AlreadyConnected,
    Unsupported,
    /// iwctl's own wording, for anything not recognized.
    Other(String),
}

impl Failure {
    /// Whether the whole screen is useless, as opposed to one action failing.
    pub fn is_fatal(&self) -> bool {
        matches!(self, Self::Unavailable | Self::NoAdapter)
    }

    pub fn message(&self) -> String {
        match self {
            Self::Unavailable => "Wi-Fi service is not running".into(),
            Self::NoAdapter => "No Wi-Fi adapter found".into(),
            Self::Rejected => "Connection refused: check the password".into(),
            Self::Missing => "Network is out of range".into(),
            Self::Busy => "Wi-Fi is busy; try again".into(),
            Self::Cancelled => "Cancelled".into(),
            Self::TimedOut => "Wi-Fi did not answer in time".into(),
            Self::NotConnected => "Not connected".into(),
            Self::AlreadyConnected => "Already connected".into(),
            Self::Unsupported => "Not supported by this network".into(),
            Self::Other(text) => text.clone(),
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.message()) }
}

/// Joining a network. `passphrase` is None for open networks and for networks
/// whose credentials iwd has already saved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectRequest {
    pub ssid: Ssid,
    pub visibility: Visibility,
    pub security: Option<Security>,
    pub passphrase: Option<Passphrase>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssids_follow_the_802_11_limits() {
        assert_eq!(Ssid::new(""), Err(SsidError::Empty));
        assert_eq!(Ssid::new(&"x".repeat(33)), Err(SsidError::TooLong));
        assert_eq!(Ssid::new("bad\nname"), Err(SsidError::ControlCharacter));
        assert!(Ssid::new(&"x".repeat(32)).is_ok());
        assert_eq!(Ssid::new("Café 5G").unwrap().as_str(), "Café 5G");
        // The limit is in bytes, not characters.
        assert_eq!(Ssid::new(&"é".repeat(17)), Err(SsidError::TooLong));
    }

    #[test]
    fn passphrases_are_8_to_63_printable_characters() {
        assert_eq!(Passphrase::new("short"), Err(PassphraseError::TooShort));
        assert_eq!(Passphrase::new(&"a".repeat(64)), Err(PassphraseError::TooLong));
        assert_eq!(Passphrase::new("pässwörd123"), Err(PassphraseError::NotPrintable));
        assert!(Passphrase::new("with space 1").is_ok());
        assert_eq!(Passphrase::new(&"a".repeat(63)).unwrap().expose().len(), 63);
    }

    #[test]
    fn passphrases_never_print() {
        let passphrase = Passphrase::new("hunter2hunter2").unwrap();
        assert!(!format!("{passphrase:?}").contains("hunter"));
        let request = ConnectRequest { ssid: Ssid::new("Home").unwrap(), visibility: Visibility::Listed, security: Some(Security::Psk), passphrase: Some(passphrase) };
        assert!(!format!("{request:?}").contains("hunter"));
    }

    #[test]
    fn bars_use_iwds_thresholds() {
        let bars = |dbm| Signal::from_dbm(dbm).bars();
        assert_eq!([bars(-30), bars(-59), bars(-60), bars(-66), bars(-67), bars(-74), bars(-75), bars(-90)], [4, 4, 3, 3, 2, 2, 1, 1]);
        assert_eq!(Signal::from_centi_dbm(-4400).dbm(), -44);
    }

    #[test]
    fn bands_follow_the_channel_frequency() {
        assert_eq!(Band::from_mhz(2412), Band::Ghz2);
        assert_eq!(Band::from_mhz(5180), Band::Ghz5);
        assert_eq!(Band::from_mhz(5955), Band::Ghz6);
    }

    #[test]
    fn security_words_round_trip() {
        for word in ["open", "psk", "8021x"] {
            assert_eq!(Security::parse(word).and_then(Security::iwctl), Some(word));
        }
        assert_eq!(Security::parse("nonsense"), None);
        assert_eq!(Security::from_description("WPA2-Personal"), Security::Psk);
        assert_eq!(Security::from_description("WPA3-Enterprise"), Security::Enterprise);
        assert!(!Security::Enterprise.joinable());
    }
}
