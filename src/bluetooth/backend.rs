//! What the screen talks to: the real BlueZ worker on the device, a scripted fake in
//! tests and in the desktop simulator.

use super::model::{Event, Order};

pub trait Backend {
    /// Starts an order without waiting for it.
    fn send(&mut self, order: Order);
    /// Everything reported since the last call.
    fn poll(&mut self) -> Vec<Event>;
}

/// The simulator has no BlueZ, so it shows a believable world instead
/// (`GHAZAL_BLUETOOTH=real` talks to the host's BlueZ).
pub fn open() -> anyhow::Result<Box<dyn Backend>> {
    #[cfg(feature = "desktop")]
    if std::env::var_os("GHAZAL_BLUETOOTH").is_none_or(|choice| choice != "real") {
        return Ok(Box::new(fake::Fake::demo()));
    }
    Ok(Box::new(super::worker::Worker::spawn()?))
}

#[cfg(any(test, feature = "desktop"))]
pub mod fake {
    use super::{Backend, Event, Order};
    use crate::bluetooth::model::{Address, Device, Kind, Link, Observation, Operation, Radio};

    /// A world that obeys orders instantly. Pairing a device connects it, as BlueZ
    /// does once the device is trusted.
    pub struct Fake {
        radio: Radio,
        devices: Vec<Device>,
        /// Devices that appear when scanning starts.
        nearby: Vec<Device>,
        reports: Vec<Event>,
        failure: Option<String>,
        reported: bool,
    }

    impl Fake {
        pub fn new(radio: Radio, devices: Vec<Device>, nearby: Vec<Device>) -> Self {
            Self { radio, devices, nearby, reports: Vec::new(), failure: None, reported: false }
        }

        pub fn demo() -> Self {
            let device = |last, name: &str, kind, link, rssi| Device { address: Address::new([0xE4, 0x17, 0xD8, 0, 0, last]), name: name.into(), kind, link, rssi };
            Self::new(
                Radio::On { scanning: false },
                vec![device(1, "Wireless Controller", Kind::Controller, Link::Paired { connected: true }, None)],
                vec![
                    device(2, "Pro Controller", Kind::Controller, Link::New, Some(-48)),
                    device(3, "8BitDo SN30 Pro", Kind::Controller, Link::New, Some(-63)),
                    device(4, "WH-1000XM5", Kind::Audio, Link::New, Some(-71)),
                ],
            )
        }

        /// The next operation fails with this reason.
        #[cfg(test)]
        pub fn failing(mut self, reason: &str) -> Self {
            self.failure = Some(reason.to_owned());
            self
        }

        fn observation(&self) -> Event {
            Event::Observed(Observation { radio: self.radio, devices: self.devices.clone() })
        }

        fn run(&mut self, operation: Operation) -> Result<(), String> {
            if let Some(reason) = self.failure.take() {
                return Err(reason);
            }
            let link_of = |devices: &mut Vec<Device>, address, link| {
                if let Some(device) = devices.iter_mut().find(|device| device.address == address) {
                    device.link = link;
                }
            };
            match operation {
                Operation::Power(on) => self.radio = if on { Radio::On { scanning: false } } else { Radio::Off },
                Operation::Pair(address) | Operation::Connect(address) => {
                    self.radio = Radio::On { scanning: false };
                    link_of(&mut self.devices, address, Link::Paired { connected: true });
                }
                Operation::Disconnect(address) => link_of(&mut self.devices, address, Link::Paired { connected: false }),
                Operation::Forget(address) => link_of(&mut self.devices, address, Link::New),
            }
            Ok(())
        }
    }

    impl Backend for Fake {
        fn send(&mut self, order: Order) {
            match order {
                Order::Run(operation) => {
                    let result = self.run(operation);
                    self.reports.push(Event::Finished(operation, result));
                }
                Order::Scan(on) => {
                    if let Radio::On { scanning } = &mut self.radio {
                        *scanning = on;
                        if on {
                            let known = self.devices.clone();
                            self.devices.extend(self.nearby.iter().filter(|device| !known.iter().any(|seen| seen.address == device.address)).cloned());
                        }
                    }
                }
                Order::Cancel => {}
            }
            self.reports.push(self.observation());
        }

        fn poll(&mut self) -> Vec<Event> {
            if !std::mem::replace(&mut self.reported, true) {
                self.reports.insert(0, self.observation());
            }
            std::mem::take(&mut self.reports)
        }
    }
}
