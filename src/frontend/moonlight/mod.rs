//! Game streaming from the menu (Applications → Moonlight): pick a host, found
//! on the network or added by IP address, pair it with a PIN, pick one of its
//! apps and stream it. Moonlight Embedded does the streaming and the protocol;
//! this module drives it, remembers hosts and settings, and shows the state.
//!
//! The screens are an explicit state machine. A button press consumes the
//! current screen and returns the next (`press`), and finished background
//! jobs advance it the same way (`advance`). Nothing here blocks the menu:
//! every network exchange is a job on its own thread, and dropping a screen
//! cancels its job. Nothing starts until the entry is opened, so boot is unaffected.

mod cli;
mod discover;
mod job;
mod store;

use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::time::Duration;

use super::app::navigate;
use super::config::{self, FrontendConfig};
use super::input::Button;
use super::launcher::LaunchSpec;
use crate::device::CpuLoad;
use cli::{Done, Listed, Pin};
use discover::Found;
use job::{Captured, Job, Poll};
use store::{cycle, Choice, Host, Pairing, Store, DEFAULT_PORT};

pub type Hints = &'static [(&'static str, &'static str)];

const HOSTS_HINTS: Hints = &[("A", "Open"), ("B", "Back")];
const ADD_HINTS: Hints = &[("◀▶", "Octet"), ("▲▼", "Value"), ("A", "Add"), ("B", "Cancel")];
const HOST_HINTS: Hints = &[("A", "Select"), ("B", "Back")];
const WORKING_HINTS: Hints = &[("B", "Cancel")];
const APPS_HINTS: Hints = &[("A", "Stream"), ("B", "Back")];
const SETTINGS_HINTS: Hints = &[("◀▶", "Change"), ("B", "Back")];

/// How long the network is asked for hosts.
const SCAN_WINDOW: Duration = Duration::from_millis(2500);
/// Listing apps, unpairing and quitting are quick when the host is up. The
/// first run of any helper also generates the client's RSA key, which takes a
/// few seconds on an A53.
const HELPER_LIMIT: Duration = Duration::from_secs(25);
/// Pairing waits for a person to type the PIN on the host.
const PAIR_LIMIT: Duration = Duration::from_secs(180);
const REACHABLE_WITHIN: Duration = Duration::from_millis(1500);

/// What the UI shows for one row of the list.
pub struct RowText {
    pub label: String,
    pub detail: String,
    pub glyph: &'static str,
}

fn row(label: impl Into<String>, detail: impl Into<String>, glyph: &'static str) -> RowText {
    RowText { label: label.into(), detail: detail.into(), glyph }
}

/// A message for the menu's toast.
pub enum Notice {
    Info(String),
    Error(String),
}

/// What the menu does after a button press.
pub enum Step {
    Stay,
    /// Back to Applications; the session ends.
    Leave,
    /// Run the stream with the display and gamepad handed over.
    Launch(LaunchSpec),
}

/// Work in progress on a host's behalf.
#[derive(Clone, Copy, PartialEq)]
enum Task {
    ListApps,
    Unpair,
    QuitApp,
}

#[derive(Clone, Copy, PartialEq)]
enum HostsRow {
    Host(usize),
    Find,
    Add,
    Settings,
}

#[derive(Clone, Copy, PartialEq)]
enum HostAction {
    Apps,
    Pair,
    Unpair,
    Remove,
}

const HOST_ACTIONS: [HostAction; 4] = [HostAction::Apps, HostAction::Pair, HostAction::Unpair, HostAction::Remove];
/// Where the cursor lands on a host's screen when the next step is pairing.
const PAIR_ROW: usize = 1;

#[derive(Clone, Copy, PartialEq)]
enum Setting {
    Resolution,
    Fps,
    Bitrate,
    QuitAfter,
}

const SETTINGS: [Setting; 4] = [Setting::Resolution, Setting::Fps, Setting::Bitrate, Setting::QuitAfter];

/// An IPv4 address entered one octet at a time, for hosts mDNS does not find.
#[derive(Debug, Clone, Copy, PartialEq)]
struct AddressEditor {
    octets: [u8; 4],
    field: usize,
}

enum Editing {
    Continue(AddressEditor),
    Confirmed(Ipv4Addr),
    Cancelled,
}

impl AddressEditor {
    /// Starts at this device's own address, on the last octet: a host is
    /// usually a few addresses away.
    fn seeded() -> Self {
        Self { octets: local_ipv4().map_or([192, 168, 1, 2], |ip| ip.octets()), field: 3 }
    }

