//! The Syncthing screen as a state machine. There is no I/O here: `step` maps a
//! screen and an event (a button, or what the worker learned or did) to the
//! next screen and at most one command for the controller to run.
//!
//! Every state that can return to the list keeps it (`Browse`) inside, so the
//! list lives in one place and refreshes keep it current underneath.

use std::time::Duration;

use crate::frontend::app::navigate;
use crate::frontend::input::Button;
use crate::syncthing::{Destination, DeviceId, Exposure, Failure, FolderId, LastSeen, Listening, Observation, Order, Password, PendingDevice, PendingFolder, Presence, Service, Snapshot, Summary, Switch, Timestamp, WebInterface};

pub type Hints = &'static [(&'static str, &'static str)];

#[derive(Clone, Debug)]
pub enum Screen {
    /// The first observation is on its way.
    Loading,
    Browse(Browse),
    Confirm(Confirm),
    Working(Working),
    /// B on the list: the menu leaves the screen.
    Closed,
}

/// What the cursor is on. Holding IDs instead of a row number keeps the cursor
/// on its folder or device when a refresh changes the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selection {
    Service,
    Me,
    PendingDevice(DeviceId),
    PendingFolder(FolderId),
    Folder(FolderId),
    Device(DeviceId),
    Web,
    Login,
}

#[derive(Clone, Debug)]
pub struct Browse {
    observation: Observation,
    /// The player's row. A restart lists only the switch for a few seconds;
    /// the row stays chosen and is selected again when the list returns.
    selection: Selection,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Question {
    /// Accepting a folder into a directory that already has files.
    Merge(PendingFolder, std::path::PathBuf),
    IgnoreDevice(PendingDevice),
    IgnoreFolder(PendingFolder),
    NewPassword,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Choice {
    Yes,
    No,
}

#[derive(Clone, Debug)]
pub struct Confirm {
    back: Browse,
    question: Question,
    /// Starts on No: a stray A press must not merge folders.
    choice: Choice,
}

#[derive(Clone, Debug)]
pub struct Working {
    back: Browse,
    order: Order,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Button(Button),
    Observed(Observation),
    Done(Order, Result<(), Failure>),
}

/// Work for the worker thread; each command answers with one event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Observe,
    CarryOut(Order),
}

/// A message for the toast line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    Info(String),
    Error(String),
}

impl Notice {
    #[cfg(test)]
    pub fn text(&self) -> &str {
        match self { Self::Info(text) | Self::Error(text) => text }
    }
}

#[derive(Debug)]
pub struct Step {
    pub screen: Screen,
    pub command: Option<Command>,
    pub notice: Option<Notice>,
}

impl Step {
    fn to(screen: Screen) -> Self { Self { screen, command: None, notice: None } }
    fn browse(browse: Browse) -> Self { Self::to(Screen::Browse(browse)) }
    fn run(mut self, command: Command) -> Self { self.command = Some(command); self }
    fn say(mut self, notice: Notice) -> Self { self.notice = Some(notice); self }
    fn work(back: Browse, order: Order) -> Self { Self::to(Screen::Working(Working { back, order: order.clone() })).run(Command::CarryOut(order)) }
    fn ask(back: Browse, question: Question) -> Self { Self::to(Screen::Confirm(Confirm { back, question, choice: Choice::No })) }
}

fn entries(observation: &Observation) -> Vec<Selection> {
    let Some(snapshot) = observation.service.snapshot() else { return vec![Selection::Service] };
    [Selection::Service, Selection::Me].into_iter()
        .chain(snapshot.pending_devices.iter().map(|device| Selection::PendingDevice(device.id.clone())))
        .chain(snapshot.pending_folders.iter().map(|folder| Selection::PendingFolder(folder.id.clone())))
        .chain(snapshot.folders.iter().map(|folder| Selection::Folder(folder.id.clone())))
        .chain(snapshot.devices.iter().map(|device| Selection::Device(device.id.clone())))
        .chain([Selection::Web])
        .chain(observation.web.login().map(|_| Selection::Login))
        .collect()
}

impl Browse {
    fn new(observation: Observation) -> Self { Self { observation, selection: Selection::Service } }

    fn entries(&self) -> Vec<Selection> { entries(&self.observation) }

    fn cursor(&self) -> usize { self.entries().iter().position(|entry| *entry == self.selection).unwrap_or(0) }

    /// The row the cursor is on: the player's, or the switch while the list is hidden.
    fn selected(&self) -> Selection {
        if self.entries().contains(&self.selection) { self.selection.clone() } else { Selection::Service }
    }

    fn snapshot(&self) -> Option<&Snapshot> { self.observation.service.snapshot() }

