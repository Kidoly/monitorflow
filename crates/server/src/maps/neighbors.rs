//! LLDP and CDP neighbours of an SNMP device (ADR-0185), read with the
//! walks of `snmp_read` (ADR-0157), so relays need nothing new.
//!
//! LLDP-MIB's remote table gives each neighbour's system name and port,
//! its management address table their addresses, and its local port
//! table the device's own ports; CISCO-CDP-MIB's cache gives the same per
//! interface index. The device's own port is named as its interfaces
//! check names it (ifName, else ifDescr, ADR-0149), so a link can show
//! that interface's traffic. Everything here is text from the network:
//! bounded, and shown as text.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// lldpRemTable entries.
pub const LLDP_REMOTE: &str = "1.0.8802.1.1.2.1.4.1.1";
/// lldpRemManAddrTable entries.
pub const LLDP_ADDRESSES: &str = "1.0.8802.1.1.2.1.4.2.1";
/// lldpLocPortTable entries.
pub const LLDP_LOCAL: &str = "1.0.8802.1.1.2.1.3.7.1";
/// cdpCacheTable entries.
pub const CDP: &str = "1.3.6.1.4.1.9.9.23.1.2.1.1";
/// ifName.
pub const IF_NAME: &str = "1.3.6.1.2.1.31.1.1.1.1";
/// ifDescr.
pub const IF_DESCR: &str = "1.3.6.1.2.1.2.2.1.2";

/// What one read walks, in order.
pub const WALKS: [&str; 6] = [
    LLDP_REMOTE,
    LLDP_ADDRESSES,
    LLDP_LOCAL,
    CDP,
    IF_NAME,
    IF_DESCR,
];

/// Most neighbours kept per device.
pub const MAX_NEIGHBORS: usize = 500;
/// Longest text kept.
const MAX_TEXT: usize = 256;

/// One neighbour of a device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Neighbor {
    /// `lldp` or `cdp`.
    pub protocol: &'static str,
    /// The device's own port, as its interfaces check names it.
    pub local_port: String,
    /// The neighbour's system name, or its chassis ID without one.
    pub remote_name: String,
    /// The neighbour's port; may be empty.
    pub remote_port: String,
    pub remote_address: Option<IpAddr>,
}

/// The objects one walk answered: `(oid, value)`.
pub type Rows = Vec<(String, String)>;

fn arcs(oid: &str) -> Option<Vec<u32>> {
    oid.trim_start_matches('.')
        .split('.')
        .map(|a| a.parse().ok())
        .collect()
}

/// The arcs of `oid` after `root`, when it is under it.
fn under(root: &str, oid: &str) -> Option<Vec<u32>> {
    let root = arcs(root)?;
    let oid = arcs(oid)?;
    oid.strip_prefix(root.as_slice()).map(<[u32]>::to_vec)
}

fn text(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_TEXT)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// Whether `value` is a MAC address as `snmp_read` writes bytes.
fn is_mac(value: &str) -> bool {
    value.len() == 17
        && value
            .split(':')
            .all(|b| b.len() == 2 && b.chars().all(|c| c.is_ascii_hexdigit()))
}

/// An address as text, or as the bytes `snmp_read` writes `0a:00:00:01`.
fn address(value: &str) -> Option<IpAddr> {
    if let Ok(address) = value.trim().parse() {
        return Some(address);
    }
    let bytes: Vec<u8> = value
        .split(':')
        .map(|b| u8::from_str_radix(b, 16).ok())
        .collect::<Option<_>>()?;
    match bytes.len() {
        4 => Some(IpAddr::V4(Ipv4Addr::new(
            bytes[0], bytes[1], bytes[2], bytes[3],
        ))),
        16 => <[u8; 16]>::try_from(bytes)
            .ok()
            .map(|b| IpAddr::V6(Ipv6Addr::from(b))),
        _ => None,
    }
}

/// `column → index → value` of a table walked from `root`, the index
/// being the arcs after the column.
fn table(root: &str, rows: &Rows) -> BTreeMap<u32, BTreeMap<Vec<u32>, String>> {
    let mut table: BTreeMap<u32, BTreeMap<Vec<u32>, String>> = BTreeMap::new();
    for (oid, value) in rows {
        if let Some(rest) = under(root, oid) {
            if let Some((column, index)) = rest.split_first() {
                table
                    .entry(*column)
                    .or_default()
                    .insert(index.to_vec(), text(value));
            }
        }
    }
    table
}

