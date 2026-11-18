//! The name of an address, as the system would give it: `/etc/hosts`
//! first, then a PTR query to the first name server of
//! `/etc/resolv.conf` over UDP. Small on purpose: one question, one
//! answer, no recursion of our own, no cache.

use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::time::timeout;

/// Longest a name may be, as DNS allows.
pub const MAX_NAME: usize = 253;
const DNS_PORT: u16 = 53;

/// The name of `ip`, or `None` when nothing names it in `wait`.
pub async fn name_of(ip: IpAddr, wait: Duration) -> Option<String> {
    if let Some(name) = read(Path::new("/etc/hosts"))
        .as_deref()
        .and_then(|hosts| from_hosts(hosts, ip))
    {
        return Some(name);
    }
    let server = read(Path::new("/etc/resolv.conf")).and_then(|c| name_server(&c))?;
    timeout(wait, query(server, ip)).await.ok().flatten()
}

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// The first name of `ip` in a hosts file, if a usable one.
pub fn from_hosts(hosts: &str, ip: IpAddr) -> Option<String> {
    hosts.lines().find_map(|line| {
        let line = line.split('#').next().unwrap_or("");
        let mut words = line.split_whitespace();
        let address: IpAddr = words.next()?.parse().ok()?;
        if address != ip {
            return None;
        }
        words.map(clean).find_map(|n| n)
    })
}

/// The first `nameserver` of a resolv.conf.
pub fn name_server(conf: &str) -> Option<SocketAddr> {
    conf.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        (words.next()? == "nameserver")
            .then(|| words.next())
            .flatten()
            .and_then(|a| a.split('%').next())
            .and_then(|a| a.parse::<IpAddr>().ok())
            .map(|ip| SocketAddr::new(ip, DNS_PORT))
    })
}

/// A name as shown: lowercase, without the final dot, letters, digits,
/// `-`, `_` and dots only, at most [`MAX_NAME`] bytes.
pub fn clean(name: &str) -> Option<String> {
    let name = name.trim_end_matches('.').to_ascii_lowercase();
    (!name.is_empty()
        && name.len() <= MAX_NAME
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.'))
    .then_some(name)
}

/// The PTR name of `ip`, `4.3.2.1.in-addr.arpa` or nibbles in `ip6.arpa`.
pub fn reverse_name(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            format!("{}.{}.{}.{}.in-addr.arpa", o[3], o[2], o[1], o[0])
        }
        IpAddr::V6(v6) => {
            let mut parts: Vec<String> = Vec::with_capacity(34);
            for byte in v6.octets().iter().rev() {
                parts.push(format!("{:x}", byte & 0x0f));
                parts.push(format!("{:x}", byte >> 4));
            }
            parts.push("ip6.arpa".into());
            parts.join(".")
        }
    }
}

/// A query for the PTR record of `ip`, with `id`.
pub fn ptr_query(id: u16, ip: IpAddr) -> Vec<u8> {
    let mut packet = Vec::with_capacity(64);
    packet.extend_from_slice(&id.to_be_bytes());
    // Recursion desired, one question.
    packet.extend_from_slice(&[0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0]);
    for label in reverse_name(ip).split('.') {
        packet.push(label.len() as u8);
        packet.extend_from_slice(label.as_bytes());
    }
    // Root, type PTR (12), class IN.
    packet.extend_from_slice(&[0, 0, 12, 0, 1]);
    packet
}

/// The name of the first PTR answer of `packet`, an answer to query `id`.
pub fn ptr_answer(packet: &[u8], id: u16) -> Option<String> {
    if packet.len() < 12 || packet[0..2] != id.to_be_bytes() || packet[2] & 0x80 == 0 {
        return None;
    }
    if packet[3] & 0x0f != 0 {
        return None;
    }
    let questions = u16::from_be_bytes([packet[4], packet[5]]);
    let answers = u16::from_be_bytes([packet[6], packet[7]]);
    let mut at = 12;
    for _ in 0..questions {
        at = skip_name(packet, at)? + 4;
    }
    for _ in 0..answers {
        at = skip_name(packet, at)?;
        let header = packet.get(at..at + 10)?;
        let kind = u16::from_be_bytes([header[0], header[1]]);
        let length = usize::from(u16::from_be_bytes([header[8], header[9]]));
        at += 10;
        if kind == 12 {
            return read_name(packet, at).and_then(|n| clean(&n));
        }
        at += length;
    }
    None
}

fn skip_name(packet: &[u8], mut at: usize) -> Option<usize> {
    loop {
        let length = *packet.get(at)?;
        match length {
            0 => return Some(at + 1),
            l if l & 0xc0 == 0xc0 => return Some(at + 2),
            l => at += 1 + usize::from(l),
        }
    }
}

fn read_name(packet: &[u8], mut at: usize) -> Option<String> {
    let mut labels: Vec<String> = Vec::new();
    // Bounds the pointers a hostile answer could loop through.
    for _ in 0..128 {
        let length = *packet.get(at)?;
        if length == 0 {
            return Some(labels.join("."));
        }
        if length & 0xc0 == 0xc0 {
            let low = *packet.get(at + 1)?;
            at = usize::from(u16::from_be_bytes([length & 0x3f, low]));
            continue;
        }
        let label = packet.get(at + 1..at + 1 + usize::from(length))?;
        labels.push(String::from_utf8_lossy(label).into_owned());
        at += 1 + usize::from(length);
    }
    None
}

