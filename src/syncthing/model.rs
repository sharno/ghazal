//! What the menu knows about Syncthing. `wire` turns Syncthing's JSON into
//! these once; nothing after that sees raw strings for IDs or states.

use std::fmt;
use std::net::Ipv4Addr;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A device ID: 56 base32 characters, written as 8 groups of 7 joined by `-`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeviceId(String);

impl DeviceId {
    const GROUPS: usize = 8;
    const GROUP: usize = 7;

    /// Accepts Syncthing's grouped form and the same characters without dashes,
    /// in either case.
    pub fn parse(text: &str) -> Option<Self> {
        let plain: String = text.chars().filter(|c| !matches!(c, '-' | ' ')).map(|c| c.to_ascii_uppercase()).collect();
        let valid = plain.len() == Self::GROUPS * Self::GROUP && plain.chars().all(|c| matches!(c, 'A'..='Z' | '2'..='7'));
        valid.then(|| Self(plain.as_bytes().chunks(Self::GROUP).map(|group| String::from_utf8_lossy(group).into_owned()).collect::<Vec<_>>().join("-")))
    }

    pub fn as_str(&self) -> &str { &self.0 }

    /// The ID two groups to a line, for a narrow panel.
    pub fn lines(&self) -> Vec<String> {
        self.0.split('-').collect::<Vec<_>>().chunks(2).map(|pair| pair.join("-")).collect()
    }

    /// The first group, which is how Syncthing itself abbreviates a device.
    pub fn short(&self) -> &str { &self.0[..Self::GROUP] }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.0) }
}

/// A folder ID, chosen by whoever created the folder; only its emptiness is ruled out.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FolderId(String);

impl FolderId {
    pub fn parse(text: &str) -> Option<Self> { (!text.is_empty()).then(|| Self(text.to_owned())) }
    pub fn as_str(&self) -> &str { &self.0 }
}

impl fmt::Display for FolderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.0) }
}

/// Seconds since the Unix epoch, from Syncthing's RFC 3339 timestamps.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp(i64);

impl Timestamp {
    pub fn from_unix(seconds: i64) -> Self { Self(seconds) }
    pub fn unix(self) -> i64 { self.0 }

    /// `2026-11-02T14:40:14Z`, `2026-11-02T14:40:14.590276163+01:00` and the like.
    pub fn parse(text: &str) -> Option<Self> {
        let number = |range: std::ops::Range<usize>| text.get(range).filter(|digits| digits.bytes().all(|b| b.is_ascii_digit())).and_then(|digits| digits.parse::<i64>().ok());
        let separators = [(4, b'-'), (7, b'-'), (10, b'T'), (13, b':'), (16, b':')];
        if !separators.iter().all(|&(at, byte)| text.as_bytes().get(at).is_some_and(|found| found.eq_ignore_ascii_case(&byte))) { return None; }
        let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
        let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
        if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 { return None; }
        let rest = text.get(19..)?;
        let zone = rest.trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
        // Only one dot, at the start of the fraction.
        if rest.len() > zone.len() && (!rest.starts_with('.') || rest[1..rest.len() - zone.len()].contains('.')) { return None; }
        let offset = match zone.as_bytes() {
            [b'Z' | b'z'] => 0,
            [sign @ (b'+' | b'-'), h1, h2, b':', m1, m2] if [h1, h2, m1, m2].iter().all(|digit| digit.is_ascii_digit()) => {
                let digit = |byte: &u8| i64::from(byte - b'0');
                let seconds = (digit(h1) * 10 + digit(h2)) * 3600 + (digit(m1) * 10 + digit(m2)) * 60;
                if *sign == b'+' { seconds } else { -seconds }
            }
            _ => return None,
        };
        Some(Self(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second - offset))
    }

    /// RFC 3339 in UTC, as Syncthing writes it.
    pub fn format(self) -> String {
        let (days, seconds) = (self.0.div_euclid(86_400), self.0.rem_euclid(86_400));
        let (year, month, day) = civil_from_days(days);
        format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", seconds / 3600, seconds % 3600 / 60, seconds % 60)
    }
}

/// Days from 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 { month_index + 3 } else { month_index - 9 };
    (year_of_era + era * 400 + i64::from(month <= 2), month, day)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FolderState {
    /// Paused on this handheld.
    Paused,
    /// No other device has this folder.
    Unshared,
    UpToDate,
    Scanning,
    /// Pulling changes; never 100 while it lasts.
    Syncing { percent: u8 },
    /// Versioning or temporary-file cleanup.
    Cleaning,
    /// Idle, with files it could not pull.
    Errors(u64),
    /// Idle and behind, with nobody to get the rest from.
    Waiting,
    /// Syncthing stopped the folder: its path is missing, the disk is full and so on.
    Stopped(String),
    /// A state a newer Syncthing reports.
    Other(String),
}

