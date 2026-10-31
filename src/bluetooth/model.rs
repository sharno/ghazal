//! The Bluetooth screen's state machine, free of I/O: what the radio and the
//! devices look like, which single operation is running, and the orders a backend
//! is given. The BlueZ backend and the fake used by tests and the desktop simulator
//! speak the same orders and events.

use std::fmt;
use std::str::FromStr;

use anyhow::{ensure, Context};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Address([u8; 6]);

impl Address {
    #[cfg(any(test, feature = "desktop"))]
    pub const fn new(bytes: [u8; 6]) -> Self {
        Self(bytes)
    }

    /// The suffix of BlueZ's device object path: `dev_00_11_22_33_44_55`.
    pub fn object_name(self) -> String {
        format!("dev_{}", self.to_string().replace(':', "_"))
    }
}

impl fmt::Display for Address {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a, b, c, d, e, f] = self.0;
        write!(formatter, "{a:02X}:{b:02X}:{c:02X}:{d:02X}:{e:02X}:{f:02X}")
    }
}

impl FromStr for Address {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> anyhow::Result<Self> {
        let parts: Vec<&str> = text.split(':').collect();
        ensure!(parts.len() == 6, "{text:?} is not a Bluetooth address (expected AA:BB:CC:DD:EE:FF)");
        let mut bytes = [0; 6];
        for (byte, part) in bytes.iter_mut().zip(parts) {
            ensure!(part.len() == 2, "{text:?} is not a Bluetooth address (expected AA:BB:CC:DD:EE:FF)");
            *byte = u8::from_str_radix(part, 16).with_context(|| format!("{text:?} is not a Bluetooth address"))?;
        }
        Ok(Self(bytes))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Controller,
    Audio,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    /// Seen while scanning; nothing exchanged yet.
    New,
    Paired { connected: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub address: Address,
    pub name: String,
    pub kind: Kind,
    pub link: Link,
    /// Signal strength, known only while scanning.
    pub rssi: Option<i16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Radio {
    Off,
    On { scanning: bool },
}

/// What a backend sees of the world at one moment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub radio: Radio,
    pub devices: Vec<Device>,
}

/// An action that takes a while and excludes the others: the screen runs one at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Power(bool),
    Pair(Address),
    Connect(Address),
    Disconnect(Address),
    Forget(Address),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    Run(Operation),
    /// Start or stop looking for devices. Quick, so it never blocks the screen.
    Scan(bool),
    /// Abandon the running pairing or connection attempt.
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Observed(Observation),
    Finished(Operation, Result<(), String>),
    ScanFailed(String),
}

/// What the user asked for, before it is resolved against the current state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    TogglePower,
    ToggleScan,
    /// Pair a new device, connect a paired one, or disconnect a connected one.
    Activate(Address),
    Forget(Address),
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The backend has not reported yet.
    NotObserved,
    Busy(Operation),
    PoweredOff,
    UnknownDevice,
    NotPaired,
    NothingToCancel,
}

impl Refusal {
    pub fn text(self) -> &'static str {
        match self {
            Self::NotObserved => "Bluetooth is still starting",
            Self::Busy(Operation::Power(true)) => "Bluetooth is turning on",
            Self::Busy(Operation::Power(false)) => "Bluetooth is turning off",
            Self::Busy(Operation::Pair(_)) => "Pairing; press B to cancel",
            Self::Busy(Operation::Connect(_)) => "Connecting; press B to cancel",
            Self::Busy(Operation::Disconnect(_)) => "Disconnecting",
            Self::Busy(Operation::Forget(_)) => "Forgetting the device",
            Self::PoweredOff => "Turn Bluetooth on first",
            Self::UnknownDevice => "That device is gone",
            Self::NotPaired => "Only paired devices can be forgotten",
            Self::NothingToCancel => "Nothing to cancel",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub text: String,
    pub error: bool,
}

/// An event that no state could have produced: a backend bug worth logging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stray(pub Operation);

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct State {
    /// None until the backend's first observation.
    radio: Option<Radio>,
    devices: Vec<Device>,
    running: Option<Operation>,
}

impl State {
    pub fn radio(&self) -> Option<Radio> {
        self.radio
    }

    pub fn devices(&self) -> &[Device] {
        &self.devices
    }

    pub fn running(&self) -> Option<Operation> {
        self.running
    }

    pub fn device(&self, address: Address) -> Option<&Device> {
        self.devices.iter().find(|device| device.address == address)
    }

