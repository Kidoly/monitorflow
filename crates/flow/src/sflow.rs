//! sFlow v5 (sflow.org, "sFlow Version 5"): a datagram of samples from
//! one agent. Flow samples describe one packet in `sampling_rate`, so each
//! counts as `sampling_rate` packets of its frame length. Counter samples
//! are skipped: SNMP already reads interfaces (ADR-0149).

use std::net::IpAddr;

use crate::packet::{self, Headers};
use crate::read::Reader;
use crate::{scaled, Flow};

/// More samples than this in one datagram is a lie.
const MAX_SAMPLES: u32 = 1024;
const MAX_RECORDS: u32 = 64;

/// Appends one flow per flow sample of `data` to `out`, and sets `agent`;
/// `None` when the datagram is malformed.
pub fn decode(data: &[u8], agent: &mut Option<IpAddr>, out: &mut Vec<Flow>) -> Option<()> {
    let mut r = Reader::new(data);
    r.skip(4)?;
    *agent = Some(match r.u32()? {
        1 => IpAddr::V4(r.ipv4()?),
        2 => IpAddr::V6(r.ipv6()?),
        _ => return None,
    });
    // Sub-agent, sequence, uptime.
    r.skip(12)?;
    let samples = r.u32()?;
    if samples > MAX_SAMPLES {
        return None;
    }
    for _ in 0..samples {
        let format = r.u32()?;
        let length = usize::try_from(r.u32()?).ok()?;
        let body = r.sub(length)?;
        match format {
            1 => sample(body, false, out)?,
            3 => sample(body, true, out)?,
            // Counter samples, and enterprise formats.
            _ => {}
        }
    }
    Some(())
}

/// A flow sample, `expanded` (format 3) or compact (format 1).
fn sample(mut r: Reader<'_>, expanded: bool, out: &mut Vec<Flow>) -> Option<()> {
    let (rate, input, output) = if expanded {
        // Sequence, source ID type and index.
        r.skip(12)?;
        let rate = r.u32()?;
        // Sample pool, drops.
        r.skip(8)?;
        let input = interface(r.u32()?, r.u32()?);
        let output = interface(r.u32()?, r.u32()?);
        (rate, input, output)
    } else {
        r.skip(8)?;
        let rate = r.u32()?;
        r.skip(8)?;
        // Top 2 bits: the format; 0 is a single ifIndex.
        let compact = |v: u32| interface(v >> 30, v & 0x3fff_ffff);
        let input = compact(r.u32()?);
        let output = compact(r.u32()?);
        (rate, input, output)
    };
    let records = r.u32()?;
    if records > MAX_RECORDS {
        return None;
    }
    let mut found: Option<(Headers, u64)> = None;
    for _ in 0..records {
        let format = r.u32()?;
        let length = usize::try_from(r.u32()?).ok()?;
        let body = r.sub(length)?;
        if found.is_some() {
            continue;
        }
        found = match format {
            1 => raw_header(body),
            3 => sampled_ipv4(body),
            4 => sampled_ipv6(body),
            _ => None,
        };
    }
    if let Some((h, length)) = found {
        let rate = u64::from(rate);
        out.push(Flow {
            src: h.src,
            dst: h.dst,
            protocol: h.protocol,
            src_port: h.src_port,
            dst_port: h.dst_port,
            input,
            output,
            bytes: scaled(length, rate),
            packets: rate.max(1),
        });
    }
    Some(())
}

fn interface(format: u32, value: u32) -> u32 {
    if format == 0 {
        value
    } else {
        0
    }
}

/// Raw packet header record: the frame's length and its first bytes.
fn raw_header(mut r: Reader<'_>) -> Option<(Headers, u64)> {
    let protocol = r.u32()?;
    let frame_length = r.u32()?;
    r.skip(4)?;
    let length = usize::try_from(r.u32()?).ok()?;
    let header = r.bytes(length.min(r.remaining()))?;
    let headers = match protocol {
        1 => packet::ethernet(header),
        11 => packet::ipv4(Reader::new(header)),
        12 => packet::ipv6(Reader::new(header)),
        _ => None,
    }?;
    Some((headers, u64::from(frame_length)))
}

fn sampled_ipv4(mut r: Reader<'_>) -> Option<(Headers, u64)> {
    let length = r.u32()?;
    let protocol = u8::try_from(r.u32()?).ok()?;
    let src = IpAddr::V4(r.ipv4()?);
    let dst = IpAddr::V4(r.ipv4()?);
    let src_port = u16::try_from(r.u32()?).unwrap_or(0);
    let dst_port = u16::try_from(r.u32()?).unwrap_or(0);
    Some((
        Headers {
            src,
            dst,
            protocol,
            src_port,
            dst_port,
        },
        u64::from(length),
    ))
}