    /// Takes a new observation. When the selected row is gone from a listed
    /// Syncthing (an accepted request, a removed folder) the cursor stays where
    /// it was on the screen.
    fn accept(&mut self, observation: Observation) {
        let cursor = self.cursor();
        self.observation = observation;
        let entries = self.entries();
        if self.snapshot().is_some() && !entries.contains(&self.selection) {
            self.selection = entries.get(cursor).or(entries.last()).cloned().unwrap_or(Selection::Service);
        }
    }
}

impl Screen {
    fn browse_mut(&mut self) -> Option<&mut Browse> {
        match self {
            Self::Browse(browse) | Self::Confirm(Confirm { back: browse, .. }) | Self::Working(Working { back: browse, .. }) => Some(browse),
            Self::Loading | Self::Closed => None,
        }
    }

    pub fn closed(&self) -> bool { matches!(self, Self::Closed) }

    /// Whether the list is worth refreshing now, and how soon.
    pub fn refresh_interval(&self) -> Option<Duration> {
        let Self::Browse(browse) = self else { return None };
        let changing = match &browse.observation.service {
            Service::Starting => true,
            Service::Running(snapshot) => snapshot.folders.iter().any(|folder| folder.state.busy()) || restarting(&browse.observation.web, snapshot.listening),
            Service::Off | Service::NoDataCard | Service::Failing(_) => false,
        };
        Some(Duration::from_secs(if changing { 2 } else { 5 }))
    }

    /// What the status icon shows, when this screen knows it.
    pub fn summary(&self) -> Option<Summary> {
        match self {
            Self::Browse(browse) | Self::Confirm(Confirm { back: browse, .. }) | Self::Working(Working { back: browse, .. }) => Some(browse.observation.service.summary()),
            Self::Loading | Self::Closed => None,
        }
    }
}

/// The player chose one address and Syncthing still listens on the other.
fn restarting(web: &WebInterface, listening: Listening) -> bool {
    !matches!((web.exposure(), listening), (Exposure::Local, Listening::Local) | (Exposure::Network, Listening::Network))
}

pub fn step(screen: Screen, event: Event) -> Step {
    match event {
        Event::Button(button) => on_button(screen, button),
        Event::Observed(observation) => on_observed(screen, observation),
        Event::Done(order, result) => on_done(screen, &order, result),
    }
}

fn on_button(screen: Screen, button: Button) -> Step {
    match screen {
        Screen::Loading if button == Button::B => Step::to(Screen::Closed),
        Screen::Loading | Screen::Closed => Step::to(screen),
        Screen::Browse(browse) => browse_button(browse, button),
        Screen::Confirm(confirm) => confirm_button(confirm, button),
        // Orders take a second at most; there is nothing to cancel.
        Screen::Working(_) => Step::to(screen),
    }
}

fn browse_button(mut browse: Browse, button: Button) -> Step {
    match button {
        Button::B => Step::to(Screen::Closed),
        Button::Up | Button::Down | Button::L1 | Button::R1 => {
            let entries = browse.entries();
            if let Some(entry) = entries.get(navigate(browse.cursor(), entries.len(), button)) { browse.selection = entry.clone(); }
            Step::browse(browse)
        }
        Button::A => activate(browse),
        Button::Y => ignore(browse),
        Button::X | Button::Left | Button::Right | Button::L2 | Button::R2 | Button::Select | Button::Start | Button::Menu => Step::browse(browse),
    }
}

fn activate(browse: Browse) -> Step {
    let selection = browse.selected();
    let observation = &browse.observation;
    let snapshot = browse.snapshot();
    match selection {
        Selection::Service => {
            let switch = match observation.service { Service::Off => Switch::On, Service::NoDataCard | Service::Starting | Service::Failing(_) | Service::Running(_) => Switch::Off };
            Step::work(browse, Order::Switch(switch))
        }
        // The QR code is beside the list; A spells the ID out for small screens.
        Selection::Me => match snapshot.map(|snapshot| snapshot.me.id.to_string()) {
            Some(id) => Step::browse(browse).say(Notice::Info(id)),
            None => Step::browse(browse),
        },
        Selection::PendingDevice(id) => match snapshot.and_then(|snapshot| snapshot.pending_device(&id)).cloned() {
            Some(device) => Step::work(browse, Order::AcceptDevice(device)),
            None => Step::browse(browse),
        },
        Selection::PendingFolder(id) => match snapshot.and_then(|snapshot| snapshot.pending_folder(&id)).cloned() {
            Some(folder) => accept_folder(browse, folder),
            None => Step::browse(browse),
        },
        Selection::Folder(id) => Step::work(browse, Order::Rescan(id)),
        Selection::Device(_) => Step::browse(browse),
        Selection::Web => {
            let exposure = match observation.web.exposure() { Exposure::Local => Exposure::Network, Exposure::Network => Exposure::Local };
            Step::work(browse, Order::Expose(exposure))
        }
        Selection::Login => Step::ask(browse, Question::NewPassword),
    }
}