impl FolderState {
    pub fn describe(&self) -> String {
        match self {
            Self::Paused => "Paused".into(),
            Self::Unshared => "Not shared".into(),
            Self::UpToDate => "Up to date".into(),
            Self::Scanning => "Scanning".into(),
            Self::Syncing { percent } => format!("Syncing {percent}%"),
            Self::Cleaning => "Cleaning up".into(),
            Self::Errors(1) => "1 error".into(),
            Self::Errors(count) => format!("{count} errors"),
            Self::Waiting => "Waiting for a device".into(),
            Self::Stopped(_) => "Stopped".into(),
            Self::Other(state) => state.clone(),
        }
    }

    pub fn failing(&self) -> bool { matches!(self, Self::Errors(_) | Self::Stopped(_)) }
    pub fn syncing(&self) -> bool { matches!(self, Self::Syncing { .. }) }
    /// Whether it may change soon, so the screen looks again sooner.
    pub fn busy(&self) -> bool { matches!(self, Self::Scanning | Self::Syncing { .. } | Self::Cleaning) }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Folder {
    pub id: FolderId,
    /// The label, or the ID when the folder has none.
    pub label: String,
    pub path: PathBuf,
    pub state: FolderState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LastSeen {
    Never,
    At(Timestamp),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presence {
    Connected,
    /// Paused on this handheld.
    Paused,
    Away(LastSeen),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub id: DeviceId,
    pub name: String,
    pub presence: Presence,
}

/// This handheld, as other devices see it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Me {
    pub id: DeviceId,
    pub name: String,
}

/// A device that tried to connect and is not paired.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingDevice {
    pub id: DeviceId,
    /// What it calls itself; may be empty.
    pub name: String,
    pub address: String,
}

impl PendingDevice {
    pub fn title(&self) -> String {
        if self.name.is_empty() { self.id.short().to_owned() } else { self.name.clone() }
    }
}

/// Every paired device that offers a folder: at least one, by construction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offerers {
    pub first: DeviceId,
    pub others: Vec<DeviceId>,
}

impl Offerers {
    pub fn iter(&self) -> impl Iterator<Item = &DeviceId> { std::iter::once(&self.first).chain(&self.others) }
}

/// Where accepting a shared folder puts it on the data card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Destination {
    /// Nothing there yet: Syncthing creates the directory.
    New(PathBuf),
    /// An existing directory without files.
    Empty(PathBuf),
    /// An existing directory with files: they are merged with the other device's.
    Merge(PathBuf),
    /// Another Syncthing folder already uses this directory.
    Taken { path: PathBuf, by: String },
    /// The label cannot name a directory on the card (only dots or slashes).
    Unusable,
}

/// A folder a paired device shares and this handheld has not accepted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingFolder {
    pub id: FolderId,
    /// The label, or the ID when the folder has none.
    pub label: String,
    pub offered_by: Offerers,
    pub destination: Destination,
}

/// Where Syncthing's web interface listens right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Listening {
    Local,
    Network,
}

impl Listening {
    /// `guiAddressUsed`: `127.0.0.1:8384`, `0.0.0.0:8384`, `[::1]:8384`…
    pub fn from_address(address: &str) -> Self {
        let host = address.rsplit_once(':').map_or(address, |(host, _)| host).trim_start_matches('[').trim_end_matches(']');
        match host.parse::<std::net::IpAddr>() {
            Ok(ip) if ip.is_loopback() => Self::Local,
            Err(_) if host == "localhost" => Self::Local,
            _ => Self::Network,
        }
    }
}

/// Everything one refresh learns from a running Syncthing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub me: Me,
    pub folders: Vec<Folder>,
    /// Other devices, in Syncthing's order.
    pub devices: Vec<Device>,
    pub pending_devices: Vec<PendingDevice>,
    pub pending_folders: Vec<PendingFolder>,
    pub listening: Listening,
}

impl Snapshot {
    pub fn folder(&self, id: &FolderId) -> Option<&Folder> { self.folders.iter().find(|folder| &folder.id == id) }
    pub fn device(&self, id: &DeviceId) -> Option<&Device> { self.devices.iter().find(|device| &device.id == id) }
    pub fn pending_device(&self, id: &DeviceId) -> Option<&PendingDevice> { self.pending_devices.iter().find(|device| &device.id == id) }
    pub fn pending_folder(&self, id: &FolderId) -> Option<&PendingFolder> { self.pending_folders.iter().find(|folder| &folder.id == id) }

    /// A device's name, or its short ID when it is not paired.
    pub fn device_name(&self, id: &DeviceId) -> String {
        self.device(id).map_or_else(|| id.short().to_owned(), |device| device.name.clone())
    }

