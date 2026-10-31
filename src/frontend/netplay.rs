//! Netplay: how Ghazal hosts and joins RetroArch sessions, and finds hosts on
//! the LAN.
//!
//! RetroArch does the netplay itself; Ghazal starts it with `--host` or
//! `--connect`. To let the player pick a host without typing an address, Ghazal
//! also speaks RetroArch's LAN discovery: a UDP broadcast on port 55435 that
//! every host answers with the game it is playing.

use std::ffi::OsString;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use super::activity::GameId;
use super::config::Launch;
use super::library::{Game, Library, System};

/// RetroArch's default netplay port: TCP for the session, UDP for discovery.
pub const DEFAULT_PORT: u16 = 55435;
/// RetroArch's discovery port does not follow `--port`.
const DISCOVERY_PORT: u16 = 55435;
const QUERY_MAGIC: u32 = 0x5241_4E51; // "RANQ"
const RESPONSE_MAGIC: u32 = 0x5241_4E53; // "RANS"

/// RetroArch's `struct ad_packet` (network/netplay/netplay_frontend.c): four
/// big-endian words, then NUL-padded text fields.
const PACKET_LEN: usize = 4 * 4 + 5 * 32 + 2 * 256;
const PORT_AT: usize = 8;
const PASSWORD_AT: usize = 12;
const NICK: Range<usize> = 16..48;
const CORE: Range<usize> = 80..112;
const CONTENT: Range<usize> = 176..432;

/// RetroArch keeps nicknames to 31 characters.
const NICK_MAX: usize = 24;
/// How often a scan wakes to check its clock.
const SCAN_TICK: Duration = Duration::from_millis(100);

/// A host that answered a LAN query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    pub address: Ipv4Addr,
    pub port: u16,
    pub nick: String,
    /// The core's name, e.g. "Genesis Plus GX".
    pub core: String,
    /// The game's file name without its extension.
    pub content: String,
    /// Joining needs a password, which Ghazal cannot ask for.
    pub password: bool,
}

/// A host the player listed by hand, for networks a broadcast cannot cross.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    pub address: String,
    pub port: u16,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Role {
    Host,
    Join { address: String, port: u16 },
}

/// Everything a netplay launch needs besides the game.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub role: Role,
    pub nick: String,
}

impl Host {
    fn role(&self) -> Role { Role::Join { address: self.address.to_string(), port: self.port } }
}

impl Saved {
    fn role(&self) -> Role { Role::Join { address: self.address.clone(), port: self.port } }
}

/// The nickname other players see: the hostname. Every Ghazal handheld is
/// called "ghazal" until renamed, so the default gets part of the machine id
/// to tell two of them apart.
pub fn nickname(hostname: &str, machine_id: &str) -> String {
    let name: String = hostname.trim().chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')).take(NICK_MAX).collect();
    let suffix: String = machine_id.trim().chars().filter(char::is_ascii_hexdigit).take(4).collect();
    match name.as_str() {
        "" | "ghazal" if suffix.len() == 4 => format!("ghazal-{suffix}"),
        "" => "ghazal".to_owned(),
        _ => name,
    }
}

/// This handheld's nickname, from the kernel's hostname (init sets it; there is
/// no `/etc/hostname`) and the machine id.
pub fn local_nickname() -> String {
    let read = |path| std::fs::read_to_string(path).unwrap_or_default();
    nickname(&read("/proc/sys/kernel/hostname"), &read("/etc/machine-id"))
}

/// RetroArch's command line for a netplay session. `append` is the settings
/// file for netplay launches; `host_port` is where a host listens.
pub fn arguments(session: &Session, host_port: u16, append: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["--appendconfig".into(), append.into()];
    let (role, port): (Vec<OsString>, u16) = match &session.role {
        Role::Host => (vec!["--host".into()], host_port),
        // A joining handheld must not write the host's save over its own: the
        // host's state arrives through netplay, SRAM included.
        Role::Join { address, port } => (vec!["--connect".into(), address.into(), "--sram-mode".into(), "noload-nosave".into()], *port),
    };
    args.extend(role);
    args.extend(["--port".into(), port.to_string().into(), "--nick".into(), (&session.nick).into()]);
    args
}

