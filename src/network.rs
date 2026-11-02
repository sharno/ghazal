//! Is the handheld on a network? Wi-Fi starts in the supervisor's background
//! phase and takes seconds to connect, so a game that needs the network (it
//! signs in to RetroAchievements, or joins a netplay host) waits a little for it.

use std::net::{IpAddr, Ipv4Addr, UdpSocket};
use std::path::Path;
use std::time::{Duration, Instant};

/// iwd keeps one file per known network here.
pub const IWD_STATE: &str = "/var/lib/iwd";

const POLL: Duration = Duration::from_millis(100);

/// A Wi-Fi network is saved, so waiting for a connection can pay off. Without
/// one (an offline handheld) nothing would ever come up.
pub fn configured(iwd_state: &Path) -> bool {
    std::fs::read_dir(iwd_state).is_ok_and(|entries| {
        entries.flatten().any(|entry| matches!(entry.path().extension().and_then(|ext| ext.to_str()), Some("psk" | "open" | "8021x")))
    })
}

/// The address other devices on the network reach this handheld at. Connecting
/// a UDP socket picks a route without sending anything, and fails while the
/// interface has no address.
pub fn lan_address() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(address) if !address.is_loopback() => Some(address),
        IpAddr::V4(_) | IpAddr::V6(_) => None,
    }
}

/// The kernel has a route out.
pub fn online() -> bool {
    lan_address().is_some()
}

/// Waits up to `limit` for a route. Returns whether there is one.
pub fn wait_online(limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        if online() { return true; }
        if Instant::now() >= deadline { return false; }
        std::thread::sleep(POLL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_network_is_any_iwd_profile() {
        let dir = std::env::temp_dir().join(format!("ghazal-iwd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!configured(&dir));
        std::fs::write(dir.join("lease.txt"), b"").unwrap();
        assert!(!configured(&dir));
        std::fs::write(dir.join("=4d79.psk"), b"").unwrap();
        assert!(configured(&dir));
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(!configured(&dir), "a missing directory is not a network");
    }

    #[test]
    fn waiting_gives_up_at_the_limit() {
        // The sandbox may or may not be online; either way this returns promptly.
        let started = Instant::now();
        let _ = wait_online(Duration::from_millis(150));
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
