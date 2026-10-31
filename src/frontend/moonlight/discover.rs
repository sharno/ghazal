//! Finds GameStream hosts on the LAN: Sunshine and GeForce Experience announce
//! `_nvstream._tcp` over mDNS. This is a one-shot browse, so no mDNS daemon
//! (Avahi) is needed: ask, collect answers for a moment, read the records.

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::time::{Duration, Instant};

const SERVICE: &str = "_nvstream._tcp.local";
const GROUP: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(224, 0, 0, 251), 5353);
/// Not zero: a query from an ephemeral port is a legacy one, answered by
/// unicast with this ID echoed.
const QUERY_ID: u16 = 0x4c47;
const RESEND: Duration = Duration::from_millis(700);

const TYPE_A: u16 = 1;
const TYPE_PTR: u16 = 12;
const TYPE_SRV: u16 = 33;
/// "Unicast response requested" in a question's class.
const CLASS_IN_UNICAST: u16 = 0x8001;

#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub name: String,
    pub address: Ipv4Addr,
    pub port: u16,
}

#[derive(Debug, Clone, PartialEq)]
enum Record {
    Ptr { owner: String, target: String },
    Srv { owner: String, port: u16, target: String },
    A { owner: String, address: Ipv4Addr },
}

/// A question for `name`, as a whole mDNS packet.
fn query(name: &str, kind: u16) -> Vec<u8> {
    let mut packet = Vec::new();
    packet.extend(QUERY_ID.to_be_bytes());
    packet.extend([0, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
    for label in name.split('.') {
        packet.push(label.len() as u8);
        packet.extend(label.as_bytes());
    }
    packet.push(0);
    packet.extend(kind.to_be_bytes());
    packet.extend(CLASS_IN_UNICAST.to_be_bytes());
    packet
}

fn be16(packet: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*packet.get(at)?, *packet.get(at + 1)?]))
}

/// A domain name at `at`, following compression pointers (bounded, since a
/// malicious packet can point at itself), and the offset just past it in place.
fn read_name(packet: &[u8], at: usize) -> Option<(String, usize)> {
    let mut labels = Vec::new();
    let (mut cursor, mut resume, mut jumps) = (at, None, 0);
    loop {
        let length = *packet.get(cursor)? as usize;
        match length {
            0 => return Some((labels.join("."), resume.unwrap_or(cursor + 1))),
            l if l & 0xc0 == 0xc0 => {
                jumps += 1;
                if jumps > 16 { return None; }
                resume.get_or_insert(cursor + 2);
                cursor = (l & 0x3f) << 8 | *packet.get(cursor + 1)? as usize;
            }
            l if l & 0xc0 != 0 => return None,
            l => {
                labels.push(String::from_utf8_lossy(packet.get(cursor + 1..cursor + 1 + l)?).into_owned());
                cursor += 1 + l;
            }
        }
    }
}

/// The PTR, SRV and A records of a response; anything else is skipped.
fn parse(packet: &[u8]) -> Option<Vec<Record>> {
    if be16(packet, 2)? & 0x8000 == 0 { return None; }
    let questions = be16(packet, 4)? as usize;
    let records = be16(packet, 6)? as usize + be16(packet, 8)? as usize + be16(packet, 10)? as usize;
    let mut at = 12;
    for _ in 0..questions {
        at = read_name(packet, at)?.1 + 4;
    }
    let mut found = Vec::new();
    for _ in 0..records {
        let (owner, next) = read_name(packet, at)?;
        let kind = be16(packet, next)?;
        let length = be16(packet, next + 8)? as usize;
        let data = next + 10;
        let rdata = packet.get(data..data + length)?;
        match (kind, rdata) {
            (TYPE_PTR, _) => found.push(Record::Ptr { owner, target: read_name(packet, data)?.0 }),
            (TYPE_SRV, _) => found.push(Record::Srv { owner, port: be16(packet, data + 4)?, target: read_name(packet, data + 6)?.0 }),
            (TYPE_A, [a, b, c, d]) => found.push(Record::A { owner, address: Ipv4Addr::new(*a, *b, *c, *d) }),
            _ => {}
        }
        at = data + length;
    }
    Some(found)
}

/// Hosts announced by the records of the answers, each paired with the address
/// it came from. A host's own address record wins; without one the answering
/// device is the host, as for a unicast answer from the host itself.
fn services(answers: &[(Ipv4Addr, Vec<Record>)]) -> Vec<Found> {
    let all = || answers.iter().flat_map(|(source, records)| records.iter().map(move |record| (*source, record)));
    let mut hosts: BTreeMap<Ipv4Addr, Found> = BTreeMap::new();
    for (source, record) in all() {
        let Record::Ptr { owner, target: instance } = record else { continue };
        if !owner.eq_ignore_ascii_case(SERVICE) { continue; }
        let Some((port, host)) = all().find_map(|(_, r)| match r {
            Record::Srv { owner, port, target } if owner.eq_ignore_ascii_case(instance) => Some((*port, target)),
            _ => None,
        }) else { continue };
        let address = all()
            .find_map(|(_, r)| match r { Record::A { owner, address } if owner.eq_ignore_ascii_case(host) => Some(*address), _ => None })
            .unwrap_or(source);
        let name = instance.strip_suffix(&format!(".{SERVICE}")).unwrap_or(instance).to_owned();
        hosts.entry(address).or_insert(Found { name, address, port });
    }
    hosts.into_values().collect()
}