fn query() -> [u8; 4] { QUERY_MAGIC.to_be_bytes() }

fn word(packet: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(packet.get(at..at + 4)?.try_into().ok()?))
}

fn text(packet: &[u8], range: Range<usize>) -> Option<String> {
    let field = packet.get(range)?;
    let end = field.iter().position(|byte| *byte == 0).unwrap_or(field.len());
    Some(String::from_utf8_lossy(field.get(..end)?).into_owned())
}

/// One datagram of RetroArch's discovery answer; anything else is ignored.
pub fn parse_response(packet: &[u8], from: Ipv4Addr) -> Option<Host> {
    if packet.len() != PACKET_LEN || word(packet, 0)? != RESPONSE_MAGIC { return None; }
    Some(Host {
        address: from,
        port: u16::try_from(word(packet, PORT_AT)?).ok().filter(|port| *port != 0)?,
        nick: text(packet, NICK)?,
        core: text(packet, CORE)?,
        content: text(packet, CONTENT)?,
        password: word(packet, PASSWORD_AT)? & 1 != 0,
    })
}

/// Asks the LAN who is hosting and collects answers for `window`.
pub fn scan(window: Duration) -> std::io::Result<Vec<Host>> {
    scan_at(SocketAddrV4::new(Ipv4Addr::BROADCAST, DISCOVERY_PORT), window)
}

fn scan_at(target: SocketAddrV4, window: Duration) -> std::io::Result<Vec<Host>> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.set_broadcast(true)?;
    let started = Instant::now();
    socket.send_to(&query(), target)?;
    let mut resent = false;
    let mut hosts: Vec<Host> = Vec::new();
    let mut buffer = [0u8; 1024];
    while let Some(left) = window.checked_sub(started.elapsed()).filter(|left| !left.is_zero()) {
        // UDP loses datagrams, and a host may start listening just after the first query.
        if !resent && started.elapsed() >= window / 2 {
            socket.send_to(&query(), target)?;
            resent = true;
        }
        socket.set_read_timeout(Some(left.min(SCAN_TICK)))?;
        match socket.recv_from(&mut buffer) {
            Ok((length, SocketAddr::V4(from))) => {
                if let Some(host) = buffer.get(..length).and_then(|packet| parse_response(packet, *from.ip())) {
                    hosts.retain(|known| (known.address, known.port) != (host.address, host.port));
                    hosts.push(host);
                }
            }
            Ok(_) => {}
            Err(error) if matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(error) => return Err(error),
        }
    }
    hosts.sort_by(|a, b| (&a.nick, a.address).cmp(&(&b.nick, b.address)));
    Ok(hosts)
}

/// `address[:port] [name]` per line; `#` starts a comment.
pub fn parse_saved(text: &str, default_port: u16) -> Vec<Saved> {
    text.lines().filter_map(|line| {
        let line = line.split('#').next()?.trim();
        let (target, name) = line.split_once(char::is_whitespace).map_or((line, ""), |(target, name)| (target, name.trim()));
        let (address, port) = match target.rsplit_once(':') {
            Some((address, port)) => (address, port.parse().ok()?),
            None => (target, default_port),
        };
        valid_address(address).then(|| Saved { address: address.to_owned(), port, name: name.to_owned() })
    }).collect()
}

/// A host name or IPv4 address: letters, digits, dots and dashes.
fn valid_address(address: &str) -> bool {
    !address.is_empty() && address.len() <= 253 && !address.starts_with(['-', '.'])
        && address.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
}

/// What the player wants to join: any host's game, or hosts of one game.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Any,
    Game { id: GameId, stem: String },
}

impl Target {
    pub fn game(system: &System, game: &Game) -> Self {
        Self::Game { id: GameId::new(system, game), stem: game.path.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default() }
    }
}

struct Found {
    hosts: Vec<Host>,
    saved: Vec<Saved>,
}

