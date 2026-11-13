//! Entries of `api_trusted_proxies` (ADR-0110): the reverse proxies whose
//! `X-Forwarded-For` the REST API believes.

use std::fmt;
use std::net::IpAddr;
use std::str::FromStr;

/// One trusted proxy: an address, a network in CIDR notation, or a host
/// name the server resolves, like the `web` service of the compose files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustedProxy {
    /// An address and the length of its prefix; a plain address has the
    /// full length.
    Network(IpAddr, u8),
    Host(String),
}

impl TrustedProxy {
    /// Whether `addr` is in this network. Always false for a host name,
    /// which the caller resolves.
    pub fn contains(&self, addr: IpAddr) -> bool {
        let Self::Network(net, prefix) = self else {
            return false;
        };
        match (net, addr.to_canonical()) {
            (IpAddr::V4(net), IpAddr::V4(addr)) => {
                same_prefix(u32::from(*net).into(), u32::from(addr).into(), 32, *prefix)
            }
            (IpAddr::V6(net), IpAddr::V6(addr)) => {
                same_prefix(u128::from(*net), u128::from(addr), 128, *prefix)
            }
            _ => false,
        }
    }
}

fn same_prefix(a: u128, b: u128, bits: u8, prefix: u8) -> bool {
    let shift = u32::from(bits - prefix);
    a.checked_shr(shift).unwrap_or(0) == b.checked_shr(shift).unwrap_or(0)
}

impl FromStr for TrustedProxy {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        let wrong = || format!("`{s}` is not an IP address, a CIDR network or a host name");
        if let Some((addr, prefix)) = s.split_once('/') {
            let addr: IpAddr = addr.parse().map_err(|_| wrong())?;
            let prefix: u8 = prefix.parse().map_err(|_| wrong())?;
            let bits = if addr.is_ipv4() { 32 } else { 128 };
            if prefix > bits {
                return Err(wrong());
            }
            return Ok(Self::Network(addr.to_canonical(), prefix));
        }
        if let Ok(addr) = s.parse::<IpAddr>() {
            let addr = addr.to_canonical();
            let bits = if addr.is_ipv4() { 32 } else { 128 };
            return Ok(Self::Network(addr, bits));
        }
        let host = !s.is_empty()
            && s.len() <= 253
            && s.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            });
        if host {
            Ok(Self::Host(s.to_ascii_lowercase()))
        } else {
            Err(wrong())
        }
    }
}

impl fmt::Display for TrustedProxy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Network(addr, prefix) => write!(f, "{addr}/{prefix}"),
            Self::Host(host) => f.write_str(host),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn parses_addresses_networks_and_host_names() {
        let p = |s: &str| s.parse::<TrustedProxy>();
        assert_eq!(
            p("127.0.0.1"),
            Ok(TrustedProxy::Network(ip("127.0.0.1"), 32))
        );
        assert_eq!(p(" ::1 "), Ok(TrustedProxy::Network(ip("::1"), 128)));
        assert_eq!(
            p("10.0.0.0/8"),
            Ok(TrustedProxy::Network(ip("10.0.0.0"), 8))
        );
        assert_eq!(p("Web"), Ok(TrustedProxy::Host("web".into())));
        assert_eq!(
            p("proxy.example.net"),
            Ok(TrustedProxy::Host("proxy.example.net".into()))
        );
        for wrong in [
            "",
            "10.0.0.0/33",
            "::/129",
            "a b",
            "-web",
            "x/8",
            "1.2.3.4/x",
        ] {
            assert!(p(wrong).is_err(), "{wrong}");
        }
    }

    #[test]
    fn networks_contain_their_addresses_only() {
        let net: TrustedProxy = "172.18.0.0/16".parse().unwrap();
        assert!(net.contains(ip("172.18.3.4")));
        assert!(!net.contains(ip("172.19.0.1")));
        assert!(!net.contains(ip("::1")));
        // A v4 client on a dual-stack socket.
        assert!(net.contains(ip("::ffff:172.18.0.9")));
        let one: TrustedProxy = "::1".parse().unwrap();
        assert!(one.contains(ip("::1")));
        assert!(!one.contains(ip("::2")));
        let all: TrustedProxy = "0.0.0.0/0".parse().unwrap();
        assert!(all.contains(ip("198.51.100.1")));
        let v6: TrustedProxy = "fd00::/8".parse().unwrap();
        assert!(v6.contains(ip("fd12::1")));
        assert!(!v6.contains(ip("fe80::1")));
        assert!(!TrustedProxy::Host("web".into()).contains(ip("127.0.0.1")));
    }
}
