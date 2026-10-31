//! `ghazal bluetooth`: the screen's operations from a shell, for diagnosing over SSH and
//! for the supervisor's boot-time restore.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};

use super::bluez::Session;
use super::hardware::{self, Controller, Preference};
use super::model::{Address, Device, Kind, Link, Observation, Operation, Radio};

pub const USAGE: &str = "usage: ghazal bluetooth status|on|off|devices|restore|scan [SECONDS]|pair|connect|disconnect|forget ADDRESS";

pub fn run(args: &[String]) -> Result<()> {
    let mut session = Session::new(Arc::new(AtomicBool::new(false)));
    let operation = |session: &mut Session, operation: Operation| session.execute(operation).map_err(|reason| anyhow!(reason));
    let address = |index: usize| -> Result<Address> { args.get(index).ok_or_else(|| anyhow!(USAGE))?.parse() };
    match args.first().map(String::as_str) {
        Some("status") | None => {
            status(&mut session);
            Ok(())
        }
        Some("on") => operation(&mut session, Operation::Power(true)),
        Some("off") => operation(&mut session, Operation::Power(false)),
        Some("devices") => {
            print_devices(&session.observe());
            Ok(())
        }
        // Boot: power Bluetooth if it was on when the device last ran.
        Some("restore") if Preference::load().enabled => operation(&mut session, Operation::Power(true)),
        Some("restore") => {
            println!("Bluetooth is off");
            Ok(())
        }
        Some("scan") => {
            let seconds = args.get(1).map_or(Ok(10), |text| text.parse::<u64>())?;
            scan(&mut session, Duration::from_secs(seconds))
        }
        Some("pair") => operation(&mut session, Operation::Pair(address(1)?)),
        Some("connect") => operation(&mut session, Operation::Connect(address(1)?)),
        Some("disconnect") => operation(&mut session, Operation::Disconnect(address(1)?)),
        Some("forget") => operation(&mut session, Operation::Forget(address(1)?)),
        Some(other) => bail!("unknown bluetooth command {other:?}\n{USAGE}"),
    }
}

fn status(session: &mut Session) {
    let controller = match hardware::controller() {
        Controller::Absent => "absent",
        Controller::Dormant(_) => "not started",
        Controller::Registered => "registered",
    };
    println!("controller: {controller}");
    println!("preference: {}", if Preference::load().enabled { "on" } else { "off" });
    let observation = session.observe();
    print_devices(&observation);
}

fn print_devices(observation: &Observation) {
    println!("radio: {}", match observation.radio {
        Radio::Off => "off",
        Radio::On { scanning: false } => "on",
        Radio::On { scanning: true } => "on, scanning",
    });
    observation.devices.iter().for_each(|device| println!("{}", describe(device)));
}

fn describe(device: &Device) -> String {
    let link = match device.link {
        Link::New => "new",
        Link::Paired { connected: false } => "paired",
        Link::Paired { connected: true } => "connected",
    };
    let kind = match device.kind {
        Kind::Controller => "controller",
        Kind::Audio => "audio",
        Kind::Other => "other",
    };
    let signal = device.rssi.map_or(String::new(), |rssi| format!(" {rssi} dBm"));
    format!("{} {link:<9} {kind:<10} {}{signal}", device.address, device.name)
}

/// Scans for `duration`, printing each device the first time it shows up.
fn scan(session: &mut Session, duration: Duration) -> Result<()> {
    session.scan(true).map_err(|reason| anyhow!(reason))?;
    let deadline = Instant::now() + duration;
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        session.idle(Duration::from_secs(1));
        for device in session.observe().devices {
            if !seen.contains(&device.address) {
                println!("{}", describe(&device));
                seen.push(device.address);
            }
        }
    }
    session.shutdown();
    println!("{} device(s) found", seen.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devices_are_described_on_one_line() {
        let device = Device {
            address: Address::new([0xE4, 0x17, 0xD8, 0, 0, 1]),
            name: "Pro Controller".into(),
            kind: Kind::Controller,
            link: Link::Paired { connected: true },
            rssi: Some(-48),
        };
        assert_eq!(describe(&device), "E4:17:D8:00:00:01 connected controller Pro Controller -48 dBm");
    }

    #[test]
    fn arguments_are_validated_before_touching_bluetooth() {
        assert!(run(&["frobnicate".into()]).unwrap_err().to_string().contains("usage"));
        assert!(run(&["pair".into()]).unwrap_err().to_string().contains("usage"));
        assert!(run(&["pair".into(), "nonsense".into()]).is_err());
        assert!(run(&["scan".into(), "soon".into()]).is_err());
    }
}