fn sampled_ipv6(mut r: Reader<'_>) -> Option<(Headers, u64)> {
    let length = r.u32()?;
    let protocol = u8::try_from(r.u32()?).ok()?;
    let src = IpAddr::V6(r.ipv6()?);
    let dst = IpAddr::V6(r.ipv6()?);
    let src_port = u16::try_from(r.u32()?).unwrap_or(0);
    let dst_port = u16::try_from(r.u32()?).unwrap_or(0);
    Some((
        Headers {
            src,
            dst,
            protocol,
            src_port,
            dst_port,
        },
        u64::from(length),
    ))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::packet::tests::frame;

    fn padded(mut b: Vec<u8>) -> Vec<u8> {
        while !b.len().is_multiple_of(4) {
            b.push(0);
        }
        b
    }

    fn tlv(format: u32, body: &[u8]) -> Vec<u8> {
        let mut v = format.to_be_bytes().to_vec();
        v.extend((body.len() as u32).to_be_bytes());
        v.extend(body);
        v
    }

    /// A compact flow sample of one raw Ethernet header.
    pub fn flow_sample(rate: u32, frame_length: u32, header: &[u8]) -> Vec<u8> {
        let mut raw = Vec::new();
        raw.extend(1u32.to_be_bytes());
        raw.extend(frame_length.to_be_bytes());
        raw.extend(4u32.to_be_bytes());
        raw.extend((header.len() as u32).to_be_bytes());
        raw.extend(padded(header.to_vec()));
        let mut s = Vec::new();
        s.extend(1u32.to_be_bytes());
        s.extend(5u32.to_be_bytes());
        s.extend(rate.to_be_bytes());
        s.extend([0u8; 8]);
        s.extend(4u32.to_be_bytes());
        s.extend(9u32.to_be_bytes());
        s.extend(1u32.to_be_bytes());
        s.extend(tlv(1, &raw));
        tlv(1, &s)
    }

    pub fn datagram(agent: [u8; 4], samples: &[Vec<u8>]) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend(5u32.to_be_bytes());
        d.extend(1u32.to_be_bytes());
        d.extend(agent);
        d.extend([0u8; 12]);
        d.extend((samples.len() as u32).to_be_bytes());
        for s in samples {
            d.extend(s);
        }
        d
    }

    #[test]
    fn flow_samples_count_rate_times_the_frame() {
        let f = frame([192, 0, 2, 1], [198, 51, 100, 1], 6, 50_000, 443);
        // A counter sample, skipped, then a flow sample.
        let counters = tlv(2, &[0u8; 12]);
        let d = datagram([192, 0, 2, 254], &[counters, flow_sample(512, 1514, &f)]);
        let (mut agent, mut flows) = (None, Vec::new());
        assert_eq!(decode(&d, &mut agent, &mut flows), Some(()));
        assert_eq!(agent, Some("192.0.2.254".parse().unwrap()));
        assert_eq!(flows.len(), 1);
        let f = flows[0];
        assert_eq!((f.bytes, f.packets), (1514 * 512, 512));
        assert_eq!((f.input, f.output, f.dst_port), (4, 9, 443));
    }

    #[test]
    fn expanded_samples_and_sampled_ipv4() {
        let mut ip = Vec::new();
        ip.extend(100u32.to_be_bytes());
        ip.extend(17u32.to_be_bytes());
        ip.extend([203, 0, 113, 1, 203, 0, 113, 2]);
        ip.extend(123u32.to_be_bytes());
        ip.extend(123u32.to_be_bytes());
        ip.extend([0u8; 8]);
        let mut s = vec![0u8; 12];
        s.extend(10u32.to_be_bytes());
        s.extend([0u8; 8]);
        s.extend([0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 1, 0, 0, 0, 2]);
        s.extend(1u32.to_be_bytes());
        s.extend(tlv(3, &ip));
        let d = datagram([192, 0, 2, 254], &[tlv(3, &s)]);
        let (mut agent, mut flows) = (None, Vec::new());
        assert_eq!(decode(&d, &mut agent, &mut flows), Some(()));
        assert_eq!(
            (flows[0].input, flows[0].output),
            (7, 0),
            "output was not one interface"
        );
        assert_eq!((flows[0].bytes, flows[0].packets), (1000, 10));
    }
}
