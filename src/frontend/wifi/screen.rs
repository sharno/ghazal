//! The Wi-Fi screen as a state machine. There is no I/O here: `step` maps a
//! screen and an event (a button, or the result of something the worker ran)
//! to the next screen and at most one effect for the controller to carry out.
//!
//! Every state that can return to the list keeps that list (`Browse`) inside
//! it, so there is exactly one place the list lives and no state can exist
//! without somewhere to go back to.

use super::types::{ConnectRequest, Failure, Link, Network, Passphrase, Security, Snapshot, Ssid, Visibility};
use crate::frontend::app::navigate;
use crate::frontend::input::Button;
use crate::frontend::keyboard::{Display as KeyboardDisplay, Echo, Keyboard, Note, Outcome};

pub type Hints = &'static [(&'static str, &'static str)];

#[derive(Clone, Debug)]
pub enum Screen {
    /// The first refresh is running.
    Loading,
    /// iwd, D-Bus or the adapter is missing; A tries again.
    Unavailable(Failure),
    Browse(Browse),
    Confirm(Confirm),
    Entering(Entering),
    Working(Working),
    /// B on the list: the menu leaves the screen.
    Closed,
}

/// What the cursor is on. Holding the network's name instead of a row number
/// keeps the cursor on it when a refresh reorders or changes the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selection {
    Status,
    Scan,
    Network(Ssid),
    Hidden,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScanPhase {
    Idle,
    /// The scan request is on its way to iwd.
    Starting,
    /// iwd accepted it; ends with the first snapshot that is not scanning.
    Running,
}

#[derive(Clone, Debug)]
pub struct Browse {
    snapshot: Snapshot,
    selection: Selection,
    scan: ScanPhase,
}

#[derive(Clone, Debug)]
pub struct Confirm {
    back: Browse,
    action: Action,
    /// Starts on No: a stray A press must not forget a password.
    choice: Choice,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Choice {
    Yes,
    No,
}

impl Choice {
    fn other(self) -> Self {
        match self { Self::Yes => Self::No, Self::No => Self::Yes }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Disconnect(Ssid),
    Forget(Ssid),
}

#[derive(Clone, Debug)]
pub struct Entering {
    back: Browse,
    prompt: Prompt,
    /// Why the last attempt was refused, shown above the keyboard.
    error: Option<String>,
}

#[derive(Clone, Debug)]
struct Prompt {
    purpose: Purpose,
    keyboard: Keyboard,
}

#[derive(Clone, Debug)]
enum Purpose {
    /// A visible network that needs its password.
    Passphrase(Ssid),
    /// The name of a network that does not advertise itself.
    HiddenName,
    /// That network's password; empty means an open (or already saved) network.
    HiddenSecret(Ssid),
}

#[derive(Clone, Debug)]
pub struct Working {
    back: Browse,
    operation: Operation,
    /// The password prompt to return to if a typed password is refused.
    prompt: Option<Prompt>,
}

#[derive(Clone, Debug)]
enum Operation {
    Connecting(Ssid),
    Cancelling(Ssid),
    Disconnecting(Ssid),
    Forgetting(Ssid),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Button(Button),
    Refreshed(Result<Snapshot, Failure>),
    ScanStarted(Result<(), Failure>),
    Connected(Result<(), Failure>),
    Disconnected(Result<(), Failure>),
    Forgotten(Result<(), Failure>),
}

/// Work for the worker thread; each command answers with one event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Refresh,
    Scan,
    Connect(ConnectRequest),
    Disconnect,
    Forget(Ssid),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    Run(Command),
    /// Stop the connection attempt in flight, then drop whatever it left.
    Abort,
}

/// A message for the toast line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    Info(String),
    Error(String),
}

impl Notice {
    fn info(text: impl Into<String>) -> Self { Self::Info(text.into()) }
    fn error(text: impl Into<String>) -> Self { Self::Error(text.into()) }

    #[cfg(test)]
    pub fn text(&self) -> &str {
        match self { Self::Info(text) | Self::Error(text) => text }
    }
}

#[derive(Debug)]
pub struct Step {
    pub screen: Screen,
    pub effect: Option<Effect>,
    pub notice: Option<Notice>,
}

impl Step {
    fn to(screen: Screen) -> Self { Self { screen, effect: None, notice: None } }
    fn run(mut self, command: Command) -> Self { self.effect = Some(Effect::Run(command)); self }
    fn say(mut self, notice: Notice) -> Self { self.notice = Some(notice); self }
    fn browse(browse: Browse) -> Self { Self::to(Screen::Browse(browse)) }
    /// Back to the list, which then reflects what just happened.
    fn refreshed(browse: Browse) -> Self { Self::browse(browse).run(Command::Refresh) }
}

impl Browse {
    fn new(snapshot: Snapshot) -> Self {
        let selection = snapshot.networks.first().map_or(Selection::Scan, |network| Selection::Network(network.ssid.clone()));
        Self { snapshot, selection, scan: ScanPhase::Idle }
    }

    fn entries(&self) -> Vec<Selection> {
        [Selection::Status, Selection::Scan].into_iter()
            .chain(self.snapshot.networks.iter().map(|network| Selection::Network(network.ssid.clone())))
            .chain([Selection::Hidden])
            .collect()
    }

    fn cursor(&self) -> usize {
        self.entries().iter().position(|entry| *entry == self.selection).unwrap_or(0)
    }

    pub fn scanning(&self) -> bool { self.scan != ScanPhase::Idle || self.snapshot.scanning }

    fn selected_network(&self) -> Option<&Network> {
        match &self.selection { Selection::Network(ssid) => self.snapshot.network(ssid), _ => None }
    }

    fn accept(&mut self, snapshot: Snapshot) {
        self.scan = match (self.scan, snapshot.scanning) {
            (ScanPhase::Running, false) => ScanPhase::Idle,
            (phase, _) => phase,
        };
        self.snapshot = snapshot;
        if matches!(&self.selection, Selection::Network(ssid) if self.snapshot.network(ssid).is_none()) {
            self.selection = Selection::Scan;
        }
    }
}