/// Looking for hosts on a worker thread, so the menu keeps drawing.
pub struct Scanning {
    target: Target,
    results: Receiver<Result<Found, String>>,
}

pub enum Polled {
    Waiting(Scanning),
    Found(Choosing),
    Failed(String),
}

impl Scanning {
    pub fn start(target: Target, window: Duration, hosts_file: Option<PathBuf>, default_port: u16) -> Result<Self> {
        let (sender, results) = mpsc::channel();
        std::thread::Builder::new().name("ghazal-netplay-scan".into()).spawn(move || {
            let found = scan(window).map_err(describe).map(|hosts| Found {
                hosts,
                saved: hosts_file.and_then(|path| std::fs::read_to_string(path).ok()).map(|text| parse_saved(&text, default_port)).unwrap_or_default(),
            });
            let _ = sender.send(found);
        }).context("starting the netplay scan")?;
        Ok(Self { target, results })
    }

    pub fn poll(self) -> Polled {
        match self.results.try_recv() {
            Ok(Ok(found)) => Polled::Found(Choosing::new(self.target, found)),
            Ok(Err(message)) => Polled::Failed(message),
            Err(TryRecvError::Empty) => Polled::Waiting(self),
            Err(TryRecvError::Disconnected) => Polled::Failed("The netplay scan stopped unexpectedly".into()),
        }
    }
}

fn describe(error: std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::NetworkUnreachable | std::io::ErrorKind::AddrNotAvailable => "No network: connect to Wi-Fi first".into(),
        _ => format!("Looking for hosts failed: {error}"),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Lan(Host),
    Saved(Saved),
}

/// The hosts found, with the player's cursor among them.
#[derive(Debug)]
pub struct Choosing {
    target: Target,
    entries: Vec<Entry>,
    pub cursor: usize,
}

impl Choosing {
    fn new(target: Target, found: Found) -> Self {
        let lan = found.hosts.into_iter()
            .filter(|host| match &target {
                Target::Any => true,
                Target::Game { stem, .. } => host.content.eq_ignore_ascii_case(stem),
            })
            .map(Entry::Lan);
        // A saved host says nothing about its game: only a chosen game can join one.
        let saved = found.saved.into_iter().filter(|_| matches!(target, Target::Game { .. })).map(Entry::Saved);
        let entries = lan.chain(saved).collect();
        Self { target, entries, cursor: 0 }
    }

    pub fn len(&self) -> usize { self.entries.len() }

    pub fn target(&self) -> &Target { &self.target }

    /// The row's label and detail text.
    pub fn row(&self, index: usize) -> Option<(String, String)> {
        Some(match self.entries.get(index)? {
            Entry::Lan(host) => (
                if host.content.is_empty() { host.nick.clone() } else { host.content.clone() },
                if host.password { format!("{} (password)", host.nick) } else { host.nick.clone() },
            ),
            Entry::Saved(saved) => (if saved.name.is_empty() { saved.address.clone() } else { saved.name.clone() }, "saved".to_owned()),
        })
    }

    /// The game to start and how to join, for the host under the cursor.
    pub fn choose(&self, library: &Library) -> Result<(GameId, Role), String> {
        match (self.entries.get(self.cursor).ok_or("No host to join")?, &self.target) {
            (Entry::Lan(host), _) if host.password => Err(format!("{} needs a password: join it from RetroArch's Netplay menu", host.nick)),
            (Entry::Lan(host), Target::Game { id, .. }) => Ok((id.clone(), host.role())),
            (Entry::Lan(host), Target::Any) => find_game(library, host)
                .map(|(system, game)| (GameId::new(system, game), host.role()))
                .ok_or_else(|| format!("{} is not in your library", host.content)),
            (Entry::Saved(saved), Target::Game { id, .. }) => Ok((id.clone(), saved.role())),
            (Entry::Saved(_), Target::Any) => Err("Open the game first, then choose Join netplay".into()),
        }
    }
}

