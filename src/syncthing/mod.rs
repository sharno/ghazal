//! Syncthing on the handheld: the supervised service (`ghazal syncthing serve`),
//! the player's choices on the data card, a REST client for the menu's
//! Configuration → Syncthing screen and status icon, and `ghazal syncthing` for SSH.
//! docs/syncthing.md describes what the player sees.
//!
//! Layers, from the outside in: `wire` turns Syncthing's JSON into `model`
//! values; `client` speaks HTTP to Syncthing; `system` adds the service and the
//! data card and implements `Backend`, which the screen drives.

mod client;
mod model;
mod preference;
mod system;
mod wire;

use std::path::PathBuf;

use anyhow::{anyhow, bail, Result};

pub use client::Failure;
pub use model::{Destination, DeviceId, Exposure, FolderId, LastSeen, Listening, Observation, Password, PendingDevice, PendingFolder, Presence, Service, Snapshot, Summary, Switch, Timestamp, WebInterface};
/// For the screen's pretend Syncthing.
#[cfg(any(test, feature = "desktop"))]
pub use model::{Device, Folder, FolderState, Me, Offerers};
pub use system::{clock, System};

/// What the player can ask of Syncthing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Order {
    Switch(Switch),
    Rescan(FolderId),
    AcceptDevice(PendingDevice),
    IgnoreDevice(PendingDevice),
    /// The folder, and where on the card it goes.
    AcceptFolder(PendingFolder, PathBuf),
    IgnoreFolder(PendingFolder),
    Expose(Exposure),
    NewPassword,
}

/// Everything blocks for up to a few seconds: callers run these on a worker thread.
pub trait Backend: Send + Sync {
    fn observe(&self) -> Observation;
    fn carry_out(&self, order: &Order) -> Result<(), Failure>;
}

/// Whether this image has Syncthing.
pub fn available() -> bool {
    std::path::Path::new(system::BINARY).exists()
}

pub const USAGE: &str = "usage: ghazal syncthing status|on|off|serve|restore|rescan FOLDER-ID|accept ID|ignore ID|web on|off|password";

/// `ghazal syncthing`: the service's entry points and the screen's actions from a shell.
pub fn run(args: &[String]) -> Result<()> {
    let system = System::default();
    let order = |order: Order| system.carry_out(&order).map_err(|failure| anyhow!(failure));
    let argument = |index: usize| args.get(index).map(String::as_str).ok_or_else(|| anyhow!(USAGE));
    match args.first().map(String::as_str) {
        Some("serve") => system.serve(),
        Some("restore") => system.restore(),
        Some("status") | None => { print!("{}", describe(&system.observe())); Ok(()) }
        Some("on") => order(Order::Switch(Switch::On)),
        Some("off") => order(Order::Switch(Switch::Off)),
        Some("rescan") => order(Order::Rescan(FolderId::parse(argument(1)?).ok_or_else(|| anyhow!(USAGE))?)),
        Some("accept") => order(pending_order(&system.observe(), Answer::Accept, argument(1)?)?),
        Some("ignore") => order(pending_order(&system.observe(), Answer::Ignore, argument(1)?)?),
        Some("web") => order(Order::Expose(match argument(1)? { "on" => Exposure::Network, "off" => Exposure::Local, _ => bail!(USAGE) })),
        Some("password") => {
            order(Order::NewPassword)?;
            let observation = system.observe();
            println!("user {} password {}", Password::USER, observation.web.login().map_or("(none)", Password::expose));
            Ok(())
        }
        Some(other) => bail!("unknown syncthing command {other:?}\n{USAGE}"),
    }
}

#[derive(Clone, Copy)]
enum Answer {
    Accept,
    Ignore,
}

/// Accepting or ignoring the pending device or folder with this ID.
fn pending_order(observation: &Observation, answer: Answer, id: &str) -> Result<Order> {
    let snapshot = observation.service.snapshot().ok_or_else(|| anyhow!("Syncthing is not running"))?;
    if let Some(device) = DeviceId::parse(id).and_then(|id| snapshot.pending_device(&id).cloned()) {
        return Ok(match answer { Answer::Accept => Order::AcceptDevice(device), Answer::Ignore => Order::IgnoreDevice(device) });
    }
    let folder = FolderId::parse(id).and_then(|id| snapshot.pending_folder(&id).cloned()).ok_or_else(|| anyhow!("nothing pending is called {id:?}"))?;
    match (answer, &folder.destination) {
        (Answer::Ignore, _) => Ok(Order::IgnoreFolder(folder)),
        (Answer::Accept, Destination::New(path) | Destination::Empty(path) | Destination::Merge(path)) => {
            let path = path.clone();
            Ok(Order::AcceptFolder(folder, path))
        }
        (Answer::Accept, Destination::Taken { by, .. }) => bail!("that directory is already synced as {by:?}"),
        (Answer::Accept, Destination::Unusable) => bail!("the label {:?} cannot name a directory", folder.label),
    }
}

/// The observation as text, one fact per line.
fn describe(observation: &Observation) -> String {
    let service = match &observation.service {
        Service::Off => "off".to_owned(),
        Service::NoDataCard => "waiting for the data card".to_owned(),
        Service::Starting => "starting".to_owned(),
        Service::Failing(failure) => format!("failing: {failure}"),
        Service::Running(snapshot) => format!("running ({:?})", snapshot.summary()),
    };
    let listening = observation.service.snapshot().map(|snapshot| snapshot.listening);
    let web = match (&observation.web, listening) {
        (WebInterface::Local(_), _) => "on this handheld".to_owned(),
        (WebInterface::Network(_), Some(Listening::Network)) => format!("on the network at http://{}:8384", observation.address.map_or_else(|| "?".to_owned(), |address| address.to_string())),
        (WebInterface::Network(_), Some(Listening::Local) | None) => "chosen for the network, still on this handheld".to_owned(),
    };
    let lines = [format!("syncthing: {service}"), format!("web interface: {web}")].into_iter();
    let Some(snapshot) = observation.service.snapshot() else { return lines.map(|line| line + "\n").collect() };
    let details = [format!("this device: {} {}", snapshot.me.id, snapshot.me.name)].into_iter()
        .chain(snapshot.folders.iter().map(|folder| format!("folder {} {:?} {} {}", folder.id, folder.label, folder.path.display(), folder.state.describe())))
        .chain(snapshot.devices.iter().map(|device| format!("device {} {:?} {:?}", device.id, device.name, device.presence)))
        .chain(snapshot.pending_devices.iter().map(|device| format!("pending device {} {:?} from {}", device.id, device.name, device.address)))
        .chain(snapshot.pending_folders.iter().map(|folder| format!("pending folder {} {:?} from {} to {:?}", folder.id, folder.label, folder.offered_by.first, folder.destination)));
    lines.chain(details).map(|line| line + "\n").collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_are_checked_before_touching_syncthing() {
        assert!(run(&["frobnicate".into()]).unwrap_err().to_string().contains("usage"));
        assert!(run(&["web".into(), "maybe".into()]).unwrap_err().to_string().contains("usage"));
        assert!(run(&["rescan".into()]).unwrap_err().to_string().contains("usage"));
    }
}