/// Asks the LAN for GameStream hosts and collects answers for `window`.
pub fn browse(window: Duration) -> Result<Vec<Found>, String> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).map_err(|error| format!("opening a socket: {error}"))?;
    socket.set_multicast_ttl_v4(255).map_err(|error| error.to_string())?;
    socket.set_read_timeout(Some(Duration::from_millis(100))).map_err(|error| error.to_string())?;
    let question = query(SERVICE, TYPE_PTR);
    let deadline = Instant::now() + window;
    let mut next_query = Instant::now();
    let mut answers = Vec::new();
    let mut buffer = [0u8; 4096];
    let mut sent = false;
    while Instant::now() < deadline {
        if Instant::now() >= next_query {
            // No route (Wi-Fi down) is the usual failure: report it when
            // nothing has gone out yet, ignore a later hiccup.
            match socket.send_to(&question, GROUP) {
                Ok(_) => sent = true,
                Err(error) if !sent => return Err(format!("network unavailable: {error}")),
                Err(_) => {}
            }
            next_query = Instant::now() + RESEND;
        }
        if let Ok((size, SocketAddr::V4(from))) = socket.recv_from(&mut buffer) {
            if let Some(records) = parse(&buffer[..size]) { answers.push((*from.ip(), records)); }
        }
    }
    Ok(services(&answers))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded(name: &str) -> Vec<u8> {
        name.split('.').flat_map(|label| [&[label.len() as u8][..], label.as_bytes()].concat()).chain([0]).collect()
    }

    /// A response as a responder builds it: records appended after the header.
    struct Response(Vec<u8>);

    impl Response {
        fn new() -> Self {
            Self(vec![0x4c, 0x47, 0x84, 0, 0, 0, 0, 0, 0, 0, 0, 0])
        }

        /// Appends a record and returns the offset of its data.
        fn add(&mut self, owner: &[u8], kind: u16, data: &[u8]) -> usize {
            self.0.extend(owner);
            self.0.extend(kind.to_be_bytes());
            self.0.extend([0x80, 1, 0, 0, 0, 120]);
            self.0.extend((data.len() as u16).to_be_bytes());
            let at = self.0.len();
            self.0.extend(data);
            let count = u16::from_be_bytes([self.0[6], self.0[7]]) + 1;
            self.0[6..8].copy_from_slice(&count.to_be_bytes());
            at
        }
    }

    /// Sunshine's announcement: PTR to the instance, SRV to `pc.local`, and
    /// optionally its A record, with names compressed.
    fn announcement(with_address: bool) -> Vec<u8> {
        let mut response = Response::new();
        let service_at = response.0.len();
        let instance_at = response.add(&encoded(SERVICE), TYPE_PTR, &[&[9][..], b"Gaming PC", &[0xc0, service_at as u8]].concat());
        let host = encoded("pc.local");
        response.add(&[0xc0, instance_at as u8], TYPE_SRV, &[&[0, 0, 0, 0, 0xbb, 0x75][..], &host].concat());
        if with_address { response.add(&host, TYPE_A, &[192, 168, 1, 20]); }
        response.0
    }

    #[test]
    fn the_question_asks_for_the_service_with_a_unicast_reply() {
        let packet = query(SERVICE, TYPE_PTR);
        assert_eq!(&packet[..12], &[0x4c, 0x47, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
        assert_eq!(packet[12..], [encoded(SERVICE), vec![0, 12, 0x80, 1]].concat());
    }

    #[test]
    fn names_follow_compression_pointers_and_loops_are_refused() {
        let packet = [&[0u8; 12][..], &[3], b"abc", &[0], &[1], b"x", &[0xc0, 12]].concat();
        assert_eq!(read_name(&packet, 17), Some(("x.abc".into(), 21)));
        let looping = [&[0u8; 12][..], &[0xc0, 12]].concat();
        assert_eq!(read_name(&looping, 12), None);
        assert_eq!(read_name(&packet[..14], 12), None);
    }

    #[test]
    fn an_announcement_yields_the_host_with_the_address_it_advertises() {
        let records = parse(&announcement(true)).expect("a response");
        assert_eq!(records.len(), 3);
        let elsewhere = Ipv4Addr::new(192, 168, 1, 99);
        assert_eq!(services(&[(elsewhere, records)]), [Found { name: "Gaming PC".into(), address: Ipv4Addr::new(192, 168, 1, 20), port: 47_989 }]);
    }

    #[test]
    fn without_an_address_record_the_answering_device_is_the_host() {
        let source = Ipv4Addr::new(192, 168, 1, 20);
        let records = parse(&announcement(false)).expect("a response");
        assert_eq!(services(&[(source, records)]), [Found { name: "Gaming PC".into(), address: source, port: 47_989 }]);
    }

    #[test]
    fn repeated_answers_make_one_host() {
        let source = Ipv4Addr::new(192, 168, 1, 20);
        let answers = [(source, parse(&announcement(false)).unwrap()), (source, parse(&announcement(true)).unwrap())];
        assert_eq!(services(&answers).len(), 1);
    }

    #[test]
    fn queries_and_garbage_are_not_answers() {
        assert_eq!(parse(&query(SERVICE, TYPE_PTR)), None);
        assert_eq!(parse(&[0x4c, 0x47, 0x84, 0, 0, 0, 0, 9, 0, 0, 0, 0, 1, 2]), None);
        assert_eq!(parse(&[]), None);
        assert_eq!(services(&[]), []);
    }
}