async fn query(server: SocketAddr, ip: IpAddr) -> Option<String> {
    let bind: SocketAddr = if server.is_ipv4() {
        "0.0.0.0:0".parse().ok()?
    } else {
        "[::]:0".parse().ok()?
    };
    let socket = UdpSocket::bind(bind).await.ok()?;
    socket.connect(server).await.ok()?;
    let id = random_id();
    socket.send(&ptr_query(id, ip)).await.ok()?;
    let mut buffer = [0u8; 1500];
    // Answers to other queries on this port are skipped, not believed.
    loop {
        let read = socket.recv(&mut buffer).await.ok()?;
        if let Some(name) = ptr_answer(&buffer[..read], id) {
            return Some(name);
        }
        if read >= 4 && buffer[0..2] == id.to_be_bytes() {
            return None;
        }
    }
}

fn random_id() -> u16 {
    use std::hash::BuildHasher;
    std::collections::hash_map::RandomState::new().hash_one(std::time::Instant::now()) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_and_resolv_conf_are_read() {
        let hosts = "127.0.0.1 localhost\n# 10.0.0.5 old\n10.0.0.5\tNas.Lan nas # the NAS\n";
        let ip: IpAddr = "10.0.0.5".parse().unwrap();
        assert_eq!(from_hosts(hosts, ip).as_deref(), Some("nas.lan"));
        assert_eq!(from_hosts(hosts, "10.0.0.6".parse().unwrap()), None);
        let conf = "search lan\nnameserver fe80::1%eth0\nnameserver 10.0.0.1\n";
        assert_eq!(name_server(conf), Some("[fe80::1]:53".parse().unwrap()));
        assert_eq!(name_server("search lan\n"), None);
    }

    #[test]
    fn names_are_cleaned() {
        assert_eq!(
            clean("Web-01.Example.com.").as_deref(),
            Some("web-01.example.com")
        );
        assert_eq!(clean("bad name"), None);
        assert_eq!(clean("a\u{7}b"), None);
        assert_eq!(clean(&"a".repeat(MAX_NAME + 1)), None);
        assert_eq!(clean("."), None);
    }

    #[test]
    fn reverse_names() {
        assert_eq!(
            reverse_name("192.0.2.10".parse().unwrap()),
            "10.2.0.192.in-addr.arpa"
        );
        let v6 = reverse_name("2001:db8::1".parse().unwrap());
        assert!(v6.starts_with("1.0.0.0.0.0.0.0."));
        assert!(v6.ends_with(".8.b.d.0.1.0.0.2.ip6.arpa"));
    }

    /// An answer as a server would write it, with a compressed owner.
    fn answer(id: u16, ip: IpAddr, name: &str) -> Vec<u8> {
        let query = ptr_query(id, ip);
        let mut packet = query.clone();
        packet[2] = 0x81;
        packet[3] = 0x80;
        packet[7] = 1;
        packet.extend_from_slice(&[0xc0, 12, 0, 12, 0, 1, 0, 0, 0, 60]);
        let mut rdata = Vec::new();
        for label in name.split('.') {
            rdata.push(label.len() as u8);
            rdata.extend_from_slice(label.as_bytes());
        }
        rdata.push(0);
        packet.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
        packet.extend_from_slice(&rdata);
        packet
    }

    #[test]
    fn answers_are_parsed() {
        let ip: IpAddr = "192.0.2.10".parse().unwrap();
        let packet = answer(7, ip, "printer.lan");
        assert_eq!(ptr_answer(&packet, 7).as_deref(), Some("printer.lan"));
        // Another query's answer, a question, an error, a truncation.
        assert_eq!(ptr_answer(&packet, 8), None);
        assert_eq!(ptr_answer(&ptr_query(7, ip), 7), None);
        let mut refused = packet.clone();
        refused[3] = 0x83;
        assert_eq!(ptr_answer(&refused, 7), None);
        assert_eq!(ptr_answer(&packet[..packet.len() - 3], 7), None);
    }

    #[test]
    fn a_pointer_loop_ends() {
        let ip: IpAddr = "192.0.2.10".parse().unwrap();
        let mut packet = answer(7, ip, "x");
        let at = packet.len() - 3;
        // The answer's name points at itself.
        packet[at] = 0xc0;
        packet[at + 1] = at as u8;
        assert_eq!(ptr_answer(&packet, 7), None);
    }

    #[tokio::test]
    async fn a_name_server_is_asked() {
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = server.local_addr().unwrap();
        let ip: IpAddr = "192.0.2.10".parse().unwrap();
        tokio::spawn(async move {
            let mut buffer = [0u8; 512];
            let (_, from) = server.recv_from(&mut buffer).await.unwrap();
            let id = u16::from_be_bytes([buffer[0], buffer[1]]);
            server
                .send_to(&answer(id, ip, "Switch-1.lan"), from)
                .await
                .unwrap();
        });
        assert_eq!(query(address, ip).await.as_deref(), Some("switch-1.lan"));
    }
}