    fn press(self, button: Button) -> Editing {
        let shifted = |delta: i16| {
            let mut octets = self.octets;
            octets[self.field] = (i16::from(octets[self.field]) + delta).rem_euclid(256) as u8;
            Editing::Continue(Self { octets, ..self })
        };
        match button {
            Button::Up => shifted(1),
            Button::Down => shifted(-1),
            Button::R1 => shifted(10),
            Button::L1 => shifted(-10),
            Button::Left => Editing::Continue(Self { field: (self.field + 3) % 4, ..self }),
            Button::Right => Editing::Continue(Self { field: (self.field + 1) % 4, ..self }),
            Button::A => Editing::Confirmed(Ipv4Addr::from(self.octets)),
            Button::B => Editing::Cancelled,
            _ => Editing::Continue(self),
        }
    }
}

impl std::fmt::Display for AddressEditor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let octets: Vec<String> = self.octets.iter().enumerate().map(|(at, octet)| if at == self.field { format!("[{octet}]") } else { octet.to_string() }).collect();
        write!(formatter, "{}", octets.join(" . "))
    }
}

/// This device's LAN address; connecting a UDP socket selects the route
/// without sending anything.
fn local_ipv4() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(address) => Some(address),
        IpAddr::V6(_) => None,
    }
}

/// Whether the host's streaming port answers, so a stream is not started
/// (and the display handed over) against a host that is off. Names are not
/// resolved here, only addresses are checked.
fn reachable(host: &Host) -> bool {
    host.address.as_str().parse::<IpAddr>().map_or(true, |ip| TcpStream::connect_timeout(&SocketAddr::new(ip, host.port), REACHABLE_WITHIN).is_ok())
}

enum Screen {
    Hosts { cursor: usize, scan: Option<Job<Result<Vec<Found>, String>>> },
    AddHost(AddressEditor),
    Host { host: usize, cursor: usize },
    Working { host: usize, task: Task, job: Job<Captured> },
    /// Waiting for the PIN to be typed on the host.
    Pairing { host: usize, pin: Pin, job: Job<Captured> },
    Apps { host: usize, apps: Vec<String>, cursor: usize },
    Settings { cursor: usize },
}

impl Default for Screen {
    fn default() -> Self { Self::Hosts { cursor: 0, scan: None } }
}

pub struct Moonlight {
    config: config::Moonlight,
    launch_after: Vec<PathBuf>,
    store: Store,
    screen: Screen,
    notice: Option<Notice>,
}

impl Moonlight {
    /// `None` on a device whose profile has no Moonlight. With no host known
    /// yet it starts looking for one.
    pub fn open(config: &FrontendConfig) -> Option<Self> {
        let moonlight = config.moonlight.clone()?;
        let (store, warning) = Store::load(&cli::store_path(&moonlight));
        let scan = store.hosts.is_empty().then(|| job::scan(SCAN_WINDOW));
        Some(Self { config: moonlight, launch_after: config.launch_after.clone(), store, screen: Screen::Hosts { cursor: 0, scan }, notice: warning.map(Notice::Error) })
    }

    pub fn take_notice(&mut self) -> Option<Notice> { self.notice.take() }

    /// A helper or the network search is running: the menu must keep polling.
    pub fn busy(&self) -> bool {
        matches!(self.screen, Screen::Hosts { scan: Some(_), .. } | Screen::Working { .. } | Screen::Pairing { .. })
    }

    pub fn handle(&mut self, button: Button) -> Step {
        let current = std::mem::take(&mut self.screen);
        let (screen, step) = self.press(current, button);
        self.screen = screen;
        step
    }

    /// Takes finished jobs' results; true when the screen changed.
    pub fn poll(&mut self) -> bool {
        let current = std::mem::take(&mut self.screen);
        let (screen, changed) = self.advance(current);
        self.screen = screen;
        changed
    }

    fn info(&mut self, text: impl Into<String>) { self.notice = Some(Notice::Info(text.into())); }

    fn error(&mut self, text: impl Into<String>) { self.notice = Some(Notice::Error(text.into())); }

    fn save(&mut self) {
        if let Err(error) = self.store.save(&cli::store_path(&self.config)) { self.error(format!("{error:#}")); }
    }

    fn remember(&mut self, host: usize, pairing: Pairing) {
        if self.store.hosts.get(host).is_some_and(|known| known.pairing != pairing) {
            self.store.set_pairing(host, pairing);
            self.save();
        }
    }

    fn helper(&self, args: Vec<std::ffi::OsString>, limit: Duration) -> Job<Captured> {
        job::run(&self.config.bin, args, &self.config.env, limit)
    }

    fn work(&self, host: usize, task: Task) -> Screen {
        let Some(entry) = self.store.hosts.get(host) else { return Screen::default() };
        let args = match task {
            Task::ListApps => cli::list(&self.config, entry),
            Task::Unpair => cli::unpair(&self.config, entry),
            Task::QuitApp => cli::quit(&self.config, entry),
        };
        Screen::Working { host, task, job: self.helper(args, HELPER_LIMIT) }
    }

    fn hosts_rows(&self) -> Vec<HostsRow> {
        (0..self.store.hosts.len()).map(HostsRow::Host).chain([HostsRow::Find, HostsRow::Add, HostsRow::Settings]).collect()
    }

