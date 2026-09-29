use std::fs;
use std::path::Path;

use crate::device;

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Status {
    pub percent: u8,
    pub charging: bool,
}

/// Reads the configured battery, or else the first power supply of type "Battery".
pub fn read(cfg: Option<&device::Battery>) -> Option<Status> {
    let read = |p: &Path| fs::read_to_string(p).map(|s| s.trim().to_string()).ok();
    let dir = match cfg {
        Some(b) => b.path.clone(),
        None => fs::read_dir("/sys/class/power_supply")
            .ok()?
            .flatten()
            .map(|e| e.path())
            .find(|p| read(&p.join("type")).as_deref() == Some("Battery"))?,
    };
    let percent = read(&dir.join("capacity"))?.parse::<u8>().ok()?.min(100);
    let status = read(&dir.join("status")).unwrap_or_default();
    let online = cfg.and_then(|b| b.charger_online.as_deref()).and_then(read).is_some_and(|s| s == "1");
    Some(Status { percent, charging: online || status == "Charging" || status == "Full" })
}

/// Local wall-clock time as (hour, minute).
pub fn local_time() -> (u8, u8) {
    // SAFETY: time/localtime_r only write into the stack values we pass.
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        (tm.tm_hour as u8, tm.tm_min as u8)
    }
}
