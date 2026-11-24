//! Traffic flows (ADR-0184). A probe (the central server or a relay)
//! receives NetFlow v5 and v9, IPFIX and sFlow v5 datagrams over UDP from
//! the devices it was told about, decodes them into [`Flow`]s, and adds
//! them up per exporter and window ([`aggregate`]). Only those sums leave
//! the probe: the largest addresses, applications, protocols and
//! interfaces of each window, and its largest conversations.
//!
//! Decoding never panics and never trusts a length: every read is
//! bounded by the datagram ([`read::Reader`]), templates are bounded per
//! exporter, and a malformed part ends the datagram's decoding with what
//! was read so far. The tests feed each decoder mutated and random
//! datagrams to hold that.

use std::net::IpAddr;

pub mod aggregate;
pub mod apps;
pub mod collector;
mod netflow5;
mod packet;
pub mod read;
mod sflow;
mod template;

#[cfg(test)]
mod fuzz_tests;

pub use template::Templates;

/// Default UDP ports: NetFlow v5 and v9, sFlow, IPFIX.
pub const NETFLOW_PORT: u16 = 2055;
pub const SFLOW_PORT: u16 = 6343;
pub const IPFIX_PORT: u16 = 4739;

/// One flow, or one sampled packet, with sampling undone: `bytes` and
/// `packets` are what it stands for on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flow {
    pub src: IpAddr,
    pub dst: IpAddr,
    /// IANA protocol number: 6 TCP, 17 UDP, 1 ICMP.
    pub protocol: u8,
    pub src_port: u16,
    pub dst_port: u16,
    /// The exporter's ifIndex of the interface it came in and went out
    /// by; 0 when not said.
    pub input: u32,
    pub output: u32,
    pub bytes: u64,
    pub packets: u64,
}

/// What one datagram held.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Datagram {
    pub format: Option<Format>,
    /// The agent address an sFlow datagram names, which may differ from
    /// the address it was sent from.
    pub agent: Option<IpAddr>,
    pub flows: Vec<Flow>,
    /// Whether decoding stopped early on something malformed.
    pub malformed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    NetflowV5,
    NetflowV9,
    Ipfix,
    Sflow,
}

/// Decodes one datagram from `exporter`, whatever port it came on: the
/// version at its start says the format. Templates of NetFlow v9 and
/// IPFIX are kept in `templates`, per exporter.
pub fn decode(data: &[u8], exporter: IpAddr, templates: &mut Templates) -> Datagram {
    let mut out = Datagram::default();
    let version = data.get(..2).map(|v| u16::from_be_bytes([v[0], v[1]]));
    match version {
        Some(5) => {
            out.format = Some(Format::NetflowV5);
            out.malformed = netflow5::decode(data, &mut out.flows).is_none();
        }
        Some(9) => {
            out.format = Some(Format::NetflowV9);
            out.malformed = templates.netflow9(exporter, data, &mut out.flows).is_none();
        }
        Some(10) => {
            out.format = Some(Format::Ipfix);
            out.malformed = templates.ipfix(exporter, data, &mut out.flows).is_none();
        }
        Some(0) if data.get(..4) == Some(&[0, 0, 0, 5]) => {
            out.format = Some(Format::Sflow);
            out.malformed = sflow::decode(data, &mut out.agent, &mut out.flows).is_none();
        }
        _ => out.malformed = true,
    }
    out
}

/// Multiplies a count by a sampling rate, saturating: a lying rate must
/// not wrap a total around.
pub(crate) fn scaled(count: u64, rate: u64) -> u64 {
    count.saturating_mul(rate.max(1))
}
