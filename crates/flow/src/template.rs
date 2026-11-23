//! NetFlow v9 (RFC 3954) and IPFIX (RFC 7011): exporters first send
//! templates that say which fields a record holds and how long each is,
//! then data records that only make sense with them. Templates are kept
//! per exporter address, observation domain (v9's source ID) and template
//! ID, and a NetFlow v9 template never decodes an IPFIX record.
//!
//! Options records that say an exporter samples (samplingInterval,
//! samplingPacketInterval, samplerRandomInterval) set its rate, which
//! multiplies its records that do not carry their own.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;

use crate::read::{address, unsigned, Reader};
use crate::{scaled, Flow};

/// Templates kept in all; more are dropped until some are forgotten.
pub const MAX_TEMPLATES: usize = 16_384;
/// Fields of one template: more is not a real exporter.
const MAX_FIELDS: u16 = 512;
/// IPFIX's variable length marker (RFC 7011, section 7).
const VARIABLE: u16 = 0xffff;

// Information elements read; NetFlow v9 field types share the numbers.
const OCTETS: u16 = 1;
const PACKETS: u16 = 2;
const PROTOCOL: u16 = 4;
const SRC_PORT: u16 = 7;
const SRC_V4: u16 = 8;
const INPUT: u16 = 10;
const DST_PORT: u16 = 11;
const DST_V4: u16 = 12;
const OUTPUT: u16 = 14;
const POST_OCTETS: u16 = 23;
const POST_PACKETS: u16 = 24;
const SRC_V6: u16 = 27;
const DST_V6: u16 = 28;
const SAMPLING_INTERVAL: u16 = 34;
const SAMPLER_RANDOM_INTERVAL: u16 = 50;
const TOTAL_OCTETS: u16 = 85;
const TOTAL_PACKETS: u16 = 86;
const SAMPLING_PACKET_INTERVAL: u16 = 305;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Protocol {
    V9,
    Ipfix,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    exporter: IpAddr,
    domain: u32,
    id: u16,
    protocol: Protocol,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Field {
    /// 0 for a v9 options scope, never read.
    id: u16,
    /// An enterprise-specific element, never read.
    enterprise: bool,
    length: u16,
}

#[derive(Debug)]
struct Template {
    fields: Vec<Field>,
    options: bool,
    /// Bytes of a record without variable-length fields' contents.
    min_length: usize,
}

impl Template {
    fn new(fields: Vec<Field>, options: bool) -> Self {
        let min_length = fields
            .iter()
            .map(|f| {
                if f.length == VARIABLE {
                    1
                } else {
                    usize::from(f.length)
                }
            })
            .sum();
        Self {
            fields,
            options,
            min_length,
        }
    }
}

/// The templates and sampling rates exporters announced.
#[derive(Debug, Default)]
pub struct Templates {
    templates: HashMap<Key, Arc<Template>>,
    /// Sampling rate per exporter and domain, from options records.
    sampling: HashMap<(IpAddr, u32), u64>,
    /// Templates dropped because [`MAX_TEMPLATES`] were kept.
    pub dropped: u64,
}

impl Templates {
    pub fn len(&self) -> usize {
        self.templates.len()
    }

    pub fn is_empty(&self) -> bool {
        self.templates.is_empty()
    }

    /// Drops what every exporter not in `keep` announced.
    pub fn retain(&mut self, keep: impl Fn(&IpAddr) -> bool) {
        self.templates.retain(|k, _| keep(&k.exporter));
        self.sampling.retain(|(e, _), _| keep(e));
    }

    fn insert(&mut self, key: Key, template: Template) {
        if self.templates.len() >= MAX_TEMPLATES && !self.templates.contains_key(&key) {
            self.dropped += 1;
            return;
        }
        self.templates.insert(key, Arc::new(template));
    }

    /// A NetFlow v9 datagram; `None` when it is malformed, after what
    /// could be read.
    pub(crate) fn netflow9(
        &mut self,
        exporter: IpAddr,
        data: &[u8],
        out: &mut Vec<Flow>,
    ) -> Option<()> {
        let mut r = Reader::new(data);
        let mut header = r.sub(20)?;
        header.skip(16)?;
        let domain = header.u32()?;
        self.sets(exporter, domain, Protocol::V9, r, out)
    }

    /// An IPFIX message; `None` when it is malformed.
    pub(crate) fn ipfix(
        &mut self,
        exporter: IpAddr,
        data: &[u8],
        out: &mut Vec<Flow>,
    ) -> Option<()> {
        let mut r = Reader::new(data);
        let mut header = r.sub(16)?;
        header.skip(2)?;
        let length = usize::from(header.u16()?);
        header.skip(8)?;
        let domain = header.u32()?;
        // The message's own length bounds its sets; bytes past it are
        // not part of it.
        let sets = r.sub(length.checked_sub(16)?)?;
        self.sets(exporter, domain, Protocol::Ipfix, sets, out)
    }

    fn sets(
        &mut self,
        exporter: IpAddr,
        domain: u32,
        protocol: Protocol,
        mut r: Reader<'_>,
        out: &mut Vec<Flow>,
    ) -> Option<()> {
        let (templates, options) = match protocol {
            Protocol::V9 => (0, 1),
            Protocol::Ipfix => (2, 3),
        };
        // v9 datagrams may end with padding shorter than a set header.
        while r.remaining() >= 4 {
            let id = r.u16()?;
            let length = usize::from(r.u16()?);
            let body = r.sub(length.checked_sub(4)?)?;
            let key = |id| Key {
                exporter,
                domain,
                id,
                protocol,
            };
            if id == templates {
                self.template_set(body, protocol, &key)?;
            } else if id == options {
                self.options_set(body, protocol, &key)?;
            } else if id >= 256 {
                // Data before its template is not an error: the template
                // comes again, and later records decode.
                if let Some(template) = self.templates.get(&key(id)).cloned() {
                    self.data_set(body, &template, exporter, domain, out);
                }
            }
        }
        Some(())
    }

    fn template_set(
        &mut self,
        mut body: Reader<'_>,
        protocol: Protocol,
        key: &impl Fn(u16) -> Key,
    ) -> Option<()> {
        while body.remaining() >= 4 {
            let id = body.u16()?;
            let count = body.u16()?;
            if id == 0 && count == 0 {
                // Padding to the end of the set.
                break;
            }
            if id < 256 || count > MAX_FIELDS {
                return None;
            }
            if count == 0 {
                // IPFIX withdraws a template with no fields.
                self.templates.remove(&key(id));
                continue;
            }
            let fields = fields(&mut body, count, protocol)?;
            self.insert(key(id), Template::new(fields, false));
        }
        Some(())
    }

    fn options_set(
        &mut self,
        mut body: Reader<'_>,
        protocol: Protocol,
        key: &impl Fn(u16) -> Key,
    ) -> Option<()> {
        while body.remaining() >= 6 {
            let id = body.u16()?;
            if id == 0 {
                break;
            }
            if id < 256 {
                return None;
            }
            let fields = match protocol {
                Protocol::V9 => {
                    let scope_length = body.u16()?;
                    let option_length = body.u16()?;
                    if scope_length % 4 != 0 || option_length % 4 != 0 {
                        return None;
                    }
                    let scope = scope_length / 4;
                    let count = scope.checked_add(option_length / 4)?;
                    if count == 0 || count > MAX_FIELDS {
                        return None;
                    }
                    let mut all = fields(&mut body, count, protocol)?;
                    // v9 scope types (system, interface...) are not
                    // information elements.
                    for field in all.iter_mut().take(usize::from(scope)) {
                        field.id = 0;
                    }
                    all
                }
                Protocol::Ipfix => {
                    let count = body.u16()?;
                    let scope = body.u16()?;
                    if count == 0 {
                        self.templates.remove(&key(id));
                        continue;
                    }
                    if count > MAX_FIELDS || scope == 0 || scope > count {
                        return None;
                    }
                    fields(&mut body, count, protocol)?
                }
            };
            self.insert(key(id), Template::new(fields, true));
        }
        Some(())
    }

    fn data_set(
        &mut self,
        mut body: Reader<'_>,
        template: &Template,
        exporter: IpAddr,
        domain: u32,
        out: &mut Vec<Flow>,
    ) {
        // A template of empty fields would read records of nothing forever.
        if template.min_length == 0 {
            return;
        }
        while body.remaining() >= template.min_length {
            let Some(record) = record(&mut body, template) else {
                // A record cut short: the rest is padding or garbage.
                return;
            };
            if template.options {
                if let Some(rate) = record.sampling.filter(|r| *r > 0) {
                    if self.sampling.len() < MAX_TEMPLATES
                        || self.sampling.contains_key(&(exporter, domain))
                    {
                        self.sampling.insert((exporter, domain), rate);
                    }
                }
                continue;
            }
            let rate = record
                .sampling
                .or_else(|| self.sampling.get(&(exporter, domain)).copied())
                .unwrap_or(1);
            if let Some(flow) = record.flow(rate) {
                out.push(flow);
            }
        }
    }
}

fn fields(body: &mut Reader<'_>, count: u16, protocol: Protocol) -> Option<Vec<Field>> {
    let mut fields = Vec::with_capacity(usize::from(count));
    for _ in 0..count {
        let raw = body.u16()?;
        let length = body.u16()?;
        let enterprise = protocol == Protocol::Ipfix && raw & 0x8000 != 0;
        if enterprise {
            body.skip(4)?;
        }
        if length == VARIABLE && protocol == Protocol::V9 {
            return None;
        }
        fields.push(Field {
            id: if enterprise { raw & 0x7fff } else { raw },
            enterprise,
            length,
        });
    }
    Some(fields)
}

/// The fields of one record that matter.
#[derive(Debug, Default)]
struct Record {
    src: Option<IpAddr>,
    dst: Option<IpAddr>,
    protocol: u8,
    src_port: u16,
    dst_port: u16,
    input: u32,
    output: u32,
    octets: Option<u64>,
    packets: Option<u64>,
    post_octets: Option<u64>,
    post_packets: Option<u64>,
    total_octets: Option<u64>,
    total_packets: Option<u64>,
    sampling: Option<u64>,
}

impl Record {
    fn flow(&self, rate: u64) -> Option<Flow> {
        let bytes = self.octets.or(self.post_octets).or(self.total_octets)?;
        let packets = self
            .packets
            .or(self.post_packets)
            .or(self.total_packets)
            .unwrap_or(0);
        Some(Flow {
            src: self.src?,
            dst: self.dst?,
            protocol: self.protocol,
            src_port: self.src_port,
            dst_port: self.dst_port,
            input: self.input,
            output: self.output,
            bytes: scaled(bytes, rate),
            packets: scaled(packets, rate),
        })
    }
}

fn record(body: &mut Reader<'_>, template: &Template) -> Option<Record> {
    let mut record = Record::default();
    for field in &template.fields {
        let length = if field.length == VARIABLE {
            match body.u8()? {
                255 => usize::from(body.u16()?),
                n => usize::from(n),
            }
        } else {
            usize::from(field.length)
        };
        let value = body.bytes(length)?;
        if field.enterprise {
            continue;
        }
        let number = || unsigned(value);
        let small = |n: Option<u64>| n.and_then(|n| u32::try_from(n).ok());
        match field.id {
            OCTETS => record.octets = number(),
            PACKETS => record.packets = number(),
            POST_OCTETS => record.post_octets = number(),
            POST_PACKETS => record.post_packets = number(),
            TOTAL_OCTETS => record.total_octets = number(),
            TOTAL_PACKETS => record.total_packets = number(),
            PROTOCOL => record.protocol = number().and_then(|n| u8::try_from(n).ok()).unwrap_or(0),
            SRC_PORT => record.src_port = number().and_then(|n| u16::try_from(n).ok()).unwrap_or(0),
            DST_PORT => record.dst_port = number().and_then(|n| u16::try_from(n).ok()).unwrap_or(0),
            INPUT => record.input = small(number()).unwrap_or(0),
            OUTPUT => record.output = small(number()).unwrap_or(0),
            SRC_V4 | SRC_V6 => record.src = address(value).or(record.src),
            DST_V4 | DST_V6 => record.dst = address(value).or(record.dst),
            SAMPLING_INTERVAL | SAMPLING_PACKET_INTERVAL | SAMPLER_RANDOM_INTERVAL => {
                record.sampling = number().filter(|n| *n > 0).or(record.sampling)
            }
            _ => {}
        }
    }
    Some(record)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub const EXPORTER: &str = "192.0.2.254";

    /// Fields of the test template: src, dst, protocol, ports, in, out,
    /// octets, packets.
    pub const FIELDS: [(u16, u16); 9] = [
        (SRC_V4, 4),
        (DST_V4, 4),
        (PROTOCOL, 1),
        (SRC_PORT, 2),
        (DST_PORT, 2),
        (INPUT, 2),
        (OUTPUT, 2),
        (OCTETS, 4),
        (PACKETS, 4),
    ];

    pub fn set(id: u16, body: &[u8]) -> Vec<u8> {
        let mut s = id.to_be_bytes().to_vec();
        s.extend(((body.len() + 4) as u16).to_be_bytes());
        s.extend(body);
        s
    }

    pub fn template(id: u16, fields: &[(u16, u16)]) -> Vec<u8> {
        let mut t = id.to_be_bytes().to_vec();
        t.extend((fields.len() as u16).to_be_bytes());
        for (f, l) in fields {
            t.extend(f.to_be_bytes());
            t.extend(l.to_be_bytes());
        }
        t
    }

    pub fn record(src: [u8; 4], dst: [u8; 4], port: u16, octets: u32, packets: u32) -> Vec<u8> {
        let mut r = Vec::new();
        r.extend(src);
        r.extend(dst);
        r.push(6);
        r.extend(50_000u16.to_be_bytes());
        r.extend(port.to_be_bytes());
        r.extend(1u16.to_be_bytes());
        r.extend(2u16.to_be_bytes());
        r.extend(octets.to_be_bytes());
        r.extend(packets.to_be_bytes());
        r
    }

    pub fn v9(sets: &[Vec<u8>]) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend(9u16.to_be_bytes());
        d.extend((sets.len() as u16).to_be_bytes());
        d.extend([0u8; 12]);
        d.extend(42u32.to_be_bytes());
        for s in sets {
            d.extend(s);
        }
        d
    }

    pub fn ipfix(sets: &[Vec<u8>]) -> Vec<u8> {
        let body: Vec<u8> = sets.concat();
        let mut d = Vec::new();
        d.extend(10u16.to_be_bytes());
        d.extend(((body.len() + 16) as u16).to_be_bytes());
        d.extend([0u8; 8]);
        d.extend(7u32.to_be_bytes());
        d.extend(body);
        d
    }

    fn exporter() -> IpAddr {
        EXPORTER.parse().unwrap()
    }

    #[test]
    fn v9_data_waits_for_its_template() {
        let mut t = Templates::default();
        let data = set(
            300,
            &record([192, 0, 2, 1], [198, 51, 100, 2], 443, 1000, 2),
        );
        let mut flows = Vec::new();
        assert_eq!(
            t.netflow9(exporter(), &v9(std::slice::from_ref(&data)), &mut flows),
            Some(())
        );
        assert!(flows.is_empty(), "no template yet");
        let tpl = set(0, &template(300, &FIELDS));
        // Padding after the last set is allowed.
        let mut d = v9(&[tpl, data]);
        d.extend([0, 0]);
        assert_eq!(t.netflow9(exporter(), &d, &mut flows), Some(()));
        assert_eq!(flows.len(), 1);
        let f = flows[0];
        assert_eq!(f.src, "192.0.2.1".parse::<IpAddr>().unwrap());
        assert_eq!((f.protocol, f.src_port, f.dst_port), (6, 50_000, 443));
        assert_eq!((f.input, f.output, f.bytes, f.packets), (1, 2, 1000, 2));
        // Another exporter's records do not use it.
        let mut other = Vec::new();
        let d = v9(&[set(300, &record([192, 0, 2, 1], [192, 0, 2, 2], 22, 10, 1))]);
        t.netflow9("192.0.2.253".parse().unwrap(), &d, &mut other);
        assert!(other.is_empty());
    }

    #[test]
    fn v9_options_set_the_sampling_rate() {
        let mut t = Templates::default();
        // Options template 260: scope "system" (4 bytes), samplingInterval.
        let mut options = 260u16.to_be_bytes().to_vec();
        options.extend(4u16.to_be_bytes());
        options.extend(4u16.to_be_bytes());
        options.extend([0, 1, 0, 4]);
        options.extend(SAMPLING_INTERVAL.to_be_bytes());
        options.extend(4u16.to_be_bytes());
        let mut rate = vec![0, 0, 0, 0];
        rate.extend(100u32.to_be_bytes());
        let d = v9(&[
            set(1, &options),
            set(260, &rate),
            set(0, &template(300, &FIELDS)),
            set(300, &record([192, 0, 2, 1], [192, 0, 2, 2], 80, 500, 1)),
        ]);
        let mut flows = Vec::new();
        assert_eq!(t.netflow9(exporter(), &d, &mut flows), Some(()));
        assert_eq!((flows[0].bytes, flows[0].packets), (50_000, 100));
    }

    #[test]
    fn ipfix_with_ipv6_variable_and_enterprise_fields() {
        let mut t = Templates::default();
        let mut tpl = 400u16.to_be_bytes().to_vec();
        tpl.extend(5u16.to_be_bytes());
        for (f, l) in [(SRC_V6, 16u16), (DST_V6, 16), (OCTETS, 8)] {
            tpl.extend(f.to_be_bytes());
            tpl.extend(l.to_be_bytes());
        }
        // An enterprise field of variable length, then a sampling rate.
        tpl.extend((0x8000u16 | 12).to_be_bytes());
        tpl.extend(VARIABLE.to_be_bytes());
        tpl.extend(9u32.to_be_bytes());
        tpl.extend(SAMPLING_PACKET_INTERVAL.to_be_bytes());
        tpl.extend(2u16.to_be_bytes());
        let mut rec = Vec::new();
        rec.extend(
            "2001:db8::1"
                .parse::<std::net::Ipv6Addr>()
                .unwrap()
                .octets(),
        );
        rec.extend(
            "2001:db8::2"
                .parse::<std::net::Ipv6Addr>()
                .unwrap()
                .octets(),
        );
        rec.extend(1234u64.to_be_bytes());
        rec.extend([3, b'a', b'b', b'c']);
        rec.extend(4u16.to_be_bytes());
        let d = ipfix(&[set(2, &tpl), set(400, &rec)]);
        let mut flows = Vec::new();
        assert_eq!(t.ipfix(exporter(), &d, &mut flows), Some(()));
        assert_eq!(flows.len(), 1);
        assert_eq!(flows[0].dst, "2001:db8::2".parse::<IpAddr>().unwrap());
        assert_eq!(flows[0].bytes, 1234 * 4);
        assert_eq!(flows[0].packets, 0, "not said");
        // Withdrawn: the next records are not decoded.
        let withdraw = ipfix(&[set(2, &[1, 144, 0, 0])]);
        assert_eq!(t.ipfix(exporter(), &withdraw, &mut flows), Some(()));
        assert!(t.is_empty());
    }

    #[test]
    fn templates_are_bounded_and_forgotten() {
        let mut t = Templates::default();
        for i in 0..(MAX_TEMPLATES + 10) {
            let key = Key {
                exporter: exporter(),
                domain: i as u32,
                id: 256,
                protocol: Protocol::V9,
            };
            t.insert(key, Template::new(vec![], false));
        }
        assert_eq!(t.len(), MAX_TEMPLATES);
        assert_eq!(t.dropped, 10);
        t.retain(|_| false);
        assert!(t.is_empty());
    }

    #[test]
    fn a_template_of_empty_fields_decodes_nothing() {
        let mut t = Templates::default();
        let d = v9(&[set(0, &template(300, &[(OCTETS, 0)])), set(300, &[0; 64])]);
        let mut flows = Vec::new();
        assert_eq!(t.netflow9(exporter(), &d, &mut flows), Some(()));
        assert!(flows.is_empty());
    }
}
