//! The status bar's icons for what runs in the background: Wi-Fi, Bluetooth,
//! Syncthing and HDMI. Each is read from sysfs or /proc without starting a
//! process, so the menu can look every few seconds.

use std::path::Path;

use super::wifi::Signal;
use crate::hdmi::{self, Connection, Layout};
use crate::ui::{IndicatorIcons, Indicators, Lamp};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wifi {
    /// No wireless interface.
    Absent,
    Disconnected,
    /// `None` when the driver reports no signal level.
    Connected(Option<Signal>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bluetooth {
    /// No controller, or the player has not turned it on.
    Off,
    On,
    /// At least one device has a link.
    Connected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Syncthing {
    Stopped,
    Running,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Readings {
    pub wifi: Wifi,
    pub bluetooth: Bluetooth,
    pub syncthing: Syncthing,
    /// From the frontend's own connector polling; `None` before the first poll.
    pub hdmi: Option<hdmi::Status>,
}

impl Readings {
    pub fn read(hdmi: Option<hdmi::Status>) -> Self {
        Self { wifi: read_wifi(), bluetooth: read_bluetooth(), syncthing: read_syncthing(), hdmi }
    }

    pub fn view(&self) -> Indicators {
        let (wifi, wifi_arcs) = match self.wifi {
            Wifi::Absent => (Lamp::Off, 0),
            Wifi::Disconnected => (Lamp::Idle, 0),
            // One bar is the dot alone; no reading shows a full fan rather than a weak one.
            Wifi::Connected(signal) => (Lamp::On, signal.map_or(3, |signal| i32::from(signal.bars()) - 1)),
        };
        let bluetooth = match self.bluetooth {
            Bluetooth::Off => Lamp::Off,
            Bluetooth::On => Lamp::Idle,
            Bluetooth::Connected => Lamp::On,
        };
        let syncthing = match self.syncthing {
            Syncthing::Stopped => Lamp::Off,
            Syncthing::Running => Lamp::Idle,
        };
        let hdmi = match self.hdmi.map(|status| (status.connection, status.layout)) {
            None | Some((Connection::Absent | Connection::Disconnected, _)) => Lamp::Off,
            Some((Connection::Connected, Layout::Panel)) => Lamp::Idle,
            Some((Connection::Connected, Layout::Hdmi | Layout::Mirror)) => Lamp::On,
        };
        Indicators { wifi, wifi_arcs, bluetooth, syncthing, hdmi }
    }
}

fn read_wifi() -> Wifi {
    let Some(interface) = std::fs::read_dir("/sys/class/net").ok().and_then(|interfaces| {
        interfaces.flatten().find(|interface| interface.path().join("phy80211").exists()).map(|interface| interface.path())
    }) else {
        return Wifi::Absent;
    };
    // iwd raises the link once it has associated and finished the handshake.
    match std::fs::read_to_string(interface.join("operstate")).as_deref().map(str::trim) {
        Ok("up") => {
            let name = interface.file_name().unwrap_or_default().to_string_lossy();
            Wifi::Connected(std::fs::read_to_string("/proc/net/wireless").ok().and_then(|table| signal(&table, &name)))
        }
        _ => Wifi::Disconnected,
    }
}

/// The signal level of `interface` in /proc/net/wireless:
/// `wlan0: 0000   54.  -56.  -256 ...` (status, link quality, level in dBm, noise).
fn signal(table: &str, interface: &str) -> Option<Signal> {
    let (_, fields) = table.lines().filter_map(|line| line.trim_start().split_once(':')).find(|(name, _)| *name == interface)?;
    let dbm: f32 = fields.split_whitespace().nth(2)?.trim_end_matches('.').parse().ok()?;
    // Drivers without a reading report 0.
    (dbm < 0.0).then(|| Signal::from_dbm(dbm.round() as i32))
}

fn read_bluetooth() -> Bluetooth {
    use crate::bluetooth::hardware::{self, Controller};
    match hardware::controller() {
        Controller::Absent | Controller::Dormant(_) => Bluetooth::Off,
        Controller::Registered if !Path::new(hardware::MARKER).exists() => Bluetooth::Off,
        Controller::Registered if hardware::linked() => Bluetooth::Connected,
        Controller::Registered => Bluetooth::On,
    }
}

fn read_syncthing() -> Syncthing {
    if crate::sys::running("syncthing") { Syncthing::Running } else { Syncthing::Stopped }
}

/// The icons build.rs rasterized, for `AppWindow::set_indicator_icons`.
pub fn icons() -> IndicatorIcons {
    /// Matches ICON_SIZE in build.rs.
    const SIZE: u32 = 40;
    fn image(pixels: &[u8]) -> slint::Image {
        let mut buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(SIZE, SIZE);
        buffer.make_mut_bytes().copy_from_slice(pixels);
        slint::Image::from_rgba8_premultiplied(buffer)
    }
    macro_rules! icon {
        ($name:literal) => { image(include_bytes!(concat!(env!("OUT_DIR"), "/", $name, ".rgba"))) };
    }
    IndicatorIcons {
        wifi_dot: icon!("wifi-dot"),
        wifi_near: icon!("wifi-near"),
        wifi_mid: icon!("wifi-mid"),
        wifi_far: icon!("wifi-far"),
        bluetooth: icon!("bluetooth"),
        syncthing: icon!("syncthing"),
        hdmi: icon!("hdmi"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIRELESS: &str = "Inter-| sta-|   Quality        |   Discarded packets               | Missed | WE\n \
                            face | tus | link level noise |  nwid  crypt   frag  retry   misc | beacon | 22\n \
                            wlan0: 0000   54.  -56.  -256        0      0      0      0      0        0\n";

    #[test]
    fn the_signal_level_comes_from_the_interfaces_line() {
        assert_eq!(signal(WIRELESS, "wlan0"), Some(Signal::from_dbm(-56)));
        assert_eq!(signal(WIRELESS, "wlan1"), None);
        assert_eq!(signal(&WIRELESS.replace("-56.", "0."), "wlan0"), None);
    }

    fn view(wifi: Wifi, bluetooth: Bluetooth, syncthing: Syncthing, hdmi: Option<(Connection, Layout)>) -> Indicators {
        Readings { wifi, bluetooth, syncthing, hdmi: hdmi.map(|(connection, layout)| hdmi::Status { connection, layout, mode: None }) }.view()
    }

    #[test]
    fn idle_services_are_dim_and_absent_ones_hidden() {
        let quiet = view(Wifi::Absent, Bluetooth::Off, Syncthing::Stopped, Some((Connection::Disconnected, Layout::Panel)));
        assert_eq!((quiet.wifi, quiet.bluetooth, quiet.syncthing, quiet.hdmi), (Lamp::Off, Lamp::Off, Lamp::Off, Lamp::Off));
        let idle = view(Wifi::Disconnected, Bluetooth::On, Syncthing::Running, Some((Connection::Connected, Layout::Panel)));
        assert_eq!((idle.wifi, idle.wifi_arcs, idle.bluetooth, idle.syncthing, idle.hdmi), (Lamp::Idle, 0, Lamp::Idle, Lamp::Idle, Lamp::Idle));
    }

    #[test]
    fn signal_bars_light_the_arcs_above_the_dot() {
        let arcs = |signal| view(Wifi::Connected(signal), Bluetooth::Connected, Syncthing::Running, Some((Connection::Connected, Layout::Hdmi))).wifi_arcs;
        assert_eq!([-50, -65, -70, -80].map(|dbm| arcs(Some(Signal::from_dbm(dbm)))), [3, 2, 1, 0]);
        assert_eq!(arcs(None), 3);
        let busy = view(Wifi::Connected(None), Bluetooth::Connected, Syncthing::Running, Some((Connection::Connected, Layout::Mirror)));
        assert_eq!((busy.wifi, busy.bluetooth, busy.hdmi), (Lamp::On, Lamp::On, Lamp::On));
    }
}
