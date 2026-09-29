use std::fs;

pub struct Battery {
    pub percent: u8,
    pub charging: bool,
}

/// First power supply of type "Battery" (axp2202-battery on H700 devices).
pub fn battery() -> Option<Battery> {
    for entry in fs::read_dir("/sys/class/power_supply").ok()?.flatten() {
        let dir = entry.path();
        let read = |name| fs::read_to_string(dir.join(name)).map(|s| s.trim().to_string());
        if read("type").ok().as_deref() != Some("Battery") {
            continue;
        }
        let percent = read("capacity").ok()?.parse().ok()?;
        let charging = read("status").is_ok_and(|s| s == "Charging" || s == "Full");
        return Some(Battery { percent, charging });
    }
    None
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
