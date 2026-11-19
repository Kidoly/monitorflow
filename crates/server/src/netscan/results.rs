//! What a probe answered to one chunk of a scan, believed only as far as
//! it was asked (ADR-0154): an address outside the chunk, a port that was
//! not tried, a name or text that does not fit are dropped and counted.

use std::collections::BTreeSet;
use std::net::IpAddr;

use monitorflow_common::proto::v1::Sample;
use monitorflow_snmp::Group;

use crate::snmp;

/// The metric of each address that answered.
pub const HOST: &str = "discover_host";
/// The summary of a test.
pub const SCANNED: &str = "discover_scanned";

/// One address that answered, as stored.
#[derive(Debug, Clone, PartialEq)]
pub struct Host {
    pub address: IpAddr,
    pub ping: bool,
    pub rtt_seconds: Option<f64>,
    pub ports: Vec<u16>,
    pub name: Option<String>,
    pub mac: Option<String>,
    /// The set of credentials that answered SNMP, from 1.
    pub snmp_set: Option<usize>,
    pub snmp_descr: Option<String>,
    pub snmp_name: Option<String>,
    pub snmp_object_id: Option<String>,
    pub snmp_groups: Vec<String>,
    /// The template detection suggests.
    pub template: Option<String>,
}

impl Host {
    pub fn new(address: IpAddr) -> Self {
        Self {
            address,
            ping: false,
            rtt_seconds: None,
            ports: Vec::new(),
            name: None,
            mac: None,
            snmp_set: None,
            snmp_descr: None,
            snmp_name: None,
            snmp_object_id: None,
            snmp_groups: Vec::new(),
            template: None,
        }
    }
}

/// What one test said.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Chunk {
    pub hosts: Vec<Host>,
    /// Samples or fields not believed.
    pub dropped: u32,
    pub ping_error: Option<String>,
}

/// Text from the network as shown: control characters out, at most
/// `max` characters, `None` when nothing is left.
fn text(value: Option<&String>, max: usize) -> Option<String> {
    let cleaned: String = value?
        .chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect();
    let cleaned = cleaned.trim().to_owned();
    (!cleaned.is_empty()).then_some(cleaned)
}

