//! Configuration → Bluetooth: the list the menu shows and the buttons that drive it.
//! Everything that changes the radio goes through the state machine in
//! `crate::bluetooth::model`; this is only layout and input.

use anyhow::Result;

use super::app::navigate;
use super::input::Button;
use crate::bluetooth::backend::{self, Backend};
use crate::bluetooth::model::{Address, Device, Intent, Kind, Link, Notice, Operation, Radio, State, Stray};

pub type Hints = &'static [(&'static str, &'static str)];
const HINTS: Hints = &[("A", "Select"), ("X", "Scan"), ("Y", "Forget"), ("B", "Back")];
const BUSY_HINTS: Hints = &[("B", "Cancel")];

/// What a line of the list stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Entry {
    Power,
    Scan,
    Device(Address),
}

pub struct Line {
    pub label: String,
    pub detail: String,
    pub glyph: &'static str,
}

pub enum Press {
    Stay(Option<Notice>),
    Leave,
}

pub struct Screen {
    state: State,
    backend: Box<dyn Backend>,
    cursor: usize,
}

impl Screen {
    pub fn open() -> Result<Self> {
        Ok(Self::with(backend::open()?))
    }

    fn with(backend: Box<dyn Backend>) -> Self {
        Self { state: State::default(), backend, cursor: 0 }
    }

    fn entries(&self) -> Vec<Entry> {
        let mut entries = vec![Entry::Power];
        if matches!(self.state.radio(), Some(Radio::On { .. })) {
            entries.push(Entry::Scan);
            entries.extend(self.state.devices().iter().map(|device| Entry::Device(device.address)));
        }
        entries
    }

    pub fn len(&self) -> usize {
        self.entries().len()
    }

    pub fn cursor(&self) -> usize {
        self.cursor.min(self.len() - 1)
    }

    pub fn hints(&self) -> Hints {
        match self.state.running() {
            Some(Operation::Pair(_) | Operation::Connect(_)) => BUSY_HINTS,
            _ => HINTS,
        }
    }

    pub fn line(&self, index: usize) -> Option<Line> {
        Some(match *self.entries().get(index)? {
            Entry::Power => Line {
                label: "Bluetooth".into(),
                detail: match (self.state.running(), self.state.radio()) {
                    (Some(Operation::Power(true)), _) => "Turning on…",
                    (Some(Operation::Power(false)), _) => "Turning off…",
                    (_, None) => "…",
                    (_, Some(Radio::Off)) => "Off",
                    (_, Some(Radio::On { .. })) => "On",
                }
                .into(),
                glyph: "",
            },
            Entry::Scan => {
                let scanning = matches!(self.state.radio(), Some(Radio::On { scanning: true }));
                Line { label: if scanning { "Stop scanning" } else { "Scan for devices" }.into(), detail: if scanning { "Scanning…" } else { "" }.into(), glyph: "" }
            }
            Entry::Device(address) => {
                let device = self.state.device(address)?;
                Line {
                    label: device.name.clone(),
                    detail: self.device_detail(device).into(),
                    glyph: match device.kind {
                        Kind::Controller => "◆",
                        Kind::Audio => "♪",
                        Kind::Other => "•",
                    },
                }
            }
        })
    }

