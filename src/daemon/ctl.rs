//! Control socket: one text command per connection, one reply line.
//!
//!   status                 -> ok brightness=70 volume=60 battery=85 charging=0
//!   brightness 50|+10|-10  -> ok ...
//!   volume 40|+5|-5        -> ok ...
//!   suspend | poweroff | reboot
//!
//! `oxmux ctl <command...>` is the client; the frontend uses `request()` directly.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};

use super::Daemon;
use crate::system::Action;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Status {
    pub brightness: u8,
    pub volume: u8,
    pub battery: Option<u8>,
    pub charging: bool,
    pub suspend: bool,
}

impl Status {
    fn to_line(self) -> String {
        let battery = self.battery.map_or("none".to_string(), |b| b.to_string());
        format!("ok brightness={} volume={} battery={battery} charging={} suspend={}", self.brightness, self.volume, self.charging as u8, self.suspend as u8)
    }

    #[cfg(test)]
    fn parse(line: &str) -> Result<Self> {
        let mut s = Status { brightness: 0, volume: 0, battery: None, charging: false, suspend: false };
        let body = line.strip_prefix("ok").ok_or_else(|| anyhow!("{}", line.trim()))?;
        for kv in body.split_whitespace() {
            let Some((k, v)) = kv.split_once('=') else { continue };
            match k {
                "brightness" => s.brightness = v.parse()?,
                "volume" => s.volume = v.parse()?,
                "battery" => s.battery = v.parse().ok(),
                "charging" => s.charging = v == "1",
                "suspend" => s.suspend = v == "1",
                _ => {}
            }
        }
        Ok(s)
    }
}

/// Parses "50", "+10" or "-10" relative to `current`.
fn level(arg: Option<&str>, current: u8) -> Result<u8> {
    let arg = arg.ok_or_else(|| anyhow!("missing value"))?;
    let n: i32 = arg.trim_start_matches('+').parse().with_context(|| format!("bad value {arg:?}"))?;
    let v = if arg.starts_with('+') || arg.starts_with('-') { current as i32 + n } else { n };
    Ok(v.clamp(0, 100) as u8)
}

pub(super) fn handle(d: &mut Daemon, stream: UnixStream) -> Result<()> {
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
    let mut words = line.split_whitespace();
    let reply = (|| -> Result<String> {
        match words.next() {
            Some("status") | None => {}
            Some("activity") => d.activity(),
            Some("brightness") => {
                let v = level(words.next(), d.settings.brightness)?;
                d.set_brightness(v);
            }
            Some("volume") => {
                let v = level(words.next(), d.settings.volume)?;
                d.set_volume(v);
            }
            Some("stop-emulator") => crate::emulator::stop()?,
            // Reply before acting: these may not return (poweroff) or block (suspend).
            Some(cmd @ ("suspend" | "poweroff" | "reboot")) => {
                if cmd == "suspend" && d.sys.power.suspend == crate::system::SuspendPolicy::Disabled {
                    bail!("suspend is disabled until native wake is validated");
                }
                (&stream).write_all(b"ok\n")?;
                let action = match cmd {
                    "suspend" => Action::Suspend,
                    "poweroff" => Action::Poweroff,
                    _ => Action::Reboot,
                };
                d.act(&action);
                return Ok(String::new());
            }
            Some(other) => bail!("unknown command {other:?}"),
        }
        Ok(d.status().to_line())
    })();
    let text = match reply {
        Ok(s) if s.is_empty() => return Ok(()),
        Ok(s) => s,
        Err(e) => format!("err {e:#}"),
    };
    (&stream).write_all(format!("{text}\n").as_bytes())?;
    Ok(())
}

/// Sends one command and returns the reply line.
pub fn request(socket: &Path, command: &str) -> Result<String> {
    let mut stream = UnixStream::connect(socket).with_context(|| format!("connecting to {}", socket.display()))?;
    // Emulator recovery can wait 1.5 seconds for TERM and another 1.5 for KILL.
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.write_all(format!("{command}\n").as_bytes())?;
    let mut reply = String::new();
    BufReader::new(&stream).read_line(&mut reply)?;
    let reply = reply.trim_end();
    if let Some(error) = reply.strip_prefix("err ") { bail!("{error}"); }
    if !reply.starts_with("ok") { bail!("invalid daemon reply {reply:?}"); }
    Ok(reply.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels() {
        assert_eq!(level(Some("+10"), 50).unwrap(), 60);
        assert_eq!(level(Some("-70"), 50).unwrap(), 0);
        assert_eq!(level(Some("30"), 50).unwrap(), 30);
        assert!(level(Some("x"), 50).is_err());
    }

    #[test]
    fn status_roundtrip() {
        let s = Status { brightness: 40, volume: 55, battery: Some(80), charging: true, suspend: true };
        assert_eq!(Status::parse(&s.to_line()).unwrap(), s);
        let none = Status { battery: None, ..s };
        assert_eq!(Status::parse(&none.to_line()).unwrap(), none);
        assert!(Status::parse("err nope").is_err());
    }
}
