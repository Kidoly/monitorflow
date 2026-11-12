//! `sensors.port_targets`: the addresses and host names the `port` sensor
//! may connect to, set by the agent's administrator (ADR-0111).
//!
//! Each entry is a host part with an optional port part:
//! - `10.0.0.0/8`, `192.168.1.10`, `fd00::/8`, `::1`: an address range in
//!   CIDR notation, or one address;
//! - `db.example.com`: a host name, compared without case;
//! - `:22`, `:8000-8999` after the host part: a port or a range of ports;
//!   without one, any port. An IPv6 host part with ports goes in brackets:
//!   `[fd00::/8]:443`.
//!
//! A listed host name may connect to whatever it resolves to. Any other
//! host name connects only to the resolved addresses inside a listed range
//! for its port. IPv4-mapped IPv6 addresses are matched as IPv4.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::ops::RangeInclusive;

use super::{parse_host, Host};

/// An entry of `sensors.port_targets` that cannot be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("sensors.port_targets: `{entry}` {reason}")]
pub struct PortTargetsError {
    entry: String,
    reason: String,
}

/// The parsed allowlist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortTargets {
    entries: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    host: Pattern,
    ports: RangeInclusive<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Pattern {
    Net(Net),
    /// Lower case.
    Name(String),
}

/// An address range: the address with its host bits cleared, and the
/// prefix length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Net {
    addr: IpAddr,
    prefix: u8,
}

impl Net {
    fn parse(text: &str) -> Result<Self, String> {
        let (addr, prefix) = match text.split_once('/') {
            Some((addr, prefix)) => (addr, Some(prefix)),
            None => (text, None),
        };
        let addr: IpAddr = addr
            .parse()
            .map_err(|_| format!("`{addr}` is not an IP address"))?;
        let max = if addr.is_ipv4() { 32 } else { 128 };
        let prefix =
            match prefix {
                None => max,
                Some(p) => p.parse::<u8>().ok().filter(|p| *p <= max).ok_or_else(|| {
                    format!("has a prefix length that is not between 0 and {max}")
                })?,
            };
        let net = Self { addr, prefix };
        if net.cleared() != addr {
            return Err(format!(
                "has bits set after its prefix; write `{}/{prefix}`",
                net.cleared()
            ));
        }
        Ok(net)
    }

    fn cleared(&self) -> IpAddr {
        match self.addr {
            IpAddr::V4(a) => Ipv4Addr::from(u32::from(a) & mask(self.prefix, 32) as u32).into(),
            IpAddr::V6(a) => Ipv6Addr::from(u128::from(a) & mask(self.prefix, 128)).into(),
        }
    }

    fn contains(&self, ip: IpAddr) -> bool {
        match (self.addr, ip.to_canonical()) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => {
                u32::from(ip) & mask(self.prefix, 32) as u32 == u32::from(net)
            }
            (IpAddr::V6(net), IpAddr::V6(ip)) => {
                u128::from(ip) & mask(self.prefix, 128) == u128::from(net)
            }
            _ => false,
        }
    }
}

/// The top `prefix` bits of a `bits`-wide address, in the low bits.
fn mask(prefix: u8, bits: u8) -> u128 {
    let all = if bits == 128 {
        u128::MAX
    } else {
        (1u128 << bits) - 1
    };
    match prefix {
        0 => 0,
        p => all & (all << (bits - p)),
    }
}

fn parse_ports(text: &str) -> Result<RangeInclusive<u16>, String> {
    let port = |p: &str| p.trim().parse::<u16>().ok().filter(|p| *p > 0);
    let (low, high) = match text.split_once('-') {
        Some((low, high)) => (port(low), port(high)),
        None => (port(text), port(text)),
    };
    match (low, high) {
        (Some(low), Some(high)) if low <= high => Ok(low..=high),
        _ => Err(format!(
            "has ports `{text}`; expected a port or a range like `8000-8999`, between 1 and 65535"
        )),
    }
}

impl Entry {
    fn parse(text: &str) -> Result<Self, String> {
        let (host, ports) = if let Some(rest) = text.strip_prefix('[') {
            let (host, after) = rest.split_once(']').ok_or("misses its `]`")?;
            match after {
                "" => (host, None),
                after => (
                    host,
                    Some(after.strip_prefix(':').ok_or("expects `:` after `]`")?),
                ),
            }
        } else if text.matches(':').count() > 1 {
            // A bare IPv6 address or range, without ports.
            (text, None)
        } else {
            match text.split_once(':') {
                Some((host, ports)) => (host, Some(ports)),
                None => (text, None),
            }
        };
        let ports = ports.map_or(Ok(1..=u16::MAX), parse_ports)?;
        let host = if host.contains('/') {
            Pattern::Net(Net::parse(host)?)
        } else {
            match parse_host(host)? {
                Host::Ip(addr) => Pattern::Net(Net::parse(&addr.to_string())?),
                Host::Name(name) => Pattern::Name(name.to_ascii_lowercase()),
            }
        };
        Ok(Self { host, ports })
    }
}

