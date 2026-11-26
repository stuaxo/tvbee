//! A minimal mDNS / DNS-SD browser: ask the local network which devices offer
//! some service types, and read back their names, ports and TXT records.
//!
//! Queries go out from an ordinary UDP port to 224.0.0.251:5353. That makes
//! them "legacy unicast" queries (RFC 6762 6.7), which responders answer
//! directly to the sender, so this needn't bind port 5353 (which avahi or
//! Bonjour usually hold) and works the same on Linux and Windows.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

const MDNS: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(224, 0, 0, 251)), 5353);
const PTR: u16 = 12;
const TXT: u16 = 16;
const SRV: u16 = 33;
const A: u16 = 1;

/// One advertised service instance, e.g. "Living Room TV" on _googlecast._tcp.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Service {
    /// The service type it answered for, e.g. "_googlecast._tcp.local".
    pub kind: String,
    /// Instance name, e.g. "Chromecast-fbaf07..._googlecast._tcp.local".
    pub instance: String,
    pub port: Option<u16>,
    pub txt: BTreeMap<String, String>,
    pub ip: Option<Ipv4Addr>,
}

/// A DNS query for PTR records of each name.
pub fn query(names: &[&str]) -> Vec<u8> {
    let mut p = vec![0, 0, 0, 0]; // id 0, flags 0 (standard query)
    p.extend_from_slice(&(names.len() as u16).to_be_bytes());
    p.extend_from_slice(&[0, 0, 0, 0, 0, 0]); // no answer/authority/additional
    for n in names {
        for label in n.trim_end_matches('.').split('.') {
            p.push(label.len() as u8);
            p.extend_from_slice(label.as_bytes());
        }
        p.push(0);
        p.extend_from_slice(&PTR.to_be_bytes());
        p.extend_from_slice(&1u16.to_be_bytes()); // class IN
    }
    p
}

/// Read a (possibly compressed) name at `at`; returns it and the offset just
/// past it in the original position.
fn read_name(p: &[u8], mut at: usize) -> Option<(String, usize)> {
    let mut labels = Vec::new();
    let mut end = None;
    for _ in 0..64 {
        let len = *p.get(at)? as usize;
        if len == 0 {
            return Some((labels.join("."), end.unwrap_or(at + 1)));
        }
        if len & 0xc0 == 0xc0 {
            // pointer to an earlier name
            let ptr = ((len & 0x3f) << 8) | *p.get(at + 1)? as usize;
            end.get_or_insert(at + 2);
            at = ptr;
            continue;
        }
        labels.push(String::from_utf8_lossy(p.get(at + 1..at + 1 + len)?).into_owned());
        at += 1 + len;
    }
    None
}

fn u16_at(p: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*p.get(at)?, *p.get(at + 1)?]))
}

/// A resource record.
#[derive(Debug, Clone, PartialEq)]
pub enum Record {
    Ptr { name: String, target: String },
    Srv { name: String, port: u16, host: String },
    Txt { name: String, entries: Vec<(String, String)> },
    A { name: String, ip: Ipv4Addr },
}

/// The records in one response packet (unknown types skipped).
pub fn parse(p: &[u8]) -> Vec<Record> {
    let mut out = Vec::new();
    let (Some(qd), Some(an), Some(ns), Some(ar)) = (u16_at(p, 4), u16_at(p, 6), u16_at(p, 8), u16_at(p, 10)) else {
        return out;
    };
    let mut at = 12;
    for _ in 0..qd {
        let Some((_, next)) = read_name(p, at) else { return out };
        at = next + 4;
    }
    for _ in 0..(an as usize + ns as usize + ar as usize) {
        let Some((name, next)) = read_name(p, at) else { return out };
        let (Some(kind), Some(len)) = (u16_at(p, next), u16_at(p, next + 8)) else { return out };
        let data = next + 10;
        let len = len as usize;
        if p.len() < data + len {
            return out;
        }
        let rec = match kind {
            PTR => read_name(p, data).map(|(target, _)| Record::Ptr { name: name.clone(), target }),
            SRV => u16_at(p, data + 4).and_then(|port| {
                read_name(p, data + 6).map(|(host, _)| Record::Srv { name: name.clone(), port, host })
            }),
            TXT => {
                let (mut entries, mut i) = (Vec::new(), data);
                while i < data + len {
                    let n = p[i] as usize;
                    let s = String::from_utf8_lossy(p.get(i + 1..i + 1 + n).unwrap_or_default()).into_owned();
                    let (k, v) = s.split_once('=').map_or((s.clone(), String::new()), |(k, v)| (k.into(), v.into()));
                    if !k.is_empty() {
                        entries.push((k, v));
                    }
                    i += 1 + n;
                }
                Some(Record::Txt { name: name.clone(), entries })
            }
            A if len == 4 => Some(Record::A { name: name.clone(), ip: Ipv4Addr::new(p[data], p[data + 1], p[data + 2], p[data + 3]) }),
            _ => None,
        };
        out.extend(rec);
        at = data + len;
    }
    out
}

