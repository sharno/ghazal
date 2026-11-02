//! Syncthing on this handheld: the service the supervisor runs, the player's
//! choices on the data card, and the REST client, behind one `Backend`.

use std::net::{Ipv4Addr, SocketAddr};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

use super::client::{Client, Failure, GuiConfig};
use super::model::{Destination, Exposure, Folder, Observation, Password, Service, Summary, Switch, Timestamp, WebInterface};
use super::preference::Preference;
use super::{Backend, Order};

pub const BINARY: &str = "/usr/bin/syncthing";
/// The service waits for this (`wait_for` in system.toml); `restore` makes it at boot.
pub const MARKER: &str = "/run/ghazal/syncthing-requested";
/// The data card, where accepted folders go.
const DATA: &str = "/mnt/games";
/// Syncthing's identity, configuration and database, plus the player's choices.
const HOME: &str = "/mnt/games/.ghazal/syncthing";
const PORT: u16 = 8384;

/// The real thing: the supervisor's service and the data card.
pub struct System {
    data: PathBuf,
    home: PathBuf,
}

impl Default for System {
    fn default() -> Self { Self { data: DATA.into(), home: HOME.into() } }
}

/// Where the web interface listens: everywhere only with a login.
pub fn gui_address(web: &WebInterface, has_login: bool) -> SocketAddr {
    let host = match (web.exposure(), has_login) {
        (Exposure::Network, true) => Ipv4Addr::UNSPECIFIED,
        (Exposure::Network, false) | (Exposure::Local, _) => Ipv4Addr::LOCALHOST,
    };
    SocketAddr::from((host, PORT))
}

/// The name a shared folder gets on the card: its label without path
/// separators, control characters or leading dots (no hidden folders, so never
/// `.ghazal`).
pub fn directory_name(label: &str) -> Option<String> {
    let name: String = label.trim().chars().map(|c| if c == '/' || c == '\\' || c.is_control() { '-' } else { c }).collect();
    let name = name.trim_start_matches('.').trim();
    (!name.is_empty() && name.chars().any(|c| c != '-')).then(|| name.to_owned())
}

/// Whether one directory is the other or inside it. exFAT ignores case.
fn overlaps(a: &Path, b: &Path) -> bool {
    let lower = |path: &Path| PathBuf::from(path.to_string_lossy().to_lowercase());
    let (a, b) = (lower(a), lower(b));
    a.starts_with(&b) || b.starts_with(&a)
}

/// Where a shared folder labelled `label` goes under `root`: a directory of
/// that name in any case (so `Saves` lands in `SAVES`), else a new one.
pub fn destination(root: &Path, label: &str, folders: &[Folder]) -> Destination {
    let Some(name) = directory_name(label) else { return Destination::Unusable };
    let existing = std::fs::read_dir(root).into_iter().flatten().flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .find(|entry| entry.to_lowercase() == name.to_lowercase());
    let path = root.join(existing.as_deref().unwrap_or(&name));
    if let Some(folder) = folders.iter().find(|folder| overlaps(&folder.path, &path)) {
        return Destination::Taken { path, by: folder.label.clone() };
    }
    match existing {
        None => Destination::New(path),
        Some(_) if std::fs::read_dir(&path).is_ok_and(|mut entries| entries.next().is_none()) => Destination::Empty(path),
        Some(_) => Destination::Merge(path),
    }
}

/// The wall clock, for "last seen" and for when something was ignored.
pub fn clock() -> Timestamp {
    let seconds = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
    Timestamp::from_unix(i64::try_from(seconds).unwrap_or(i64::MAX))
}

fn generate_password() -> Result<Password, Failure> {
    use std::io::Read;
    let mut bytes = [0u8; 64];
    std::fs::File::open("/dev/urandom").and_then(|mut random| random.read_exact(&mut bytes)).map_err(|error| Failure::Io(format!("reading /dev/urandom: {error}")))?;
    Password::generate(bytes).ok_or_else(|| Failure::Io("could not make a password".into()))
}

fn storage(error: anyhow::Error) -> Failure { Failure::Io(format!("{error:#}")) }