    fn hosts_cursor(&self, row: HostsRow) -> usize {
        self.hosts_rows().iter().position(|candidate| *candidate == row).unwrap_or(0)
    }

    fn stream(&self, host: usize, app: &str) -> Result<LaunchSpec, String> {
        let entry = self.store.hosts.get(host).ok_or("That host is no longer in the list")?;
        if !reachable(entry) { return Err(format!("{} is not reachable", entry.name)); }
        Ok(LaunchSpec {
            program: self.config.bin.clone(),
            args: cli::stream(&self.config, entry, app, &self.store.stream),
            log: self.config.log.clone(),
            env: self.config.env.iter().map(|(key, value)| (key.clone(), value.clone())).collect(),
            after: self.launch_after.clone(),
            // Software decoding needs every core at full clock.
            cpu: CpuLoad::Full,
        })
    }

    fn press(&mut self, screen: Screen, button: Button) -> (Screen, Step) {
        match screen {
            Screen::Hosts { cursor, scan } => self.press_hosts(cursor, scan, button),
            Screen::AddHost(editor) => self.press_add(editor, button),
            Screen::Host { host, cursor } => self.press_host(host, cursor, button),
            // Backing out drops the job, which cancels it.
            Screen::Working { host, .. } | Screen::Pairing { host, .. } if button == Button::B => (Screen::Host { host, cursor: 0 }, Step::Stay),
            waiting @ (Screen::Working { .. } | Screen::Pairing { .. }) => (waiting, Step::Stay),
            Screen::Apps { host, apps, cursor } => self.press_apps(host, apps, cursor, button),
            Screen::Settings { cursor } => self.press_settings(cursor, button),
        }
    }

    fn press_hosts(&mut self, cursor: usize, scan: Option<Job<Result<Vec<Found>, String>>>, button: Button) -> (Screen, Step) {
        let rows = self.hosts_rows();
        match button {
            Button::B => (Screen::default(), Step::Leave),
            Button::A => match rows.get(cursor) {
                Some(HostsRow::Host(host)) => (Screen::Host { host: *host, cursor: 0 }, Step::Stay),
                Some(HostsRow::Find) if scan.is_none() => (Screen::Hosts { cursor, scan: Some(job::scan(SCAN_WINDOW)) }, Step::Stay),
                Some(HostsRow::Add) => (Screen::AddHost(AddressEditor::seeded()), Step::Stay),
                Some(HostsRow::Settings) => (Screen::Settings { cursor: 0 }, Step::Stay),
                Some(HostsRow::Find) | None => (Screen::Hosts { cursor, scan }, Step::Stay),
            },
            button => (Screen::Hosts { cursor: navigate(cursor, rows.len(), button), scan }, Step::Stay),
        }
    }

    fn press_add(&mut self, editor: AddressEditor, button: Button) -> (Screen, Step) {
        match editor.press(button) {
            Editing::Continue(editor) => (Screen::AddHost(editor), Step::Stay),
            Editing::Cancelled => (Screen::Hosts { cursor: self.hosts_cursor(HostsRow::Add), scan: None }, Step::Stay),
            Editing::Confirmed(address) => {
                let host = self.store.add(Host { name: address.to_string(), address: address.into(), port: DEFAULT_PORT, pairing: Pairing::Unpaired });
                self.save();
                self.info(format!("Added {address}"));
                (Screen::Host { host, cursor: PAIR_ROW }, Step::Stay)
            }
        }
    }

    fn press_host(&mut self, host: usize, cursor: usize, button: Button) -> (Screen, Step) {
        let Some(entry) = self.store.hosts.get(host).cloned() else { return (Screen::default(), Step::Stay) };
        let stay = |screen| (screen, Step::Stay);
        match button {
            Button::B => stay(Screen::Hosts { cursor: host, scan: None }),
            Button::A => match HOST_ACTIONS[cursor.min(HOST_ACTIONS.len() - 1)] {
                HostAction::Apps => stay(self.work(host, Task::ListApps)),
                HostAction::Unpair => stay(self.work(host, Task::Unpair)),
                HostAction::Pair => {
                    let pin = Pin::random();
                    stay(Screen::Pairing { host, pin, job: self.helper(cli::pair(&self.config, &entry, pin), PAIR_LIMIT) })
                }
                HostAction::Remove => {
                    self.store.remove(host);
                    self.save();
                    self.info(format!("Removed {}", entry.name));
                    stay(Screen::Hosts { cursor: host.min(self.store.hosts.len().saturating_sub(1)), scan: None })
                }
            },
            button => stay(Screen::Host { host, cursor: navigate(cursor, HOST_ACTIONS.len(), button) }),
        }
    }

