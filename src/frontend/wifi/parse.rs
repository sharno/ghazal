//! iwctl prints tables for people, with ANSI colours. These functions turn that
//! text into `types` values. The layouts were captured from iwd 3.12 on the
//! handheld (see the fixtures in the tests).
//!
//! Table cells are padded, so a name is found by looking for the security word
//! that follows it, not by column offsets: iwd pads by bytes and an SSID may
//! hold multi-byte characters or runs of spaces.

use std::net::Ipv4Addr;

use super::types::{Band, Connection, Failure, Link, Network, Security, Signal, Snapshot, Ssid, Visibility};

/// What `station <name> show` reports.
#[derive(Debug, PartialEq, Eq)]
pub struct StationInfo {
    pub scanning: bool,
    pub link: Link,
}

/// A row of `station <name> get-networks`.
#[derive(Debug, PartialEq, Eq)]
pub struct Listed {
    pub ssid: Ssid,
    pub security: Security,
    pub signal: Signal,
}

/// A row of `known-networks list`.
#[derive(Debug, PartialEq, Eq)]
pub struct Saved {
    pub ssid: Ssid,
    pub security: Security,
    pub visibility: Visibility,
}

pub fn strip_ansi(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            plain.push(c);
        } else if chars.next() == Some('[') {
            // A CSI sequence ends at its first byte in 0x40..=0x7e.
            for end in chars.by_ref() {
                if ('@'..='~').contains(&end) { break; }
            }
        }
    }
    plain
}

/// A table's header text and the rows below it.
struct Table {
    header: String,
    rows: Vec<String>,
}

fn is_rule(line: &str) -> bool {
    line.len() >= 20 && line.chars().all(|c| c == '-')
}

/// iwctl prints a title, a rule, the column names, a rule, then the rows. With
/// nothing to list it may print just a sentence such as "No networks available".
fn table(output: &str) -> Result<Table, Failure> {
    let plain = strip_ansi(output);
    let lines: Vec<&str> = plain.lines().collect();
    let rules: Vec<usize> = lines.iter().enumerate().filter(|(_, line)| is_rule(line)).map(|(index, _)| index).collect();
    match rules.as_slice() {
        [_, second, ..] => Ok(Table {
            header: lines.get(second - 1).copied().unwrap_or_default().to_owned(),
            rows: lines[second + 1..].iter().filter(|line| !line.trim().is_empty()).map(|line| (*line).to_owned()).collect(),
        }),
        _ => match lines.iter().map(|line| line.trim()).find(|line| !line.is_empty()) {
            Some(sentence) if sentence.starts_with("No ") => Ok(Table { header: String::new(), rows: Vec::new() }),
            Some(sentence) => Err(Failure::Other(sentence.chars().take(60).collect())),
            // Nothing printed is an empty list; the callers that need rows
            // (the station's properties) then report what is missing.
            None => Ok(Table { header: String::new(), rows: Vec::new() }),
        },
    }
}

/// Where a table's first column starts: the position of its title in the header.
fn first_column(header: &str, title: &str) -> usize {
    header.find(title).unwrap_or(2)
}

/// Splits a row into its first cell and the cells after it. The first cell
/// ends where two or more spaces are followed by a security word.
fn split_name(row: &str, column: usize) -> Option<(Ssid, Security, Vec<&str>)> {
    let skip = row.char_indices().nth(column).map_or(row.len(), |(index, _)| index);
    let body = &row[skip..];
    body.match_indices("  ").find_map(|(end, _)| {
        let mut rest = body[end..].split_whitespace();
        let security = Security::parse(rest.next()?)?;
        let ssid = Ssid::new(body[..end].trim()).ok()?;
        Some((ssid, security, rest.collect()))
    })
}

/// The first station's name, from `station list`.
pub fn station_name(output: &str) -> Result<String, Failure> {
    let table = table(output)?;
    let column = first_column(&table.header, "Name");
    table.rows.iter().find_map(|row| row.chars().skip(column).collect::<String>().split_whitespace().next().map(str::to_owned)).ok_or(Failure::NoAdapter)
}

pub fn listed_networks(output: &str) -> Result<Vec<Listed>, Failure> {
    let table = table(output)?;
    let column = first_column(&table.header, "Network name");
    Ok(table.rows.iter().filter_map(|row| {
        let (ssid, security, rest) = split_name(row, column)?;
        Some(Listed { ssid, security, signal: signal(rest.first()?)? })
    }).collect())
}