impl Screen {
    /// The list underneath, for every state that has one.
    fn browse_mut(&mut self) -> Option<&mut Browse> {
        match self {
            Self::Browse(browse) => Some(browse),
            Self::Confirm(Confirm { back, .. }) | Self::Entering(Entering { back, .. }) | Self::Working(Working { back, .. }) => Some(back),
            Self::Loading | Self::Unavailable(_) | Self::Closed => None,
        }
    }

    #[cfg(test)]
    fn browse(&self) -> Option<&Browse> {
        match self {
            Self::Browse(browse) => Some(browse),
            Self::Confirm(Confirm { back, .. }) | Self::Entering(Entering { back, .. }) | Self::Working(Working { back, .. }) => Some(back),
            Self::Loading | Self::Unavailable(_) | Self::Closed => None,
        }
    }

    /// The screen is gone.
    pub fn closed(&self) -> bool { matches!(self, Self::Closed) }

    /// The keyboard is up, so Start and Menu belong to it, not to the menu.
    pub fn typing(&self) -> bool { matches!(self, Self::Entering(_)) }

    /// Whether the list is worth refreshing now, and how soon.
    pub fn refresh_interval(&self) -> Option<std::time::Duration> {
        let Self::Browse(browse) = self else { return None };
        let settling = matches!(&browse.snapshot.link, Link::Connecting | Link::Disconnecting) || browse.snapshot.connected().is_some_and(|connection| connection.address.is_none());
        Some(std::time::Duration::from_secs(if browse.scanning() || settling { 1 } else { 6 }))
    }
}

pub fn step(screen: Screen, event: Event) -> Step {
    match event {
        Event::Button(button) => on_button(screen, button),
        Event::Refreshed(result) => on_refreshed(screen, result),
        Event::ScanStarted(result) => on_scan_started(screen, result),
        Event::Connected(result) => on_connected(screen, result),
        Event::Disconnected(result) => on_disconnected(screen, result),
        Event::Forgotten(result) => on_forgotten(screen, result),
    }
}

fn on_button(screen: Screen, button: Button) -> Step {
    match screen {
        Screen::Loading if button == Button::B => Step::to(Screen::Closed),
        Screen::Unavailable(_) if button == Button::B => Step::to(Screen::Closed),
        Screen::Unavailable(_) if button == Button::A => Step::to(Screen::Loading).run(Command::Refresh),
        Screen::Loading | Screen::Unavailable(_) | Screen::Closed => Step::to(screen),
        Screen::Browse(browse) => browse_button(browse, button),
        Screen::Confirm(confirm) => confirm_button(confirm, button),
        Screen::Entering(entering) => entering_button(entering, button),
        Screen::Working(working) => working_button(working, button),
    }
}

fn browse_button(mut browse: Browse, button: Button) -> Step {
    match button {
        Button::B => Step::to(Screen::Closed),
        Button::Up | Button::Down | Button::L1 | Button::R1 => {
            let entries = browse.entries();
            browse.selection = entries[navigate(browse.cursor(), entries.len(), button)].clone();
            Step::browse(browse)
        }
        Button::X => start_scan(browse),
        Button::A => activate(browse),
        Button::Y => match browse.selected_network().filter(|network| network.known) {
            Some(network) => {
                let action = Action::Forget(network.ssid.clone());
                Step::to(Screen::Confirm(Confirm { back: browse, action, choice: Choice::No }))
            }
            None => Step::browse(browse),
        },
        Button::Left | Button::Right | Button::L2 | Button::R2 | Button::Select | Button::Start | Button::Menu => Step::browse(browse),
    }
}

fn start_scan(mut browse: Browse) -> Step {
    if browse.scanning() { return Step::browse(browse); }
    browse.scan = ScanPhase::Starting;
    Step::browse(browse).run(Command::Scan)
}

fn activate(browse: Browse) -> Step {
    match browse.selection.clone() {
        Selection::Status => {
            let notice = Notice::info(status_details(&browse.snapshot));
            Step::browse(browse).say(notice)
        }
        Selection::Scan => start_scan(browse),
        Selection::Hidden => {
            let prompt = Prompt { purpose: Purpose::HiddenName, keyboard: Keyboard::new(Ssid::MAX_BYTES, Echo::Plain) };
            Step::to(Screen::Entering(Entering { back: browse, prompt, error: None }))
        }
        Selection::Network(ssid) => match browse.snapshot.network(&ssid).cloned() {
            Some(network) => join(browse, network),
            None => Step::browse(browse),
        },
    }
}

fn join(browse: Browse, network: Network) -> Step {
    match (&network.security, browse.snapshot.is_connected(&network.ssid), network.known) {
        (_, true, _) => {
            let action = Action::Disconnect(network.ssid);
            Step::to(Screen::Confirm(Confirm { back: browse, action, choice: Choice::No }))
        }
        (security, false, _) if !security.joinable() => {
            let notice = Notice::error(format!("{} networks can't be joined here", security.label()));
            Step::browse(browse).say(notice)
        }
        (Security::Psk, false, false) => {
            let prompt = Prompt { purpose: Purpose::Passphrase(network.ssid), keyboard: Keyboard::new(Passphrase::MAX, Echo::Masked) };
            Step::to(Screen::Entering(Entering { back: browse, prompt, error: None }))
        }
        // Open, or saved: iwd already has everything it needs.
        (security, false, _) => {
            let request = ConnectRequest { ssid: network.ssid, visibility: network.visibility, security: Some(*security), passphrase: None };
            start_connect(browse, request, None)
        }
    }
}

fn start_connect(back: Browse, request: ConnectRequest, prompt: Option<Prompt>) -> Step {
    let operation = Operation::Connecting(request.ssid.clone());
    Step::to(Screen::Working(Working { back, operation, prompt })).run(Command::Connect(request))
}