    fn press_apps(&mut self, host: usize, apps: Vec<String>, cursor: usize, button: Button) -> (Screen, Step) {
        match button {
            Button::B => (Screen::Host { host, cursor: 0 }, Step::Stay),
            Button::A => match apps.get(cursor) {
                None => (self.work(host, Task::QuitApp), Step::Stay),
                Some(app) => match self.stream(host, app) {
                    Ok(spec) => (Screen::Apps { host, apps, cursor }, Step::Launch(spec)),
                    Err(message) => {
                        self.error(message);
                        (Screen::Apps { host, apps, cursor }, Step::Stay)
                    }
                },
            },
            button => {
                let cursor = navigate(cursor, apps.len() + 1, button);
                (Screen::Apps { host, apps, cursor }, Step::Stay)
            }
        }
    }

    fn press_settings(&mut self, cursor: usize, button: Button) -> (Screen, Step) {
        match button {
            Button::B => (Screen::Hosts { cursor: self.hosts_cursor(HostsRow::Settings), scan: None }, Step::Stay),
            Button::A | Button::Left | Button::Right => {
                self.change(SETTINGS[cursor.min(SETTINGS.len() - 1)], button != Button::Left);
                (Screen::Settings { cursor }, Step::Stay)
            }
            button => (Screen::Settings { cursor: navigate(cursor, SETTINGS.len(), button) }, Step::Stay),
        }
    }

    fn change(&mut self, setting: Setting, forward: bool) {
        let stream = &mut self.store.stream;
        match setting {
            Setting::Resolution => stream.resolution = cycle(stream.resolution, forward),
            Setting::Fps => stream.fps = cycle(stream.fps, forward),
            Setting::Bitrate => stream.bitrate = cycle(stream.bitrate, forward),
            Setting::QuitAfter => stream.quit_app_after = !stream.quit_app_after,
        }
        self.save();
    }

    fn advance(&mut self, screen: Screen) -> (Screen, bool) {
        match screen {
            Screen::Hosts { cursor, scan: Some(job) } => match job.poll() {
                Poll::Pending => (Screen::Hosts { cursor, scan: Some(job) }, false),
                Poll::Ready(Ok(found)) => {
                    if self.store.merge(&found) > 0 { self.save(); }
                    self.info(match found.len() {
                        0 => "No hosts found; add one by its address".to_owned(),
                        1 => "Found 1 host".to_owned(),
                        count => format!("Found {count} hosts"),
                    });
                    (Screen::Hosts { cursor, scan: None }, true)
                }
                Poll::Ready(Err(message)) => {
                    self.error(format!("Searching: {message}"));
                    (Screen::Hosts { cursor, scan: None }, true)
                }
                Poll::Lost => {
                    self.error("The search stopped unexpectedly");
                    (Screen::Hosts { cursor, scan: None }, true)
                }
            },
            Screen::Working { host, task, job } => match job.poll() {
                Poll::Pending => (Screen::Working { host, task, job }, false),
                Poll::Ready(run) => (self.finished(host, task, &run), true),
                Poll::Lost => {
                    self.error("Moonlight stopped unexpectedly");
                    (Screen::Host { host, cursor: 0 }, true)
                }
            },
            Screen::Pairing { host, pin, job } => match job.poll() {
                Poll::Pending => (Screen::Pairing { host, pin, job }, false),
                Poll::Ready(run) => (self.paired(host, &run), true),
                Poll::Lost => {
                    self.error("Moonlight stopped unexpectedly");
                    (Screen::Host { host, cursor: PAIR_ROW }, true)
                }
            },
            idle => (idle, false),
        }
    }

    fn finished(&mut self, host: usize, task: Task, run: &Captured) -> Screen {
        let name = self.store.hosts.get(host).map_or_else(String::new, |entry| entry.name.clone());
        match task {
            Task::ListApps => match cli::listed(run) {
                Listed::Apps(apps) => {
                    self.remember(host, Pairing::Paired);
                    Screen::Apps { host, apps, cursor: 0 }
                }
                Listed::NotPaired => {
                    self.remember(host, Pairing::Unpaired);
                    self.error(format!("Not paired with {name} yet: choose Pair"));
                    Screen::Host { host, cursor: PAIR_ROW }
                }
                Listed::Failed(reason) => {
                    self.error(reason);
                    Screen::Host { host, cursor: 0 }
                }
            },
            Task::Unpair => {
                match cli::unpaired(run) {
                    Done::Yes => {
                        self.remember(host, Pairing::Unpaired);
                        self.info(format!("Unpaired from {name}"));
                    }
                    Done::Failed(reason) => self.error(format!("Unpairing failed: {reason}")),
                }
                Screen::Host { host, cursor: 0 }
            }
            Task::QuitApp => {
                match cli::quit_failure(run) {
                    None => self.info(format!("Asked {name} to close its app")),
                    Some(reason) => self.error(reason),
                }
                self.work(host, Task::ListApps)
            }
        }
    }