/// iwctl prints `-4400` for -44 dBm with `rssi-dbms`, or asterisks without it.
fn signal(cell: &str) -> Option<Signal> {
    match cell.parse::<i32>() {
        Ok(centi_dbm) => Some(Signal::from_centi_dbm(centi_dbm)),
        Err(_) if !cell.is_empty() && cell.chars().all(|c| c == '*') => Some(Signal::from_dbm(match cell.len() { 4.. => -55, 3 => -63, 2 => -71, _ => -80 })),
        Err(_) => None,
    }
}

pub fn saved_networks(output: &str) -> Result<Vec<Saved>, Failure> {
    let table = table(output)?;
    let column = first_column(&table.header, "Name");
    // After the security word comes the Hidden cell, which says `yes` or is
    // empty; the next word is then the month of the last connection.
    Ok(table.rows.iter().filter_map(|row| split_name(row, column).map(|(ssid, security, rest)| Saved { ssid, security, visibility: if rest.first() == Some(&"yes") { Visibility::Hidden } else { Visibility::Listed } })).collect())
}

/// `station <name> show` is a two-column table of properties.
pub fn station_info(output: &str) -> Result<StationInfo, Failure> {
    let table = table(output)?;
    let property = |wanted: &str| table.rows.iter().find_map(|row| {
        let row = row.trim_start();
        // Properties that can be changed carry a `*` before their name.
        let row = row.strip_prefix('*').map_or(row, str::trim_start);
        let (name, value) = row.split_once("  ").unwrap_or((row, ""));
        (name == wanted).then(|| value.trim().to_owned())
    });
    let scanning = property("Scanning").is_some_and(|value| value == "yes");
    let state = property("State").ok_or_else(|| Failure::Other("Unexpected Wi-Fi status".into()))?;
    let link = match state.as_str() {
        "connected" | "roaming" => match property("Connected network").and_then(|name| Ssid::new(&name).ok()) {
            Some(ssid) => Link::Connected(Connection {
                ssid,
                address: property("IPv4 address").and_then(|text| text.parse::<Ipv4Addr>().ok()),
                signal: property("RSSI").and_then(|text| text.split_whitespace().next()?.parse().ok()).map(Signal::from_dbm),
                band: property("Frequency").and_then(|text| text.parse().ok()).map(Band::from_mhz),
                security: property("Security").map_or(Security::Other, |text| Security::from_description(&text)),
            }),
            None => Link::Connecting,
        },
        "connecting" => Link::Connecting,
        "disconnecting" => Link::Disconnecting,
        _ => Link::Disconnected,
    };
    Ok(StationInfo { scanning, link })
}

/// Joins the three views of the radio into what the screen lists: the connected
/// network first, saved networks in range, other networks by signal, then saved
/// networks that are out of range.
pub fn assemble(info: StationInfo, listed: Vec<Listed>, saved: Vec<Saved>) -> Snapshot {
    let saved_as = |ssid: &Ssid| saved.iter().find(|known| &known.ssid == ssid);
    let connected = match &info.link { Link::Connected(connection) => Some(connection.clone()), _ => None };

    let mut visible: Vec<Network> = listed.into_iter().map(|network| Network {
        known: saved_as(&network.ssid).is_some(),
        visibility: saved_as(&network.ssid).map_or(Visibility::Listed, |known| known.visibility),
        ssid: network.ssid,
        security: network.security,
        signal: Some(network.signal),
    }).collect();
    // A stale scan list may lack the network we are on.
    if let Some(connection) = &connected {
        if !visible.iter().any(|network| network.ssid == connection.ssid) {
            visible.push(Network { ssid: connection.ssid.clone(), security: connection.security, signal: connection.signal, known: true, visibility: saved_as(&connection.ssid).map_or(Visibility::Listed, |known| known.visibility) });
        }
    }
    // Strongest first; the sort is stable, so equal signals keep iwd's order.
    visible.sort_by_key(|network| {
        let rank = match (&connected, network.known) {
            (Some(connection), _) if connection.ssid == network.ssid => 0,
            (_, true) => 1,
            (_, false) => 2,
        };
        (rank, std::cmp::Reverse(network.signal))
    });
    let mut offline: Vec<Network> = saved.into_iter()
        .filter(|known| !visible.iter().any(|network| network.ssid == known.ssid))
        .map(|known| Network { ssid: known.ssid, security: known.security, signal: None, known: true, visibility: known.visibility })
        .collect();
    offline.sort_by_key(|network| network.ssid.as_str().to_lowercase());
    visible.extend(offline);
    Snapshot { link: info.link, scanning: info.scanning, networks: visible }
}

