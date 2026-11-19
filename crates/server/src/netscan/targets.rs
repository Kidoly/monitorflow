//! The addresses of a scan (ADR-0154): IP addresses, CIDR blocks and
//! ranges, expanded to at most [`MAX_ADDRESSES`]. Never DNS names: what a
//! name resolves to is not what the person read.

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Most addresses one scan tries: a /22.
pub const MAX_ADDRESSES: usize = 1024;
/// Most entries in a scan's list.
pub const MAX_ENTRIES: usize = 64;
/// The ports a scan tries when not told: remote access, web, file
/// sharing and Windows management.
pub const DEFAULT_PORTS: [u16; 11] = [22, 23, 80, 135, 139, 443, 445, 3389, 5985, 8080, 8443];
/// Most ports a scan tries.
pub const MAX_PORTS: usize = 32;

/// Why an address may not be scanned, or `None` when it may.
fn refused(ip: IpAddr) -> Option<&'static str> {
    match ip {
        IpAddr::V4(v4) => {
            if v4.is_loopback() {
                Some("a loopback address")
            } else if v4.is_multicast() || v4 == Ipv4Addr::BROADCAST {
                Some("a multicast or broadcast address")
            } else if v4.is_unspecified() {
                Some("the unspecified address")
            } else if v4.is_link_local() {
                Some("a link-local address")
            } else {
                None
            }
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return refused(IpAddr::V4(v4));
            }
            if v6.is_loopback() {
                Some("a loopback address")
            } else if v6.is_multicast() {
                Some("a multicast address")
            } else if v6.is_unspecified() {
                Some("the unspecified address")
            } else if v6.segments()[0] & 0xffc0 == 0xfe80 {
                Some("a link-local address")
            } else {
                None
            }
        }
    }
}

fn too_many() -> String {
    format!("a scan tries at most {MAX_ADDRESSES} addresses, a /22")
}

/// Adds the addresses of one entry to `out`.
fn expand_one(entry: &str, out: &mut BTreeSet<IpAddr>) -> Result<(), String> {
    let quoted = entry.escape_debug();
    if let Some((base, prefix)) = entry.split_once('/') {
        let base: IpAddr = base
            .trim()
            .parse()
            .map_err(|_| format!("`{quoted}` is not an IP address block like 192.168.1.0/24"))?;
        let bits = if base.is_ipv4() { 32 } else { 128 };
        let prefix: u32 = prefix
            .trim()
            .parse()
            .ok()
            .filter(|p| *p <= bits)
            .ok_or_else(|| format!("`{quoted}`: the prefix must be 0 to {bits}"))?;
        let size_bits = bits - prefix;
        if size_bits > 10 {
            return Err(too_many());
        }
        if let Some(why) = refused(base) {
            return Err(format!("`{quoted}` is {why}, which is never scanned"));
        }
        match base {
            IpAddr::V4(v4) => {
                let mask = if prefix == 0 {
                    0
                } else {
                    u32::MAX << size_bits
                };
                let first = u32::from(v4) & mask;
                let count = 1u32 << size_bits;
                // Network and broadcast addresses are not hosts.
                let (from, to) = if prefix < 31 {
                    (first + 1, first + count - 2)
                } else {
                    (first, first + count - 1)
                };
                for n in from..=to {
                    out.insert(IpAddr::V4(Ipv4Addr::from(n)));
                }
            }
            IpAddr::V6(v6) => {
                let mask = if prefix == 0 {
                    0
                } else {
                    u128::MAX << size_bits
                };
                let first = u128::from(v6) & mask;
                for n in 0..(1u128 << size_bits) {
                    out.insert(IpAddr::V6(Ipv6Addr::from(first + n)));
                }
            }
        }
        return Ok(());
    }
    if let Some((from, to)) = entry.split_once('-') {
        let start: IpAddr = from
            .trim()
            .parse()
            .map_err(|_| format!("`{quoted}` is not a range like 192.168.1.10-50"))?;
        let to = to.trim();
        let end: IpAddr = match (start, to.parse::<u8>()) {
            (IpAddr::V4(v4), Ok(last)) => {
                let o = v4.octets();
                IpAddr::V4(Ipv4Addr::new(o[0], o[1], o[2], last))
            }
            _ => to
                .parse()
                .map_err(|_| format!("`{quoted}` is not a range like 192.168.1.10-50"))?,
        };
        let (a, b) = match (start, end) {
            (IpAddr::V4(a), IpAddr::V4(b)) => (u128::from(u32::from(a)), u128::from(u32::from(b))),
            (IpAddr::V6(a), IpAddr::V6(b)) => (u128::from(a), u128::from(b)),
            _ => return Err(format!("`{quoted}` mixes IPv4 and IPv6")),
        };
        if b < a {
            return Err(format!("`{quoted}` ends before it starts"));
        }
        if b - a >= MAX_ADDRESSES as u128 {
            return Err(too_many());
        }
        for n in a..=b {
            let ip = if start.is_ipv4() {
                IpAddr::V4(Ipv4Addr::from(n as u32))
            } else {
                IpAddr::V6(Ipv6Addr::from(n))
            };
            if let Some(why) = refused(ip) {
                return Err(format!("`{quoted}` holds {why}, which is never scanned"));
            }
            out.insert(ip);
        }
        return Ok(());
    }
    let ip: IpAddr = entry
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse()
        .map_err(|_| {
            format!("`{quoted}` is not an IP address: a scan takes addresses, blocks and ranges, never names")
        })?;
    if let Some(why) = refused(ip) {
        return Err(format!("`{quoted}` is {why}, which is never scanned"));
    }
    out.insert(ip);
    Ok(())
}

