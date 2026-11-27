//! Which server or device each virtual machine is (ADR-0156): the first
//! signal that points at exactly one server of the source's customer.

use std::collections::HashMap;

use serde::Serialize;
use uuid::Uuid;

use crate::remediation::snapshot::discovery::{mac, reported_uuid};

/// What tied a virtual machine to a server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Signal {
    Uuid,
    Mac,
    Address,
    Name,
}

impl Signal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Uuid => "uuid",
            Self::Mac => "mac",
            Self::Address => "address",
            Self::Name => "name",
        }
    }
}

/// What a virtual machine says of itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VmFacts {
    pub id: Uuid,
    pub uuid: Option<Uuid>,
    pub macs: Vec<String>,
    pub addresses: Vec<String>,
    /// Its guest host name, else its name.
    pub name: String,
}

impl VmFacts {
    /// From a stored object's instance and `_info` details.
    pub fn from_details(id: Uuid, instance: &str, details: &serde_json::Value) -> Self {
        let text = |k: &str| details.get(k).and_then(|v| v.as_str()).unwrap_or_default();
        let list = |k: &str| -> Vec<String> {
            text(k)
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        };
        // A Kubernetes node's is its `hostname`.
        let guest = Some(text("guest_hostname"))
            .filter(|g| !g.is_empty())
            .unwrap_or_else(|| text("hostname"));
        Self {
            id,
            uuid: reported_uuid(text("uuid")),
            macs: list("macs").iter().filter_map(|m| mac(m)).collect(),
            addresses: list("addresses"),
            name: if guest.is_empty() { instance } else { guest }.to_owned(),
        }
    }
}

/// What the inventory knows of a server or device.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerFacts {
    pub id: Uuid,
    pub hostname: String,
    pub uuid: Option<Uuid>,
    pub macs: Vec<String>,
    pub address: Option<String>,
}

/// `uuid` with its first three fields byte-swapped, as older VMware
/// virtual hardware and some firmware report it.
pub fn swapped(uuid: Uuid) -> Uuid {
    let mut b = *uuid.as_bytes();
    b[0..4].reverse();
    b[4..6].reverse();
    b[6..8].reverse();
    Uuid::from_bytes(b)
}

