//! Opens a page of RetroArch's own menu for the player. RetroArch has no
//! command line option for a menu path, but it takes menu keys on its network
//! command port (`network_cmd_enable` in retroarch.cfg), so a short-lived
//! thread presses them once RetroArch answers.
//!
//! The key sequence is data (`[retroarch.menu_paths]` in frontend.toml): it
//! depends on RetroArch's menu layout, so a wrong one only leaves the player on
//! another page of the same menu.

use std::net::UdpSocket;
use std::time::{Duration, Instant};

use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MenuKey { Up, Down, Left, Right, A, B }

impl MenuKey {
    fn command(self) -> &'static str {
        match self {
            Self::Up => "MENU_UP", Self::Down => "MENU_DOWN", Self::Left => "MENU_LEFT",
            Self::Right => "MENU_RIGHT", Self::A => "MENU_A", Self::B => "MENU_B",
        }
    }
}

/// RetroArch's command socket takes one command per datagram.
const STATUS_QUERY: &[u8] = b"GET_STATUS";
/// Up to this long for RetroArch to start listening.
const START_TIMEOUT: Duration = Duration::from_secs(20);
const PROBE_INTERVAL: Duration = Duration::from_millis(400);
/// The menu's first frame is drawn a moment after the command port opens.
const SETTLE: Duration = Duration::from_millis(800);
/// Each key is read on a frame; keep presses apart so none is merged or lost.
const KEY_GAP: Duration = Duration::from_millis(180);

/// Presses `keys` in RetroArch's menu from a detached thread. Failures are
/// logged, never fatal: the menu is usable by hand either way.
pub fn open(keys: Vec<MenuKey>, port: u16) {
    if keys.is_empty() { return; }
    let spawned = std::thread::Builder::new().name("ghazal-ra-menu".into()).spawn(move || {
        if let Err(error) = press(&keys, port) { eprintln!("ghazal: opening RetroArch's menu page: {error}"); }
    });
    if let Err(error) = spawned { eprintln!("ghazal: starting the menu helper: {error}"); }
}

fn press(keys: &[MenuKey], port: u16) -> std::io::Result<()> {
    let socket = UdpSocket::bind("127.0.0.1:0")?;
    socket.connect(("127.0.0.1", port))?;
    socket.set_read_timeout(Some(PROBE_INTERVAL))?;
    wait_until_listening(&socket)?;
    std::thread::sleep(SETTLE);
    keys.iter().try_for_each(|key| {
        socket.send(key.command().as_bytes())?;
        std::thread::sleep(KEY_GAP);
        Ok(())
    })
}

/// Any reply to a status query means the command port is up.
fn wait_until_listening(socket: &UdpSocket) -> std::io::Result<()> {
    let deadline = Instant::now() + START_TIMEOUT;
    let mut reply = [0u8; 256];
    loop {
        // Nothing listens yet: the kernel answers with a refusal, which we treat as "not yet".
        let _ = socket.send(STATUS_QUERY);
        if socket.recv(&mut reply).is_ok() { return Ok(()); }
        if Instant::now() >= deadline {
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "RetroArch's command port did not answer"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_use_retroarchs_command_names() {
        let keys: Vec<MenuKey> = serde_json::from_str(r#"["up","down","left","right","a","b"]"#).unwrap();
        assert_eq!(keys.iter().map(|key| key.command()).collect::<Vec<_>>(), ["MENU_UP", "MENU_DOWN", "MENU_LEFT", "MENU_RIGHT", "MENU_A", "MENU_B"]);
        assert!(serde_json::from_str::<Vec<MenuKey>>(r#"["start"]"#).is_err());
    }

    /// A stand-in for RetroArch: answers GET_STATUS and records menu commands.
    #[test]
    fn keys_arrive_after_the_command_port_answers() {
        let listener = UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let fake = std::thread::spawn(move || {
            let mut received = Vec::new();
            let mut buffer = [0u8; 64];
            while received.len() < 3 {
                let (length, from) = listener.recv_from(&mut buffer).unwrap();
                let command = String::from_utf8_lossy(&buffer[..length]).into_owned();
                if command == "GET_STATUS" {
                    listener.send_to(b"GET_STATUS CONTENTLESS", from).unwrap();
                } else {
                    received.push(command);
                }
            }
            received
        });
        press(&[MenuKey::Up, MenuKey::A, MenuKey::Down], port).unwrap();
        assert_eq!(fake.join().unwrap(), ["MENU_UP", "MENU_A", "MENU_DOWN"]);
    }
}