impl System {
    fn preference_path(&self) -> PathBuf { self.home.join("ghazal.toml") }
    fn config_path(&self) -> PathBuf { self.home.join("config.xml") }
    fn preference(&self) -> Preference { Preference::load(&self.preference_path()) }
    fn save(&self, preference: &Preference) -> Result<(), Failure> { preference.save(&self.preference_path()).map_err(storage) }
    /// Only the prepared data card has ROMS (the service waits for it too): never
    /// start Syncthing on the empty mount point.
    fn card_ready(&self) -> bool { self.data.join("ROMS").exists() }

    fn client(&self) -> Result<Client, Failure> {
        Client::from_config(SocketAddr::from((Ipv4Addr::LOCALHOST, PORT)), &self.config_path())
    }

    /// `ghazal syncthing serve`, the service itself: Syncthing with the web
    /// interface address the player chose. The command line's address overrides
    /// config.xml, so an address someone set there never exposes the interface.
    pub fn serve(&self) -> Result<()> {
        let preference = self.preference();
        let has_login = GuiConfig::read(&self.config_path()).is_ok_and(|gui| gui.has_login);
        let address = gui_address(&preference.web, has_login);
        if preference.web.exposure() == Exposure::Network && address.ip().is_loopback() {
            eprintln!("ghazal: Syncthing's web interface has no login; keeping it on this handheld");
        }
        let error = Command::new(BINARY)
            .arg("serve")
            .arg(format!("--home={}", self.home.display()))
            .args(["--no-browser", "--no-restart", "--no-upgrade"])
            .arg(format!("--gui-address=http://{address}"))
            .exec();
        Err(error).with_context(|| format!("starting {BINARY}"))
    }

    /// `ghazal syncthing restore`, at boot: releases the service unless the
    /// player turned Syncthing off.
    pub fn restore(&self) -> Result<()> {
        match self.preference().service {
            Switch::On => crate::sys::write_atomic(Path::new(MARKER), b"1").context("releasing Syncthing"),
            Switch::Off => { println!("Syncthing is off"); Ok(()) }
        }
    }

    fn switch(&self, service: Switch) -> Result<(), Failure> {
        let preference = self.preference();
        self.save(&Preference { service, ..preference })?;
        match service {
            Switch::On => crate::sys::write_atomic(Path::new(MARKER), b"1").map_err(storage),
            Switch::Off => {
                // Without the marker the supervisor holds Syncthing back once it exits.
                match std::fs::remove_file(MARKER) {
                    Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(Failure::Io(format!("{MARKER}: {error}"))),
                    _ => {}
                }
                match self.client().and_then(|client| client.shutdown()) {
                    Ok(()) | Err(Failure::NotRunning | Failure::NoApiKey) => Ok(()),
                    Err(failure) => Err(failure),
                }
            }
        }
    }

    /// The login goes to Syncthing before the address changes, so the interface
    /// is never on the network without one; `serve` checks again.
    fn expose(&self, exposure: Exposure) -> Result<(), Failure> {
        let preference = self.preference();
        let web = match exposure {
            Exposure::Network => {
                let password = preference.web.login().cloned().map_or_else(generate_password, Ok)?;
                self.client()?.set_login(&password)?;
                WebInterface::Network(password)
            }
            Exposure::Local => WebInterface::Local(preference.web.login().cloned()),
        };
        self.save(&Preference { web, ..preference })?;
        match self.client().and_then(|client| client.restart()) {
            // Stopped: it starts with the new address anyway.
            Ok(()) | Err(Failure::NotRunning | Failure::NoApiKey) => Ok(()),
            Err(failure) => Err(failure),
        }
    }

    fn new_password(&self) -> Result<(), Failure> {
        let preference = self.preference();
        let password = generate_password()?;
        self.client()?.set_login(&password)?;
        let web = match preference.web {
            WebInterface::Local(_) => WebInterface::Local(Some(password)),
            WebInterface::Network(_) => WebInterface::Network(password),
        };
        self.save(&Preference { web, ..preference })
    }

    /// The status bar's word, without the pending lists and statistics.
    pub fn summary(&self) -> Summary {
        match (self.preference().service, self.card_ready()) {
            (Switch::Off, _) => Summary::Off,
            (Switch::On, false) => Summary::Stopped,
            (Switch::On, true) => match self.client().and_then(|client| client.summary()) {
                Ok(summary) => summary,
                Err(Failure::NotRunning | Failure::NoApiKey) => Summary::Stopped,
                Err(_) => Summary::Error,
            },
        }
    }
}