fn accept_folder(browse: Browse, folder: PendingFolder) -> Step {
    match folder.destination.clone() {
        Destination::New(path) | Destination::Empty(path) => Step::work(browse, Order::AcceptFolder(folder, path)),
        Destination::Merge(path) => Step::ask(browse, Question::Merge(folder, path)),
        Destination::Taken { path, by } => {
            let notice = Notice::Error(format!("{} already syncs as {by}; accept it in the web interface", path.display()));
            Step::browse(browse).say(notice)
        }
        Destination::Unusable => {
            let notice = Notice::Error(format!("\"{}\" can't name a folder; accept it in the web interface", folder.label));
            Step::browse(browse).say(notice)
        }
    }
}

fn ignore(browse: Browse) -> Step {
    let snapshot = browse.snapshot();
    let question = match &browse.selected() {
        Selection::PendingDevice(id) => snapshot.and_then(|snapshot| snapshot.pending_device(id)).cloned().map(Question::IgnoreDevice),
        Selection::PendingFolder(id) => snapshot.and_then(|snapshot| snapshot.pending_folder(id)).cloned().map(Question::IgnoreFolder),
        Selection::Service | Selection::Me | Selection::Folder(_) | Selection::Device(_) | Selection::Web | Selection::Login => None,
    };
    match question {
        Some(question) => Step::ask(browse, question),
        None => Step::browse(browse),
    }
}

fn confirm_button(mut confirm: Confirm, button: Button) -> Step {
    match (button, confirm.choice) {
        (Button::B, _) | (Button::A, Choice::No) => Step::browse(confirm.back),
        (Button::A, Choice::Yes) => {
            let order = match confirm.question {
                Question::Merge(folder, path) => Order::AcceptFolder(folder, path),
                Question::IgnoreDevice(device) => Order::IgnoreDevice(device),
                Question::IgnoreFolder(folder) => Order::IgnoreFolder(folder),
                Question::NewPassword => Order::NewPassword,
            };
            Step::work(confirm.back, order)
        }
        (Button::Up | Button::Down, _) => {
            confirm.choice = match confirm.choice { Choice::Yes => Choice::No, Choice::No => Choice::Yes };
            Step::to(Screen::Confirm(confirm))
        }
        (Button::X | Button::Y | Button::Left | Button::Right | Button::L1 | Button::R1 | Button::L2 | Button::R2 | Button::Select | Button::Start | Button::Menu, _) => Step::to(Screen::Confirm(confirm)),
    }
}

fn on_observed(mut screen: Screen, observation: Observation) -> Step {
    match screen {
        Screen::Loading => Step::browse(Browse::new(observation)),
        Screen::Closed => Step::to(Screen::Closed),
        Screen::Browse(_) | Screen::Confirm(_) | Screen::Working(_) => {
            if let Some(browse) = screen.browse_mut() { browse.accept(observation); }
            Step::to(screen)
        }
    }
}

fn on_done(screen: Screen, order: &Order, result: Result<(), Failure>) -> Step {
    let notice = match result {
        Ok(()) => Notice::Info(done(order)),
        Err(failure) => Notice::Error(failure.message()),
    };
    match screen {
        Screen::Working(working) => Step::browse(working.back).run(Command::Observe).say(notice),
        // Nothing else waits for an order; the list just catches up.
        screen @ (Screen::Browse(_) | Screen::Confirm(_)) => Step::to(screen).run(Command::Observe),
        screen @ (Screen::Loading | Screen::Closed) => Step::to(screen),
    }
}

fn done(order: &Order) -> String {
    match order {
        Order::Switch(Switch::On) => "Syncthing is starting".into(),
        Order::Switch(Switch::Off) => "Syncthing is off".into(),
        Order::Rescan(folder) => format!("Rescanning {folder}"),
        Order::AcceptDevice(device) => format!("Added {}; its folders appear here when it shares them", device.title()),
        Order::IgnoreDevice(device) => format!("Ignoring {}", device.title()),
        Order::AcceptFolder(folder, path) => format!("Syncing {} in {}", folder.label, path.display()),
        Order::IgnoreFolder(folder) => format!("Ignoring {}", folder.label),
        Order::Expose(Exposure::Network) => "Web interface is restarting on the network".into(),
        Order::Expose(Exposure::Local) => "Web interface is back on this handheld only".into(),
        Order::NewPassword => "New web interface password set".into(),
    }
}

// What the menu draws.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowView {
    pub label: String,
    pub detail: String,
    pub glyph: &'static str,
}