#[cfg(test)]
pub(super) mod fixtures {
    //! Output captured from iwd 3.12 on an RG40XX-V, colours included.
    pub const STATION_LIST: &str = "                            Devices in Station Mode\x1b[1;90m                            \x1b[0m\n\x1b[90m--------------------------------------------------------------------------------\n\x1b[0m\x1b[1;90m  Name                  State            Scanning\n\x1b[0m\x1b[90m--------------------------------------------------------------------------------\n\x1b[0m  wlan0                 connected        \n\n";

    pub const STATION_SHOW: &str = "                                 Station: wlan0\x1b[1;90m                                \x1b[0m\n\x1b[90m--------------------------------------------------------------------------------\n\x1b[0m\x1b[1;90m  Settable  Property              Value                                          \n\x1b[0m\x1b[90m--------------------------------------------------------------------------------\n\x1b[0m            Scanning              no                                               \n            State                 connected                                        \n            Connected network     Tomato24                                         \n            IPv4 address          192.168.1.133                                    \n            IPv6 address          2600:4040:573d:2b00:6a8f:c9ff:fed3:b8f0          \n            ConnectedBss          80:69:1a:21:ab:c3                                \n            Frequency             5180                                             \n            Channel               36                                               \n            Security              WPA2-Personal                                    \n            RSSI                  -44 dBm                                          \n            AverageRSSI           -42 dBm                                          \n\n";

    pub const STATION_SHOW_IDLE: &str = "                                 Station: wlan0\x1b[1;90m                                \x1b[0m\n\x1b[90m--------------------------------------------------------------------------------\n\x1b[0m\x1b[1;90m  Settable  Property              Value                                          \n\x1b[0m\x1b[90m--------------------------------------------------------------------------------\n\x1b[0m            Scanning              yes                                              \n            State                 disconnected                                     \n\n";

    pub const GET_NETWORKS: &str = "                               Available networks\x1b[1;90m                              \x1b[0m\n\x1b[90m--------------------------------------------------------------------------------\n\x1b[0m\x1b[1;90m      Network name                      Security            Signal\n\x1b[0m\x1b[90m--------------------------------------------------------------------------------\n\x1b[0m  \x1b[1;90m> \x1b[0m  Tomato24                          psk                 -4400   \n      Café  Wi-Fi                       psk                 -6800   \n      Guest                             open                -7900   \n      Campus                            8021x               -5000   \n\n";

    pub const KNOWN_NETWORKS: &str = "                                 Known Networks\x1b[1;90m                                \x1b[0m\n\x1b[90m--------------------------------------------------------------------------------\n\x1b[0m\x1b[1;90m  Name                              Security     Hidden     Last connected     \n\x1b[0m\x1b[90m--------------------------------------------------------------------------------\n\x1b[0m  Tomato24                          psk                     Oct 31,  4:10 AM\n  Grandma's house                   psk          yes        Sep  2,  9:00 PM\n\n";
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    fn ssid(name: &str) -> Ssid { Ssid::new(name).unwrap() }

    #[test]
    fn colours_are_stripped() {
        assert_eq!(strip_ansi("\x1b[1;90m  Name\x1b[0m\n\x1b[90m--"), "  Name\n--");
        assert_eq!(strip_ansi("plain"), "plain");
        assert_eq!(strip_ansi("cut\x1b"), "cut");
    }

    #[test]
    fn the_station_is_named_by_its_first_row() {
        assert_eq!(station_name(STATION_LIST), Ok("wlan0".into()));
        assert_eq!(station_name("No devices in Station mode available.\n"), Err(Failure::NoAdapter));
        assert_eq!(station_name("Failed to retrieve IWD dbus objects, quitting...\n"), Err(Failure::Other("Failed to retrieve IWD dbus objects, quitting...".into())));
    }

    #[test]
    fn a_connected_station_reports_its_connection() {
        let info = station_info(STATION_SHOW).unwrap();
        assert!(!info.scanning);
        let Link::Connected(connection) = info.link else { panic!("expected a connection") };
        assert_eq!(connection.ssid, ssid("Tomato24"));
        assert_eq!(connection.address, Some(Ipv4Addr::new(192, 168, 1, 133)));
        assert_eq!(connection.signal, Some(Signal::from_dbm(-44)));
        assert_eq!(connection.band, Some(Band::Ghz5));
        assert_eq!(connection.security, Security::Psk);
    }