    /// Resolves an intent into the order to send, entering the running state when
    /// the order starts an operation.
    pub fn request(&mut self, intent: Intent) -> Result<Order, Refusal> {
        let radio = self.radio.ok_or(Refusal::NotObserved)?;
        if intent == Intent::Cancel {
            return match self.running {
                Some(Operation::Pair(_) | Operation::Connect(_)) => Ok(Order::Cancel),
                _ => Err(Refusal::NothingToCancel),
            };
        }
        if let Some(operation) = self.running {
            return Err(Refusal::Busy(operation));
        }
        let operation = match (intent, radio) {
            (Intent::TogglePower, Radio::Off) => Operation::Power(true),
            (Intent::TogglePower, Radio::On { .. }) => Operation::Power(false),
            (Intent::ToggleScan, Radio::Off) => return Err(Refusal::PoweredOff),
            (Intent::ToggleScan, Radio::On { scanning }) => return Ok(Order::Scan(!scanning)),
            (_, Radio::Off) => return Err(Refusal::PoweredOff),
            (Intent::Activate(address), Radio::On { .. }) => match self.device(address).ok_or(Refusal::UnknownDevice)?.link {
                Link::New => Operation::Pair(address),
                Link::Paired { connected: false } => Operation::Connect(address),
                Link::Paired { connected: true } => Operation::Disconnect(address),
            },
            (Intent::Forget(address), Radio::On { .. }) => match self.device(address).ok_or(Refusal::UnknownDevice)?.link {
                Link::New => return Err(Refusal::NotPaired),
                Link::Paired { .. } => Operation::Forget(address),
            },
            (Intent::Cancel, _) => unreachable!("cancel is answered before the radio is considered"),
        };
        self.running = Some(operation);
        Ok(Order::Run(operation))
    }

    /// Folds in what a backend reported. A finished operation yields the notice to show.
    pub fn apply(&mut self, event: Event) -> Result<Option<Notice>, Stray> {
        match event {
            Event::Observed(observation) => {
                self.radio = Some(observation.radio);
                self.devices = observation.devices;
                Ok(None)
            }
            Event::Finished(operation, result) if self.running == Some(operation) => {
                self.running = None;
                Ok(Some(self.notice(operation, result)))
            }
            Event::Finished(operation, _) => Err(Stray(operation)),
            Event::ScanFailed(text) => Ok(Some(Notice { text: format!("Scanning failed: {text}"), error: true })),
        }
    }

    fn name_of(&self, address: Address) -> String {
        self.device(address).map_or_else(|| address.to_string(), |device| device.name.clone())
    }