    fn device_detail(&self, device: &Device) -> &'static str {
        match (self.state.running(), device.link) {
            (Some(Operation::Pair(address)), _) if address == device.address => "Pairing…",
            (Some(Operation::Connect(address)), _) if address == device.address => "Connecting…",
            (Some(Operation::Disconnect(address)), _) if address == device.address => "Disconnecting…",
            (Some(Operation::Forget(address)), _) if address == device.address => "Forgetting…",
            // BlueZ is built without A2DP: headphones pair but play nothing (docs/bluetooth.md).
            (_, Link::New) if device.kind == Kind::Audio => "No audio yet",
            (_, Link::New) => "",
            (_, Link::Paired { connected: false }) => "Paired",
            (_, Link::Paired { connected: true }) => "Connected",
        }
    }

    pub fn press(&mut self, button: Button) -> Press {
        let entry = self.entries()[self.cursor()];
        let intent = match (button, entry) {
            (Button::A, Entry::Power) => Intent::TogglePower,
            (Button::A | Button::X, Entry::Scan) | (Button::X, _) => Intent::ToggleScan,
            (Button::A, Entry::Device(address)) => Intent::Activate(address),
            (Button::Y, Entry::Device(address)) => Intent::Forget(address),
            (Button::B, _) if self.hints() == HINTS => return Press::Leave,
            (Button::B, _) => Intent::Cancel,
            (button, _) => {
                self.cursor = navigate(self.cursor(), self.len(), button);
                return Press::Stay(None);
            }
        };
        Press::Stay(match self.state.request(intent) {
            Ok(order) => {
                self.backend.send(order);
                None
            }
            Err(refusal) => Some(Notice { text: refusal.text().to_owned(), error: false }),
        })
    }

    /// Takes in what the backend has reported. Returns whether anything changed, and notices to show.
    pub fn poll(&mut self) -> (bool, Vec<Notice>) {
        let events = self.backend.poll();
        let changed = !events.is_empty();
        let selected = self.entries()[self.cursor()];
        let mut notices = Vec::new();
        for event in events {
            match self.state.apply(event) {
                Ok(notice) => notices.extend(notice),
                Err(Stray(operation)) => eprintln!("ghazal: bluetooth reported an unexpected result for {operation:?}"),
            }
        }
        // The list reorders as signal strengths change; the cursor stays on its device.
        if let Some(index) = self.entries().iter().position(|entry| *entry == selected) {
            self.cursor = index;
        }
        (changed, notices)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bluetooth::backend::fake::Fake;
    use crate::bluetooth::model::{Event, Observation, Order};

    fn labels(screen: &Screen) -> Vec<(String, String)> {
        (0..screen.len()).filter_map(|index| screen.line(index)).map(|line| (line.label, line.detail)).collect()
    }

    fn open() -> Screen {
        let mut screen = Screen::with(Box::new(Fake::demo()));
        screen.poll();
        screen
    }

    fn press(screen: &mut Screen, buttons: &[Button]) -> Vec<Notice> {
        let mut notices = Vec::new();
        for button in buttons {
            if let Press::Stay(Some(notice)) = screen.press(*button) {
                notices.push(notice);
            }
            notices.extend(screen.poll().1);
        }
        notices
    }

    fn row(screen: &Screen, name: &str) -> usize {
        (0..screen.len()).position(|index| screen.line(index).is_some_and(|line| line.label == name)).unwrap()
    }

    #[test]
    fn the_list_starts_with_the_radio_and_the_known_devices() {
        let screen = open();
        assert_eq!(
            labels(&screen),
            [("Bluetooth", "On"), ("Scan for devices", ""), ("Wireless Controller", "Connected")].map(|(label, detail)| (label.to_owned(), detail.to_owned()))
        );
    }

    #[test]
    fn nothing_is_listed_until_the_backend_reports() {
        let screen = Screen::with(Box::new(Fake::demo()));
        assert_eq!(labels(&screen), [("Bluetooth".to_owned(), "…".to_owned())]);
    }

    #[test]
    fn turning_bluetooth_off_hides_the_rest() {
        let mut screen = open();
        let notices = press(&mut screen, &[Button::A]);
        assert_eq!(notices, [Notice { text: "Bluetooth off".into(), error: false }]);
        assert_eq!(labels(&screen), [("Bluetooth".to_owned(), "Off".to_owned())]);
        assert_eq!(press(&mut screen, &[Button::X]), [Notice { text: "Turn Bluetooth on first".into(), error: false }]);
        press(&mut screen, &[Button::A]);
        assert_eq!(screen.len(), 3);
    }

    #[test]
    fn scanning_finds_devices_and_pairing_connects_them() {
        let mut screen = open();
        press(&mut screen, &[Button::X]);
        let names: Vec<String> = (2..screen.len()).filter_map(|index| screen.line(index)).map(|line| line.label).collect();
        assert_eq!(names, ["Wireless Controller", "Pro Controller", "8BitDo SN30 Pro", "WH-1000XM5"]);
        assert_eq!(screen.line(1).map(|line| line.label), Some("Stop scanning".to_owned()));

        screen.cursor = row(&screen, "Pro Controller");
        let notices = press(&mut screen, &[Button::A]);
        assert_eq!(notices, [Notice { text: "Paired with Pro Controller".into(), error: false }]);
        assert_eq!(screen.line(screen.cursor()).map(|line| line.detail), Some("Connected".to_owned()));
        assert_eq!(screen.line(screen.cursor()).map(|line| line.glyph), Some("◆"));
    }

    #[test]
    fn headphones_are_listed_but_say_they_cannot_play_yet() {
        let mut screen = open();
        press(&mut screen, &[Button::X]);
        let headphones = row(&screen, "WH-1000XM5");
        assert_eq!(screen.line(headphones).map(|line| (line.detail, line.glyph)), Some(("No audio yet".to_owned(), "♪")));
    }

    #[test]
    fn a_paired_device_disconnects_connects_and_is_forgotten() {
        let mut screen = open();
        screen.cursor = 2;
        assert_eq!(press(&mut screen, &[Button::A]), [Notice { text: "Disconnected from Wireless Controller".into(), error: false }]);
        assert_eq!(screen.line(2).map(|line| line.detail), Some("Paired".to_owned()));
        assert_eq!(press(&mut screen, &[Button::A]), [Notice { text: "Connected to Wireless Controller".into(), error: false }]);
        assert_eq!(press(&mut screen, &[Button::Y]), [Notice { text: "Forgot Wireless Controller".into(), error: false }]);
        assert_eq!(screen.line(2).map(|line| line.detail), Some("".to_owned()));
        assert_eq!(press(&mut screen, &[Button::Y]), [Notice { text: "Only paired devices can be forgotten".into(), error: false }]);
    }

    #[test]
    fn a_failure_is_reported_and_the_list_stays_usable() {
        let mut screen = Screen::with(Box::new(Fake::demo().failing("authentication failed")));
        screen.poll();
        screen.cursor = 2;
        let notices = press(&mut screen, &[Button::A]);
        assert_eq!(notices, [Notice { text: "Disconnecting Wireless Controller failed: authentication failed".into(), error: true }]);
        assert_eq!(screen.state.running(), None);
    }

    #[test]
    fn leaving_is_b_unless_something_is_pairing() {
        struct Silent(Vec<Order>);
        impl Backend for Silent {
            fn send(&mut self, order: Order) {
                self.0.push(order);
            }
            fn poll(&mut self) -> Vec<Event> {
                vec![]
            }
        }
        let mut screen = open();
        assert!(matches!(screen.press(Button::B), Press::Leave));

        // A backend that never answers keeps the pairing running.
        let mut screen = Screen::with(Box::new(Silent(Vec::new())));
        screen.state.apply(Event::Observed(Observation { radio: Radio::On { scanning: false }, devices: vec![Device { address: Address::new([1; 6]), name: "Pad".into(), kind: Kind::Controller, link: Link::New, rssi: None }] })).unwrap();
        screen.cursor = 2;
        let Press::Stay(None) = screen.press(Button::A) else { panic!("pairing did not start") };
        assert_eq!(screen.hints(), BUSY_HINTS);
        let Press::Stay(None) = screen.press(Button::B) else { panic!("B did not cancel") };
        assert_eq!(screen.state.running(), Some(Operation::Pair(Address::new([1; 6]))));
        let Press::Stay(Some(refusal)) = screen.press(Button::A) else { panic!("a second operation started") };
        assert_eq!(refusal.text, "Pairing; press B to cancel");
    }

    #[test]
    fn the_cursor_stays_on_its_device_when_the_list_reorders() {
        let mut screen = open();
        press(&mut screen, &[Button::X]);
        screen.cursor = row(&screen, "8BitDo SN30 Pro");
        let pad = |last: u8, name: &str, rssi: i16| Device { address: Address::new([0xE4, 0x17, 0xD8, 0, 0, last]), name: name.into(), kind: Kind::Controller, link: Link::New, rssi: Some(rssi) };
        screen.backend = Box::new(Reorder(Some(Observation {
            radio: Radio::On { scanning: true },
            devices: vec![pad(3, "8BitDo SN30 Pro", -40), pad(2, "Pro Controller", -60)],
        })));
        screen.poll();
        assert_eq!(screen.cursor, 2);
        assert_eq!(screen.line(screen.cursor()).map(|line| line.label), Some("8BitDo SN30 Pro".to_owned()));

        struct Reorder(Option<Observation>);
        impl Backend for Reorder {
            fn send(&mut self, _: Order) {}
            fn poll(&mut self) -> Vec<Event> {
                self.0.take().map(Event::Observed).into_iter().collect()
            }
        }
    }

    #[test]
    fn buttons_move_the_cursor_through_the_list() {
        let mut screen = open();
        press(&mut screen, &[Button::Down, Button::Down]);
        assert_eq!(screen.cursor(), 2);
        press(&mut screen, &[Button::Down]);
        assert_eq!(screen.cursor(), 0);
        press(&mut screen, &[Button::Up]);
        assert_eq!(screen.cursor(), 2);
    }
}