impl PortTargets {
    /// Parses `sensors.port_targets`. An empty list allows nothing.
    pub fn parse<S: AsRef<str>>(entries: &[S]) -> Result<Self, PortTargetsError> {
        let entries = entries
            .iter()
            .map(|entry| {
                let entry = entry.as_ref().trim();
                Entry::parse(entry).map_err(|reason| PortTargetsError {
                    entry: entry.chars().take(64).collect(),
                    reason,
                })
            })
            .collect::<Result<_, _>>()?;
        Ok(Self { entries })
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether `ip` is in a listed range for `port`.
    pub fn allows_ip(&self, ip: IpAddr, port: u16) -> bool {
        self.entries.iter().any(|e| {
            e.ports.contains(&port) && matches!(&e.host, Pattern::Net(net) if net.contains(ip))
        })
    }

    /// Whether the host name `name` is listed for `port`.
    pub fn allows_name(&self, name: &str, port: u16) -> bool {
        self.entries.iter().any(|e| {
            e.ports.contains(&port)
                && matches!(&e.host, Pattern::Name(n) if n.eq_ignore_ascii_case(name))
        })
    }

    /// Whether some address range is listed for `port`, so that an
    /// unlisted host name may resolve into it.
    pub fn has_range_for(&self, port: u16) -> bool {
        self.entries
            .iter()
            .any(|e| e.ports.contains(&port) && matches!(e.host, Pattern::Net(_)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    fn targets(entries: &[&str]) -> PortTargets {
        PortTargets::parse(entries).unwrap()
    }

    #[test]
    fn ranges_match_their_addresses_only() {
        let t = targets(&["10.0.0.0/8", "192.168.1.10", "fd00::/8", "0.0.0.0/0:443"]);
        for allowed in ["10.0.0.1", "10.255.255.255", "192.168.1.10", "fd12::1"] {
            assert!(t.allows_ip(ip(allowed), 22), "{allowed}");
        }
        for refused in ["11.0.0.1", "192.168.1.11", "fe80::1", "127.0.0.1", "::1"] {
            assert!(!t.allows_ip(ip(refused), 22), "{refused}");
        }
        // Any IPv4 address on 443, but no IPv6 one outside fd00::/8.
        assert!(t.allows_ip(ip("169.254.169.254"), 443));
        assert!(!t.allows_ip(ip("2001:db8::1"), 443));
    }

    #[test]
    fn ipv4_mapped_addresses_are_matched_as_ipv4() {
        let t = targets(&["10.0.0.0/8"]);
        assert!(t.allows_ip(ip("::ffff:10.1.2.3"), 22));
        assert!(!t.allows_ip(ip("::ffff:169.254.169.254"), 22));
        assert!(!targets(&["::/0"]).allows_ip(ip("::ffff:127.0.0.1"), 22));
    }

    #[test]
    fn ports_and_port_ranges() {
        let t = targets(&[
            "10.0.0.0/8:22",
            "192.168.0.0/16:8000-8999",
            "[fd00::/8]:443",
            "[::1]",
            "db.example.com:5432",
        ]);
        assert!(t.allows_ip(ip("10.0.0.1"), 22));
        assert!(!t.allows_ip(ip("10.0.0.1"), 23));
        assert!(t.allows_ip(ip("192.168.3.4"), 8000));
        assert!(t.allows_ip(ip("192.168.3.4"), 8999));
        assert!(!t.allows_ip(ip("192.168.3.4"), 9000));
        assert!(t.allows_ip(ip("fd00::1"), 443));
        assert!(!t.allows_ip(ip("fd00::1"), 80));
        assert!(t.allows_ip(ip("::1"), 1));
        assert!(t.allows_name("DB.example.com", 5432));
        assert!(!t.allows_name("db.example.com", 22));
        assert!(!t.allows_name("example.com", 5432));
        assert!(t.has_range_for(22));
        // `[::1]` allows any port.
        assert!(t.has_range_for(5432));
        assert!(!targets(&["10.0.0.0/8:22", "db.example.com:5432"]).has_range_for(5432));
    }

    #[test]
    fn names_are_not_ranges() {
        let t = targets(&["db.example.com"]);
        assert!(t.allows_name("db.example.com", 1));
        assert!(!t.has_range_for(1));
        assert!(!t.allows_ip(ip("127.0.0.1"), 1));
        assert!(targets(&[]).is_empty());
    }

    #[test]
    fn bad_entries_are_refused_with_a_reason() {
        for (entry, reason) in [
            ("10.0.0.1/8", "write `10.0.0.0/8`"),
            ("10.0.0.0/33", "between 0 and 32"),
            ("fd00::/129", "between 0 and 128"),
            ("example.com/8", "not an IP address"),
            ("10.0.0.1:0", "between 1 and 65535"),
            ("10.0.0.1:ssh", "between 1 and 65535"),
            ("10.0.0.1:90-80", "range"),
            ("10.0.0.1:", "between 1 and 65535"),
            ("[fd00::/8", "misses its `]`"),
            ("[fd00::/8]443", "expects `:`"),
            ("a b", "neither"),
            ("", "neither"),
        ] {
            let error = PortTargets::parse(&[entry]).unwrap_err().to_string();
            assert!(error.contains(reason), "{entry:?}: {error}");
            assert!(error.starts_with("sensors.port_targets: "), "{error}");
        }
    }

    #[test]
    fn masks() {
        assert_eq!(mask(0, 32), 0);
        assert_eq!(mask(8, 32), 0xff00_0000);
        assert_eq!(mask(32, 32), 0xffff_ffff);
        assert_eq!(mask(128, 128), u128::MAX);
        assert_eq!(mask(1, 128), 1 << 127);
    }
}