    fn notice(&self, operation: Operation, result: Result<(), String>) -> Notice {
        let (success, failure) = match operation {
            Operation::Power(true) => ("Bluetooth on".to_owned(), "Turning Bluetooth on failed".to_owned()),
            Operation::Power(false) => ("Bluetooth off".to_owned(), "Turning Bluetooth off failed".to_owned()),
            Operation::Pair(address) => (format!("Paired with {}", self.name_of(address)), format!("Pairing with {} failed", self.name_of(address))),
            Operation::Connect(address) => (format!("Connected to {}", self.name_of(address)), format!("Connecting to {} failed", self.name_of(address))),
            Operation::Disconnect(address) => (format!("Disconnected from {}", self.name_of(address)), format!("Disconnecting {} failed", self.name_of(address))),
            Operation::Forget(address) => (format!("Forgot {}", self.name_of(address)), format!("Forgetting {} failed", self.name_of(address))),
        };
        match result {
            Ok(()) => Notice { text: success, error: false },
            Err(reason) => Notice { text: format!("{failure}: {reason}"), error: true },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAD: Address = Address::new([0, 1, 2, 3, 4, 5]);
    const HEADSET: Address = Address::new([9, 9, 9, 9, 9, 9]);

    fn device(address: Address, name: &str, link: Link) -> Device {
        Device { address, name: name.into(), kind: Kind::Controller, link, rssi: None }
    }

    fn observed(radio: Radio, devices: Vec<Device>) -> Event {
        Event::Observed(Observation { radio, devices })
    }

    fn state(radio: Radio, devices: Vec<Device>) -> State {
        let mut state = State::default();
        state.apply(observed(radio, devices)).unwrap();
        state
    }

    #[test]
    fn addresses_round_trip_and_name_their_object() {
        let address: Address = "00:1a:2B:3c:4D:5e".parse().unwrap();
        assert_eq!(address.to_string(), "00:1A:2B:3C:4D:5E");
        assert_eq!(address.object_name(), "dev_00_1A_2B_3C_4D_5E");
        for bad in ["", "00:11:22:33:44", "00:11:22:33:44:55:66", "0:11:22:33:44:55", "GG:11:22:33:44:55", "001122334455"] {
            assert!(bad.parse::<Address>().is_err(), "{bad:?} parsed");
        }
    }

    #[test]
    fn nothing_is_possible_before_the_first_observation() {
        let mut state = State::default();
        for intent in [Intent::TogglePower, Intent::ToggleScan, Intent::Activate(PAD), Intent::Forget(PAD), Intent::Cancel] {
            assert_eq!(state.request(intent), Err(Refusal::NotObserved));
        }
    }

    #[test]
    fn power_toggles_through_a_running_operation() {
        let mut state = state(Radio::Off, vec![]);
        assert_eq!(state.request(Intent::TogglePower), Ok(Order::Run(Operation::Power(true))));
        assert_eq!(state.running(), Some(Operation::Power(true)));
        assert_eq!(state.request(Intent::TogglePower), Err(Refusal::Busy(Operation::Power(true))));
        let notice = state.apply(Event::Finished(Operation::Power(true), Ok(()))).unwrap().unwrap();
        assert_eq!(notice, Notice { text: "Bluetooth on".into(), error: false });
        state.apply(observed(Radio::On { scanning: false }, vec![])).unwrap();
        assert_eq!(state.request(Intent::TogglePower), Ok(Order::Run(Operation::Power(false))));
    }

    #[test]
    fn scanning_needs_the_radio_and_never_blocks() {
        let mut off = state(Radio::Off, vec![]);
        assert_eq!(off.request(Intent::ToggleScan), Err(Refusal::PoweredOff));
        let mut on = state(Radio::On { scanning: false }, vec![]);
        assert_eq!(on.request(Intent::ToggleScan), Ok(Order::Scan(true)));
        assert_eq!(on.running(), None);
        on.apply(observed(Radio::On { scanning: true }, vec![])).unwrap();
        assert_eq!(on.request(Intent::ToggleScan), Ok(Order::Scan(false)));
    }

    #[test]
    fn activating_a_device_pairs_connects_or_disconnects_by_its_link() {
        let devices = vec![
            device(PAD, "Pad", Link::New),
            device(HEADSET, "Headset", Link::Paired { connected: false }),
        ];
        let mut state = state(Radio::On { scanning: true }, devices.clone());
        assert_eq!(state.request(Intent::Activate(PAD)), Ok(Order::Run(Operation::Pair(PAD))));
        state.apply(Event::Finished(Operation::Pair(PAD), Ok(()))).unwrap();
        assert_eq!(state.request(Intent::Activate(HEADSET)), Ok(Order::Run(Operation::Connect(HEADSET))));
        state.apply(Event::Finished(Operation::Connect(HEADSET), Ok(()))).unwrap();
        state.apply(observed(Radio::On { scanning: true }, vec![device(HEADSET, "Headset", Link::Paired { connected: true })])).unwrap();
        assert_eq!(state.request(Intent::Activate(HEADSET)), Ok(Order::Run(Operation::Disconnect(HEADSET))));
        state.apply(Event::Finished(Operation::Disconnect(HEADSET), Ok(()))).unwrap();
        assert_eq!(state.request(Intent::Activate(PAD)), Err(Refusal::UnknownDevice));
    }

    #[test]
    fn only_paired_devices_are_forgotten() {
        let mut state = state(Radio::On { scanning: false }, vec![device(PAD, "Pad", Link::New), device(HEADSET, "Headset", Link::Paired { connected: true })]);
        assert_eq!(state.request(Intent::Forget(PAD)), Err(Refusal::NotPaired));
        assert_eq!(state.request(Intent::Forget(HEADSET)), Ok(Order::Run(Operation::Forget(HEADSET))));
        let notice = state.apply(Event::Finished(Operation::Forget(HEADSET), Ok(()))).unwrap().unwrap();
        assert_eq!(notice.text, "Forgot Headset");
    }

    #[test]
    fn cancel_only_applies_to_pairing_and_connecting() {
        let mut state = state(Radio::On { scanning: false }, vec![device(PAD, "Pad", Link::New)]);
        assert_eq!(state.request(Intent::Cancel), Err(Refusal::NothingToCancel));
        state.request(Intent::Activate(PAD)).unwrap();
        assert_eq!(state.request(Intent::Cancel), Ok(Order::Cancel));
        // Still running: only the backend's report ends the operation.
        assert_eq!(state.running(), Some(Operation::Pair(PAD)));
        let notice = state.apply(Event::Finished(Operation::Pair(PAD), Err("cancelled".into()))).unwrap().unwrap();
        assert_eq!(notice, Notice { text: "Pairing with Pad failed: cancelled".into(), error: true });
        state.request(Intent::TogglePower).unwrap();
        assert_eq!(state.request(Intent::Cancel), Err(Refusal::NothingToCancel));
    }

    #[test]
    fn a_report_for_an_operation_that_is_not_running_is_stray() {
        let mut state = state(Radio::On { scanning: false }, vec![]);
        assert_eq!(state.apply(Event::Finished(Operation::Power(false), Ok(()))), Err(Stray(Operation::Power(false))));
        state.request(Intent::TogglePower).unwrap();
        assert_eq!(state.apply(Event::Finished(Operation::Power(true), Ok(()))), Err(Stray(Operation::Power(true))));
        assert_eq!(state.running(), Some(Operation::Power(false)));
    }

    #[test]
    fn unknown_devices_are_named_by_address() {
        let mut state = state(Radio::On { scanning: false }, vec![device(PAD, "Pad", Link::New)]);
        state.request(Intent::Activate(PAD)).unwrap();
        state.apply(observed(Radio::On { scanning: false }, vec![])).unwrap();
        let notice = state.apply(Event::Finished(Operation::Pair(PAD), Ok(()))).unwrap().unwrap();
        assert_eq!(notice.text, "Paired with 00:01:02:03:04:05");
    }

    #[test]
    fn scan_failures_become_error_notices() {
        let mut state = state(Radio::On { scanning: false }, vec![]);
        let notice = state.apply(Event::ScanFailed("not ready".into())).unwrap().unwrap();
        assert!(notice.error && notice.text.contains("not ready"));
    }
}