    pub fn summary(&self) -> Summary {
        Summary::of(self.folders.iter().map(|folder| &folder.state), self.devices.iter().filter(|device| device.presence == Presence::Connected).count())
    }
}

/// Syncthing in one word, for the status bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Summary {
    /// The player turned it off.
    Off,
    /// On, but not answering: starting, restarting or waiting for the data card.
    Stopped,
    /// Running with no other device connected.
    NoDevices,
    UpToDate,
    Syncing,
    /// A folder is stopped or has files it cannot sync.
    Error,
}

impl Summary {
    pub fn of<'a>(folders: impl Iterator<Item = &'a FolderState> + Clone, connected: usize) -> Self {
        match (folders.clone().any(FolderState::failing), folders.clone().any(FolderState::syncing), connected) {
            (true, _, _) => Self::Error,
            (false, true, _) => Self::Syncing,
            (false, false, 0) => Self::NoDevices,
            (false, false, _) => Self::UpToDate,
        }
    }
}

/// The web interface login: the user is always `ghazal`. `Debug` is redacted.
#[derive(Clone, PartialEq, Eq)]
pub struct Password(String);

impl Password {
    pub const USER: &'static str = "ghazal";
    const ALPHABET: &'static [u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    const LENGTH: usize = 16;

    /// A stored password: printable ASCII without spaces.
    pub fn parse(text: &str) -> Option<Self> {
        (!text.is_empty() && text.len() <= 64 && text.bytes().all(|byte| byte.is_ascii_graphic())).then(|| Self(text.to_owned()))
    }

    /// 16 characters without look-alikes (about 79 bits), in groups of four.
    /// `random` is a source of uniformly random bytes.
    pub fn generate(random: impl IntoIterator<Item = u8>) -> Option<Self> {
        // Rejecting the top bytes keeps every character equally likely.
        let limit = (256 / Self::ALPHABET.len() * Self::ALPHABET.len()) as u8;
        let characters: Vec<char> = random.into_iter().filter(|byte| *byte < limit).map(|byte| char::from(Self::ALPHABET[usize::from(byte) % Self::ALPHABET.len()])).take(Self::LENGTH).collect();
        (characters.len() == Self::LENGTH).then(|| Self(characters.chunks(4).map(|group| group.iter().collect::<String>()).collect::<Vec<_>>().join("-")))
    }

    /// The secret itself: for Syncthing, the preference file and the screen.
    pub fn expose(&self) -> &str { &self.0 }
}

impl fmt::Debug for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("Password(..)") }
}

/// Whether the web interface is reachable from the network, as the player chose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WebInterface {
    /// Only on the handheld (or through an SSH tunnel). A login made for the
    /// network stays in Syncthing and protects this too.
    Local(Option<Password>),
    /// On every interface; never without a login.
    Network(Password),
}

impl WebInterface {
    pub fn login(&self) -> Option<&Password> {
        match self { Self::Local(login) => login.as_ref(), Self::Network(login) => Some(login) }
    }


    pub fn exposure(&self) -> Exposure {
        match self { Self::Local(_) => Exposure::Local, Self::Network(_) => Exposure::Network }
    }
}

/// Where the web interface listens, without the login.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Exposure {
    #[default]
    Local,
    Network,
}

/// Whether the player wants Syncthing running.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Switch {
    /// The default, so a card that synced before the switch existed keeps syncing.
    #[default]
    On,
    Off,
}

/// What the Syncthing service is doing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Service {
    Off,
    /// No data card: Syncthing waits for one.
    NoDataCard,
    /// On, and not answering yet.
    Starting,
    /// It answered, but not usefully (a refused API key, an unexpected reply).
    Failing(super::client::Failure),
    Running(Snapshot),
}

impl Service {
    pub fn summary(&self) -> Summary {
        match self {
            Self::Off => Summary::Off,
            Self::NoDataCard | Self::Starting => Summary::Stopped,
            Self::Failing(_) => Summary::Error,
            Self::Running(snapshot) => snapshot.summary(),
        }
    }

    pub fn snapshot(&self) -> Option<&Snapshot> {
        match self { Self::Running(snapshot) => Some(snapshot), Self::Off | Self::NoDataCard | Self::Starting | Self::Failing(_) => None }
    }
}

