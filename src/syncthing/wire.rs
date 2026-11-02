//! Syncthing's REST JSON, as v2 sends it, and its translation into `model`.
//! Fields the menu does not use are left out; serde skips them.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::model::{Destination, Device, DeviceId, Folder, FolderId, FolderState, LastSeen, Listening, Me, Offerers, PendingDevice, PendingFolder, Presence, Snapshot, Timestamp};

/// Why a reply could not be understood.
pub type Malformed = String;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemStatus {
    #[serde(rename = "myID")]
    pub my_id: String,
    pub gui_address_used: String,
}

#[derive(Deserialize)]
pub struct FolderConfig {
    pub id: String,
    #[serde(default)]
    pub label: String,
    pub path: String,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub devices: Vec<FolderDevice>,
}

#[derive(Deserialize, Serialize)]
pub struct FolderDevice {
    #[serde(rename = "deviceID")]
    pub device_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderStatus {
    pub state: String,
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub need_total_items: u64,
    #[serde(default)]
    pub pull_errors: u64,
    #[serde(default)]
    pub global_bytes: u64,
    #[serde(default)]
    pub in_sync_bytes: u64,
}

#[derive(Deserialize)]
pub struct DeviceConfig {
    #[serde(rename = "deviceID")]
    pub device_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub paused: bool,
}

#[derive(Deserialize)]
pub struct Connections {
    pub connections: BTreeMap<String, Connection>,
}

#[derive(Deserialize)]
pub struct Connection {
    pub connected: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceStatistics {
    pub last_seen: String,
}

#[derive(Deserialize)]
pub struct PendingDeviceEntry {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub address: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingFolderEntry {
    pub offered_by: BTreeMap<String, Offer>,
}

#[derive(Deserialize)]
pub struct Offer {
    #[serde(default)]
    pub label: String,
}

/// A new paired device; Syncthing fills in the rest from its defaults.
#[derive(Serialize)]
pub struct NewDevice<'a> {
    #[serde(rename = "deviceID")]
    pub device_id: &'a str,
    pub name: &'a str,
}

/// A new folder; Syncthing adds this handheld to `devices` and fills in the rest.
#[derive(Serialize)]
pub struct NewFolder<'a> {
    pub id: &'a str,
    pub label: &'a str,
    pub path: &'a str,
    pub devices: Vec<FolderDevice>,
}

#[derive(Serialize)]
pub struct Login<'a> {
    pub user: &'a str,
    /// Plain text; Syncthing stores only its bcrypt hash.
    pub password: &'a str,
}

/// The whole configuration, for the one list without an endpoint of its own.
/// Everything else goes back to Syncthing untouched.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WholeConfig {
    #[serde(default)]
    pub remote_ignored_devices: Vec<serde_json::Value>,
    #[serde(flatten)]
    pub rest: serde_json::Map<String, serde_json::Value>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IgnoredDevice<'a> {
    pub time: &'a str,
    #[serde(rename = "deviceID")]
    pub device_id: &'a str,
    pub name: &'a str,
    pub address: &'a str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceIgnores {
    #[serde(default)]
    pub ignored_folders: Vec<serde_json::Value>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceIgnoresUpdate {
    pub ignored_folders: Vec<serde_json::Value>,
}

#[derive(Serialize)]
pub struct IgnoredFolder<'a> {
    pub time: &'a str,
    pub id: &'a str,
    pub label: &'a str,
}

/// Every reply one refresh needs, as Syncthing sent them.
pub struct Replies {
    pub status: SystemStatus,
    pub folders: Vec<(FolderConfig, FolderStatus)>,
    pub devices: Vec<DeviceConfig>,
    pub connections: Connections,
    pub statistics: BTreeMap<String, DeviceStatistics>,
    pub pending_devices: BTreeMap<String, PendingDeviceEntry>,
    pub pending_folders: BTreeMap<String, PendingFolderEntry>,
}

fn device_id(text: &str) -> Result<DeviceId, Malformed> {
    DeviceId::parse(text).ok_or_else(|| format!("invalid device ID {text:?}"))
}

fn folder_id(text: &str) -> Result<FolderId, Malformed> {
    FolderId::parse(text).ok_or_else(|| "empty folder ID".to_owned())
}