/// The first label of a host name, without case.
fn short(name: &str) -> String {
    name.trim()
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

/// The server each virtual machine is, with the signal, when exactly one
/// server matches its first matching signal and no other virtual machine
/// claims that server too.
pub fn matches(vms: &[VmFacts], servers: &[ServerFacts]) -> HashMap<Uuid, (Uuid, Signal)> {
    let mut found: HashMap<Uuid, (Uuid, Signal)> = HashMap::new();
    for vm in vms {
        type Test<'a> = Box<dyn Fn(&ServerFacts) -> bool + 'a>;
        let tests: [(Signal, Test<'_>); 4] = [
            (
                Signal::Uuid,
                Box::new(|s: &ServerFacts| {
                    vm.uuid
                        .zip(s.uuid)
                        .is_some_and(|(v, s)| v == s || swapped(v) == s)
                }),
            ),
            (
                Signal::Mac,
                Box::new(|s: &ServerFacts| vm.macs.iter().any(|m| s.macs.contains(m))),
            ),
            (
                Signal::Address,
                Box::new(|s: &ServerFacts| {
                    s.address
                        .as_ref()
                        .is_some_and(|a| vm.addresses.iter().any(|v| v.eq_ignore_ascii_case(a)))
                }),
            ),
            (
                Signal::Name,
                Box::new(|s: &ServerFacts| {
                    let name = short(&vm.name);
                    !name.is_empty() && short(&s.hostname) == name
                }),
            ),
        ];
        for (signal, test) in &tests {
            let candidates: Vec<&ServerFacts> = servers.iter().filter(|s| test(s)).collect();
            match candidates.as_slice() {
                [] => continue,
                [one] => {
                    found.insert(vm.id, (one.id, *signal));
                }
                _ => {}
            }
            break;
        }
    }
    let mut claims: HashMap<Uuid, usize> = HashMap::new();
    for (server, _) in found.values() {
        *claims.entry(*server).or_default() += 1;
    }
    found.retain(|_, (server, _)| claims[server] == 1);
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn server(n: u128, hostname: &str) -> ServerFacts {
        ServerFacts {
            id: id(n),
            hostname: hostname.into(),
            ..ServerFacts::default()
        }
    }

    #[test]
    fn details_read_as_facts() {
        let vm = VmFacts::from_details(
            id(1),
            "web-01",
            &serde_json::json!({"uuid": "4237ABCD-0000-0000-0000-000000000001", "macs": "00:50:56:AA:BB:CC,junk", "addresses": "10.0.0.5, fe80::1"}),
        );
        assert_eq!(
            vm.uuid,
            Some(Uuid::from_u128(0x4237abcd_0000_0000_0000_000000000001))
        );
        assert_eq!(vm.macs, ["00:50:56:aa:bb:cc"]);
        assert_eq!(vm.addresses, ["10.0.0.5", "fe80::1"]);
        // A Kubernetes node names itself by `hostname`.
        let node = VmFacts::from_details(
            id(2),
            "worker-1",
            &serde_json::json!({"hostname": "w1.cluster.example"}),
        );
        assert_eq!(node.name, "w1.cluster.example");
        assert_eq!(vm.name, "web-01");
        let placeholder = VmFacts::from_details(
            id(2),
            "x",
            &serde_json::json!({"uuid": "00000000-0000-0000-0000-000000000000", "guest_hostname": "db.lan"}),
        );
        assert!(placeholder.uuid.is_none());
        assert_eq!(placeholder.name, "db.lan");
    }

    #[test]
    fn signals_in_order_and_only_when_unique() {
        let uuid = Uuid::parse_str("63d40cc5-9cba-5cc1-884a-f7f19070ecea").unwrap();
        let servers = vec![
            ServerFacts {
                uuid: Some(swapped(uuid)),
                ..server(10, "web-01")
            },
            ServerFacts {
                macs: vec!["00:50:56:00:00:02".into()],
                ..server(11, "db-01")
            },
            ServerFacts {
                address: Some("10.0.0.9".into()),
                ..server(12, "appliance")
            },
            server(13, "files.corp.example"),
            server(14, "dup"),
            server(15, "dup"),
        ];
        let vms = vec![
            VmFacts {
                id: id(1),
                uuid: Some(uuid),
                name: "db-01".into(),
                ..VmFacts::default()
            },
            VmFacts {
                id: id(2),
                macs: vec!["00:50:56:00:00:02".into()],
                ..VmFacts::default()
            },
            VmFacts {
                id: id(3),
                addresses: vec!["10.0.0.9".into()],
                ..VmFacts::default()
            },
            VmFacts {
                id: id(4),
                name: "FILES".into(),
                ..VmFacts::default()
            },
            VmFacts {
                id: id(5),
                name: "dup".into(),
                ..VmFacts::default()
            },
            VmFacts {
                id: id(6),
                name: "nothing".into(),
                ..VmFacts::default()
            },
        ];
        let found = matches(&vms, &servers);
        // The UUID wins over the name, in either byte order.
        assert_eq!(found[&id(1)], (id(10), Signal::Uuid));
        assert_eq!(found[&id(2)], (id(11), Signal::Mac));
        assert_eq!(found[&id(3)], (id(12), Signal::Address));
        assert_eq!(found[&id(4)], (id(13), Signal::Name));
        assert!(!found.contains_key(&id(5)), "two servers share the name");
        assert!(!found.contains_key(&id(6)));
    }

    #[test]
    fn a_server_claimed_twice_is_matched_to_neither() {
        let servers = vec![server(10, "web")];
        let vms = vec![
            VmFacts {
                id: id(1),
                name: "web".into(),
                ..VmFacts::default()
            },
            VmFacts {
                id: id(2),
                name: "web.lan".into(),
                ..VmFacts::default()
            },
        ];
        assert!(matches(&vms, &servers).is_empty());
    }
}