#[derive(Clone, Debug)]
pub struct View {
    pub rows: Vec<RowView>,
    pub cursor: usize,
    pub hints: Hints,
    /// This handheld's ID while its row is selected: the panel beside the list
    /// shows it as a QR code.
    pub device_id: Option<DeviceId>,
}

const SERVICE_HINTS: Hints = &[("A", "Turn on/off"), ("B", "Back")];
const ME_HINTS: Hints = &[("A", "Show ID"), ("B", "Back")];
const PENDING_HINTS: Hints = &[("A", "Accept"), ("Y", "Ignore"), ("B", "Back")];
const FOLDER_HINTS: Hints = &[("A", "Rescan"), ("B", "Back")];
const WEB_HINTS: Hints = &[("A", "Turn on/off"), ("B", "Back")];
const LOGIN_HINTS: Hints = &[("A", "New password"), ("B", "Back")];
const BACK_HINTS: Hints = &[("B", "Back")];
const CONFIRM_HINTS: Hints = &[("A", "Choose"), ("B", "No")];
const WAIT_HINTS: Hints = &[];

/// "Last seen 5 min ago" and the like.
fn last_seen(seen: LastSeen, now: Timestamp) -> String {
    let LastSeen::At(time) = seen else { return "Never connected".into() };
    let seconds = (now.unix() - time.unix()).max(0);
    match seconds {
        0..=59 => "Last seen just now".into(),
        60..=3599 => format!("Last seen {} min ago", seconds / 60),
        3600..=86_399 => format!("Last seen {} h ago", seconds / 3600),
        86_400..=172_799 => "Last seen yesterday".into(),
        _ => format!("Last seen {} days ago", seconds / 86_400),
    }
}

fn service_detail(observation: &Observation) -> String {
    match &observation.service {
        Service::Off => "Off".into(),
        Service::NoDataCard => "Waiting for the data card".into(),
        Service::Starting => "Starting…".into(),
        Service::Failing(failure) => failure.message(),
        Service::Running(snapshot) => match snapshot.summary() {
            Summary::Syncing => "On, syncing".into(),
            Summary::Error => "On, needs attention".into(),
            Summary::NoDevices => "On, no device connected".into(),
            Summary::UpToDate | Summary::Off | Summary::Stopped => "On".into(),
        },
    }
}

fn web_detail(observation: &Observation, listening: Listening) -> String {
    match (observation.web.exposure(), listening, observation.address) {
        (Exposure::Local, Listening::Local, _) => "Off".into(),
        (Exposure::Local, Listening::Network, _) => "Leaving the network…".into(),
        // Restarting, or held back by `serve` because Syncthing has no login.
        (Exposure::Network, Listening::Local, _) => "Not on the network yet".into(),
        (Exposure::Network, Listening::Network, Some(address)) => format!("http://{address}:8384"),
        (Exposure::Network, Listening::Network, None) => "On, not on a network".into(),
    }
}

fn entry_row(observation: &Observation, snapshot: Option<&Snapshot>, entry: &Selection, now: Timestamp) -> RowView {
    let row = |label: String, detail: String, glyph| RowView { label, detail, glyph };
    let missing = || row(String::new(), String::new(), "");
    match (entry, snapshot) {
        (Selection::Service, _) => row("Syncthing".into(), service_detail(observation), ""),
        (Selection::Me, Some(snapshot)) => row("This device".into(), snapshot.me.name.clone(), "◎"),
        (Selection::PendingDevice(id), Some(snapshot)) => snapshot.pending_device(id).map_or_else(missing, |device| row(format!("New device: {}", device.title()), "Wants to connect".into(), "!")),
        (Selection::PendingFolder(id), Some(snapshot)) => snapshot.pending_folder(id).map_or_else(missing, |folder| {
            let detail = match &folder.destination {
                Destination::New(_) | Destination::Empty(_) => "Wants to share",
                Destination::Merge(_) => "Wants to share",
                Destination::Taken { .. } => "Already synced",
                Destination::Unusable => "Needs another name",
            };
            row(format!("{} from {}", folder.label, snapshot.device_name(&folder.offered_by.first)), detail.into(), "!")
        }),
        (Selection::Folder(id), Some(snapshot)) => snapshot.folder(id).map_or_else(missing, |folder| row(folder.label.clone(), folder.state.describe(), "▤")),
        (Selection::Device(id), Some(snapshot)) => snapshot.device(id).map_or_else(missing, |device| match device.presence {
            Presence::Connected => row(device.name.clone(), "Connected".into(), "●"),
            Presence::Paused => row(device.name.clone(), "Paused".into(), "○"),
            Presence::Away(seen) => row(device.name.clone(), last_seen(seen, now), "○"),
        }),
        (Selection::Web, Some(snapshot)) => row("Web interface on the network".into(), web_detail(observation, snapshot.listening), ""),
        (Selection::Login, _) => row("Web interface login".into(), observation.web.login().map_or_else(String::new, |password| format!("{} / {}", Password::USER, password.expose())), ""),
        (Selection::Me | Selection::PendingDevice(_) | Selection::PendingFolder(_) | Selection::Folder(_) | Selection::Device(_) | Selection::Web, None) => missing(),
    }
}