fn confirm_button(mut confirm: Confirm, button: Button) -> Step {
    match (button, confirm.choice) {
        (Button::B, _) | (Button::A, Choice::No) => Step::browse(confirm.back),
        (Button::A, Choice::Yes) => {
            let (operation, command) = match confirm.action {
                Action::Disconnect(ssid) => (Operation::Disconnecting(ssid), Command::Disconnect),
                Action::Forget(ssid) => (Operation::Forgetting(ssid.clone()), Command::Forget(ssid)),
            };
            Step::to(Screen::Working(Working { back: confirm.back, operation, prompt: None })).run(command)
        }
        (Button::Up | Button::Down, _) => { confirm.choice = confirm.choice.other(); Step::to(Screen::Confirm(confirm)) }
        _ => Step::to(Screen::Confirm(confirm)),
    }
}

fn entering_button(mut entering: Entering, button: Button) -> Step {
    match entering.prompt.keyboard.press(button) {
        None => {
            entering.error = None;
            Step::to(Screen::Entering(entering))
        }
        Some(Outcome::Cancel) => Step::browse(entering.back),
        Some(Outcome::Submit(text)) => submit(entering, &text),
    }
}

fn submit(mut entering: Entering, text: &str) -> Step {
    let refuse = |mut entering: Entering, error: String| { entering.error = Some(error); Step::to(Screen::Entering(entering)) };
    match entering.prompt.purpose.clone() {
        Purpose::HiddenName => match Ssid::new(text) {
            Ok(ssid) => {
                entering.prompt = Prompt { purpose: Purpose::HiddenSecret(ssid), keyboard: Keyboard::new(Passphrase::MAX, Echo::Masked) };
                entering.error = None;
                Step::to(Screen::Entering(entering))
            }
            Err(error) => refuse(entering, error.to_string()),
        },
        Purpose::Passphrase(ssid) => match Passphrase::new(text) {
            Ok(passphrase) => {
                let request = ConnectRequest { ssid, visibility: Visibility::Listed, security: Some(Security::Psk), passphrase: Some(passphrase) };
                start_connect(entering.back, request, Some(entering.prompt))
            }
            Err(error) => refuse(entering, error.to_string()),
        },
        Purpose::HiddenSecret(ssid) => match (text, Passphrase::new(text)) {
            ("", _) => start_connect(entering.back, ConnectRequest { ssid, visibility: Visibility::Hidden, security: None, passphrase: None }, Some(entering.prompt)),
            (_, Ok(passphrase)) => start_connect(entering.back, ConnectRequest { ssid, visibility: Visibility::Hidden, security: None, passphrase: Some(passphrase) }, Some(entering.prompt)),
            (_, Err(error)) => refuse(entering, error.to_string()),
        },
    }
}

fn working_button(mut working: Working, button: Button) -> Step {
    match (button, &working.operation) {
        (Button::B, Operation::Connecting(ssid)) => {
            working.operation = Operation::Cancelling(ssid.clone());
            let mut step = Step::to(Screen::Working(working));
            step.effect = Some(Effect::Abort);
            step
        }
        _ => Step::to(Screen::Working(working)),
    }
}

fn on_refreshed(screen: Screen, result: Result<Snapshot, Failure>) -> Step {
    match (screen, result) {
        (Screen::Loading | Screen::Unavailable(_), Ok(snapshot)) => Step::browse(Browse::new(snapshot)),
        (Screen::Loading | Screen::Unavailable(_), Err(failure)) => Step::to(Screen::Unavailable(failure)),
        (Screen::Browse(_), Err(failure)) if failure.is_fatal() => Step::to(Screen::Unavailable(failure)),
        (mut screen @ (Screen::Browse(_) | Screen::Confirm(_) | Screen::Entering(_) | Screen::Working(_)), result) => {
            // A failed refresh keeps the list as it was; the next one retries.
            if let (Some(browse), Ok(snapshot)) = (screen.browse_mut(), result) { browse.accept(snapshot); }
            Step::to(screen)
        }
        (Screen::Closed, _) => Step::to(Screen::Closed),
    }
}

fn on_scan_started(mut screen: Screen, result: Result<(), Failure>) -> Step {
    // The scan may finish after the user has left the screen.
    let Some(browse) = screen.browse_mut() else { return Step::to(screen) };
    match result {
        Ok(()) => {
            browse.scan = ScanPhase::Running;
            Step::to(screen)
        }
        Err(failure) => {
            browse.scan = ScanPhase::Idle;
            if failure.is_fatal() { Step::to(Screen::Unavailable(failure)) } else { Step::to(screen).say(Notice::error(format!("Scan failed: {failure}"))) }
        }
    }
}

fn on_connected(screen: Screen, result: Result<(), Failure>) -> Step {
    match screen {
        Screen::Working(Working { back, operation: Operation::Connecting(ssid) | Operation::Cancelling(ssid), prompt }) => match result {
            Ok(()) => Step::refreshed(back).say(Notice::info(format!("Connected to {ssid}"))),
            Err(Failure::Cancelled) => Step::refreshed(back).say(Notice::info("Cancelled")),
            Err(failure) if failure.is_fatal() => Step::to(Screen::Unavailable(failure)),
            // Saved passwords can be wrong too: say what to do about it.
            Err(Failure::Rejected) if prompt.is_none() && back.snapshot.network(&ssid).is_some_and(|network| network.known && network.security == Security::Psk) => {
                Step::refreshed(back).say(Notice::error("Refused. Forget the network and enter the password again"))
            }
            // A typed password may just have a typo: let the user fix it.
            Err(failure) => match prompt.filter(|_| failure != Failure::Missing) {
                Some(prompt) => Step::to(Screen::Entering(Entering { back, prompt, error: Some(failure.message()) })),
                None => Step::refreshed(back).say(Notice::error(failure.message())),
            },
        },
        // Nothing is connecting: the result belongs to an attempt we left.
        screen @ (Screen::Browse(_) | Screen::Confirm(_) | Screen::Entering(_) | Screen::Working(_)) => Step::to(screen).run(Command::Refresh),
        screen @ (Screen::Loading | Screen::Unavailable(_) | Screen::Closed) => Step::to(screen),
    }
}