/// Checks `entries` and returns them trimmed, with every address they
/// stand for, sorted. The error is for the client.
pub fn expand(entries: &[String]) -> Result<(Vec<String>, Vec<IpAddr>), String> {
    let entries: Vec<String> = entries
        .iter()
        .map(|e| e.trim().to_owned())
        .filter(|e| !e.is_empty())
        .collect();
    if entries.is_empty() {
        return Err("`targets` must name at least one address, block or range".into());
    }
    if entries.len() > MAX_ENTRIES {
        return Err(format!("`targets` holds at most {MAX_ENTRIES} entries"));
    }
    let mut out = BTreeSet::new();
    for entry in &entries {
        if entry.len() > 100 || entry.chars().any(char::is_control) {
            return Err("an entry of `targets` is too long".into());
        }
        expand_one(entry, &mut out)?;
        if out.len() > MAX_ADDRESSES {
            return Err(too_many());
        }
    }
    Ok((entries, out.into_iter().collect()))
}

/// Checks the ports to try: 1 to 65535, at most [`MAX_PORTS`], sorted
/// without repeats; [`DEFAULT_PORTS`] when `None`.
pub fn ports(ports: Option<&[u16]>) -> Result<Vec<u16>, String> {
    let Some(ports) = ports else {
        return Ok(DEFAULT_PORTS.to_vec());
    };
    let set: BTreeSet<u16> = ports.iter().copied().collect();
    if set.contains(&0) {
        return Err("`ports` must be between 1 and 65535".into());
    }
    if set.len() > MAX_PORTS {
        return Err(format!("`ports` holds at most {MAX_PORTS} ports"));
    }
    Ok(set.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn of(entries: &[&str]) -> Result<Vec<IpAddr>, String> {
        let entries: Vec<String> = entries.iter().map(|e| (*e).to_owned()).collect();
        expand(&entries).map(|(_, ips)| ips)
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn blocks_leave_out_network_and_broadcast() {
        let ips = of(&["192.0.2.0/24"]).unwrap();
        assert_eq!(ips.len(), 254);
        assert_eq!(ips[0], ip("192.0.2.1"));
        assert_eq!(ips[253], ip("192.0.2.254"));
        // A base inside the block counts from the block's start.
        assert_eq!(
            of(&["192.0.2.77/30"]).unwrap(),
            [ip("192.0.2.77"), ip("192.0.2.78")]
        );
        assert_eq!(of(&["192.0.2.8/31"]).unwrap().len(), 2);
        assert_eq!(of(&["192.0.2.8/32"]).unwrap(), [ip("192.0.2.8")]);
        assert_eq!(of(&["10.0.0.0/22"]).unwrap().len(), 1022);
        assert_eq!(of(&["2001:db8::/120"]).unwrap().len(), 256);
    }

    #[test]
    fn ranges_and_lists() {
        let ips = of(&["192.0.2.10-12", " 192.0.2.12 ", "192.0.2.250-198.51.100.1"]);
        let err = ips.unwrap_err();
        assert!(err.contains("at most"), "{err}");
        let ips = of(&["192.0.2.10-12", "192.0.2.12", "198.51.100.5"]).unwrap();
        assert_eq!(
            ips,
            [
                ip("192.0.2.10"),
                ip("192.0.2.11"),
                ip("192.0.2.12"),
                ip("198.51.100.5")
            ]
        );
        assert_eq!(of(&["192.0.2.254-198.51.100.0"]).unwrap_err(), too_many());
        assert_eq!(of(&["2001:db8::1-2001:db8::3"]).unwrap().len(), 3);
        assert_eq!(of(&["[2001:db8::9]"]).unwrap(), [ip("2001:db8::9")]);
    }

    #[test]
    fn what_is_refused() {
        for bad in [
            &[][..],
            &["  "],
            &["printer.lan"],
            &["192.0.2.0/21"],
            &["10.0.0.0/8"],
            &["2001:db8::/64"],
            &["192.0.2.0/33"],
            &["192.0.2.20-10"],
            &["192.0.2.1-2001:db8::1"],
            &["127.0.0.1"],
            &["127.0.0.0/24"],
            &["224.0.0.1"],
            &["169.254.1.1"],
            &["0.0.0.0"],
            &["::1"],
            &["fe80::1"],
            &["::ffff:127.0.0.1"],
            &["255.255.255.255"],
            &["10.0.0.0/22", "10.1.0.0/24"],
        ] {
            assert!(of(bad).is_err(), "{bad:?}");
        }
        let many: Vec<String> = (0..65).map(|i| format!("192.0.2.{i}")).collect();
        assert!(expand(&many).is_err());
    }

    #[test]
    fn ports_are_checked() {
        assert_eq!(ports(None).unwrap(), DEFAULT_PORTS);
        assert_eq!(ports(Some(&[443, 22, 443])).unwrap(), [22, 443]);
        assert!(ports(Some(&[])).unwrap().is_empty());
        assert!(ports(Some(&[0])).is_err());
        let many: Vec<u16> = (1..=33).collect();
        assert!(ports(Some(&many)).is_err());
    }
}