fn labelled(label: &str, id: &str) -> String {
    if label.is_empty() { id.to_owned() } else { label.to_owned() }
}

/// How a folder looks, the way Syncthing's own web interface decides it.
pub fn folder_state(config: &FolderConfig, status: &FolderStatus) -> FolderState {
    if config.paused { return FolderState::Paused; }
    match status.state.as_str() {
        "idle" if status.pull_errors > 0 => FolderState::Errors(status.pull_errors),
        "idle" if status.need_total_items > 0 => FolderState::Waiting,
        // The list includes this handheld.
        "idle" if config.devices.len() <= 1 => FolderState::Unshared,
        "idle" => FolderState::UpToDate,
        "scanning" | "scan-waiting" | "scan-queued" => FolderState::Scanning,
        "syncing" | "sync-preparing" | "sync-waiting" | "sync-queued" => {
            let percent = (status.in_sync_bytes.saturating_mul(100)).checked_div(status.global_bytes).unwrap_or(0);
            FolderState::Syncing { percent: percent.min(99) as u8 }
        }
        "cleaning" | "clean-waiting" => FolderState::Cleaning,
        "error" => FolderState::Stopped(status.error.clone()),
        // Paused folders (handled above) and folders still starting report none.
        "" => FolderState::Other("unknown".into()),
        other => FolderState::Other(other.to_owned()),
    }
}

fn presence(config: &DeviceConfig, connections: &Connections, statistics: &BTreeMap<String, DeviceStatistics>) -> Presence {
    let connected = connections.connections.get(&config.device_id).is_some_and(|connection| connection.connected);
    match (config.paused, connected) {
        (true, _) => Presence::Paused,
        (false, true) => Presence::Connected,
        // Go's zero time and the epoch both mean "never".
        (false, false) => Presence::Away(match statistics.get(&config.device_id).and_then(|statistics| Timestamp::parse(&statistics.last_seen)) {
            Some(time) if time.unix() > 0 => LastSeen::At(time),
            _ => LastSeen::Never,
        }),
    }
}

/// A folder offered by at least one device; `None` when nobody offers it any more.
fn pending_folder(id: &str, entry: &PendingFolderEntry, destination: impl Fn(&str) -> Destination) -> Option<Result<PendingFolder, Malformed>> {
    let mut offers = entry.offered_by.iter();
    let (first, offer) = offers.next()?;
    let label = labelled(&offer.label, id);
    let offered_by = device_id(first).and_then(|first| Ok(Offerers { first, others: offers.map(|(device, _)| device_id(device)).collect::<Result<_, _>>()? }));
    Some(offered_by.and_then(|offered_by| Ok(PendingFolder { id: folder_id(id)?, destination: destination(&label), label, offered_by })))
}