/// Fold records (from any number of packets, with the address each came
/// from) into services of the asked-for kinds.
pub fn services(kinds: &[&str], packets: &[(Vec<Record>, Option<Ipv4Addr>)]) -> Vec<Service> {
    let all: Vec<(&Record, Option<Ipv4Addr>)> = packets.iter().flat_map(|(rs, from)| rs.iter().map(move |r| (r, *from))).collect();
    let mut out: Vec<Service> = Vec::new();
    for (r, from) in &all {
        let Record::Ptr { name, target } = r else { continue };
        if !kinds.iter().any(|k| k.trim_end_matches('.') == name) || out.iter().any(|s| s.instance == *target) {
            continue;
        }
        let mut s = Service { kind: name.clone(), instance: target.clone(), ip: *from, ..Default::default() };
        for (r, _) in &all {
            match r {
                Record::Srv { name, port, host } if *name == s.instance => {
                    s.port = Some(*port);
                    // the host's A record beats the packet's source address
                    if let Some(ip) = all.iter().find_map(|(r, _)| match r {
                        Record::A { name, ip } if name == host => Some(*ip),
                        _ => None,
                    }) {
                        s.ip = Some(ip);
                    }
                }
                Record::Txt { name, entries } if *name == s.instance => s.txt.extend(entries.iter().cloned()),
                _ => {}
            }
        }
        out.push(s);
    }
    out
}

/// Ask the network for `kinds` and collect answers for `wait`.
pub fn browse(kinds: &[&str], wait: Duration) -> std::io::Result<Vec<Service>> {
    let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    sock.set_multicast_ttl_v4(255)?;
    sock.set_read_timeout(Some(Duration::from_millis(200)))?;
    let q = query(kinds);
    let start = Instant::now();
    let mut packets = Vec::new();
    let mut buf = [0u8; 9000];
    // ask twice: a lost packet shouldn't hide the TV
    sock.send_to(&q, MDNS)?;
    let mut resent = false;
    while start.elapsed() < wait {
        if !resent && start.elapsed() > wait / 3 {
            let _ = sock.send_to(&q, MDNS);
            resent = true;
        }
        match sock.recv_from(&mut buf) {
            Ok((n, from)) => {
                let ip = match from.ip() {
                    IpAddr::V4(v4) => Some(v4),
                    IpAddr::V6(_) => None,
                };
                packets.push((parse(&buf[..n]), ip));
            }
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(services(kinds, &packets))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(n: &str) -> Vec<u8> {
        let mut v = Vec::new();
        for l in n.split('.') {
            v.push(l.len() as u8);
            v.extend_from_slice(l.as_bytes());
        }
        v.push(0);
        v
    }

    fn rr(owner: &[u8], kind: u16, data: &[u8]) -> Vec<u8> {
        let mut v = owner.to_vec();
        v.extend_from_slice(&kind.to_be_bytes());
        v.extend_from_slice(&[0x80, 1, 0, 0, 0x11, 0x94]); // class IN (cache flush), ttl
        v.extend_from_slice(&(data.len() as u16).to_be_bytes());
        v.extend_from_slice(data);
        v
    }

    /// A response like the TV's: PTR, then SRV/TXT/A as additional records,
    /// with a compression pointer back to the service type.
    fn tv_response() -> Vec<u8> {
        let mut p = vec![0, 0, 0x84, 0, 0, 0, 0, 1, 0, 0, 0, 3];
        let kind_at = p.len();
        let kind = name("_googlecast._tcp.local");
        // PTR: owner = service type, target = "Chromecast-x" + pointer to type
        let mut target = vec![12];
        target.extend_from_slice(b"Chromecast-x");
        target.extend_from_slice(&[0xc0, kind_at as u8]);
        p.extend(rr(&kind, PTR, &target));
        let inst = name("Chromecast-x._googlecast._tcp.local");
        let mut srv = vec![0, 0, 0, 0];
        srv.extend_from_slice(&8009u16.to_be_bytes());
        srv.extend(name("tv.local"));
        p.extend(rr(&inst, SRV, &srv));
        let mut txt = Vec::new();
        for e in ["fn=Living Room TV", "md=QM163E", "rs=YouTube"] {
            txt.push(e.len() as u8);
            txt.extend_from_slice(e.as_bytes());
        }
        p.extend(rr(&inst, TXT, &txt));
        p.extend(rr(&name("tv.local"), A, &[192, 168, 1, 19]));
        p
    }

    #[test]
    fn builds_queries() {
        let q = query(&["_googlecast._tcp.local"]);
        assert_eq!(&q[4..6], &[0, 1]);
        assert_eq!(&q[12..24], b"\x0b_googlecast");
        assert!(q.ends_with(&[0, 0, 12, 0, 1]));
    }

    #[test]
    fn reads_a_tv_response() {
        let recs = parse(&tv_response());
        assert_eq!(recs.len(), 4);
        assert_eq!(
            recs[0],
            Record::Ptr { name: "_googlecast._tcp.local".into(), target: "Chromecast-x._googlecast._tcp.local".into() }
        );
        let found = services(&["_googlecast._tcp.local"], &[(recs, Some(Ipv4Addr::new(10, 0, 0, 1)))]);
        assert_eq!(found.len(), 1);
        let s = &found[0];
        assert_eq!((s.port, s.ip), (Some(8009), Some(Ipv4Addr::new(192, 168, 1, 19))));
        assert_eq!(s.txt.get("fn").map(String::as_str), Some("Living Room TV"));
        assert_eq!(s.txt.get("md").map(String::as_str), Some("QM163E"));
        // a kind we didn't ask about is ignored
        assert!(services(&["_other._tcp.local"], &[(parse(&tv_response()), None)]).is_empty());
    }

    #[test]
    fn survives_junk() {
        assert!(parse(&[]).is_empty());
        assert!(parse(&[0; 12]).is_empty());
        let mut p = tv_response();
        p.truncate(40);
        let _ = parse(&p);
        // a pointer loop doesn't hang
        let looped = [0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0xc0, 12];
        assert!(parse(&looped).is_empty());
    }
}