fn browse_rows(browse: &Browse, now: Timestamp) -> Vec<RowView> {
    browse.entries().iter().map(|entry| entry_row(&browse.observation, browse.snapshot(), entry, now)).collect()
}

fn browse_hints(browse: &Browse) -> Hints {
    match browse.selected() {
        Selection::Service => SERVICE_HINTS,
        Selection::Me => ME_HINTS,
        Selection::PendingDevice(_) | Selection::PendingFolder(_) => PENDING_HINTS,
        Selection::Folder(_) => FOLDER_HINTS,
        Selection::Device(_) => BACK_HINTS,
        Selection::Web => WEB_HINTS,
        Selection::Login => LOGIN_HINTS,
    }
}

/// The question's lines, then what Yes says.
fn question_rows(question: &Question) -> (Vec<String>, &'static str) {
    match question {
        Question::Merge(folder, path) => (vec![format!("{} already has files", path.display()), format!("Merged with {}, both keep all of them", folder.label)], "Yes, merge"),
        Question::IgnoreDevice(device) => (vec![format!("Ignore {} from now on?", device.title())], "Yes, ignore it"),
        Question::IgnoreFolder(folder) => (vec![format!("Ignore {} from now on?", folder.label)], "Yes, ignore it"),
        Question::NewPassword => (vec!["Replace the web interface password?".into()], "Yes, make a new one"),
    }
}

fn working_text(order: &Order) -> String {
    match order {
        Order::Switch(Switch::On) => "Starting Syncthing…".into(),
        Order::Switch(Switch::Off) => "Stopping Syncthing…".into(),
        Order::Rescan(folder) => format!("Rescanning {folder}…"),
        Order::AcceptDevice(device) => format!("Adding {}…", device.title()),
        Order::IgnoreDevice(device) => format!("Ignoring {}…", device.title()),
        Order::AcceptFolder(folder, _) => format!("Adding {}…", folder.label),
        Order::IgnoreFolder(folder) => format!("Ignoring {}…", folder.label),
        Order::Expose(_) => "Restarting the web interface…".into(),
        Order::NewPassword => "Setting a new password…".into(),
    }
}