/// Builds the snapshot. `destination` says where each pending folder would go;
/// it is the only part that looks at the data card.
pub fn snapshot(replies: Replies, destination: impl Fn(&str, &[Folder]) -> Destination) -> Result<Snapshot, Malformed> {
    let me = device_id(&replies.status.my_id)?;
    let name = replies.devices.iter().find(|device| DeviceId::parse(&device.device_id).as_ref() == Some(&me)).map(|device| device.name.clone()).unwrap_or_default();
    let folders = replies.folders.iter().map(|(config, status)| Ok(Folder {
        id: folder_id(&config.id)?,
        label: labelled(&config.label, &config.id),
        path: PathBuf::from(&config.path),
        state: folder_state(config, status),
    })).collect::<Result<Vec<_>, Malformed>>()?;
    let devices = replies.devices.iter()
        .map(|config| Ok((device_id(&config.device_id)?, config)))
        .filter(|entry| !matches!(entry, Ok((id, _)) if *id == me))
        .map(|entry| entry.map(|(id, config)| Device { id, name: labelled(&config.name, &config.device_id), presence: presence(config, &replies.connections, &replies.statistics) }))
        .collect::<Result<Vec<_>, Malformed>>()?;
    let pending_devices = replies.pending_devices.iter()
        .map(|(id, entry)| Ok(PendingDevice { id: device_id(id)?, name: entry.name.clone(), address: entry.address.clone() }))
        .collect::<Result<Vec<_>, Malformed>>()?;
    let pending_folders = replies.pending_folders.iter()
        .filter_map(|(id, entry)| pending_folder(id, entry, |label| destination(label, &folders)))
        .collect::<Result<Vec<_>, Malformed>>()?;
    Ok(Snapshot { me: Me { id: me, name }, folders, devices, pending_devices, pending_folders, listening: Listening::from_address(&replies.status.gui_address_used) })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "HJ56AJG-RR4EN7L-B2WJ3UI-2NNTZCS-2PZ4P3W-R3SZJHF-S25SDML-LBSRGQI";
    const PC: &str = "VXRHI74-4BCA6TM-PVEKYIJ-CWZMMHF-7S4PYPE-AYZMM36-3DNWBSQ-LYPK7QK";
    const PHONE: &str = "IWP6Z7O-5JWUTS3-NJT7WZP-NRR2JSL-PSEOQBK-DFR2AFD-BOOPH7X-3HOMRAR";

    fn status(state: &str) -> FolderStatus {
        FolderStatus { state: state.into(), error: String::new(), need_total_items: 0, pull_errors: 0, global_bytes: 200, in_sync_bytes: 84 }
    }

    fn config(devices: usize) -> FolderConfig {
        FolderConfig { id: "saves".into(), label: "Saves".into(), path: "/mnt/games/SAVES".into(), paused: false, devices: [ME, PC, PHONE].iter().take(devices).map(|id| FolderDevice { device_id: (*id).into() }).collect() }
    }

    #[test]
    fn folder_states_follow_syncthings_web_interface() {
        let shared = config(2);
        assert_eq!(folder_state(&shared, &status("idle")), FolderState::UpToDate);
        assert_eq!(folder_state(&config(1), &status("idle")), FolderState::Unshared);
        assert_eq!(folder_state(&shared, &FolderStatus { pull_errors: 3, ..status("idle") }), FolderState::Errors(3));
        assert_eq!(folder_state(&shared, &FolderStatus { need_total_items: 5, ..status("idle") }), FolderState::Waiting);
        assert_eq!(folder_state(&shared, &status("syncing")), FolderState::Syncing { percent: 42 });
        assert_eq!(folder_state(&shared, &FolderStatus { in_sync_bytes: 200, ..status("sync-preparing") }), FolderState::Syncing { percent: 99 });
        assert_eq!(folder_state(&shared, &FolderStatus { global_bytes: 0, ..status("syncing") }), FolderState::Syncing { percent: 0 });
        assert_eq!(folder_state(&shared, &status("scan-waiting")), FolderState::Scanning);
        assert_eq!(folder_state(&shared, &FolderStatus { error: "folder path missing".into(), ..status("error") }), FolderState::Stopped("folder path missing".into()));
        assert_eq!(folder_state(&shared, &status("pondering")), FolderState::Other("pondering".into()));
        assert_eq!(folder_state(&FolderConfig { paused: true, ..config(2) }, &status("")), FolderState::Paused);
    }

    /// Replies as Syncthing 2.1.3 sent them in a two-device test, trimmed.
    fn json<T: serde::de::DeserializeOwned>(text: &str) -> T { serde_json::from_str(text).unwrap() }

    fn replies() -> Replies {
        Replies {
            status: json(&format!(r#"{{"myID":"{ME}","guiAddressUsed":"127.0.0.1:8384","uptime":23,"alloc":6519984}}"#)),
            folders: vec![(json(&format!(r#"{{"id":"abcd1-saves","label":"Saves","path":"/mnt/games/SAVES","type":"sendreceive","paused":false,"devices":[{{"deviceID":"{ME}","introducedBy":""}},{{"deviceID":"{PC}","introducedBy":""}}]}}"#)),
                json(r#"{"errors":0,"pullErrors":0,"globalBytes":623,"inSyncBytes":623,"needTotalItems":0,"state":"idle","stateChanged":"2026-11-02T14:40:46Z","error":""}"#))],
            devices: json(&format!(r#"[{{"deviceID":"{ME}","name":"ghazal","paused":false}},{{"deviceID":"{PC}","name":"Desktop PC","paused":false}},{{"deviceID":"{PHONE}","name":"","paused":false}}]"#)),
            connections: json(&format!(r#"{{"connections":{{"{PC}":{{"connected":true,"paused":false,"clientVersion":"v2.1.3"}},"{PHONE}":{{"connected":false,"paused":false}}}},"total":{{}}}}"#)),
            statistics: json(&format!(r#"{{"{ME}":{{"lastSeen":"1970-01-01T00:00:00Z"}},"{PC}":{{"lastSeen":"2026-11-02T14:46:14Z"}},"{PHONE}":{{"lastSeen":"2026-11-01T09:00:00Z","lastConnectionDurationS":0}}}}"#)),
            pending_devices: json(&format!(r#"{{"{PHONE}":{{"time":"2026-11-02T14:39:41Z","name":"Phone","address":"192.168.1.20:22000"}}}}"#)),
            pending_folders: json(&format!(r#"{{"bios-9":{{"offeredBy":{{"{PC}":{{"time":"2026-11-02T14:40:14Z","label":"bios","receiveEncrypted":false,"remoteEncrypted":false}}}}}},"lonely":{{"offeredBy":{{}}}}}}"#)),
        }
    }

    #[test]
    fn a_snapshot_is_built_from_every_reply() {
        let snapshot = snapshot(replies(), |label, _| Destination::New(format!("/mnt/games/{label}").into())).unwrap();
        assert_eq!((snapshot.me.id.as_str(), snapshot.me.name.as_str()), (ME, "ghazal"));
        assert_eq!(snapshot.listening, Listening::Local);
        assert_eq!(snapshot.folders.len(), 1);
        assert_eq!((snapshot.folders[0].label.as_str(), &snapshot.folders[0].state), ("Saves", &FolderState::UpToDate));
        // This handheld is not one of its own devices; an unnamed device goes by its ID.
        let devices: Vec<_> = snapshot.devices.iter().map(|device| (device.name.as_str(), device.presence)).collect();
        assert_eq!(devices, [("Desktop PC", Presence::Connected), (PHONE, Presence::Away(LastSeen::At(Timestamp::parse("2026-11-01T09:00:00Z").unwrap())))]);
        assert_eq!(snapshot.pending_devices.len(), 1);
        assert_eq!(snapshot.pending_devices[0].title(), "Phone");
        // A pending folder nobody offers any more is dropped.
        assert_eq!(snapshot.pending_folders.len(), 1);
        let bios = &snapshot.pending_folders[0];
        assert_eq!((bios.id.as_str(), bios.label.as_str(), bios.offered_by.first.as_str()), ("bios-9", "bios", PC));
        assert_eq!(bios.destination, Destination::New("/mnt/games/bios".into()));
        assert_eq!(snapshot.summary(), super::super::model::Summary::UpToDate);
    }

    #[test]
    fn a_device_never_seen_and_a_paused_device_say_so() {
        let mut replies = replies();
        replies.statistics.clear();
        replies.devices[1].paused = true;
        let snapshot = snapshot(replies, |_, _| Destination::Unusable).unwrap();
        assert_eq!(snapshot.devices[0].presence, Presence::Paused);
        assert_eq!(snapshot.devices[1].presence, Presence::Away(LastSeen::Never));
    }

    #[test]
    fn a_malformed_device_id_is_an_error_not_a_skip() {
        let mut replies = replies();
        replies.status.my_id = "nonsense".into();
        assert!(snapshot(replies, |_, _| Destination::Unusable).unwrap_err().contains("nonsense"));
    }

    #[test]
    fn the_whole_config_round_trips_what_it_does_not_touch() {
        let text = r#"{"version":52,"folders":[{"id":"x"}],"gui":{"password":"$2a$10$hash"},"remoteIgnoredDevices":[{"deviceID":"A","time":"t","name":"","address":""}]}"#;
        let mut config: WholeConfig = serde_json::from_str(text).unwrap();
        config.remote_ignored_devices.push(serde_json::to_value(IgnoredDevice { time: "now", device_id: PHONE, name: "Phone", address: "x" }).unwrap());
        let written: serde_json::Value = serde_json::to_value(&config).unwrap();
        assert_eq!(written["gui"]["password"], "$2a$10$hash");
        assert_eq!(written["folders"][0]["id"], "x");
        assert_eq!(written["remoteIgnoredDevices"][1]["deviceID"], PHONE);
    }
}