    #[test]
    fn a_disconnected_station_may_be_scanning() {
        assert_eq!(station_info(STATION_SHOW_IDLE), Ok(StationInfo { scanning: true, link: Link::Disconnected }));
    }

    #[test]
    fn show_without_a_state_is_an_error() {
        assert!(matches!(station_info("Device 'wlan0' is not in station mode.\n"), Err(Failure::Other(_))));
    }

    #[test]
    fn networks_keep_spaces_and_non_ascii_names() {
        let listed = listed_networks(GET_NETWORKS).unwrap();
        assert_eq!(listed.iter().map(|n| n.ssid.as_str()).collect::<Vec<_>>(), ["Tomato24", "Café  Wi-Fi", "Guest", "Campus"]);
        assert_eq!(listed.iter().map(|n| n.security).collect::<Vec<_>>(), [Security::Psk, Security::Psk, Security::Open, Security::Enterprise]);
        assert_eq!(listed[0].signal, Signal::from_dbm(-44));
        assert_eq!(listed[2].signal.bars(), 1);
    }

    #[test]
    fn a_network_named_like_a_security_word_still_parses() {
        let text = "  Available networks\n--------------------------------------------------------------------------------\n      Network name                      Security            Signal\n--------------------------------------------------------------------------------\n      psk                               psk                 -5000   \n";
        let listed = listed_networks(text).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].ssid, ssid("psk"));
    }

    #[test]
    fn asterisk_signals_are_accepted_too() {
        assert_eq!(signal("****").map(Signal::bars), Some(4));
        assert_eq!(signal("*").map(Signal::bars), Some(1));
        assert_eq!(signal("junk"), None);
    }

    #[test]
    fn known_networks_list_names_and_security() {
        let saved = saved_networks(KNOWN_NETWORKS).unwrap();
        assert_eq!(saved, [
            Saved { ssid: ssid("Tomato24"), security: Security::Psk, visibility: Visibility::Listed },
            Saved { ssid: ssid("Grandma's house"), security: Security::Psk, visibility: Visibility::Hidden },
        ]);
    }

    #[test]
    fn empty_lists_are_not_errors() {
        assert_eq!(listed_networks("No networks available\n").unwrap(), []);
        assert_eq!(saved_networks("No known networks available.\n").unwrap(), []);
        assert!(saved_networks("something else went wrong\n").is_err());
        // Silence is an empty list for lists, and a missing state for a status.
        assert_eq!(saved_networks("").unwrap(), []);
        assert_eq!(listed_networks("\n").unwrap(), []);
        assert!(matches!(station_info(""), Err(Failure::Other(_))));
        assert_eq!(station_name(""), Err(Failure::NoAdapter));
    }

    #[test]
    fn a_snapshot_orders_connected_then_saved_then_signal_then_offline() {
        let info = station_info(STATION_SHOW).unwrap();
        let mut listed = listed_networks(GET_NETWORKS).unwrap();
        // iwd lists by signal; make the open network the strongest after ours.
        listed[2].signal = Signal::from_dbm(-50);
        let snapshot = assemble(info, listed, saved_networks(KNOWN_NETWORKS).unwrap());
        let names: Vec<_> = snapshot.networks.iter().map(|n| n.ssid.as_str()).collect();
        assert_eq!(names, ["Tomato24", "Guest", "Campus", "Café  Wi-Fi", "Grandma's house"]);
        assert!(snapshot.network(&ssid("Tomato24")).unwrap().known);
        assert_eq!(snapshot.network(&ssid("Grandma's house")).unwrap().signal, None);
        assert_eq!(snapshot.network(&ssid("Grandma's house")).unwrap().visibility, Visibility::Hidden);
        assert_eq!(snapshot.network(&ssid("Tomato24")).unwrap().visibility, Visibility::Listed);
        assert!(snapshot.is_connected(&ssid("Tomato24")));
    }

    #[test]
    fn a_stale_scan_list_still_shows_the_connected_network() {
        let snapshot = assemble(station_info(STATION_SHOW).unwrap(), Vec::new(), Vec::new());
        assert_eq!(snapshot.networks.len(), 1);
        assert_eq!(snapshot.networks[0].ssid, ssid("Tomato24"));
        assert!(snapshot.networks[0].known);
    }
}