    fn paired(&mut self, host: usize, run: &Captured) -> Screen {
        let name = self.store.hosts.get(host).map_or_else(String::new, |entry| entry.name.clone());
        match cli::paired(run) {
            Done::Yes => {
                self.remember(host, Pairing::Paired);
                self.info(format!("Paired with {name}"));
                self.work(host, Task::ListApps)
            }
            Done::Failed(reason) => {
                self.error(format!("Pairing failed: {reason}"));
                Screen::Host { host, cursor: PAIR_ROW }
            }
        }
    }

    fn host_name(&self, host: usize) -> &str {
        self.store.hosts.get(host).map_or("", |entry| entry.name.as_str())
    }

    pub fn heading(&self) -> String {
        match &self.screen {
            Screen::Hosts { .. } => "Moonlight".to_owned(),
            Screen::AddHost(_) => "Add host".to_owned(),
            Screen::Host { host, .. } | Screen::Working { host, .. } | Screen::Apps { host, .. } => self.host_name(*host).to_owned(),
            Screen::Pairing { host, .. } => format!("Pair with {}", self.host_name(*host)),
            Screen::Settings { .. } => "Stream settings".to_owned(),
        }
    }

    pub fn hints(&self) -> Hints {
        match self.screen {
            Screen::Hosts { .. } => HOSTS_HINTS,
            Screen::AddHost(_) => ADD_HINTS,
            Screen::Host { .. } => HOST_HINTS,
            Screen::Working { .. } | Screen::Pairing { .. } => WORKING_HINTS,
            Screen::Apps { .. } => APPS_HINTS,
            Screen::Settings { .. } => SETTINGS_HINTS,
        }
    }

    pub fn len(&self) -> usize {
        match &self.screen {
            Screen::Hosts { .. } => self.hosts_rows().len(),
            Screen::AddHost(_) | Screen::Working { .. } => 1,
            Screen::Host { .. } => HOST_ACTIONS.len(),
            Screen::Pairing { .. } => 3,
            Screen::Apps { apps, .. } => apps.len() + 1,
            Screen::Settings { .. } => SETTINGS.len(),
        }
    }

    pub fn cursor(&self) -> usize {
        match &self.screen {
            Screen::Hosts { cursor, .. } | Screen::Host { cursor, .. } | Screen::Apps { cursor, .. } | Screen::Settings { cursor } => *cursor,
            Screen::AddHost(_) | Screen::Working { .. } | Screen::Pairing { .. } => 0,
        }
    }

