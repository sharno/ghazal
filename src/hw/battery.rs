use std::fs;
use std::path::Path;

use crate::device;

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Status {
    pub percent: u8,
    /// On external power, including after the charger stops at full.
    pub charging: bool,
}

/// Reads the configured battery, or else the first power supply of type "Battery".
pub fn read(cfg: Option<&device::Battery>) -> Option<Status> {
    let read = |p: &Path| fs::read_to_string(p).map(|s| s.trim().to_string()).ok();
    let supplies = || fs::read_dir("/sys/class/power_supply").into_iter().flatten().flatten().map(|e| e.path());
    let is_battery = |p: &Path| read(&p.join("type")).as_deref() == Some("Battery");
    let dir = match cfg {
        Some(b) => b.path.clone(),
        None => supplies().find(|p| is_battery(p))?,
    };
    let percent = read(&dir.join("capacity"))?.parse::<u8>().ok()?.min(100);
    let status = read(&dir.join("status")).unwrap_or_default();
    // A full AXP717 reports "Not charging" while plugged in, so status alone
    // misses the charger; ask the USB/mains supply.
    let online = match cfg.and_then(|b| b.charger_online.as_deref()) {
        Some(path) => read(path).as_deref() == Some("1"),
        None => supplies().any(|p| !is_battery(&p) && read(&p.join("online")).as_deref() == Some("1")),
    };
    Some(Status { percent, charging: online || status == "Charging" || status == "Full" })
}