pub fn view(screen: &Screen, now: Timestamp) -> View {
    let plain = |rows: Vec<RowView>, cursor, hints| View { rows, cursor, hints, device_id: None };
    let row = |label: String, glyph| RowView { label, detail: String::new(), glyph };
    match screen {
        Screen::Loading | Screen::Closed => plain(vec![row("Asking Syncthing…".into(), "◌")], 0, BACK_HINTS),
        Screen::Browse(browse) => View {
            rows: browse_rows(browse, now),
            cursor: browse.cursor(),
            hints: browse_hints(browse),
            device_id: browse.snapshot().filter(|_| browse.selected() == Selection::Me).map(|snapshot| snapshot.me.id.clone()),
        },
        Screen::Confirm(confirm) => {
            let (question, yes) = question_rows(&confirm.question);
            let cursor = question.len() + match confirm.choice { Choice::Yes => 0, Choice::No => 1 };
            let rows = question.into_iter().enumerate().map(|(line, text)| row(text, if line == 0 { "?" } else { "" })).chain([row(yes.into(), ""), row("No".into(), "")]).collect();
            plain(rows, cursor, CONFIRM_HINTS)
        }
        Screen::Working(working) => plain(vec![row(working_text(&working.order), "◌")], 0, WAIT_HINTS),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syncthing::{Device, Folder, FolderState, Me, Offerers};

    const ME: &str = "HJ56AJG-RR4EN7L-B2WJ3UI-2NNTZCS-2PZ4P3W-R3SZJHF-S25SDML-LBSRGQI";
    const PC: &str = "VXRHI74-4BCA6TM-PVEKYIJ-CWZMMHF-7S4PYPE-AYZMM36-3DNWBSQ-LYPK7QK";
    const PHONE: &str = "IWP6Z7O-5JWUTS3-NJT7WZP-NRR2JSL-PSEOQBK-DFR2AFD-BOOPH7X-3HOMRAR";
    const NOW: i64 = 1_793_630_414;

    fn device_id(text: &str) -> DeviceId { DeviceId::parse(text).unwrap() }
    fn folder_id(text: &str) -> FolderId { FolderId::parse(text).unwrap() }

    fn pending_folder(id: &str, label: &str, destination: Destination) -> PendingFolder {
        PendingFolder { id: folder_id(id), label: label.into(), offered_by: Offerers { first: device_id(PC), others: Vec::new() }, destination }
    }

    fn snapshot() -> Snapshot {
        Snapshot {
            me: Me { id: device_id(ME), name: "ghazal".into() },
            folders: vec![
                Folder { id: folder_id("saves"), label: "Saves".into(), path: "/mnt/games/SAVES".into(), state: FolderState::Syncing { percent: 42 } },
                Folder { id: folder_id("bios"), label: "BIOS".into(), path: "/mnt/games/BIOS".into(), state: FolderState::UpToDate },
            ],
            devices: vec![
                Device { id: device_id(PC), name: "Desktop PC".into(), presence: Presence::Connected },
                Device { id: device_id(PHONE), name: "Phone".into(), presence: Presence::Away(LastSeen::At(Timestamp::from_unix(NOW - 3 * 3600))) },
            ],
            pending_devices: vec![PendingDevice { id: device_id(PHONE), name: "Laptop".into(), address: "192.168.1.20:22000".into() }],
            pending_folders: vec![
                pending_folder("roms", "ROMS", Destination::Merge("/mnt/games/ROMS".into())),
                pending_folder("shots", "Screenshots", Destination::New("/mnt/games/Screenshots".into())),
                pending_folder("old", "Saves", Destination::Taken { path: "/mnt/games/SAVES".into(), by: "Saves".into() }),
            ],
            listening: Listening::Local,
        }
    }

    fn observation(service: Service) -> Observation {
        Observation { service, web: WebInterface::Local(None), address: Some("192.168.1.5".parse().unwrap()) }
    }

    /// Plays events through the machine, collecting what it asks for.
    struct Run {
        screen: Screen,
        commands: Vec<Command>,
        notices: Vec<Notice>,
    }

    impl Run {
        fn from(screen: Screen) -> Self { Self { screen, commands: Vec::new(), notices: Vec::new() } }
        fn browsing() -> Self { Self::from(Screen::Browse(Browse::new(observation(Service::Running(snapshot()))))) }
        fn event(mut self, event: Event) -> Self {
            let step = step(self.screen, event);
            self.screen = step.screen;
            self.commands.extend(step.command);
            self.notices.extend(step.notice);
            self
        }
        fn press(self, buttons: &[Button]) -> Self { buttons.iter().fold(self, |run, button| run.event(Event::Button(*button))) }
        fn select(self, target: Selection) -> Self {
            let mut run = self;
            for _ in 0..30 {
                if run.browse().selected() == target { return run; }
                run = run.press(&[Button::Down]);
            }
            panic!("never reached {target:?}");
        }
        fn browse(&self) -> &Browse {
            match &self.screen {
                Screen::Browse(browse) | Screen::Confirm(Confirm { back: browse, .. }) | Screen::Working(Working { back: browse, .. }) => browse,
                Screen::Loading | Screen::Closed => panic!("no list"),
            }
        }
        fn last_order(&self) -> Option<&Order> {
            self.commands.iter().rev().find_map(|command| match command { Command::CarryOut(order) => Some(order), Command::Observe => None })
        }
        fn labels(&self) -> Vec<(String, String)> {
            view(&self.screen, Timestamp::from_unix(NOW)).rows.into_iter().map(|row| (row.label, row.detail)).collect()
        }
    }

    #[test]
    fn the_list_puts_requests_before_folders_and_devices() {
        let labels = Run::browsing().labels();
        let expected = [
            ("Syncthing", "On, syncing"),
            ("This device", "ghazal"),
            ("New device: Laptop", "Wants to connect"),
            ("ROMS from Desktop PC", "Wants to share"),
            ("Screenshots from Desktop PC", "Wants to share"),
            ("Saves from Desktop PC", "Already synced"),
            ("Saves", "Syncing 42%"),
            ("BIOS", "Up to date"),
            ("Desktop PC", "Connected"),
            ("Phone", "Last seen 3 h ago"),
            ("Web interface on the network", "Off"),
        ];
        assert_eq!(labels, expected.map(|(label, detail)| (label.to_owned(), detail.to_owned())));
    }

    #[test]
    fn a_stopped_syncthing_shows_only_its_switch() {
        for (service, detail) in [(Service::Off, "Off"), (Service::Starting, "Starting…"), (Service::NoDataCard, "Waiting for the data card"), (Service::Failing(Failure::Refused), "Syncthing refused the menu's API key")] {
            let run = Run::from(Screen::Loading).event(Event::Observed(observation(service)));
            assert_eq!(run.labels(), [("Syncthing".to_owned(), detail.to_owned())]);
        }
    }

    #[test]
    fn a_turns_syncthing_on_and_off() {
        let run = Run::from(Screen::Browse(Browse::new(observation(Service::Off)))).press(&[Button::A]);
        assert_eq!(run.last_order(), Some(&Order::Switch(Switch::On)));
        assert!(matches!(run.screen, Screen::Working(_)));
        let run = run.event(Event::Done(Order::Switch(Switch::On), Ok(())));
        assert!(matches!(run.screen, Screen::Browse(_)));
        assert_eq!(run.commands.last(), Some(&Command::Observe));
        assert_eq!(run.notices.last().map(Notice::text), Some("Syncthing is starting"));
        assert_eq!(Run::browsing().press(&[Button::A]).last_order(), Some(&Order::Switch(Switch::Off)));
    }

    #[test]
    fn this_devices_row_puts_its_id_beside_the_list() {
        let run = Run::browsing().select(Selection::Me);
        assert_eq!(view(&run.screen, Timestamp::from_unix(NOW)).device_id, Some(device_id(ME)));
        assert_eq!(view(&run.press(&[Button::Down]).screen, Timestamp::from_unix(NOW)).device_id, None);
        let run = Run::browsing().select(Selection::Me).press(&[Button::A]);
        assert_eq!(run.notices.last().map(Notice::text), Some(ME));
    }

    #[test]
    fn a_pending_device_is_accepted_or_ignored_after_asking() {
        let run = Run::browsing().select(Selection::PendingDevice(device_id(PHONE)));
        let accepted = Run::from(run.screen.clone()).press(&[Button::A]);
        assert!(matches!(accepted.last_order(), Some(Order::AcceptDevice(device)) if device.name == "Laptop"));
        let asked = run.press(&[Button::Y]);
        assert!(matches!(asked.screen, Screen::Confirm(_)));
        assert!(asked.last_order().is_none());
        // The question starts on No.
        assert!(Run::from(asked.screen.clone()).press(&[Button::A]).last_order().is_none());
        let ignored = asked.press(&[Button::Up, Button::A]);
        assert!(matches!(ignored.last_order(), Some(Order::IgnoreDevice(device)) if device.name == "Laptop"));
    }

    #[test]
    fn a_folder_into_a_new_directory_is_accepted_at_once() {
        let run = Run::browsing().select(Selection::PendingFolder(folder_id("shots"))).press(&[Button::A]);
        assert!(matches!(run.last_order(), Some(Order::AcceptFolder(folder, path)) if folder.label == "Screenshots" && path.ends_with("Screenshots")));
    }

    #[test]
    fn merging_into_a_directory_with_files_asks_first() {
        let run = Run::browsing().select(Selection::PendingFolder(folder_id("roms"))).press(&[Button::A]);
        assert!(run.last_order().is_none());
        assert_eq!(run.labels()[0].0, "/mnt/games/ROMS already has files");
        assert_eq!(view(&run.screen, Timestamp::from_unix(NOW)).cursor, 3);
        let run = run.press(&[Button::Down, Button::A]);
        assert!(matches!(run.last_order(), Some(Order::AcceptFolder(folder, path)) if folder.label == "ROMS" && path.ends_with("ROMS")));
    }

    #[test]
    fn a_folder_whose_directory_is_already_synced_is_refused_with_a_reason() {
        let run = Run::browsing().select(Selection::PendingFolder(folder_id("old"))).press(&[Button::A]);
        assert!(run.last_order().is_none());
        assert_eq!(run.notices.last().map(Notice::text), Some("/mnt/games/SAVES already syncs as Saves; accept it in the web interface"));
    }

    #[test]
    fn a_rescans_a_folder() {
        let run = Run::browsing().select(Selection::Folder(folder_id("bios"))).press(&[Button::A]);
        assert_eq!(run.last_order(), Some(&Order::Rescan(folder_id("bios"))));
        let run = run.event(Event::Done(Order::Rescan(folder_id("bios")), Err(Failure::NotRunning)));
        assert_eq!(run.notices.last(), Some(&Notice::Error("Syncthing is not running".into())));
    }

    #[test]
    fn an_accepted_request_leaves_the_cursor_in_place() {
        let run = Run::browsing().select(Selection::PendingFolder(folder_id("shots")));
        let mut after = snapshot();
        after.pending_folders.retain(|folder| folder.id != folder_id("shots"));
        let run = run.event(Event::Observed(observation(Service::Running(after))));
        assert_eq!(run.browse().selected(), Selection::PendingFolder(folder_id("old")));
        // Everything gone but the switch: the cursor goes to it.
        let run = run.event(Event::Observed(observation(Service::Off)));
        assert_eq!(run.browse().selected(), Selection::Service);
    }

    #[test]
    fn a_restart_returns_the_cursor_to_its_row() {
        let run = Run::browsing().select(Selection::Web).press(&[Button::A]);
        assert!(matches!(run.last_order(), Some(Order::Expose(Exposure::Network))));
        // Syncthing restarts on the new address: only the switch is listed, and A there turns Syncthing off.
        let run = run.event(Event::Done(Order::Expose(Exposure::Network), Ok(()))).event(Event::Observed(observation(Service::Starting)));
        assert_eq!((view(&run.screen, Timestamp::from_unix(NOW)).cursor, run.browse().selected()), (0, Selection::Service));
        assert_eq!(Run::from(run.screen.clone()).press(&[Button::A]).last_order(), Some(&Order::Switch(Switch::Off)));
        let run = run.event(Event::Observed(observation(Service::Running(snapshot()))));
        assert_eq!(run.browse().selected(), Selection::Web);
    }

    #[test]
    fn the_web_interface_shows_its_address_and_login_once_on() {
        let password = Password::parse("abcd-efgh-jkmn-pqrs").unwrap();
        let mut running = snapshot();
        running.listening = Listening::Network;
        let on = Observation { web: WebInterface::Network(password.clone()), ..observation(Service::Running(running)) };
        let labels = Run::from(Screen::Browse(Browse::new(on))).labels();
        assert_eq!(labels[labels.len() - 2], ("Web interface on the network".to_owned(), "http://192.168.1.5:8384".to_owned()));
        assert_eq!(labels[labels.len() - 1], ("Web interface login".to_owned(), "ghazal / abcd-efgh-jkmn-pqrs".to_owned()));
        // Chosen, but Syncthing has not restarted yet.
        let restarting = Observation { web: WebInterface::Network(password), ..observation(Service::Running(snapshot())) };
        let screen = Screen::Browse(Browse::new(restarting));
        assert_eq!(Run::from(screen.clone()).labels().iter().rev().nth(1).unwrap().1, "Not on the network yet");
        assert_eq!(screen.refresh_interval(), Some(Duration::from_secs(2)));
    }

    #[test]
    fn the_web_switch_and_a_new_password_are_orders() {
        let run = Run::browsing().select(Selection::Web).press(&[Button::A]);
        assert_eq!(run.last_order(), Some(&Order::Expose(Exposure::Network)));
        let with_login = Observation { web: WebInterface::Local(Password::parse("abcd-efgh")), ..observation(Service::Running(snapshot())) };
        let run = Run::from(Screen::Browse(Browse::new(with_login))).select(Selection::Login).press(&[Button::A]);
        assert!(matches!(run.screen, Screen::Confirm(_)));
        let run = run.press(&[Button::Up, Button::A]);
        assert_eq!(run.last_order(), Some(&Order::NewPassword));
    }

    #[test]
    fn refreshes_keep_the_list_current_under_a_question() {
        let run = Run::browsing().select(Selection::PendingFolder(folder_id("roms"))).press(&[Button::A]);
        let mut changed = snapshot();
        changed.folders[0].state = FolderState::UpToDate;
        let run = run.event(Event::Observed(observation(Service::Running(changed))));
        assert!(matches!(run.screen, Screen::Confirm(_)));
        assert_eq!(run.browse().snapshot().unwrap().folders[0].state, FolderState::UpToDate);
        assert_eq!(run.screen.refresh_interval(), None);
    }

    #[test]
    fn last_seen_reads_naturally() {
        let at = |ago: i64| last_seen(LastSeen::At(Timestamp::from_unix(NOW - ago)), Timestamp::from_unix(NOW));
        assert_eq!([at(10), at(300), at(7200), at(100_000), at(5 * 86_400)], ["Last seen just now", "Last seen 5 min ago", "Last seen 2 h ago", "Last seen yesterday", "Last seen 5 days ago"]);
        assert_eq!(last_seen(LastSeen::Never, Timestamp::from_unix(NOW)), "Never connected");
    }

    #[test]
    fn b_leaves_and_a_closed_screen_ignores_everything() {
        assert!(Run::browsing().press(&[Button::B]).screen.closed());
        assert!(Run::from(Screen::Loading).press(&[Button::B]).screen.closed());
        let run = Run::from(Screen::Closed).press(&[Button::A]).event(Event::Observed(observation(Service::Off))).event(Event::Done(Order::NewPassword, Ok(())));
        assert!(run.screen.closed());
        assert!(run.commands.is_empty());
    }

    #[test]
    fn hints_follow_the_cursor() {
        let hints = |selection| view(&Run::browsing().select(selection).screen, Timestamp::from_unix(NOW)).hints;
        assert_eq!(hints(Selection::Service), SERVICE_HINTS);
        assert_eq!(hints(Selection::PendingDevice(device_id(PHONE))), PENDING_HINTS);
        assert_eq!(hints(Selection::Folder(folder_id("saves"))), FOLDER_HINTS);
        assert_eq!(hints(Selection::Device(device_id(PC))), BACK_HINTS);
    }
}