    pub fn row(&self, index: usize) -> Option<RowText> {
        let stream = &self.store.stream;
        match &self.screen {
            Screen::Hosts { scan, .. } => Some(match self.hosts_rows().get(index)? {
                HostsRow::Host(host) => {
                    let entry = self.store.hosts.get(*host)?;
                    let (glyph, state) = match entry.pairing { Pairing::Paired => ("●", "paired"), Pairing::Unpaired => ("○", "not paired") };
                    row(entry.name.as_str(), format!("{}  {state}", entry.address.as_str()), glyph)
                }
                HostsRow::Find => row(if scan.is_some() { "Searching the network…" } else { "Find hosts on the network" }, "", "↻"),
                HostsRow::Add => row("Add host by IP address", "", "+"),
                HostsRow::Settings => row("Stream settings", "", "⚙"),
            }),
            Screen::AddHost(editor) => (index == 0).then(|| row(editor.to_string(), "", "")),
            Screen::Host { .. } => Some(match HOST_ACTIONS.get(index)? {
                HostAction::Apps => row("Games and apps", "Stream one", ""),
                HostAction::Pair => row("Pair", "Shows a PIN to enter on the host", ""),
                HostAction::Unpair => row("Unpair", "", ""),
                HostAction::Remove => row("Remove from list", "", ""),
            }),
            Screen::Working { host, task, .. } => (index == 0).then(|| row(
                match task {
                    Task::ListApps => format!("Asking {} for its apps…", self.host_name(*host)),
                    Task::Unpair => format!("Unpairing from {}…", self.host_name(*host)),
                    Task::QuitApp => format!("Asking {} to close its app…", self.host_name(*host)),
                },
                "", "")),
            Screen::Pairing { pin, .. } => Some(match index {
                0 => row(format!("PIN  {pin}"), "", "#"),
                1 => row("Enter it on the host (Sunshine: PIN tab)", "", ""),
                2 => row("Waiting for the host…", "", ""),
                _ => return None,
            }),
            Screen::Apps { apps, .. } => Some(match apps.get(index) {
                Some(app) => row(app.as_str(), "", ""),
                None if index == apps.len() => row("Close the running app on the host", "", ""),
                None => return None,
            }),
            Screen::Settings { .. } => Some(match SETTINGS.get(index)? {
                Setting::Resolution => row("Resolution", stream.resolution.label(), ""),
                Setting::Fps => row("Frame rate", stream.fps.label(), ""),
                Setting::Bitrate => row("Bitrate", stream.bitrate.label(), ""),
                Setting::QuitAfter => row("Close the app when the stream ends", if stream.quit_app_after { "On" } else { "Off" }, ""),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::store::Address;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::OnceLock;
    use std::time::Instant;

    /// A stand-in for `moonlight` that answers like a paired Sunshine host
    /// whose apps are Desktop and Steam Big Picture (`list` prints them in
    /// reverse, as the real one does).
    fn fake_moonlight() -> &'static PathBuf {
        static BIN: OnceLock<PathBuf> = OnceLock::new();
        BIN.get_or_init(|| {
            use std::os::unix::fs::PermissionsExt;
            let dir = std::env::temp_dir().join(format!("ghazal-moonlight-bin-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("moonlight");
            std::fs::write(&path, r#"#!/bin/sh
case "$1" in
  list)
    echo "Connecting to $2..."
    if [ "$2" = 10.9.9.9 ]; then echo "You must pair with the PC first" >&2; exit 255; fi
    echo "1. Steam Big Picture"; echo "2. Desktop";;
  pair) echo "Please enter the following PIN on the target PC: $5"; sleep 0.05; echo "Succesfully paired";;
  unpair) echo "Succesfully unpaired";;
  quit) ;;
  *) exit 1;;
esac
"#).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        })
    }

    fn host(address: &str, pairing: Pairing) -> Host {
        Host { name: format!("PC {address}"), address: Address::try_from(address.to_owned()).unwrap(), port: DEFAULT_PORT, pairing }
    }

    fn moonlight(hosts: Vec<Host>) -> Moonlight {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let data_dir = std::env::temp_dir().join(format!("ghazal-moonlight-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        let config = config::Moonlight { bin: fake_moonlight().clone(), data_dir, mapping: None, env: Default::default(), log: Some("/var/log/ghazal/moonlight.log".into()) };
        Moonlight { config, launch_after: vec!["/run/ghazal/udev-settled".into()], store: Store { hosts, ..Store::default() }, screen: Screen::default(), notice: None }
    }

    fn settle(app: &mut Moonlight) {
        let until = Instant::now() + Duration::from_secs(10);
        while app.busy() && Instant::now() < until {
            app.poll();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!app.busy(), "still busy");
    }

    fn press(app: &mut Moonlight, buttons: &[Button]) -> Option<LaunchSpec> {
        buttons.iter().find_map(|button| match app.handle(*button) { Step::Launch(spec) => Some(spec), _ => None })
    }

    fn labels(app: &Moonlight) -> Vec<String> { (0..app.len()).filter_map(|at| app.row(at)).map(|row| row.label).collect() }

    fn notice(app: &mut Moonlight) -> Option<String> {
        app.take_notice().map(|notice| match notice { Notice::Info(text) | Notice::Error(text) => text })
    }

    /// Something to connect to, so the stream's reachability check passes.
    fn listening() -> (std::net::TcpListener, Host) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        (listener, Host { port, ..host("127.0.0.1", Pairing::Paired) })
    }

    #[test]
    fn a_paired_host_lists_its_apps_and_streams_one() {
        let (_listener, host) = listening();
        let mut app = moonlight(vec![host]);
        assert_eq!(labels(&app), ["PC 127.0.0.1", "Find hosts on the network", "Add host by IP address", "Stream settings"]);
        press(&mut app, &[Button::A, Button::A]);
        assert!(app.busy());
        settle(&mut app);
        assert_eq!(app.heading(), "PC 127.0.0.1");
        assert_eq!(labels(&app), ["Desktop", "Steam Big Picture", "Close the running app on the host"]);
        assert_eq!(app.store.hosts[0].pairing, Pairing::Paired);
        let spec = press(&mut app, &[Button::Down, Button::A]).expect("a stream");
        let args: Vec<_> = spec.args.iter().map(|arg| arg.to_string_lossy().into_owned()).collect();
        assert_eq!(spec.program, *fake_moonlight());
        assert_eq!(&args[..2], ["stream", "127.0.0.1"]);
        assert!(args.windows(2).any(|pair| pair == ["-app", "Steam Big Picture"]) && args.windows(2).any(|pair| pair == ["-platform", "sdl"]));
        assert_eq!(spec.cpu, CpuLoad::Full);
        assert_eq!(spec.after, [PathBuf::from("/run/ghazal/udev-settled")]);
        // Back unwinds one screen at a time, and leaves Moonlight from the list.
        assert!(matches!(app.handle(Button::B), Step::Stay));
        assert_eq!(app.heading(), "PC 127.0.0.1");
        assert!(matches!(app.handle(Button::B), Step::Stay));
        assert!(matches!(app.handle(Button::B), Step::Leave));
    }

    #[test]
    fn an_unreachable_host_is_reported_before_the_display_is_handed_over() {
        let (listener, host) = listening();
        let mut app = moonlight(vec![host]);
        press(&mut app, &[Button::A, Button::A]);
        settle(&mut app);
        drop(listener);
        assert!(press(&mut app, &[Button::A]).is_none());
        assert!(notice(&mut app).is_some_and(|text| text.contains("is not reachable")));
    }

    #[test]
    fn pairing_shows_a_pin_and_goes_on_to_the_apps() {
        let mut app = moonlight(vec![host("127.0.0.1", Pairing::Unpaired)]);
        press(&mut app, &[Button::A, Button::Down, Button::A]);
        assert_eq!(app.heading(), "Pair with PC 127.0.0.1");
        let pin = app.row(0).unwrap().label;
        assert!(pin.starts_with("PIN  ") && pin[5..].len() == 4 && pin[5..].chars().all(|c| c.is_ascii_digit()), "{pin}");
        assert_eq!(app.cursor(), 0);
        settle(&mut app);
        assert_eq!(labels(&app)[0], "Desktop");
        assert_eq!(app.store.hosts[0].pairing, Pairing::Paired);
        assert_eq!(notice(&mut app).as_deref(), Some("Paired with PC 127.0.0.1"));
        let saved = Store::load(&cli::store_path(&app.config)).0;
        assert_eq!(saved.hosts[0].pairing, Pairing::Paired);
    }

    #[test]
    fn an_unpaired_host_sends_the_user_to_pair() {
        let mut app = moonlight(vec![host("10.9.9.9", Pairing::Paired)]);
        press(&mut app, &[Button::A, Button::A]);
        settle(&mut app);
        assert_eq!(app.heading(), "PC 10.9.9.9");
        assert_eq!(app.cursor(), PAIR_ROW);
        assert_eq!(app.store.hosts[0].pairing, Pairing::Unpaired);
        assert!(notice(&mut app).is_some_and(|text| text.contains("Not paired") && text.contains("choose Pair")));
    }

    #[test]
    fn unpairing_and_removing_update_the_list() {
        let mut app = moonlight(vec![host("127.0.0.1", Pairing::Paired), host("127.0.0.2", Pairing::Paired)]);
        press(&mut app, &[Button::A, Button::Down, Button::Down, Button::A]);
        settle(&mut app);
        assert_eq!(app.store.hosts[0].pairing, Pairing::Unpaired);
        assert_eq!(notice(&mut app).as_deref(), Some("Unpaired from PC 127.0.0.1"));
        // Up wraps to the last action, Remove.
        press(&mut app, &[Button::Up, Button::A]);
        assert_eq!(app.heading(), "Moonlight");
        assert_eq!(app.store.hosts.len(), 1);
        assert_eq!(app.store.hosts[0].address.as_str(), "127.0.0.2");
        assert_eq!(Store::load(&cli::store_path(&app.config)).0.hosts.len(), 1);
    }

    #[test]
    fn closing_the_running_app_refreshes_the_list() {
        let mut app = moonlight(vec![host("127.0.0.1", Pairing::Paired)]);
        press(&mut app, &[Button::A, Button::A]);
        settle(&mut app);
        press(&mut app, &[Button::Up, Button::A]);
        assert!(app.busy());
        settle(&mut app);
        assert_eq!(labels(&app)[0], "Desktop");
        assert_eq!(notice(&mut app).as_deref(), Some("Asked PC 127.0.0.1 to close its app"));
    }

    #[test]
    fn backing_out_of_a_pending_job_returns_to_the_host() {
        let mut app = moonlight(vec![host("127.0.0.1", Pairing::Unpaired)]);
        press(&mut app, &[Button::A, Button::Down, Button::A]);
        assert!(matches!(app.screen, Screen::Pairing { .. }));
        // Other buttons do nothing while waiting; Back cancels.
        app.handle(Button::A);
        assert!(matches!(app.screen, Screen::Pairing { .. }));
        app.handle(Button::B);
        assert!(matches!(app.screen, Screen::Host { host: 0, cursor: 0 }));
        assert!(!app.busy());
    }

    #[test]
    fn a_missing_moonlight_is_reported_not_fatal() {
        let mut app = moonlight(vec![host("127.0.0.1", Pairing::Paired)]);
        app.config.bin = "/nonexistent/moonlight".into();
        press(&mut app, &[Button::A, Button::A]);
        settle(&mut app);
        assert_eq!(app.heading(), "PC 127.0.0.1");
        assert!(notice(&mut app).is_some_and(|text| text.contains("/nonexistent/moonlight")));
    }

    #[test]
    fn an_address_is_dialled_in_octet_by_octet() {
        let editor = AddressEditor { octets: [192, 168, 1, 254], field: 3 };
        let Editing::Continue(editor) = editor.press(Button::Up) else { panic!() };
        let Editing::Continue(editor) = editor.press(Button::Up) else { panic!() };
        assert_eq!(editor.octets, [192, 168, 1, 0]);
        let Editing::Continue(editor) = editor.press(Button::Down) else { panic!() };
        assert_eq!(editor.octets[3], 255);
        let Editing::Continue(editor) = editor.press(Button::L1) else { panic!() };
        assert_eq!(editor.octets[3], 245);
        let Editing::Continue(editor) = editor.press(Button::Left) else { panic!() };
        let Editing::Continue(editor) = editor.press(Button::R1) else { panic!() };
        assert_eq!((editor.field, editor.octets), (2, [192, 168, 11, 245]));
        assert_eq!(editor.to_string(), "192 . 168 . [11] . 245");
        let Editing::Continue(wrapped) = AddressEditor { field: 0, ..editor }.press(Button::Left) else { panic!() };
        assert_eq!(wrapped.field, 3);
        assert!(matches!(editor.press(Button::A), Editing::Confirmed(address) if address == Ipv4Addr::new(192, 168, 11, 245)));
        assert!(matches!(editor.press(Button::B), Editing::Cancelled));
        assert!(matches!(editor.press(Button::X), Editing::Continue(same) if same == editor));
    }

    #[test]
    fn adding_a_host_by_address_leads_to_pairing_it() {
        let mut app = moonlight(vec![]);
        press(&mut app, &[Button::Down, Button::A]);
        assert_eq!(app.heading(), "Add host");
        app.screen = Screen::AddHost(AddressEditor { octets: [10, 0, 0, 7], field: 3 });
        app.handle(Button::A);
        assert_eq!(app.heading(), "PC 10.0.0.7".replace("PC ", ""));
        assert_eq!(app.cursor(), PAIR_ROW);
        assert_eq!(app.store.hosts[0].name, "10.0.0.7");
        assert_eq!(Store::load(&cli::store_path(&app.config)).0.hosts[0].address.as_str(), "10.0.0.7");
        // Adding the same address again keeps one entry.
        app.screen = Screen::AddHost(AddressEditor { octets: [10, 0, 0, 7], field: 3 });
        app.handle(Button::A);
        assert_eq!(app.store.hosts.len(), 1);
    }

    #[test]
    fn cancelling_the_address_returns_to_the_list() {
        let mut app = moonlight(vec![]);
        press(&mut app, &[Button::Down, Button::A, Button::B]);
        assert_eq!(app.heading(), "Moonlight");
        assert_eq!(app.cursor(), 1);
    }

    #[test]
    fn settings_cycle_and_persist() {
        let mut app = moonlight(vec![]);
        press(&mut app, &[Button::Up, Button::A]);
        assert_eq!(app.heading(), "Stream settings");
        let details = |app: &Moonlight| (0..app.len()).filter_map(|at| app.row(at)).map(|row| row.detail).collect::<Vec<_>>();
        assert_eq!(details(&app), ["640x360", "60 fps", "5 Mbps", "Off"]);
        press(&mut app, &[Button::Right, Button::Down, Button::Left, Button::Down, Button::Right, Button::Down, Button::A]);
        assert_eq!(details(&app), ["960x540", "30 fps", "10 Mbps", "On"]);
        assert_eq!(Store::load(&cli::store_path(&app.config)).0.stream, app.store.stream);
        app.handle(Button::B);
        assert_eq!(app.heading(), "Moonlight");
        // Back lands on the Stream settings row, after Find and Add.
        assert_eq!(app.cursor(), 2);
    }

    #[test]
    fn rows_cursor_and_hints_follow_the_screen() {
        let mut app = moonlight(vec![host("127.0.0.1", Pairing::Unpaired)]);
        assert_eq!(app.row(0).unwrap().detail, "127.0.0.1  not paired");
        assert!(app.row(4).is_none());
        assert_eq!(app.hints(), HOSTS_HINTS);
        press(&mut app, &[Button::Up]);
        assert_eq!(app.cursor(), 3);
        press(&mut app, &[Button::Down, Button::A]);
        assert_eq!(app.hints(), HOST_HINTS);
        assert_eq!(labels(&app), ["Games and apps", "Pair", "Unpair", "Remove from list"]);
    }

    #[test]
    fn moonlight_is_off_without_a_profile_for_it() {
        let text = "rom_roots = []\n[retroarch]\nbin = \"/usr/bin/retroarch\"\ncore_dirs = []\n";
        let config: FrontendConfig = toml::from_str(text).unwrap();
        assert!(Moonlight::open(&config).is_none());
        let with = format!("{text}[moonlight]\nbin = \"/usr/bin/moonlight\"\ndata_dir = \"/nonexistent/moonlight\"\n");
        let config: FrontendConfig = toml::from_str(&with).unwrap();
        let app = Moonlight::open(&config).expect("configured");
        // No host is known yet, so it starts looking for one.
        assert!(matches!(app.screen, Screen::Hosts { scan: Some(_), .. }));
    }
}
