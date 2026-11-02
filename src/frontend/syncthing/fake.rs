//! A Syncthing that exists only in memory: it drives the screen's tests and the
//! desktop simulator, which must never touch the host's Syncthing.

use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::syncthing::{
    clock, Backend, Destination, Device, DeviceId, Exposure, Failure, Folder, FolderId, FolderState, LastSeen, Listening, Me, Observation, Offerers,
    Order, Password, PendingDevice, PendingFolder, Presence, Service, Snapshot, Switch, Timestamp, WebInterface,
};

struct State {
    switch: Switch,
    snapshot: Snapshot,
    web: WebInterface,
    fail_next: Option<Failure>,
    orders: Vec<Order>,
    observations: usize,
}

pub struct FakeBackend {
    state: Mutex<State>,
}

fn device_id(text: &str) -> DeviceId { DeviceId::parse(text).expect("fixture device ID") }
fn folder_id(text: &str) -> FolderId { FolderId::parse(text).expect("fixture folder ID") }

impl FakeBackend {
    /// What the simulator shows: a handheld syncing saves with a PC, a phone
    /// away, a laptop asking to pair and the PC offering its ROMS.
    pub fn demo() -> Self {
        let pc = device_id("VXRHI74-4BCA6TM-PVEKYIJ-CWZMMHF-7S4PYPE-AYZMM36-3DNWBSQ-LYPK7QK");
        let snapshot = Snapshot {
            me: Me { id: device_id("HJ56AJG-RR4EN7L-B2WJ3UI-2NNTZCS-2PZ4P3W-R3SZJHF-S25SDML-LBSRGQI"), name: "ghazal".into() },
            folders: vec![
                Folder { id: folder_id("saves"), label: "Saves".into(), path: "/mnt/games/SAVES".into(), state: FolderState::Syncing { percent: 42 } },
                Folder { id: folder_id("bios"), label: "BIOS".into(), path: "/mnt/games/BIOS".into(), state: FolderState::UpToDate },
                Folder { id: folder_id("shots"), label: "Screenshots".into(), path: "/mnt/games/Screenshots".into(), state: FolderState::Waiting },
            ],
            devices: vec![
                Device { id: pc.clone(), name: "Desktop PC".into(), presence: Presence::Connected },
                Device { id: device_id("IWP6Z7O-5JWUTS3-NJT7WZP-NRR2JSL-PSEOQBK-DFR2AFD-BOOPH7X-3HOMRAR"), name: "Phone".into(), presence: Presence::Away(LastSeen::At(Timestamp::from_unix(clock().unix() - 3 * 3600))) },
            ],
            pending_devices: vec![PendingDevice { id: device_id("A3CBDLG-3IC4KJK-LNIRIWF-LDE4AJQ-236RYBQ-6C7KPC4-M77ZJS6-LQHJKAG"), name: "Laptop".into(), address: "192.168.1.40:22000".into() }],
            pending_folders: vec![PendingFolder {
                id: folder_id("roms"),
                label: "ROMS".into(),
                offered_by: Offerers { first: pc, others: Vec::new() },
                destination: Destination::Merge("/mnt/games/ROMS".into()),
            }],
            listening: Listening::Local,
        };
        Self { state: Mutex::new(State { switch: Switch::On, snapshot, web: WebInterface::Local(None), fail_next: None, orders: Vec::new(), observations: 0 }) }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    #[cfg(test)]
    pub fn fail_next(&self, failure: Failure) { self.state().fail_next = Some(failure); }

    #[cfg(test)]
    pub fn orders(&self) -> Vec<Order> { self.state().orders.clone() }

    #[cfg(test)]
    pub fn observations(&self) -> usize { self.state().observations }
}

impl State {
    fn apply(&mut self, order: &Order) -> Result<(), Failure> {
        let snapshot = &mut self.snapshot;
        match order {
            Order::Switch(switch) => self.switch = *switch,
            Order::Rescan(id) if snapshot.folder(id).is_some() => {}
            Order::Rescan(_) => return Err(Failure::Status { code: 404, message: "no such folder".into() }),
            Order::AcceptDevice(device) => {
                snapshot.pending_devices.retain(|pending| pending.id != device.id);
                snapshot.devices.push(Device { id: device.id.clone(), name: device.title(), presence: Presence::Connected });
            }
            Order::IgnoreDevice(device) => snapshot.pending_devices.retain(|pending| pending.id != device.id),
            Order::AcceptFolder(folder, path) => {
                snapshot.pending_folders.retain(|pending| pending.id != folder.id);
                snapshot.folders.push(Folder { id: folder.id.clone(), label: folder.label.clone(), path: path.clone(), state: FolderState::UpToDate });
            }
            Order::IgnoreFolder(folder) => snapshot.pending_folders.retain(|pending| pending.id != folder.id),
            Order::Expose(Exposure::Network) => {
                let login = self.web.login().cloned().or_else(|| Password::parse("demo-pass-word-1234")).expect("fixture password");
                self.web = WebInterface::Network(login);
                snapshot.listening = Listening::Network;
            }
            Order::Expose(Exposure::Local) => {
                self.web = WebInterface::Local(self.web.login().cloned());
                snapshot.listening = Listening::Local;
            }
            Order::NewPassword => {
                let password = Password::parse("next-pass-word-5678").expect("fixture password");
                self.web = match self.web { WebInterface::Local(_) => WebInterface::Local(Some(password)), WebInterface::Network(_) => WebInterface::Network(password) };
            }
        }
        Ok(())
    }
}

impl Backend for FakeBackend {
    fn observe(&self) -> Observation {
        let mut state = self.state();
        state.observations += 1;
        let service = match state.switch { Switch::On => Service::Running(state.snapshot.clone()), Switch::Off => Service::Off };
        Observation { service, web: state.web.clone(), address: Some("192.168.1.133".parse().expect("fixture address")) }
    }

    fn carry_out(&self, order: &Order) -> Result<(), Failure> {
        let mut state = self.state();
        if let Some(failure) = state.fail_next.take() { return Err(failure); }
        state.orders.push(order.clone());
        state.apply(order)
    }
}
