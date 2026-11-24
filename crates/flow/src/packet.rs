//! The start of a sampled packet, as sFlow sends it: Ethernet (with up
//! to two VLAN tags), then IPv4 or IPv6, then the ports of TCP, UDP or
//! SCTP. Anything else, or a header cut short, gives what was read.

use std::net::IpAddr;

use crate::read::Reader;

/// What the headers of one packet say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Headers {
    pub src: IpAddr,
    pub dst: IpAddr,
    pub protocol: u8,
    pub src_port: u16,
    pub dst_port: u16,
}

const IPV4: u16 = 0x0800;
const IPV6: u16 = 0x86dd;
const VLAN: [u16; 2] = [0x8100, 0x88a8];

/// An Ethernet frame's headers.
pub fn ethernet(frame: &[u8]) -> Option<Headers> {
    let mut r = Reader::new(frame);
    r.skip(12)?;
    let mut ether_type = r.u16()?;
    for _ in 0..2 {
        if !VLAN.contains(&ether_type) {
            break;
        }
        r.skip(2)?;
        ether_type = r.u16()?;
    }
    match ether_type {
        IPV4 => ipv4(r),
        IPV6 => ipv6(r),
        _ => None,
    }
}

/// An IPv4 packet's headers.
pub fn ipv4(mut r: Reader<'_>) -> Option<Headers> {
    let first = r.u8()?;
    if first >> 4 != 4 {
        return None;
    }
    let header = usize::from(first & 0x0f) * 4;
    r.skip(5)?;
    // A fragment after the first has no transport header.
    let fragment = r.u16()? & 0x1fff;
    r.skip(1)?;
    let protocol = r.u8()?;
    r.skip(2)?;
    let src = IpAddr::V4(r.ipv4()?);
    let dst = IpAddr::V4(r.ipv4()?);
    r.skip(header.checked_sub(20)?)?;
    let (src_port, dst_port) = if fragment == 0 {
        ports(protocol, r)
    } else {
        (0, 0)
    };
    Some(Headers {
        src,
        dst,
        protocol,
        src_port,
        dst_port,
    })
}

/// An IPv6 packet's headers; extension headers are not followed.
pub fn ipv6(mut r: Reader<'_>) -> Option<Headers> {
    if r.u8()? >> 4 != 6 {
        return None;
    }
    r.skip(5)?;
    let protocol = r.u8()?;
    r.skip(1)?;
    let src = IpAddr::V6(r.ipv6()?);
    let dst = IpAddr::V6(r.ipv6()?);
    let (src_port, dst_port) = ports(protocol, r);
    Some(Headers {
        src,
        dst,
        protocol,
        src_port,
        dst_port,
    })
}

fn ports(protocol: u8, mut r: Reader<'_>) -> (u16, u16) {
    if !matches!(protocol, 6 | 17 | 132) {
        return (0, 0);
    }
    match (r.u16(), r.u16()) {
        (Some(s), Some(d)) => (s, d),
        _ => (0, 0),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// An Ethernet frame with one VLAN tag, IPv4 and UDP.
    pub fn frame(src: [u8; 4], dst: [u8; 4], protocol: u8, sport: u16, dport: u16) -> Vec<u8> {
        let mut f = vec![0u8; 12];
        f.extend(0x8100u16.to_be_bytes());
        f.extend([0, 10]);
        f.extend(IPV4.to_be_bytes());
        f.extend([0x45, 0, 0, 60, 0, 0, 0, 0, 64, protocol, 0, 0]);
        f.extend(src);
        f.extend(dst);
        f.extend(sport.to_be_bytes());
        f.extend(dport.to_be_bytes());
        f.extend([0u8; 4]);
        f
    }

    #[test]
    fn vlan_ipv4_udp() {
        let h = ethernet(&frame([192, 0, 2, 1], [203, 0, 113, 9], 17, 5353, 53)).unwrap();
        assert_eq!(h.src, "192.0.2.1".parse::<IpAddr>().unwrap());
        assert_eq!(h.dst, "203.0.113.9".parse::<IpAddr>().unwrap());
        assert_eq!((h.protocol, h.src_port, h.dst_port), (17, 5353, 53));
    }

    #[test]
    fn ipv6_tcp_and_cut_headers() {
        let mut p = vec![0x60, 0, 0, 0, 0, 20, 6, 64];
        p.extend(
            "2001:db8::a"
                .parse::<std::net::Ipv6Addr>()
                .unwrap()
                .octets(),
        );
        p.extend(
            "2001:db8::b"
                .parse::<std::net::Ipv6Addr>()
                .unwrap()
                .octets(),
        );
        p.extend(443u16.to_be_bytes());
        p.extend(50_000u16.to_be_bytes());
        let h = ipv6(Reader::new(&p)).unwrap();
        assert_eq!((h.protocol, h.src_port, h.dst_port), (6, 443, 50_000));
        // Cut in the ports: addresses still count.
        let h = ipv6(Reader::new(&p[..41])).unwrap();
        assert_eq!((h.src_port, h.dst_port), (0, 0));
        assert_eq!(ipv6(Reader::new(&p[..20])), None);
        assert_eq!(ethernet(&[0; 13]), None);
    }
}
