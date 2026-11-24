//! A cursor over a datagram that never reads past its end: every read
//! returns `None` instead, so a short or lying packet ends its decoding
//! rather than panicking.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

#[derive(Debug, Clone, Copy)]
pub struct Reader<'a> {
    data: &'a [u8],
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data }
    }

    pub fn remaining(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// The next `n` bytes, consumed.
    pub fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        if n > self.data.len() {
            return None;
        }
        let (head, tail) = self.data.split_at(n);
        self.data = tail;
        Some(head)
    }

    /// A reader over the next `n` bytes, consumed from this one.
    pub fn sub(&mut self, n: usize) -> Option<Reader<'a>> {
        self.bytes(n).map(Reader::new)
    }

    pub fn skip(&mut self, n: usize) -> Option<()> {
        self.bytes(n).map(|_| ())
    }

    pub fn u8(&mut self) -> Option<u8> {
        self.bytes(1).map(|b| b[0])
    }

    pub fn u16(&mut self) -> Option<u16> {
        self.array().map(u16::from_be_bytes)
    }

    pub fn u32(&mut self) -> Option<u32> {
        self.array().map(u32::from_be_bytes)
    }

    pub fn u64(&mut self) -> Option<u64> {
        self.array().map(u64::from_be_bytes)
    }

    pub fn ipv4(&mut self) -> Option<Ipv4Addr> {
        self.array::<4>().map(Ipv4Addr::from)
    }

    pub fn ipv6(&mut self) -> Option<Ipv6Addr> {
        self.array::<16>().map(Ipv6Addr::from)
    }

    fn array<const N: usize>(&mut self) -> Option<[u8; N]> {
        self.bytes(N).and_then(|b| b.try_into().ok())
    }
}

/// A big-endian unsigned number of 1 to 8 bytes, as template fields are
/// sent with reduced size encoding (RFC 7011, section 6.2).
pub fn unsigned(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() || bytes.len() > 8 {
        return None;
    }
    Some(bytes.iter().fold(0u64, |n, b| (n << 8) | u64::from(*b)))
}

/// An address of 4 or 16 bytes.
pub fn address(bytes: &[u8]) -> Option<IpAddr> {
    match bytes.len() {
        4 => <[u8; 4]>::try_from(bytes).ok().map(IpAddr::from),
        16 => <[u8; 16]>::try_from(bytes).ok().map(IpAddr::from),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_stop_at_the_end() {
        let mut r = Reader::new(&[0, 1, 2]);
        assert_eq!(r.u16(), Some(1));
        assert_eq!(r.u16(), None, "one byte left");
        assert_eq!(r.u8(), Some(2));
        assert!(r.is_empty());
        assert_eq!(r.skip(1), None);
    }

    #[test]
    fn numbers_of_any_size() {
        assert_eq!(unsigned(&[1, 0]), Some(256));
        assert_eq!(unsigned(&[0xff; 8]), Some(u64::MAX));
        assert_eq!(unsigned(&[]), None);
        assert_eq!(unsigned(&[0; 9]), None);
        assert_eq!(address(&[192, 0, 2, 1]), Some("192.0.2.1".parse().unwrap()));
        assert_eq!(address(&[1, 2, 3]), None);
    }
}