/// A libretro core file ("genesis_plus_gx_libretro.so") and the name a core
/// reports ("Genesis Plus GX") compare equal once reduced to letters and digits.
fn core_key(name: &str) -> String {
    let name = name.strip_suffix(".so").unwrap_or(name);
    name.strip_suffix("_libretro").unwrap_or(name).chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_lowercase()).collect()
}

/// The game a host is playing: same file name, preferring a system whose core
/// the host also uses (one ROM name can exist on several systems).
fn find_game<'a>(library: &'a Library, host: &Host) -> Option<(&'a System, &'a Game)> {
    let mut candidates = library.systems().iter()
        .flat_map(|system| system.games.iter().map(move |game| (system, game)))
        .filter(|(_, game)| game.path.file_stem().and_then(|stem| stem.to_str()).is_some_and(|stem| stem.eq_ignore_ascii_case(&host.content)));
    let uses_host_core = |system: &System| matches!(&system.launch, Launch::Core(core) if core_key(core) == core_key(&host.core));
    let preferred = candidates.clone().find(|(system, _)| uses_host_core(system));
    preferred.or_else(|| candidates.next())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::CpuLoad;
    use crate::frontend::config::ArtSources;

    /// What RetroArch's host sends for a LAN query.
    fn response(nick: &str, core: &str, content: &str, port: u32, password: u32) -> Vec<u8> {
        let mut packet = Vec::new();
        packet.extend(RESPONSE_MAGIC.to_be_bytes());
        packet.extend(0xdead_beef_u32.to_be_bytes()); // content CRC
        packet.extend(port.to_be_bytes());
        packet.extend(password.to_be_bytes());
        let mut field = |text: &str, len: usize| {
            let mut bytes = text.as_bytes().to_vec();
            bytes.resize(len, 0);
            packet.extend(bytes);
        };
        field(nick, 32);
        field("Linux aarch64", 32);
        field(core, 32);
        field("1.0", 32);
        field("1.22.2", 32);
        field(content, 256);
        field("N/A", 256);
        assert_eq!(packet.len(), PACKET_LEN);
        packet
    }

    fn host(nick: &str, core: &str, content: &str) -> Host {
        Host { address: Ipv4Addr::new(192, 168, 1, 20), port: DEFAULT_PORT, nick: nick.into(), core: core.into(), content: content.into(), password: false }
    }

    fn system(name: &str, core: &str, games: &[&str]) -> System {
        System {
            name: name.into(), art: ArtSources::default(), launch: Launch::Core(core.into()), cpu: CpuLoad::Full,
            games: games.iter().map(|path| Game { title: (*path).into(), path: format!("/ROMS/{name}/{path}").into() }).collect(),
        }
    }

    #[test]
    fn discovery_answers_decode_to_hosts() {
        let from = Ipv4Addr::new(192, 168, 1, 20);
        assert_eq!(parse_response(&response("ghazal-1a2b", "Genesis Plus GX", "Sonic The Hedgehog 2", 55435, 0), from).unwrap(), Host {
            address: from, port: 55435, nick: "ghazal-1a2b".into(), core: "Genesis Plus GX".into(), content: "Sonic The Hedgehog 2".into(), password: false,
        });
        assert!(parse_response(&response("a", "b", "c", 55435, 1), from).unwrap().password);
        // Bit 1 is the spectator password: players can still join.
        assert!(!parse_response(&response("a", "b", "c", 55435, 2), from).unwrap().password);
        assert_eq!(parse_response(&response("a", "b", "c", 0, 0), from), None);
        assert_eq!(parse_response(&response("a", "b", "c", 70000, 0), from), None);
        assert_eq!(parse_response(&response("a", "b", "c", 55435, 0)[1..], from), None);
        assert_eq!(parse_response(&query(), from), None);
        let mut other = response("a", "b", "c", 55435, 0);
        other[3] = b'Q';
        assert_eq!(parse_response(&other, from), None);
    }

    #[test]
    fn the_query_is_retroarchs_magic_in_network_order() {
        assert_eq!(query(), *b"RANQ");
    }

    /// A stand-in for a RetroArch host on loopback: the wire format both ways.
    #[test]
    fn a_scan_collects_the_hosts_that_answer() {
        let fake = UdpSocket::bind("127.0.0.1:0").unwrap();
        let SocketAddr::V4(target) = fake.local_addr().unwrap() else { unreachable!() };
        fake.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let responder = std::thread::spawn(move || {
            let mut buffer = [0u8; 16];
            let (length, from) = fake.recv_from(&mut buffer).unwrap();
            assert_eq!(&buffer[..length], b"RANQ");
            fake.send_to(&response("beta", "Gambatte", "Tetris", 55435, 0), from).unwrap();
            // Garbage and a duplicate answer must not add entries.
            fake.send_to(b"hello", from).unwrap();
            fake.send_to(&response("beta", "Gambatte", "Tetris", 55435, 0), from).unwrap();
        });
        let hosts = scan_at(target, Duration::from_millis(600)).unwrap();
        responder.join().unwrap();
        assert_eq!(hosts.len(), 1);
        assert_eq!((hosts[0].nick.as_str(), hosts[0].content.as_str(), hosts[0].address), ("beta", "Tetris", Ipv4Addr::LOCALHOST));
    }

    #[test]
    fn a_scan_with_no_hosts_ends_at_its_window() {
        let silent = UdpSocket::bind("127.0.0.1:0").unwrap();
        let SocketAddr::V4(target) = silent.local_addr().unwrap() else { unreachable!() };
        let started = Instant::now();
        assert!(scan_at(target, Duration::from_millis(300)).unwrap().is_empty());
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn the_nickname_comes_from_the_hostname() {
        assert_eq!(nickname("retro-deck\n", "0123456789abcdef"), "retro-deck");
        assert_eq!(nickname("ghazal", "9f3a77c1000000000000000000000000"), "ghazal-9f3a");
        assert_eq!(nickname("ghazal\n", ""), "ghazal");
        assert_eq!(nickname("", "abcd"), "ghazal-abcd");
        assert_eq!(nickname("my deck!", "abcd"), "mydeck");
        assert_eq!(nickname(&"x".repeat(60), "abcd").len(), NICK_MAX);
    }

    #[test]
    fn hosting_and_joining_pass_what_retroarch_needs() {
        let append = Path::new("/run/ghazal/retroarch-netplay.cfg");
        let args = |role| arguments(&Session { role, nick: "ghazal-9f3a".into() }, 55435, append).into_iter().map(|arg| arg.to_string_lossy().into_owned()).collect::<Vec<_>>();
        assert_eq!(args(Role::Host), ["--appendconfig", "/run/ghazal/retroarch-netplay.cfg", "--host", "--port", "55435", "--nick", "ghazal-9f3a"]);
        assert_eq!(
            args(Role::Join { address: "192.168.1.20".into(), port: 55436 }),
            ["--appendconfig", "/run/ghazal/retroarch-netplay.cfg", "--connect", "192.168.1.20", "--sram-mode", "noload-nosave", "--port", "55436", "--nick", "ghazal-9f3a"],
        );
    }

    #[test]
    fn saved_hosts_accept_addresses_with_optional_ports_and_names() {
        let saved = parse_saved("# friends\n192.168.1.50 Living room\n10.0.0.2:55500\nplayer.example.org:1234  Alice # remote\n-rf bad\nbad!host:1234\nhost.example:99999\n\n", 55435);
        assert_eq!(saved, [
            Saved { address: "192.168.1.50".into(), port: 55435, name: "Living room".into() },
            Saved { address: "10.0.0.2".into(), port: 55500, name: "".into() },
            Saved { address: "player.example.org".into(), port: 1234, name: "Alice".into() },
        ]);
    }

    #[test]
    fn a_game_target_lists_only_hosts_of_that_game_plus_saved_hosts() {
        let library = Library::from_systems(vec![system("Game Boy", "gambatte_libretro.so", &["Tetris.gb", "Pokemon.gb"])]);
        let (sys, game) = (&library.systems()[0], &library.systems()[0].games[0]);
        let found = || Found {
            hosts: vec![host("a", "Gambatte", "Tetris"), host("b", "Gambatte", "pokemon"), host("c", "Gambatte", "TETRIS")],
            saved: vec![Saved { address: "10.0.0.2".into(), port: 55435, name: "Remote".into() }],
        };
        let for_game = Choosing::new(Target::game(sys, game), found());
        assert_eq!(for_game.len(), 3);
        assert_eq!(for_game.row(0), Some(("Tetris".into(), "a".into())));
        assert_eq!(for_game.row(2), Some(("Remote".into(), "saved".into())));
        assert_eq!(for_game.row(3), None);
        // Without a game, saved hosts have nothing to start.
        let any = Choosing::new(Target::Any, found());
        assert_eq!(any.len(), 3);
        assert!(any.entries.iter().all(|entry| matches!(entry, Entry::Lan(_))));
    }

    #[test]
    fn choosing_a_host_finds_the_game_or_says_why_not() {
        let library = Library::from_systems(vec![
            system("Game Boy", "gambatte_libretro.so", &["Tetris.gb"]),
            system("Mega Drive", "genesis_plus_gx_libretro.so", &["Sonic.md"]),
            system("Master System", "genesis_plus_gx_libretro.so", &["Sonic.sms"]),
        ]);
        let mut choosing = Choosing::new(Target::Any, Found {
            hosts: vec![host("a", "Genesis Plus GX", "sonic"), host("b", "Gambatte", "Zelda"), Host { password: true, ..host("c", "Gambatte", "Tetris") }],
            saved: Vec::new(),
        });
        // Both Sonic games use the host's core: the first in library order wins.
        let (id, role) = choosing.choose(&library).unwrap();
        assert_eq!(id.resolve(&library).unwrap().1.path, PathBuf::from("/ROMS/Mega Drive/Sonic.md"));
        assert_eq!(role, Role::Join { address: "192.168.1.20".into(), port: DEFAULT_PORT });
        choosing.cursor = 1;
        assert_eq!(choosing.choose(&library).unwrap_err(), "Zelda is not in your library");
        choosing.cursor = 2;
        assert!(choosing.choose(&library).unwrap_err().contains("password"));
        choosing.cursor = 9;
        assert!(choosing.choose(&library).is_err());
    }

    #[test]
    fn the_hosts_core_breaks_ties_between_systems_with_the_same_game() {
        let library = Library::from_systems(vec![
            system("Game Boy", "gambatte_libretro.so", &["Tetris.gb"]),
            system("Game Boy Advance", "mgba_libretro.so", &["Tetris.gb"]),
        ]);
        let found = find_game(&library, &host("a", "mGBA", "Tetris")).unwrap();
        assert_eq!(found.0.name, "Game Boy Advance");
        assert_eq!(find_game(&library, &host("a", "Unknown core", "Tetris")).unwrap().0.name, "Game Boy");
        assert_eq!(core_key("genesis_plus_gx_libretro.so"), core_key("Genesis Plus GX"));
        assert_eq!(core_key("mame2003_plus_libretro.so"), core_key("MAME 2003-Plus"));
    }

    #[test]
    fn a_saved_host_joins_the_chosen_game() {
        let library = Library::from_systems(vec![system("Game Boy", "gambatte_libretro.so", &["Tetris.gb"])]);
        let (sys, game) = (&library.systems()[0], &library.systems()[0].games[0]);
        let choosing = Choosing::new(Target::game(sys, game), Found { hosts: Vec::new(), saved: parse_saved("10.0.0.2:55500 Alice", DEFAULT_PORT) });
        let (id, role) = choosing.choose(&library).unwrap();
        assert_eq!(id, GameId::new(sys, game));
        assert_eq!(role, Role::Join { address: "10.0.0.2".into(), port: 55500 });
    }

    #[test]
    fn unreachable_networks_get_a_plain_message() {
        assert_eq!(describe(std::io::Error::from(std::io::ErrorKind::NetworkUnreachable)), "No network: connect to Wi-Fi first");
        assert!(describe(std::io::Error::other("boom")).contains("boom"));
    }
}