fn name(value: &str) -> Option<String> {
    let name = value.trim_end_matches('.').to_ascii_lowercase();
    (!name.is_empty()
        && name.len() <= 253
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.'))
    .then_some(name)
}

fn mac(value: &str) -> Option<String> {
    let mac = value.to_ascii_lowercase();
    let parts: Vec<&str> = mac.split(':').collect();
    (parts.len() == 6
        && parts
            .iter()
            .all(|p| p.len() == 2 && p.bytes().all(|b| b.is_ascii_hexdigit())))
    .then_some(mac)
}

fn object_id(value: &str) -> Option<String> {
    let arcs: Vec<&str> = value.trim_start_matches('.').split('.').collect();
    (!value.is_empty()
        && arcs.len() <= 128
        && arcs
            .iter()
            .all(|a| !a.is_empty() && a.len() <= 10 && a.bytes().all(|b| b.is_ascii_digit())))
    .then(|| arcs.join("."))
}

/// Reads the samples of a test of `asked` addresses on `ports`, with
/// `sets` sets of SNMP credentials.
pub fn read(samples: &[Sample], asked: &[IpAddr], ports: &[u16], sets: usize) -> Chunk {
    let asked: BTreeSet<IpAddr> = asked.iter().copied().collect();
    let ports: BTreeSet<u16> = ports.iter().copied().collect();
    let mut chunk = Chunk::default();
    let mut seen = BTreeSet::new();
    for sample in samples {
        if sample.sensor == SCANNED {
            chunk.ping_error = text(sample.labels.get("ping_error"), 512);
            continue;
        }
        if sample.sensor != HOST {
            chunk.dropped += 1;
            continue;
        }
        let Some(address) = sample
            .instance
            .parse::<IpAddr>()
            .ok()
            .filter(|a| asked.contains(a) && seen.insert(*a))
        else {
            chunk.dropped += 1;
            continue;
        };
        let label = |k: &str| sample.labels.get(k);
        let mut host = Host::new(address);
        host.ping = label("ping").map(String::as_str) == Some("1");
        host.rtt_seconds = label("rtt_seconds")
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v >= 0.0 && host.ping);
        if let Some(list) = label("ports").filter(|l| !l.is_empty()) {
            for port in list.split(',') {
                match port.parse::<u16>().ok().filter(|p| ports.contains(p)) {
                    Some(port) if !host.ports.contains(&port) => host.ports.push(port),
                    Some(_) => {}
                    None => chunk.dropped += 1,
                }
            }
            host.ports.sort_unstable();
        }
        host.name = label("name").and_then(|v| name(v));
        host.mac = label("mac").and_then(|v| mac(v));
        host.snmp_set = label("snmp")
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|n| (1..=sets).contains(n));
        if host.snmp_set.is_some() {
            host.snmp_descr = text(label("descr"), 512);
            host.snmp_name = text(label("sysname"), 255);
            host.snmp_object_id = label("object_id").and_then(|v| object_id(v));
            host.snmp_groups = label("groups")
                .map(|g| {
                    g.split(',')
                        .filter_map(Group::parse)
                        .filter(|g| !matches!(g, Group::System | Group::Detect))
                        .map(|g| g.as_str().to_owned())
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect()
                })
                .unwrap_or_default();
            let groups = host.snmp_groups.clone();
            host.template = Some(
                snmp::suggest(host.snmp_descr.as_deref().unwrap_or(""), |g| {
                    groups.iter().any(|x| x == g.as_str())
                })
                .to_owned(),
            );
        }
        if !host.ping && host.ports.is_empty() && host.snmp_set.is_none() {
            chunk.dropped += 1;
            continue;
        }
        chunk.hosts.push(host);
    }
    chunk.hosts.sort_by_key(|h| h.address);
    chunk
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(instance: &str, labels: &[(&str, &str)]) -> Sample {
        Sample {
            sensor: HOST.into(),
            instance: instance.into(),
            timestamp_unix_ms: 0,
            value: 1.0,
            labels: labels
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        }
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn answers_are_believed_as_far_as_asked() {
        let asked = [ip("192.0.2.1"), ip("192.0.2.2"), ip("192.0.2.3")];
        let samples = vec![
            host(
                "192.0.2.2",
                &[
                    ("ping", "1"),
                    ("rtt_seconds", "0.002"),
                    ("ports", "443,22,9999"),
                    ("name", "Web-1.LAN."),
                    ("mac", "02:42:AC:11:00:02"),
                ],
            ),
            host(
                "192.0.2.3",
                &[
                    ("ping", "0"),
                    ("snmp", "2"),
                    ("descr", "Smart-UPS\n1500"),
                    ("sysname", "ups-1"),
                    ("object_id", ".1.3.6.1.4.1.318"),
                    ("groups", "ups,bogus,system"),
                    ("name", "bad name"),
                    ("mac", "nope"),
                ],
            ),
            // Not asked, twice the same, nothing answered, another metric.
            host("198.51.100.1", &[("ping", "1")]),
            host("192.0.2.2", &[("ping", "1")]),
            host("192.0.2.1", &[("ping", "0"), ("snmp", "3")]),
            Sample {
                sensor: "other".into(),
                ..host("192.0.2.1", &[])
            },
            Sample {
                sensor: SCANNED.into(),
                ..host("", &[("ping_error", "cannot open an ICMP socket")])
            },
        ];
        let chunk = read(&samples, &asked, &[22, 443], 2);
        assert_eq!(chunk.dropped, 5);
        assert_eq!(
            chunk.ping_error.as_deref(),
            Some("cannot open an ICMP socket")
        );
        assert_eq!(chunk.hosts.len(), 2);
        let web = &chunk.hosts[0];
        assert!(web.ping);
        assert_eq!(web.rtt_seconds, Some(0.002));
        assert_eq!(web.ports, [22, 443]);
        assert_eq!(web.name.as_deref(), Some("web-1.lan"));
        assert_eq!(web.mac.as_deref(), Some("02:42:ac:11:00:02"));
        assert_eq!(web.template, None);
        let ups = &chunk.hosts[1];
        assert_eq!(ups.snmp_set, Some(2));
        assert_eq!(ups.snmp_descr.as_deref(), Some("Smart-UPS1500"));
        assert_eq!(ups.snmp_object_id.as_deref(), Some("1.3.6.1.4.1.318"));
        assert_eq!(ups.snmp_groups, ["ups"]);
        assert_eq!(ups.template.as_deref(), Some("ups"));
        assert_eq!((ups.name.clone(), ups.mac.clone()), (None, None));
    }
}
