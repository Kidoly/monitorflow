//! NetFlow v5: a 24-byte header and up to 30 fixed records of 48 bytes,
//! IPv4 only. The header's sampling interval multiplies every record.

use std::net::IpAddr;

use crate::read::Reader;
use crate::{scaled, Flow};

const HEADER: usize = 24;
const RECORD: usize = 48;
/// More records than a v5 datagram may hold is a lie.
const MAX_RECORDS: u16 = 30;

/// Appends the records of `data` to `out`; `None` when the datagram is
/// malformed, after the records that were whole.
pub fn decode(data: &[u8], out: &mut Vec<Flow>) -> Option<()> {
    let mut r = Reader::new(data);
    let mut header = r.sub(HEADER)?;
    header.skip(2)?;
    let count = header.u16()?;
    // Uptime, time, sequence, engine type and ID.
    header.skip(18)?;
    // Top 2 bits: the sampling mode; low 14: the interval.
    let interval = u64::from(header.u16()? & 0x3fff);
    if count > MAX_RECORDS {
        return None;
    }
    for _ in 0..count {
        let mut rec = r.sub(RECORD)?;
        let src = rec.ipv4()?;
        let dst = rec.ipv4()?;
        rec.skip(4)?;
        let input = rec.u16()?;
        let output = rec.u16()?;
        let packets = rec.u32()?;
        let bytes = rec.u32()?;
        rec.skip(8)?;
        let src_port = rec.u16()?;
        let dst_port = rec.u16()?;
        rec.skip(2)?;
        let protocol = rec.u8()?;
        out.push(Flow {
            src: IpAddr::V4(src),
            dst: IpAddr::V4(dst),
            protocol,
            src_port,
            dst_port,
            input: u32::from(input),
            output: u32::from(output),
            bytes: scaled(u64::from(bytes), interval),
            packets: scaled(u64::from(packets), interval),
        });
    }
    Some(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// One record: src, dst, proto, sport, dport, bytes, packets.
    pub type Record = ([u8; 4], [u8; 4], u8, u16, u16, u32, u32);

    /// A v5 datagram of `records`, sampled 1 in `interval`.
    pub fn datagram(records: &[Record], interval: u16) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend(5u16.to_be_bytes());
        d.extend((records.len() as u16).to_be_bytes());
        d.extend([0u8; 18]);
        d.extend((0x4000 | interval).to_be_bytes());
        for (src, dst, proto, sport, dport, bytes, packets) in records {
            d.extend(src);
            d.extend(dst);
            d.extend([0u8; 4]);
            d.extend(3u16.to_be_bytes());
            d.extend(7u16.to_be_bytes());
            d.extend(packets.to_be_bytes());
            d.extend(bytes.to_be_bytes());
            d.extend([0u8; 8]);
            d.extend(sport.to_be_bytes());
            d.extend(dport.to_be_bytes());
            d.extend([0u8, 0x18, *proto, 0]);
            d.extend([0u8; 8]);
        }
        d
    }

    #[test]
    fn records_with_sampling_undone() {
        let d = datagram(
            &[([192, 0, 2, 1], [198, 51, 100, 7], 6, 51000, 443, 1500, 3)],
            10,
        );
        let mut flows = Vec::new();
        assert_eq!(decode(&d, &mut flows), Some(()));
        assert_eq!(
            flows,
            vec![Flow {
                src: "192.0.2.1".parse().unwrap(),
                dst: "198.51.100.7".parse().unwrap(),
                protocol: 6,
                src_port: 51000,
                dst_port: 443,
                input: 3,
                output: 7,
                bytes: 15_000,
                packets: 30,
            }]
        );
    }

    #[test]
    fn a_short_datagram_keeps_whole_records() {
        let one = ([192, 0, 2, 1], [192, 0, 2, 2], 17, 53, 5353, 100, 1);
        let mut d = datagram(&[one, one], 0);
        d.truncate(HEADER + RECORD + 10);
        let mut flows = Vec::new();
        assert_eq!(decode(&d, &mut flows), None);
        assert_eq!(flows.len(), 1);
        assert_eq!(flows[0].bytes, 100, "no sampling: as sent");
    }
}