/// The column's value at `index`, when not empty.
fn cell<'a>(
    table: &'a BTreeMap<u32, BTreeMap<Vec<u32>, String>>,
    column: u32,
    index: &[u32],
) -> Option<&'a str> {
    table
        .get(&column)?
        .get(index)
        .map(String::as_str)
        .filter(|v| !v.is_empty())
}

/// The device's interfaces by index, named as its interfaces check names
/// them: ifName, else ifDescr.
fn interfaces(if_name: &Rows, if_descr: &Rows) -> BTreeMap<u32, String> {
    let mut names = BTreeMap::new();
    for (root, rows) in [(IF_DESCR, if_descr), (IF_NAME, if_name)] {
        for (oid, value) in rows {
            if let Some([index]) = under(root, oid).as_deref() {
                let value = text(value);
                if !value.is_empty() {
                    names.insert(*index, value);
                }
            }
        }
    }
    names
}

/// The neighbours a device reported, from the rows of each of [`WALKS`].
pub fn parse(walks: &[Rows; 6]) -> Vec<Neighbor> {
    let [remote, addresses, local, cdp, if_name, if_descr] = walks;
    let interfaces = interfaces(if_name, if_descr);
    let by_name: BTreeMap<&str, &String> = interfaces
        .values()
        .map(|name| (name.as_str(), name))
        .collect();
    let mut neighbors = Vec::new();

    // LLDP: rows indexed by (time mark, local port, remote index).
    let remote = table(LLDP_REMOTE, remote);
    let local = table(LLDP_LOCAL, local);
    let mut lldp_addresses: BTreeMap<(u32, u32), IpAddr> = BTreeMap::new();
    for (oid, _) in addresses {
        let Some(rest) = under(LLDP_ADDRESSES, oid) else {
            continue;
        };
        // column, time mark, local port, remote index, subtype, then the
        // address with its length first, or without it.
        let [_, _, port, index, subtype, tail @ ..] = rest.as_slice() else {
            continue;
        };
        let octets: Vec<u8> = match tail {
            [length, bytes @ ..] if *length as usize == bytes.len() => {
                bytes.iter().filter_map(|b| u8::try_from(*b).ok()).collect()
            }
            bytes => bytes.iter().filter_map(|b| u8::try_from(*b).ok()).collect(),
        };
        let parsed = match (subtype, octets.len()) {
            (1, 4) => Some(IpAddr::V4(Ipv4Addr::new(
                octets[0], octets[1], octets[2], octets[3],
            ))),
            (2, 16) => <[u8; 16]>::try_from(octets)
                .ok()
                .map(|b| IpAddr::V6(Ipv6Addr::from(b))),
            _ => None,
        };
        if let Some(parsed) = parsed {
            lldp_addresses.entry((*port, *index)).or_insert(parsed);
        }
    }
    let indexes: std::collections::BTreeSet<Vec<u32>> =
        remote.values().flat_map(|c| c.keys().cloned()).collect();
    for index in indexes {
        let [_, port, remote_index] = index.as_slice() else {
            continue;
        };
        // lldpRemSysName, else lldpRemChassisId.
        let Some(name) = cell(&remote, 9, &index).or_else(|| cell(&remote, 5, &index)) else {
            continue;
        };
        // lldpRemPortId, unless it is a MAC address: then lldpRemPortDesc.
        let remote_port = match (cell(&remote, 7, &index), cell(&remote, 8, &index)) {
            (Some(id), Some(descr)) if is_mac(id) => descr,
            (Some(id), _) => id,
            (None, Some(descr)) => descr,
            (None, None) => "",
        };
        // The local port numbers are often the interface indexes; when its
        // ID or description names an interface, that one is meant.
        let local_port = [cell(&local, 3, &[*port]), cell(&local, 4, &[*port])]
            .into_iter()
            .flatten()
            .find_map(|n| by_name.get(n).map(|s| (*s).clone()))
            .or_else(|| interfaces.get(port).cloned())
            .or_else(|| cell(&local, 3, &[*port]).map(str::to_owned))
            .unwrap_or_else(|| port.to_string());
        neighbors.push(Neighbor {
            protocol: "lldp",
            local_port,
            remote_name: name.to_owned(),
            remote_port: remote_port.to_owned(),
            remote_address: lldp_addresses.get(&(*port, *remote_index)).copied(),
        });
    }

    // CDP: rows indexed by (interface index, device index).
    let cdp = table(CDP, cdp);
    let indexes: std::collections::BTreeSet<Vec<u32>> =
        cdp.values().flat_map(|c| c.keys().cloned()).collect();
    for index in indexes {
        let [if_index, _] = index.as_slice() else {
            continue;
        };
        // cdpCacheDeviceId, cdpCacheDevicePort, cdpCacheAddress.
        let Some(name) = cell(&cdp, 6, &index) else {
            continue;
        };
        neighbors.push(Neighbor {
            protocol: "cdp",
            local_port: interfaces
                .get(if_index)
                .cloned()
                .unwrap_or_else(|| if_index.to_string()),
            remote_name: name.to_owned(),
            remote_port: cell(&cdp, 7, &index).unwrap_or_default().to_owned(),
            remote_address: cell(&cdp, 4, &index).and_then(address),
        });
    }
    // A neighbour both protocols report is one.
    let mut seen = std::collections::BTreeSet::new();
    neighbors.retain(|n| {
        seen.insert((
            n.local_port.clone(),
            crate::maps::short_name(&n.remote_name),
        ))
    });
    neighbors.truncate(MAX_NEIGHBORS);
    neighbors
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(root: &str, pairs: &[(&str, &str)]) -> Rows {
        pairs
            .iter()
            .map(|(rest, value)| (format!("{root}.{rest}"), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn lldp_and_cdp_neighbours_are_named_by_the_interfaces_check() {
        let walks = [
            rows(
                LLDP_REMOTE,
                &[
                    // Port 3: sw-core, its port Gi1/0/24.
                    ("5.0.3.1", "00:11:22:33:44:55"),
                    ("7.0.3.1", "Gi1/0/24"),
                    ("9.0.3.1", "sw-core.example.net"),
                    // Port 4: no name, a MAC as port ID: chassis and description.
                    ("5.0.4.2", "00:aa:bb:cc:dd:ee"),
                    ("7.0.4.2", "00:aa:bb:cc:dd:ef"),
                    ("8.0.4.2", "eth0"),
                ],
            ),
            rows(LLDP_ADDRESSES, &[("3.0.3.1.1.4.192.0.2.10", "2")]),
            rows(LLDP_LOCAL, &[("3.3", "ge-0/0/3"), ("4.3", "uplink")]),
            rows(
                CDP,
                &[
                    ("4.6.1", "c0:00:02:14"),
                    ("6.6.1", "ap-hall(FOC1234X0AB)"),
                    ("7.6.1", "GigabitEthernet0"),
                    // Also seen over LLDP on the same port: kept once.
                    ("6.3.1", "SW-CORE"),
                ],
            ),
            rows(
                IF_NAME,
                &[("3", "ge-0/0/3"), ("4", "ge-0/0/4"), ("5", "ge-0/0/5")],
            ),
            rows(IF_DESCR, &[("6", "port six")]),
        ];
        let found = parse(&walks);
        assert_eq!(
            found,
            vec![
                Neighbor {
                    protocol: "lldp",
                    local_port: "ge-0/0/3".into(),
                    remote_name: "sw-core.example.net".into(),
                    remote_port: "Gi1/0/24".into(),
                    remote_address: Some("192.0.2.10".parse().unwrap()),
                },
                Neighbor {
                    protocol: "lldp",
                    local_port: "ge-0/0/4".into(),
                    remote_name: "00:aa:bb:cc:dd:ee".into(),
                    remote_port: "eth0".into(),
                    remote_address: None,
                },
                Neighbor {
                    protocol: "cdp",
                    local_port: "port six".into(),
                    remote_name: "ap-hall(FOC1234X0AB)".into(),
                    remote_port: "GigabitEthernet0".into(),
                    remote_address: Some("192.0.2.20".parse().unwrap()),
                },
            ]
        );
    }

    #[test]
    fn nothing_answered_is_no_neighbour() {
        assert!(parse(&Default::default()).is_empty());
        // Rows of other tables, and a remote row without a name.
        let walks = [
            rows(LLDP_REMOTE, &[("7.0.1.1", "Gi0/1")]),
            rows(IF_NAME, &[("1", "Gi0/1")]),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ];
        assert!(parse(&walks).is_empty());
    }

    #[test]
    fn addresses_read_as_text_or_bytes() {
        assert_eq!(address("192.0.2.1"), Some("192.0.2.1".parse().unwrap()));
        assert_eq!(address("c0:00:02:01"), Some("192.0.2.1".parse().unwrap()));
        assert_eq!(address("c0:00"), None);
        assert_eq!(address("router"), None);
        assert!(is_mac("00:11:22:33:44:55"));
        assert!(!is_mac("Gi0/1"));
    }
}