fn on_disconnected(screen: Screen, result: Result<(), Failure>) -> Step {
    match screen {
        Screen::Working(Working { back, operation: Operation::Disconnecting(ssid), .. }) => match result {
            Ok(()) => Step::refreshed(back).say(Notice::info(format!("Disconnected from {ssid}"))),
            Err(failure) => Step::refreshed(back).say(Notice::error(failure.message())),
        },
        // The cleanup after a cancelled attempt, or an attempt we left.
        screen @ (Screen::Browse(_) | Screen::Confirm(_) | Screen::Entering(_) | Screen::Working(_)) => Step::to(screen).run(Command::Refresh),
        screen @ (Screen::Loading | Screen::Unavailable(_) | Screen::Closed) => Step::to(screen),
    }
}

fn on_forgotten(screen: Screen, result: Result<(), Failure>) -> Step {
    match screen {
        Screen::Working(Working { back, operation: Operation::Forgetting(ssid), .. }) => match result {
            Ok(()) => Step::refreshed(back).say(Notice::info(format!("Forgot {ssid}"))),
            Err(failure) => Step::refreshed(back).say(Notice::error(failure.message())),
        },
        screen @ (Screen::Browse(_) | Screen::Confirm(_) | Screen::Entering(_) | Screen::Working(_)) => Step::to(screen).run(Command::Refresh),
        screen @ (Screen::Loading | Screen::Unavailable(_) | Screen::Closed) => Step::to(screen),
    }
}

fn status_details(snapshot: &Snapshot) -> String {
    match &snapshot.link {
        Link::Connected(connection) => [
            Some(connection.ssid.to_string()),
            connection.signal.map(|signal| format!("{} dBm", signal.dbm())),
            connection.band.map(|band| band.label().to_owned()),
            Some(connection.security.label().to_owned()),
        ].into_iter().flatten().collect::<Vec<_>>().join(" · "),
        Link::Connecting => "Connecting…".into(),
        Link::Disconnecting => "Disconnecting…".into(),
        Link::Disconnected => "Not connected to a network".into(),
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
    /// The same, short enough for a 480-pixel-wide screen.
    pub narrow_hints: Hints,
    pub keyboard: Option<KeyboardDisplay>,
}

const BROWSE_HINTS: Hints = &[("A", "Select"), ("X", "Scan"), ("B", "Back")];
const JOIN_HINTS: Hints = &[("A", "Connect"), ("X", "Scan"), ("B", "Back")];
const SAVED_HINTS: Hints = &[("A", "Connect"), ("Y", "Forget"), ("X", "Scan"), ("B", "Back")];
const CONNECTED_HINTS: Hints = &[("A", "Disconnect"), ("Y", "Forget"), ("B", "Back")];
const CONFIRM_HINTS: Hints = &[("A", "Choose"), ("B", "No")];
const CANCEL_HINTS: Hints = &[("B", "Cancel")];
const WAIT_HINTS: Hints = &[];
const BACK_HINTS: Hints = &[("B", "Back")];
const RETRY_HINTS: Hints = &[("A", "Try again"), ("B", "Back")];
const SECRET_KEYBOARD_HINTS: Hints = &[("A", "Type"), ("B", "Delete"), ("X", "Shift"), ("Y", "Show"), ("▶", "OK"), ("≡", "Cancel")];
const KEYBOARD_HINTS: Hints = &[("A", "Type"), ("B", "Delete"), ("X", "Shift"), ("▶", "OK"), ("≡", "Cancel")];
const KEYBOARD_NARROW_HINTS: Hints = &[("A", "Type"), ("B", "Del"), ("X", "Shift"), ("▶", "OK")];

/// Characters of typed text that fit above the keyboard.
const TEXT_COLUMNS: usize = 34;

fn bars(count: u8) -> &'static str {
    ["", "▂", "▂▄", "▂▄▆", "▂▄▆█"][usize::from(count.min(4))]
}

fn entry_row(snapshot: &Snapshot, scanning: bool, entry: &Selection) -> RowView {
    let row = |label: &str, detail: &str, glyph| RowView { label: label.into(), detail: detail.into(), glyph };
    match entry {
        Selection::Status => match &snapshot.link {
            Link::Connected(connection) => RowView {
                label: format!("Connected to {}", connection.ssid),
                detail: connection.address.map_or_else(|| "getting an address…".into(), |address| address.to_string()),
                glyph: "●",
            },
            Link::Connecting => row("Connecting…", "", "◌"),
            Link::Disconnecting => row("Disconnecting…", "", "◌"),
            Link::Disconnected => row("Not connected", "", "○"),
        },
        Selection::Scan => row("Scan for networks", if scanning { "Scanning…" } else { "" }, "↻"),
        Selection::Hidden => row("Join a hidden network", "", "+"),
        Selection::Network(ssid) => match snapshot.network(ssid) {
            Some(network) => RowView {
                label: network.ssid.to_string(),
                detail: match network.signal {
                    Some(signal) => format!("{} {}", network.security.label(), bars(signal.bars())),
                    None => "Saved, out of range".into(),
                },
                glyph: if snapshot.is_connected(&network.ssid) { "●" } else if network.known { "★" } else { "" },
            },
            None => row("", "", ""),
        },
    }
}

fn browse_rows(browse: &Browse) -> Vec<RowView> {
    browse.entries().iter().map(|entry| entry_row(&browse.snapshot, browse.scanning(), entry)).collect()
}

fn browse_hints(browse: &Browse) -> Hints {
    match browse.selected_network() {
        Some(network) if browse.snapshot.is_connected(&network.ssid) => CONNECTED_HINTS,
        Some(network) if network.known => SAVED_HINTS,
        Some(_) => JOIN_HINTS,
        None => BROWSE_HINTS,
    }
}