impl Backend for System {
    fn observe(&self) -> Observation {
        let preference = self.preference();
        let service = match (preference.service, self.card_ready()) {
            (Switch::Off, _) => Service::Off,
            (Switch::On, false) => Service::NoDataCard,
            (Switch::On, true) => match self.client().and_then(|client| client.snapshot(|label, folders| destination(&self.data, label, folders))) {
                Ok(snapshot) => Service::Running(snapshot),
                // Starting for the first time writes config.xml a moment after the process starts.
                Err(Failure::NotRunning | Failure::NoApiKey) => Service::Starting,
                Err(failure) => Service::Failing(failure),
            },
        };
        Observation { service, web: preference.web, address: crate::network::lan_address() }
    }

    fn carry_out(&self, order: &Order) -> Result<(), Failure> {
        match order {
            Order::Switch(service) => self.switch(*service),
            Order::Rescan(folder) => self.client()?.rescan(folder),
            Order::AcceptDevice(device) => self.client()?.accept_device(device),
            Order::IgnoreDevice(device) => self.client()?.ignore_device(device, &clock().format()),
            Order::AcceptFolder(folder, path) => self.client()?.accept_folder(folder, path),
            Order::IgnoreFolder(folder) => self.client()?.ignore_folder(folder, &clock().format()),
            Order::Expose(exposure) => self.expose(*exposure),
            Order::NewPassword => self.new_password(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::model::{FolderId, FolderState};
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("ghazal-syncthing-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn folder(label: &str, path: &Path) -> Folder {
        Folder { id: FolderId::parse(label).unwrap(), label: label.into(), path: path.into(), state: FolderState::UpToDate }
    }

    #[test]
    fn the_network_needs_a_login() {
        let password = Password::parse("abcd-efgh").unwrap();
        assert_eq!(gui_address(&WebInterface::Network(password.clone()), true).to_string(), "0.0.0.0:8384");
        assert_eq!(gui_address(&WebInterface::Network(password.clone()), false).to_string(), "127.0.0.1:8384");
        assert_eq!(gui_address(&WebInterface::Local(Some(password)), true).to_string(), "127.0.0.1:8384");
        assert_eq!(gui_address(&WebInterface::Local(None), false).to_string(), "127.0.0.1:8384");
    }

    #[test]
    fn labels_become_plain_directory_names() {
        assert_eq!(directory_name("Saves").as_deref(), Some("Saves"));
        assert_eq!(directory_name("  PSP / saves ").as_deref(), Some("PSP - saves"));
        assert_eq!(directory_name(".ghazal").as_deref(), Some("ghazal"));
        assert_eq!(directory_name("..").as_deref(), None);
        assert_eq!(directory_name("/").as_deref(), None);
        assert_eq!(directory_name("").as_deref(), None);
    }

    #[test]
    fn folders_land_in_the_cards_directory_of_that_name_in_any_case() {
        let root = scratch("destination");
        std::fs::create_dir_all(root.join("SAVES/file")).unwrap();
        std::fs::create_dir_all(root.join("BIOS")).unwrap();
        std::fs::write(root.join("Notes"), "a file, not a directory").unwrap();
        assert_eq!(destination(&root, "Saves", &[]), Destination::Merge(root.join("SAVES")));
        assert_eq!(destination(&root, "bios", &[]), Destination::Empty(root.join("BIOS")));
        assert_eq!(destination(&root, "Screenshots", &[]), Destination::New(root.join("Screenshots")));
        assert_eq!(destination(&root, "..", &[]), Destination::Unusable);
        // A directory Syncthing already syncs, or one inside it, is not used twice.
        let synced = [folder("Old saves", &root.join("saves"))];
        assert_eq!(destination(&root, "SAVES", &synced), Destination::Taken { path: root.join("SAVES"), by: "Old saves".into() });
        let card = [folder("Everything", &root)];
        assert!(matches!(destination(&root, "Screenshots", &card), Destination::Taken { .. }));
    }
}