/// Everything the screen shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observation {
    pub service: Service,
    pub web: WebInterface,
    /// This handheld's address on the network, for the web interface's URL.
    pub address: Option<Ipv4Addr>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "HJ56AJG-RR4EN7L-B2WJ3UI-2NNTZCS-2PZ4P3W-R3SZJHF-S25SDML-LBSRGQI";

    #[test]
    fn device_ids_are_grouped_and_validated() {
        let id = DeviceId::parse(ID).unwrap();
        assert_eq!(id.as_str(), ID);
        assert_eq!(DeviceId::parse(&ID.replace('-', "").to_lowercase()), Some(id.clone()));
        assert_eq!(id.lines(), ["HJ56AJG-RR4EN7L", "B2WJ3UI-2NNTZCS", "2PZ4P3W-R3SZJHF", "S25SDML-LBSRGQI"]);
        assert_eq!(id.short(), "HJ56AJG");
        // Base32 has no 0, 1, 8 or 9; the length is fixed.
        assert_eq!(DeviceId::parse(&ID.replace('J', "1")), None);
        assert_eq!(DeviceId::parse(&ID[..62]), None);
        assert_eq!(DeviceId::parse(""), None);
    }

    #[test]
    fn timestamps_follow_rfc_3339() {
        assert_eq!(Timestamp::parse("1970-01-01T00:00:00Z"), Some(Timestamp(0)));
        assert_eq!(Timestamp::parse("2026-11-02T14:40:14Z"), Some(Timestamp(1_793_630_414)));
        assert_eq!(Timestamp::parse("2026-11-02T15:40:14.590276163+01:00"), Some(Timestamp(1_793_630_414)));
        assert_eq!(Timestamp::parse("2000-02-29T00:00:00-00:30"), Some(Timestamp(951_784_200)));
        assert!(Timestamp::parse("0001-01-01T00:00:00Z").is_some_and(|time| time.unix() < 0));
        for bad in ["", "2026-11-02", "2026-13-02T00:00:00Z", "2026-11-02T14:40:14", "2026-11-02T14:40:14+0100", "2026-11-02T14:40:14.x9Z"] {
            assert_eq!(Timestamp::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn formatting_inverts_parsing() {
        for text in ["1970-01-01T00:00:00Z", "2026-11-02T14:40:14Z", "2000-02-29T23:59:59Z", "2100-03-01T00:00:00Z"] {
            assert_eq!(Timestamp::parse(text).unwrap().format(), text);
        }
    }

    #[test]
    fn the_web_interface_address_says_where_it_listens() {
        assert_eq!(Listening::from_address("127.0.0.1:8384"), Listening::Local);
        assert_eq!(Listening::from_address("[::1]:8384"), Listening::Local);
        assert_eq!(Listening::from_address("localhost:8384"), Listening::Local);
        assert_eq!(Listening::from_address("0.0.0.0:8384"), Listening::Network);
        assert_eq!(Listening::from_address("192.168.1.5:8384"), Listening::Network);
    }

    #[test]
    fn the_summary_puts_errors_before_syncing_before_connections() {
        let summary = |states: &[FolderState], connected| Summary::of(states.iter(), connected);
        assert_eq!(summary(&[FolderState::UpToDate, FolderState::Errors(2), FolderState::Syncing { percent: 5 }], 1), Summary::Error);
        assert_eq!(summary(&[FolderState::Stopped("missing".into())], 0), Summary::Error);
        assert_eq!(summary(&[FolderState::UpToDate, FolderState::Syncing { percent: 5 }], 1), Summary::Syncing);
        assert_eq!(summary(&[FolderState::Waiting], 0), Summary::NoDevices);
        assert_eq!(summary(&[], 0), Summary::NoDevices);
        assert_eq!(summary(&[FolderState::UpToDate, FolderState::Scanning], 2), Summary::UpToDate);
    }

    #[test]
    fn generated_passwords_are_grouped_and_unbiased() {
        let password = Password::generate((0..=255).cycle()).unwrap();
        assert_eq!(password.expose().len(), 19);
        assert_eq!(password.expose().matches('-').count(), 3);
        assert!(password.expose().chars().all(|c| c == '-' || Password::ALPHABET.contains(&(c as u8))));
        // Bytes past the last whole alphabet are skipped, not folded onto its start.
        assert_eq!(Password::generate([255u8; 64]), None);
        assert!(!format!("{password:?}").contains(password.expose()));
    }

    #[test]
    fn stored_passwords_are_printable() {
        assert!(Password::parse("abcd-efgh").is_some());
        assert_eq!(Password::parse(""), None);
        assert_eq!(Password::parse("with space"), None);
        assert_eq!(Password::parse(&"x".repeat(65)), None);
    }

    #[test]
    fn folder_states_read_as_the_screen_shows_them() {
        let words: Vec<String> = [FolderState::UpToDate, FolderState::Syncing { percent: 42 }, FolderState::Errors(1), FolderState::Errors(3), FolderState::Waiting, FolderState::Paused].iter().map(FolderState::describe).collect();
        assert_eq!(words, ["Up to date", "Syncing 42%", "1 error", "3 errors", "Waiting for a device", "Paused"]);
    }
}