pub fn view(screen: &Screen) -> View {
    let plain = |rows: Vec<RowView>, cursor, hints| View { rows, cursor, hints, narrow_hints: hints, keyboard: None };
    let row = |label: &str, glyph| RowView { label: label.into(), detail: String::new(), glyph };
    match screen {
        Screen::Loading | Screen::Closed => plain(vec![row("Looking for networks…", "◌")], 0, BACK_HINTS),
        Screen::Unavailable(failure) => plain(vec![row(&failure.message(), "!"), row("Try again", "↻")], 1, RETRY_HINTS),
        Screen::Browse(browse) => plain(browse_rows(browse), browse.cursor(), browse_hints(browse)),
        Screen::Confirm(confirm) => {
            let (question, yes) = match &confirm.action {
                Action::Disconnect(ssid) => (format!("Disconnect from {ssid}?"), "Yes, disconnect"),
                Action::Forget(ssid) => (format!("Forget {ssid} and its password?"), "Yes, forget it"),
            };
            plain(vec![row(&question, "?"), row(yes, ""), row("No", "")], match confirm.choice { Choice::Yes => 1, Choice::No => 2 }, CONFIRM_HINTS)
        }
        Screen::Working(working) => {
            let (text, hints) = match &working.operation {
                Operation::Connecting(ssid) => (format!("Connecting to {ssid}…"), CANCEL_HINTS),
                Operation::Cancelling(_) => ("Cancelling…".to_owned(), WAIT_HINTS),
                Operation::Disconnecting(ssid) => (format!("Disconnecting from {ssid}…"), WAIT_HINTS),
                Operation::Forgetting(ssid) => (format!("Forgetting {ssid}…"), WAIT_HINTS),
            };
            plain(vec![row(&text, "◌")], 0, hints)
        }
        Screen::Entering(entering) => {
            let keyboard = &entering.prompt.keyboard;
            let (title, note) = match &entering.prompt.purpose {
                Purpose::Passphrase(ssid) => (format!("Password for {ssid}"), "8 to 63 characters"),
                Purpose::HiddenName => ("Hidden network name".to_owned(), "Type the name exactly"),
                Purpose::HiddenSecret(ssid) => (format!("Password for {ssid}"), "Leave empty for an open network"),
            };
            let note = match &entering.error {
                Some(error) => Note::Problem(error.clone()),
                None => Note::Help(note.to_owned()),
            };
            let view = keyboard.display(title, note, TEXT_COLUMNS);
            let hints = if matches!(entering.prompt.purpose, Purpose::HiddenName) { KEYBOARD_HINTS } else { SECRET_KEYBOARD_HINTS };
            View { rows: browse_rows(&entering.back), cursor: entering.back.cursor(), hints, narrow_hints: KEYBOARD_NARROW_HINTS, keyboard: Some(view) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::types::{Connection, Signal};
    use super::*;

    fn ssid(name: &str) -> Ssid { Ssid::new(name).unwrap() }

    fn network(name: &str, security: Security, dbm: i32, known: bool) -> Network {
        Network { ssid: ssid(name), security, signal: Some(Signal::from_dbm(dbm)), known, visibility: Visibility::Listed }
    }

    fn connection(name: &str) -> Connection {
        Connection { ssid: ssid(name), address: Some("192.168.1.5".parse().unwrap()), signal: Some(Signal::from_dbm(-44)), band: None, security: Security::Psk }
    }

    /// Home (connected, saved), Cafe (needs a password), Guest (open), Campus (802.1X).
    fn snapshot() -> Snapshot {
        Snapshot {
            link: Link::Connected(connection("Home")),
            scanning: false,
            networks: vec![
                network("Home", Security::Psk, -44, true),
                network("Cafe", Security::Psk, -60, false),
                network("Guest", Security::Open, -70, false),
                network("Campus", Security::Enterprise, -72, false),
                Network { ssid: ssid("Nan"), security: Security::Psk, signal: None, known: true, visibility: Visibility::Listed },
                Network { ssid: ssid("Secret"), security: Security::Psk, signal: None, known: true, visibility: Visibility::Hidden },
            ],
        }
    }

    /// Plays buttons through the machine, collecting what it asks for.
    struct Run {
        screen: Screen,
        effects: Vec<Effect>,
        notices: Vec<Notice>,
    }

    impl Run {
        fn from(screen: Screen) -> Self { Self { screen, effects: Vec::new(), notices: Vec::new() } }
        fn browsing() -> Self { Self::from(Screen::Browse(Browse::new(snapshot()))) }
        fn event(mut self, event: Event) -> Self {
            let step = step(self.screen, event);
            self.screen = step.screen;
            self.effects.extend(step.effect);
            self.notices.extend(step.notice);
            self
        }
        fn press(self, buttons: &[Button]) -> Self { buttons.iter().fold(self, |run, button| run.event(Event::Button(*button))) }
        fn selection(&self) -> &Selection { &self.browse().selection }
        fn browse(&self) -> &Browse { self.screen.browse().expect("a list underneath") }
        fn last_effect(&self) -> Option<&Effect> { self.effects.last() }
        fn last_notice(&self) -> Option<&str> { self.notices.last().map(Notice::text) }
        fn goto(self, name: &str) -> Self {
            let mut run = self;
            while *run.selection() != Selection::Network(ssid(name)) { run = run.press(&[Button::Down]); }
            run
        }
    }

    fn connect_request(run: &Run) -> ConnectRequest {
        match run.last_effect() { Some(Effect::Run(Command::Connect(request))) => request.clone(), other => panic!("expected a connect, got {other:?}") }
    }

    #[test]
    fn loading_ends_in_the_list_or_in_unavailable() {
        let run = Run::from(Screen::Loading).event(Event::Refreshed(Ok(snapshot())));
        assert!(matches!(run.screen, Screen::Browse(_)));
        // The cursor starts on the first network.
        assert_eq!(run.selection(), &Selection::Network(ssid("Home")));

        let run = Run::from(Screen::Loading).event(Event::Refreshed(Err(Failure::Unavailable)));
        assert!(matches!(run.screen, Screen::Unavailable(Failure::Unavailable)));
        // A retries; B leaves.
        let retry = run.press(&[Button::A]);
        assert!(matches!(retry.screen, Screen::Loading));
        assert_eq!(retry.last_effect(), Some(&Effect::Run(Command::Refresh)));
        assert!(Run::from(Screen::Unavailable(Failure::NoAdapter)).press(&[Button::B]).screen.closed());
        assert!(Run::from(Screen::Loading).press(&[Button::B]).screen.closed());
    }

    #[test]
    fn an_empty_radio_starts_on_scan() {
        let empty = Snapshot { link: Link::Disconnected, scanning: false, networks: Vec::new() };
        let run = Run::from(Screen::Loading).event(Event::Refreshed(Ok(empty)));
        assert_eq!(run.selection(), &Selection::Scan);
    }

    #[test]
    fn b_leaves_the_list() {
        assert!(Run::browsing().press(&[Button::B]).screen.closed());
    }

    #[test]
    fn the_cursor_visits_every_entry_and_wraps() {
        let mut run = Run::browsing().press(&[Button::Up]);
        // Home was first; above it are Scan and Status.
        assert_eq!(run.selection(), &Selection::Scan);
        run = run.press(&[Button::Up]);
        assert_eq!(run.selection(), &Selection::Status);
        run = run.press(&[Button::Up]);
        assert_eq!(run.selection(), &Selection::Hidden);
        assert_eq!(run.press(&[Button::Down]).selection(), &Selection::Status);
    }

    #[test]
    fn a_refresh_keeps_the_cursor_on_its_network_and_moves_it_when_the_network_goes() {
        let mut reordered = snapshot();
        reordered.networks.swap(1, 2);
        let run = Run::browsing().goto("Guest").event(Event::Refreshed(Ok(reordered)));
        assert_eq!(run.selection(), &Selection::Network(ssid("Guest")));

        let mut gone = snapshot();
        gone.networks.retain(|network| network.ssid != ssid("Guest"));
        let run = run.event(Event::Refreshed(Ok(gone)));
        assert_eq!(run.selection(), &Selection::Scan);
    }

    #[test]
    fn a_failed_refresh_keeps_the_list_unless_the_radio_is_gone() {
        let run = Run::browsing().event(Event::Refreshed(Err(Failure::TimedOut)));
        assert!(matches!(run.screen, Screen::Browse(_)));
        assert!(run.notices.is_empty());
        let run = run.event(Event::Refreshed(Err(Failure::Unavailable)));
        assert!(matches!(run.screen, Screen::Unavailable(_)));
    }

    #[test]
    fn open_networks_connect_without_a_prompt() {
        let run = Run::browsing().goto("Guest").press(&[Button::A]);
        assert!(matches!(run.screen, Screen::Working(_)));
        let request = connect_request(&run);
        assert_eq!((request.ssid, request.visibility, request.security, request.passphrase), (ssid("Guest"), Visibility::Listed, Some(Security::Open), None));
    }

    #[test]
    fn saved_networks_reconnect_without_asking_for_the_password_again() {
        let run = Run::browsing().goto("Nan").press(&[Button::A]);
        let request = connect_request(&run);
        assert_eq!((request.ssid, request.passphrase), (ssid("Nan"), None));
    }

    #[test]
    fn a_saved_hidden_network_is_joined_with_the_hidden_probe() {
        // It never appears in scan results, so a plain connect could not find it.
        let run = Run::browsing().goto("Secret").press(&[Button::A]);
        let request = connect_request(&run);
        assert_eq!((request.ssid.as_str(), request.visibility, request.passphrase), ("Secret", Visibility::Hidden, None));
    }

    #[test]
    fn a_new_secured_network_asks_for_its_password() {
        let run = Run::browsing().goto("Cafe").press(&[Button::A]);
        assert!(matches!(run.screen, Screen::Entering(_)));
        assert!(run.screen.typing());
        assert!(run.effects.is_empty());
    }

    #[test]
    fn a_short_password_is_refused_on_the_keyboard() {
        let run = Run::browsing().goto("Cafe").press(&[Button::A, Button::A, Button::A, Button::Start]);
        let Screen::Entering(entering) = &run.screen else { panic!("still typing") };
        assert_eq!(entering.error.as_deref(), Some("Passwords have at least 8 characters"));
        assert!(run.effects.is_empty());
        // Typing on clears the message.
        let run = run.press(&[Button::A]);
        let Screen::Entering(entering) = &run.screen else { panic!("still typing") };
        assert_eq!(entering.error, None);
    }

    #[test]
    fn a_good_password_connects_and_success_returns_to_the_list() {
        let run = Run::browsing().goto("Cafe").press(&[Button::A]).press(&[Button::A; 8]).press(&[Button::Start]);
        assert!(matches!(run.screen, Screen::Working(_)));
        let request = connect_request(&run);
        assert_eq!((request.ssid.as_str(), request.visibility, request.security), ("Cafe", Visibility::Listed, Some(Security::Psk)));
        assert_eq!(request.passphrase.unwrap().expose(), "qqqqqqqq");

        let run = run.event(Event::Connected(Ok(())));
        assert!(matches!(run.screen, Screen::Browse(_)));
        assert_eq!(run.last_notice(), Some("Connected to Cafe"));
        assert_eq!(run.last_effect(), Some(&Effect::Run(Command::Refresh)));
    }

    #[test]
    fn a_refused_password_returns_to_the_keyboard_with_the_text_kept() {
        let run = Run::browsing().goto("Cafe").press(&[Button::A]).press(&[Button::A; 8]).press(&[Button::Start]).event(Event::Connected(Err(Failure::Rejected)));
        let Screen::Entering(entering) = &run.screen else { panic!("back on the keyboard") };
        assert_eq!(entering.prompt.keyboard.text(), "qqqqqqqq");
        assert_eq!(entering.error.as_deref(), Some("Connection refused: check the password"));
        assert!(run.notices.is_empty());
    }

    #[test]
    fn a_network_that_vanished_goes_back_to_the_list_not_the_keyboard() {
        let run = Run::browsing().goto("Cafe").press(&[Button::A]).press(&[Button::A; 8]).press(&[Button::Start]).event(Event::Connected(Err(Failure::Missing)));
        assert!(matches!(run.screen, Screen::Browse(_)));
        assert_eq!(run.last_notice(), Some("Network is out of range"));
    }

    #[test]
    fn cancelling_the_keyboard_returns_to_the_list() {
        let run = Run::browsing().goto("Cafe").press(&[Button::A, Button::Menu]);
        assert!(matches!(run.screen, Screen::Browse(_)));
        assert!(run.effects.is_empty());
    }

    #[test]
    fn a_saved_password_that_fails_says_how_to_fix_it() {
        let run = Run::browsing().goto("Nan").press(&[Button::A]).event(Event::Connected(Err(Failure::Rejected)));
        assert!(matches!(run.screen, Screen::Browse(_)));
        assert_eq!(run.last_notice(), Some("Refused. Forget the network and enter the password again"));
    }

    #[test]
    fn other_connection_failures_are_reported_on_the_list() {
        let run = Run::browsing().goto("Guest").press(&[Button::A]).event(Event::Connected(Err(Failure::TimedOut)));
        assert!(matches!(run.screen, Screen::Browse(_)));
        assert_eq!(run.last_notice(), Some("Wi-Fi did not answer in time"));
        assert!(matches!(run.notices.last(), Some(Notice::Error(_))));
    }

    #[test]
    fn enterprise_networks_are_refused_with_an_explanation() {
        let run = Run::browsing().goto("Campus").press(&[Button::A]);
        assert!(matches!(run.screen, Screen::Browse(_)));
        assert_eq!(run.last_notice(), Some("802.1X networks can't be joined here"));
        assert!(run.effects.is_empty());
    }

    #[test]
    fn a_connection_attempt_can_be_cancelled() {
        let run = Run::browsing().goto("Guest").press(&[Button::A, Button::B]);
        assert_eq!(run.last_effect(), Some(&Effect::Abort));
        // Further presses do nothing while it winds down.
        let run = run.press(&[Button::B, Button::A]);
        assert_eq!(run.effects.iter().filter(|effect| **effect == Effect::Abort).count(), 1);
        let run = run.event(Event::Connected(Err(Failure::Cancelled)));
        assert!(matches!(run.screen, Screen::Browse(_)));
        assert_eq!(run.last_notice(), Some("Cancelled"));
        // The disconnect queued behind it only refreshes the list.
        let run = run.event(Event::Disconnected(Ok(())));
        assert_eq!(run.last_effect(), Some(&Effect::Run(Command::Refresh)));
        assert!(matches!(run.screen, Screen::Browse(_)));
    }

    #[test]
    fn disconnecting_asks_first_and_defaults_to_no() {
        let run = Run::browsing().press(&[Button::A]);
        assert!(matches!(run.screen, Screen::Confirm(_)));
        let declined = run.clone_screen().press(&[Button::A]);
        assert!(matches!(declined.screen, Screen::Browse(_)));
        assert!(declined.effects.is_empty());

        let run = run.press(&[Button::Up, Button::A]);
        assert!(matches!(run.screen, Screen::Working(_)));
        assert_eq!(run.last_effect(), Some(&Effect::Run(Command::Disconnect)));
        let run = run.event(Event::Disconnected(Ok(())));
        assert_eq!(run.last_notice(), Some("Disconnected from Home"));
        assert_eq!(run.last_effect(), Some(&Effect::Run(Command::Refresh)));
    }

    impl Run {
        fn clone_screen(&self) -> Run { Run::from(self.screen.clone()) }
    }

    #[test]
    fn forgetting_asks_first_and_only_applies_to_saved_networks() {
        let unsaved = Run::browsing().goto("Cafe").press(&[Button::Y]);
        assert!(matches!(unsaved.screen, Screen::Browse(_)));

        let run = Run::browsing().goto("Nan").press(&[Button::Y]);
        assert!(matches!(run.screen, Screen::Confirm(_)));
        assert!(Run::from(run.screen.clone()).press(&[Button::B]).effects.is_empty());
        let run = run.press(&[Button::Down, Button::A]);
        assert_eq!(run.last_effect(), Some(&Effect::Run(Command::Forget(ssid("Nan")))));
        let run = run.event(Event::Forgotten(Ok(())));
        assert_eq!(run.last_notice(), Some("Forgot Nan"));
        assert!(matches!(run.screen, Screen::Browse(_)));
    }

    #[test]
    fn scanning_starts_once_and_follows_iwd() {
        let run = Run::browsing().press(&[Button::X]);
        assert_eq!(run.last_effect(), Some(&Effect::Run(Command::Scan)));
        assert!(run.browse().scanning());
        // A second request while scanning is ignored.
        let run = run.press(&[Button::X]);
        assert_eq!(run.effects.len(), 1);

        let mut scanning = snapshot();
        scanning.scanning = true;
        let run = run.event(Event::ScanStarted(Ok(()))).event(Event::Refreshed(Ok(scanning)));
        assert!(run.browse().scanning());
        // The first snapshot that is no longer scanning ends it.
        let run = run.event(Event::Refreshed(Ok(snapshot())));
        assert!(!run.browse().scanning());
    }

    #[test]
    fn a_refresh_that_predates_the_scan_does_not_end_it() {
        let run = Run::browsing().press(&[Button::X]).event(Event::Refreshed(Ok(snapshot())));
        assert!(run.browse().scanning());
    }

    #[test]
    fn a_failed_scan_is_reported_and_clears() {
        let run = Run::browsing().press(&[Button::X]).event(Event::ScanStarted(Err(Failure::TimedOut)));
        assert!(!run.browse().scanning());
        assert_eq!(run.last_notice(), Some("Scan failed: Wi-Fi did not answer in time"));
    }

    #[test]
    fn hidden_networks_take_a_name_then_an_optional_password() {
        let run = Run::browsing().press(&[Button::Up, Button::Up, Button::Up, Button::A]);
        assert_eq!(*run.selection(), Selection::Hidden);
        assert!(run.screen.typing());
        // An empty name is refused.
        let empty = Run::from(run.screen.clone()).press(&[Button::Start]);
        let Screen::Entering(entering) = &empty.screen else { panic!("still typing") };
        assert_eq!(entering.error.as_deref(), Some("Enter the network name"));

        let named = run.press(&[Button::A, Button::A, Button::Start]);
        let Screen::Entering(entering) = &named.screen else { panic!("password next") };
        assert!(matches!(&entering.prompt.purpose, Purpose::HiddenSecret(name) if name.as_str() == "qq"));
        // An empty password means an open network.
        let open = Run::from(named.screen.clone()).press(&[Button::Start]);
        let request = connect_request(&open);
        assert_eq!((request.ssid.as_str(), request.visibility, request.security, request.passphrase), ("qq", Visibility::Hidden, None, None));

        let secured = named.press(&[Button::A; 8]).press(&[Button::Start]);
        let request = connect_request(&secured);
        assert_eq!(request.visibility, Visibility::Hidden);
        assert_eq!(request.passphrase.unwrap().expose(), "qqqqqqqq");
    }

    #[test]
    fn the_status_row_reports_the_link_and_a_tap_explains_it() {
        let run = Run::browsing().press(&[Button::Up, Button::Up, Button::A]);
        assert_eq!(*run.selection(), Selection::Status);
        assert_eq!(run.last_notice(), Some("Home · -44 dBm · WPA"));
    }

    #[test]
    fn late_results_for_an_abandoned_attempt_only_refresh() {
        for event in [Event::Connected(Ok(())), Event::Disconnected(Ok(())), Event::Forgotten(Ok(()))] {
            let run = Run::browsing().event(event);
            assert!(matches!(run.screen, Screen::Browse(_)));
            assert_eq!(run.last_effect(), Some(&Effect::Run(Command::Refresh)));
            assert!(run.notices.is_empty());
        }
    }

    #[test]
    fn a_closed_screen_ignores_everything() {
        let run = Run::from(Screen::Closed).press(&[Button::A, Button::B]).event(Event::Refreshed(Ok(snapshot()))).event(Event::Connected(Ok(())));
        assert!(run.screen.closed());
        assert!(run.effects.is_empty());
    }

    #[test]
    fn refreshes_during_a_prompt_update_the_list_underneath() {
        let mut changed = snapshot();
        changed.networks.push(network("Zed", Security::Open, -80, false));
        let run = Run::browsing().goto("Cafe").press(&[Button::A]).event(Event::Refreshed(Ok(changed)));
        assert!(run.screen.typing());
        assert!(run.browse().snapshot.network(&ssid("Zed")).is_some());
    }

    #[test]
    fn refreshing_is_faster_while_something_is_changing() {
        let seconds = |screen: &Screen| screen.refresh_interval().map(|interval| interval.as_secs());
        assert_eq!(seconds(&Screen::Browse(Browse::new(snapshot()))), Some(6));
        let mut scanning = snapshot();
        scanning.scanning = true;
        assert_eq!(seconds(&Screen::Browse(Browse::new(scanning))), Some(1));
        let mut no_address = snapshot();
        no_address.link = Link::Connected(Connection { address: None, ..connection("Home") });
        assert_eq!(seconds(&Screen::Browse(Browse::new(no_address))), Some(1));
        assert_eq!(seconds(&Screen::Loading), None);
        assert_eq!(seconds(&Run::browsing().goto("Cafe").press(&[Button::A]).screen), None);
    }

    #[test]
    fn rows_describe_the_link_and_every_network() {
        let view = view(&Screen::Browse(Browse::new(snapshot())));
        let labels: Vec<_> = view.rows.iter().map(|row| (row.label.as_str(), row.detail.as_str(), row.glyph)).collect();
        assert_eq!(labels, [
            ("Connected to Home", "192.168.1.5", "●"),
            ("Scan for networks", "", "↻"),
            ("Home", "WPA ▂▄▆█", "●"),
            ("Cafe", "WPA ▂▄▆", ""),
            ("Guest", "Open ▂▄", ""),
            ("Campus", "802.1X ▂▄", ""),
            ("Nan", "Saved, out of range", "★"),
            ("Secret", "Saved, out of range", "★"),
            ("Join a hidden network", "", "+"),
        ]);
        assert_eq!(view.cursor, 2);
    }

    #[test]
    fn hints_follow_what_the_cursor_is_on() {
        let hints = |name: Option<&str>| match name {
            Some(name) => view(&Run::browsing().goto(name).screen).hints,
            None => view(&Run::browsing().press(&[Button::Up]).screen).hints,
        };
        assert_eq!(hints(Some("Home")), CONNECTED_HINTS);
        assert_eq!(hints(Some("Nan")), SAVED_HINTS);
        assert_eq!(hints(Some("Cafe")), JOIN_HINTS);
        assert_eq!(hints(None), BROWSE_HINTS);
    }

    #[test]
    fn the_keyboard_view_carries_title_text_and_keys() {
        // The first A opens the keyboard; the next two type.
        let run = Run::browsing().goto("Cafe").press(&[Button::A, Button::A, Button::A]);
        let view = view(&run.screen);
        let keyboard = view.keyboard.expect("keyboard");
        assert_eq!((keyboard.title.as_str(), keyboard.shown.as_str()), ("Password for Cafe", "••▏"));
        assert_eq!(keyboard.note, Note::Help("8 to 63 characters".into()));
        assert_eq!(keyboard.keys.len(), 45);
        assert_eq!(keyboard.keys[keyboard.selected].label, "q");
        assert_eq!(view.hints, SECRET_KEYBOARD_HINTS);
    }

    #[test]
    fn working_screens_name_what_is_happening() {
        let run = Run::browsing().goto("Guest").press(&[Button::A]);
        let view = view(&run.screen);
        assert_eq!(view.rows[0].label, "Connecting to Guest…");
        assert_eq!(view.hints, CANCEL_HINTS);
        assert_eq!(super::view(&Run::from(Screen::Unavailable(Failure::NoAdapter)).screen).rows[0].label, "No Wi-Fi adapter found");
    }
}
