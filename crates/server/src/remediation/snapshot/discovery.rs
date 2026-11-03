//! Finding each server's Proxmox VM or container instead of mapping it by
//! hand (ADR-0093, discovery).
//!
//! A pass lists the guests of every configured cluster
//! (`/cluster/resources?type=vm`), reads each one's configuration for its
//! identity, and keeps them in `proxmox_guests`. Then it matches servers
//! to guests on what agents report in `Hello`, most trusted first:
//!
//! 1. the SMBIOS system UUID, which is a VM's `smbios1` uuid;
//! 2. a MAC address, which Proxmox sets on every VM and container network
//!    device;
//! 3. the host name against the guest's name, only for agents that report
//!    neither, as older agents do: a name is typed by people and reused.
//!
//! A server is mapped only when exactly one guest matches it at the first
//! signal that matches anything, and no other server matches that guest.
//! Otherwise its candidates are kept for the page to ask. A cluster serves
//! every customer, one customer, or one site; a server is never matched
//! to a guest of a cluster that does not serve it. Mappings made by hand
//! are left alone, and a guest a person rejected is never matched to that
//! server again. Mapped guests that migrated are followed to their node.

use std::collections::{HashMap, HashSet};

use monitorflow_common::config::ProxmoxClusterConfig;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use super::proxmox::{ProxmoxSnapshotter, Resource};
use super::store::GuestKind;
use super::SnapshotError;
use crate::api::audit::{self, Event};

/// MAC addresses kept per server or guest.
const MAX_MACS: usize = 32;
/// Longest guest name kept, as the table allows.
const MAX_NAME: usize = 128;

/// Firmware placeholders, which many machines share.
const PLACEHOLDER_UUIDS: [Uuid; 3] = [
    Uuid::nil(),
    Uuid::max(),
    Uuid::from_u128(0x0300_0200_0400_0500_0006_0007_0008_0009),
];

/// Which servers a cluster's guests may be mapped to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Owner {
    /// The MonitorFlow operator's cluster, hosting any customer's servers.
    #[default]
    Everyone,
    Customer(Uuid),
    Site(Uuid),
}

impl Owner {
    /// From the cluster's `customer` or `site`, which the configuration
    /// checked.
    pub fn from_config(cluster: &ProxmoxClusterConfig) -> Self {
        let id = |s: &Option<String>| s.as_deref().and_then(|s| Uuid::parse_str(s).ok());
        match (id(&cluster.customer), id(&cluster.site)) {
            (_, Some(site)) => Self::Site(site),
            (Some(customer), None) => Self::Customer(customer),
            (None, None) => Self::Everyone,
        }
    }

    /// Whether a server of `customer` and `site` may be mapped to a guest
    /// of the cluster.
    pub fn serves(self, customer: Uuid, site: Uuid) -> bool {
        match self {
            Self::Everyone => true,
            Self::Customer(c) => c == customer,
            Self::Site(s) => s == site,
        }
    }
}

/// Names of the clusters of `owners` that serve `customer` and `site`.
pub fn serving(owners: &HashMap<String, Owner>, customer: Uuid, site: Uuid) -> Vec<String> {
    let mut names: Vec<String> = owners
        .iter()
        .filter(|(_, owner)| owner.serves(customer, site))
        .map(|(name, _)| name.clone())
        .collect();
    names.sort();
    names
}

/// The system UUID an agent reported, unless empty, malformed or a
/// placeholder.
pub fn reported_uuid(text: &str) -> Option<Uuid> {
    Uuid::try_parse(text.trim())
        .ok()
        .filter(|uuid| !PLACEHOLDER_UUIDS.contains(uuid))
}

/// `text` as a lowercase `aa:bb:cc:dd:ee:ff`, unless it is not one or is
/// all zeros.
pub fn mac(text: &str) -> Option<String> {
    let text = text.trim().to_ascii_lowercase();
    let parts: Vec<&str> = text.split([':', '-']).collect();
    let valid = parts.len() == 6
        && parts
            .iter()
            .all(|p| p.len() == 2 && p.bytes().all(|b| b.is_ascii_hexdigit()));
    (valid && parts.iter().any(|p| *p != "00")).then(|| parts.join(":"))
}

/// The MAC addresses an agent reported, normalized, without duplicates.
pub fn reported_macs(list: &[String]) -> Vec<String> {
    let mut macs: Vec<String> = list.iter().filter_map(|m| mac(m)).collect();
    macs.sort();
    macs.dedup();
    macs.truncate(MAX_MACS);
    macs
}

/// What matched a server to a guest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Signal {
    Uuid,
    Mac,
    Hostname,
}

impl Signal {
    /// Stored in `matched_by`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Uuid => "uuid",
            Self::Mac => "mac",
            Self::Hostname => "hostname",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "uuid" => Some(Self::Uuid),
            "mac" => Some(Self::Mac),
            "hostname" => Some(Self::Hostname),
            _ => None,
        }
    }
}

/// A guest of a cluster and what identifies it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Guest {
    pub cluster: String,
    pub vmid: u32,
    pub node: String,
    pub kind: GuestKind,
    /// The VM's name, or the container's host name.
    pub name: String,
    /// A VM's `smbios1` uuid.
    pub system_uuid: Option<Uuid>,
    pub macs: Vec<String>,
}

impl Guest {
    /// From the cluster's listing of the guest and its configuration.
    pub fn new(
        cluster: &str,
        resource: &Resource,
        kind: GuestKind,
        config: &Map<String, Value>,
    ) -> Self {
        let text = |key: &str| config.get(key).and_then(Value::as_str);
        // `smbios1: uuid=…,manufacturer=…`; containers have none.
        let system_uuid = text("smbios1")
            .filter(|_| kind == GuestKind::Qemu)
            .and_then(|smbios| {
                smbios
                    .split(',')
                    .find_map(|part| part.strip_prefix("uuid="))
                    .and_then(reported_uuid)
            });
        // `net0: virtio=BC:24:11:…,bridge=vmbr0` for a VM,
        // `net0: name=eth0,hwaddr=BC:24:11:…,…` for a container.
        let mut macs: Vec<String> = config
            .iter()
            .filter(|(key, _)| {
                key.strip_prefix("net")
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            })
            .filter_map(|(_, value)| value.as_str())
            .filter_map(|net| {
                net.split(',')
                    .filter_map(|part| part.split_once('='))
                    .find_map(|(_, value)| mac(value))
            })
            .collect();
        macs.sort();
        macs.dedup();
        macs.truncate(MAX_MACS);
        let name = match kind {
            GuestKind::Lxc => text("hostname").unwrap_or(&resource.name),
            GuestKind::Qemu => &resource.name,
        };
        Self {
            cluster: cluster.to_owned(),
            vmid: resource.vmid,
            node: resource.node.clone(),
            kind,
            name: name.chars().take(MAX_NAME).collect(),
            system_uuid,
            macs,
        }
    }

    fn key(&self) -> GuestKey {
        (self.cluster.clone(), self.vmid)
    }
}

/// A guest: its cluster and ID.
pub type GuestKey = (String, u32);

/// A server, as discovery sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerFacts {
    pub id: Uuid,
    pub customer: Uuid,
    pub site: Uuid,
    pub hostname: String,
    pub system_uuid: Option<Uuid>,
    pub macs: Vec<String>,
    /// The guest it was mapped to by hand, which discovery leaves alone.
    pub manual: Option<GuestKey>,
}

/// What discovery found for one server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    One(GuestKey, Signal),
    /// Several guests match it, or its one match also matches another
    /// server: nothing is mapped and the page asks.
    Several(Vec<(GuestKey, Signal)>),
    Nothing,
}

/// The first label of a host name, lowercase: `web-1` for `Web-1.lan`.
fn short_name(name: &str) -> String {
    name.split('.')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

/// What each server without a manual mapping matches among `guests`.
pub fn match_servers(
    servers: &[ServerFacts],
    guests: &[Guest],
    owners: &HashMap<String, Owner>,
    rejected: &HashSet<(Uuid, GuestKey)>,
) -> HashMap<Uuid, Found> {
    let taken: HashSet<GuestKey> = servers.iter().filter_map(|s| s.manual.clone()).collect();
    let mut found: HashMap<Uuid, Found> = HashMap::new();
    for server in servers.iter().filter(|s| s.manual.is_none()) {
        let candidates: Vec<&Guest> = guests
            .iter()
            .filter(|g| {
                owners
                    .get(&g.cluster)
                    .is_some_and(|o| o.serves(server.customer, server.site))
            })
            .filter(|g| !taken.contains(&g.key()))
            .filter(|g| !rejected.contains(&(server.id, g.key())))
            .collect();
        let by = |signal: Signal, test: &dyn Fn(&Guest) -> bool| -> Vec<(GuestKey, Signal)> {
            candidates
                .iter()
                .filter(|g| test(g))
                .map(|g| (g.key(), signal))
                .collect()
        };
        let uuid = server.system_uuid;
        let mut matches = by(Signal::Uuid, &|g| uuid.is_some() && g.system_uuid == uuid);
        if matches.is_empty() {
            matches = by(Signal::Mac, &|g| {
                g.macs.iter().any(|m| server.macs.contains(m))
            });
        }
        // Only for agents that report no hardware: their names are all
        // there is.
        if matches.is_empty() && uuid.is_none() && server.macs.is_empty() {
            let name = short_name(&server.hostname);
            matches = by(Signal::Hostname, &|g| {
                !name.is_empty() && short_name(&g.name) == name
            });
        }
        let result = match matches.len() {
            0 => Found::Nothing,
            1 => {
                let (key, signal) = matches.remove(0);
                Found::One(key, signal)
            }
            _ => Found::Several(matches),
        };
        found.insert(server.id, result);
    }
    // A guest one server matches alone, but another too: neither is sure.
    let mut claims: HashMap<GuestKey, Vec<Uuid>> = HashMap::new();
    for (server, result) in &found {
        if let Found::One(key, _) = result {
            claims.entry(key.clone()).or_default().push(*server);
        }
    }
    for servers in claims.values().filter(|s| s.len() > 1) {
        for server in servers {
            if let Some(Found::One(key, signal)) = found.get(server).cloned() {
                found.insert(*server, Found::Several(vec![(key, signal)]));
            }
        }
    }
    found
}

/// What a pass did, for logs and tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Discovered {
    pub guests: usize,
    /// Servers newly mapped, or mapped to another guest.
    pub mapped: usize,
    /// Automatic mappings removed: the guest no longer matches alone.
    pub unmapped: usize,
    /// Mapped guests followed to another node.
    pub moved: usize,
    /// Servers with several candidates.
    pub ambiguous: usize,
    /// Clusters that could not be listed; nothing is matched then.
    pub failed_clusters: Vec<String>,
}

async fn record(
    tx: &mut Transaction<'_, Postgres>,
    action: &str,
    server: Uuid,
    details: Value,
) -> Result<(), sqlx::Error> {
    audit::record(
        &mut **tx,
        Event {
            actor_kind: "system",
            actor_id: "proxmox-discovery",
            action,
            target: Some(("server", &server.to_string())),
            details,
        },
    )
    .await
}

impl ProxmoxSnapshotter {
    /// One discovery pass over every configured cluster.
    pub async fn discover(&self) -> Result<Discovered, SnapshotError> {
        let mut report = Discovered::default();
        let mut listed: Vec<(String, Vec<Guest>)> = Vec::new();
        for (name, client) in self.clients() {
            match self.list_guests(name, client).await {
                Ok(guests) => listed.push((name.clone(), guests)),
                Err(error) => {
                    tracing::warn!(cluster = %name, %error, "cannot list the guests of a Proxmox cluster");
                    report.failed_clusters.push(name.clone());
                }
            }
        }
        report.failed_clusters.sort();
        let mut tx = self.pool().begin().await?;
        for (cluster, guests) in &listed {
            report.guests += guests.len();
            store_guests(&mut tx, cluster, guests).await?;
        }
        report.moved = follow_migrations(&mut tx).await?;
        if report.failed_clusters.is_empty() {
            self.apply_matches(&mut tx, &mut report).await?;
        }
        tx.commit().await?;
        Ok(report)
    }

    async fn list_guests(
        &self,
        cluster: &str,
        client: &super::ProxmoxClient,
    ) -> Result<Vec<Guest>, SnapshotError> {
        let mut guests = Vec::new();
        for resource in client.resources().await? {
            let Some(kind) = GuestKind::parse(&resource.kind) else {
                continue;
            };
            if resource.template || !super::store::VMID_RANGE.contains(&resource.vmid) {
                continue;
            }
            let vm = super::VmRef {
                cluster: cluster.to_owned(),
                node: resource.node.clone(),
                vmid: resource.vmid,
                kind,
            };
            if vm.check().is_err() {
                continue;
            }
            let config = client.guest_config(&vm).await?;
            guests.push(Guest::new(cluster, &resource, kind, &config));
        }
        Ok(guests)
    }

    async fn apply_matches(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        report: &mut Discovered,
    ) -> Result<(), sqlx::Error> {
        let guests = load_guests(tx).await?;
        let servers = load_servers(tx).await?;
        let rejected: HashSet<(Uuid, GuestKey)> = sqlx::query_as::<_, (Uuid, String, i32)>(
            "SELECT server_id, cluster, vmid FROM server_proxmox_rejections",
        )
        .fetch_all(&mut **tx)
        .await?
        .into_iter()
        .map(|(server, cluster, vmid)| (server, (cluster, vmid as u32)))
        .collect();
        let found = match_servers(&servers, &guests, self.owners(), &rejected);
        let by_key: HashMap<GuestKey, &Guest> = guests.iter().map(|g| (g.key(), g)).collect();
        let automatic: HashMap<Uuid, (GuestKey, String)> =
            sqlx::query_as::<_, (Uuid, String, i32, String)>(
                "SELECT server_id, cluster, vmid, node FROM server_proxmox_vms
             WHERE matched_by IS NOT NULL FOR UPDATE",
            )
            .fetch_all(&mut **tx)
            .await?
            .into_iter()
            .map(|(server, cluster, vmid, node)| (server, ((cluster, vmid as u32), node)))
            .collect();

        // Removals first, so a guest can pass from one server to another.
        for (server, (key, node)) in &automatic {
            let kept = matches!(found.get(server), Some(Found::One(k, _)) if k == key);
            if kept {
                continue;
            }
            sqlx::query("DELETE FROM server_proxmox_vms WHERE server_id = $1")
                .bind(server)
                .execute(&mut **tx)
                .await?;
            if !matches!(found.get(server), Some(Found::One(..))) {
                report.unmapped += 1;
                let why = match found.get(server) {
                    Some(Found::Several(_)) => "several guests match",
                    _ => "no guest matches",
                };
                record(
                    tx,
                    "server.proxmox.unmatch",
                    *server,
                    json!({ "cluster": key.0, "node": node, "vmid": key.1, "reason": why }),
                )
                .await?;
            }
        }
        for (server, result) in &found {
            let Found::One(key, signal) = result else {
                continue;
            };
            let Some(guest) = by_key.get(key) else {
                continue;
            };
            let previous = automatic.get(server);
            if previous.is_some_and(|(k, _)| k == key) {
                sqlx::query("UPDATE server_proxmox_vms SET matched_by = $2 WHERE server_id = $1")
                    .bind(server)
                    .bind(signal.as_str())
                    .execute(&mut **tx)
                    .await?;
                continue;
            }
            sqlx::query(
                "INSERT INTO server_proxmox_vms (server_id, cluster, node, vmid, kind, matched_by)
                 VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(server)
            .bind(&guest.cluster)
            .bind(&guest.node)
            .bind(guest.vmid as i32)
            .bind(guest.kind.as_str())
            .bind(signal.as_str())
            .execute(&mut **tx)
            .await?;
            report.mapped += 1;
            let previous = previous.map(
                |((cluster, vmid), node)| json!({ "cluster": cluster, "node": node, "vmid": vmid }),
            );
            record(
                tx,
                "server.proxmox.discover",
                *server,
                json!({ "cluster": guest.cluster, "node": guest.node, "vmid": guest.vmid,
                        "kind": guest.kind, "matched_by": signal, "previous": previous }),
            )
            .await?;
        }

        sqlx::query("DELETE FROM server_proxmox_candidates")
            .execute(&mut **tx)
            .await?;
        for (server, result) in &found {
            let Found::Several(list) = result else {
                continue;
            };
            report.ambiguous += 1;
            for ((cluster, vmid), signal) in list {
                sqlx::query(
                    "INSERT INTO server_proxmox_candidates (server_id, cluster, vmid, matched_by)
                     VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
                )
                .bind(server)
                .bind(cluster)
                .bind(*vmid as i32)
                .bind(signal.as_str())
                .execute(&mut **tx)
                .await?;
            }
        }
        Ok(())
    }
}

/// Replaces the guests of `cluster`.
async fn store_guests(
    tx: &mut Transaction<'_, Postgres>,
    cluster: &str,
    guests: &[Guest],
) -> Result<(), sqlx::Error> {
    let ids: Vec<i32> = guests.iter().map(|g| g.vmid as i32).collect();
    sqlx::query("DELETE FROM proxmox_guests WHERE cluster = $1 AND NOT (vmid = ANY($2))")
        .bind(cluster)
        .bind(&ids)
        .execute(&mut **tx)
        .await?;
    for g in guests {
        sqlx::query(
            "INSERT INTO proxmox_guests (cluster, vmid, node, kind, name, system_uuid, mac_addresses)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             ON CONFLICT (cluster, vmid) DO UPDATE
             SET node = $3, kind = $4, name = $5, system_uuid = $6, mac_addresses = $7,
                 seen_at = now()",
        )
        .bind(cluster)
        .bind(g.vmid as i32)
        .bind(&g.node)
        .bind(g.kind.as_str())
        .bind(&g.name)
        .bind(g.system_uuid)
        .bind(&g.macs)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

/// Moves mappings, made by hand or not, to the node their guest is on now.
async fn follow_migrations(tx: &mut Transaction<'_, Postgres>) -> Result<usize, sqlx::Error> {
    let moved: Vec<(Uuid, String, i32, String, String)> = sqlx::query_as(
        "SELECT v.server_id, v.cluster, v.vmid, v.node, g.node
         FROM server_proxmox_vms v
         JOIN proxmox_guests g ON g.cluster = v.cluster AND g.vmid = v.vmid AND g.kind = v.kind
         WHERE g.node <> v.node
         FOR UPDATE OF v",
    )
    .fetch_all(&mut **tx)
    .await?;
    for (server, cluster, vmid, from, to) in &moved {
        sqlx::query(
            "UPDATE server_proxmox_vms SET node = $2, updated_at = now() WHERE server_id = $1",
        )
        .bind(server)
        .bind(to)
        .execute(&mut **tx)
        .await?;
        record(
            tx,
            "server.proxmox.move",
            *server,
            json!({ "cluster": cluster, "vmid": vmid, "from": from, "to": to }),
        )
        .await?;
        tracing::info!(%cluster, vmid, %from, %to, "followed a Proxmox guest to its new node");
    }
    Ok(moved.len())
}

async fn load_guests(tx: &mut Transaction<'_, Postgres>) -> Result<Vec<Guest>, sqlx::Error> {
    type Row = (
        String,
        i32,
        String,
        String,
        String,
        Option<Uuid>,
        Vec<String>,
    );
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT cluster, vmid, node, kind, name, system_uuid, mac_addresses FROM proxmox_guests",
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(cluster, vmid, node, kind, name, system_uuid, macs)| Guest {
                cluster,
                vmid: vmid as u32,
                node,
                kind: GuestKind::parse(&kind).unwrap_or_default(),
                name,
                system_uuid,
                macs,
            },
        )
        .collect())
}

async fn load_servers(tx: &mut Transaction<'_, Postgres>) -> Result<Vec<ServerFacts>, sqlx::Error> {
    type Row = (
        Uuid,
        Uuid,
        Uuid,
        String,
        Option<Uuid>,
        Vec<String>,
        Option<String>,
        Option<i32>,
    );
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT s.id, si.customer_id, s.site_id, s.hostname, s.system_uuid, s.mac_addresses,
                v.cluster, v.vmid
         FROM servers s
         JOIN sites si ON si.id = s.site_id
         LEFT JOIN server_proxmox_vms v ON v.server_id = s.id AND v.matched_by IS NULL
         WHERE s.revoked_at IS NULL",
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(id, customer, site, hostname, system_uuid, macs, cluster, vmid)| ServerFacts {
                id,
                customer,
                site,
                hostname,
                system_uuid,
                macs,
                manual: cluster.zip(vmid).map(|(c, v)| (c, v as u32)),
            },
        )
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACME: Uuid = Uuid::from_u128(0xa);
    const GLOBEX: Uuid = Uuid::from_u128(0xb);
    const PARIS: Uuid = Uuid::from_u128(0xa1);
    const BERLIN: Uuid = Uuid::from_u128(0xb1);

    fn guest(vmid: u32, name: &str, uuid: Option<u128>, macs: &[&str]) -> Guest {
        Guest {
            cluster: "pve".into(),
            vmid,
            node: "pve1".into(),
            kind: GuestKind::Qemu,
            name: name.into(),
            system_uuid: uuid.map(Uuid::from_u128),
            macs: macs.iter().map(|m| m.to_string()).collect(),
        }
    }

    fn server(n: u128, hostname: &str, uuid: Option<u128>, macs: &[&str]) -> ServerFacts {
        ServerFacts {
            id: Uuid::from_u128(n),
            customer: ACME,
            site: PARIS,
            hostname: hostname.into(),
            system_uuid: uuid.map(Uuid::from_u128),
            macs: macs.iter().map(|m| m.to_string()).collect(),
            manual: None,
        }
    }

    fn everyone() -> HashMap<String, Owner> {
        HashMap::from([("pve".to_owned(), Owner::Everyone)])
    }

    fn one(vmid: u32, signal: Signal) -> Found {
        Found::One(("pve".into(), vmid), signal)
    }

    #[test]
    fn identity_comes_from_the_guest_configuration() {
        let resource = Resource {
            vmid: 104,
            node: "pve2".into(),
            kind: "qemu".into(),
            name: "web-1".into(),
            template: false,
        };
        let config = serde_json::json!({
            "smbios1": "uuid=1B3E2F80-6C0A-4E1B-9C3D-2A4B5C6D7E8F,manufacturer=QEMU",
            "net0": "virtio=BC:24:11:AA:BB:01,bridge=vmbr0,firewall=1",
            "net1": "e1000=bc:24:11:aa:bb:02,bridge=vmbr1",
            "netx": "virtio=BC:24:11:AA:BB:03",
        });
        let vm = Guest::new(
            "pve",
            &resource,
            GuestKind::Qemu,
            config.as_object().unwrap(),
        );
        assert_eq!(
            vm.system_uuid,
            Some(Uuid::from_u128(0x1b3e2f80_6c0a_4e1b_9c3d_2a4b5c6d7e8f))
        );
        assert_eq!(vm.macs, ["bc:24:11:aa:bb:01", "bc:24:11:aa:bb:02"]);
        assert_eq!(
            (vm.name.as_str(), vm.node.as_str(), vm.vmid),
            ("web-1", "pve2", 104)
        );

        let config = serde_json::json!({
            "hostname": "files-1",
            "net0": "name=eth0,bridge=vmbr0,hwaddr=BC:24:11:CC:DD:01,ip=dhcp,type=veth",
            "smbios1": "uuid=1B3E2F80-6C0A-4E1B-9C3D-2A4B5C6D7E8F",
        });
        let ct = Guest::new(
            "pve",
            &Resource {
                kind: "lxc".into(),
                name: "CT104".into(),
                ..resource
            },
            GuestKind::Lxc,
            config.as_object().unwrap(),
        );
        assert_eq!(ct.system_uuid, None, "containers have no SMBIOS");
        assert_eq!(ct.macs, ["bc:24:11:cc:dd:01"]);
        assert_eq!(ct.name, "files-1");
    }

    #[test]
    fn reports_are_normalized() {
        assert_eq!(
            reported_uuid("1b3e2f80-6c0a-4e1b-9c3d-2a4b5c6d7e8f"),
            Some(Uuid::from_u128(0x1b3e2f80_6c0a_4e1b_9c3d_2a4b5c6d7e8f))
        );
        for bad in [
            "",
            "x",
            "00000000-0000-0000-0000-000000000000",
            "03000200-0400-0500-0006-000700080009",
        ] {
            assert_eq!(reported_uuid(bad), None, "{bad}");
        }
        let macs = reported_macs(&[
            "BC:24:11:AA:BB:01".into(),
            "bc-24-11-aa-bb-01".into(),
            "junk".into(),
            "00:00:00:00:00:00".into(),
        ]);
        assert_eq!(macs, ["bc:24:11:aa:bb:01"]);
    }

    #[test]
    fn the_uuid_wins_then_the_mac_then_the_name_for_old_agents() {
        let guests = [
            guest(101, "web-1", Some(1), &["bc:24:11:00:00:01"]),
            guest(102, "web-2", Some(2), &["bc:24:11:00:00:02"]),
            guest(103, "db-1", None, &["bc:24:11:00:00:03"]),
            guest(104, "old-1", None, &[]),
        ];
        let servers = [
            // UUID of 101, MAC of 102: the UUID decides.
            server(1, "anything", Some(1), &["bc:24:11:00:00:02"]),
            server(2, "db-1", None, &["bc:24:11:00:00:03"]),
            // An old agent: its name.
            server(3, "OLD-1.example.net", None, &[]),
            // A new agent whose hardware matches nothing: the name is not
            // enough.
            server(4, "web-2", Some(9), &["02:42:ac:11:00:01"]),
        ];
        let found = match_servers(&servers, &guests, &everyone(), &HashSet::new());
        assert_eq!(found[&Uuid::from_u128(1)], one(101, Signal::Uuid));
        assert_eq!(found[&Uuid::from_u128(2)], one(103, Signal::Mac));
        assert_eq!(found[&Uuid::from_u128(3)], one(104, Signal::Hostname));
        assert_eq!(found[&Uuid::from_u128(4)], Found::Nothing);
    }

    #[test]
    fn ambiguity_maps_nothing_and_keeps_the_candidates() {
        let guests = [
            guest(101, "web", None, &["bc:24:11:00:00:01"]),
            guest(102, "web", None, &["bc:24:11:00:00:01"]),
            guest(103, "app", None, &["bc:24:11:00:00:03"]),
        ];
        let servers = [
            server(1, "x", None, &["bc:24:11:00:00:01"]),
            // Two servers claim one guest: a re-enrolled machine, say.
            server(2, "app", None, &["bc:24:11:00:00:03"]),
            server(3, "app-copy", None, &["bc:24:11:00:00:03"]),
        ];
        let found = match_servers(&servers, &guests, &everyone(), &HashSet::new());
        assert_eq!(
            found[&Uuid::from_u128(1)],
            Found::Several(vec![
                (("pve".into(), 101), Signal::Mac),
                (("pve".into(), 102), Signal::Mac)
            ])
        );
        let both = Found::Several(vec![(("pve".into(), 103), Signal::Mac)]);
        assert_eq!(found[&Uuid::from_u128(2)], both);
        assert_eq!(found[&Uuid::from_u128(3)], both);
    }

    #[test]
    fn manual_mappings_and_rejections_win() {
        let guests = [
            guest(101, "web-1", Some(1), &[]),
            guest(102, "web-1b", Some(1), &[]),
        ];
        let mut manual = server(1, "web-1", Some(1), &[]);
        manual.manual = Some(("pve".into(), 101));
        let other = server(2, "web-1", Some(1), &[]);
        let rejected = HashSet::from([(Uuid::from_u128(2), ("pve".to_owned(), 102))]);
        let found = match_servers(&[manual, other.clone()], &guests, &everyone(), &rejected);
        assert!(!found.contains_key(&Uuid::from_u128(1)), "left alone");
        // 101 is taken by hand, 102 rejected.
        assert_eq!(found[&Uuid::from_u128(2)], Found::Nothing);
        let found = match_servers(&[other], &guests, &everyone(), &HashSet::new());
        assert!(matches!(found[&Uuid::from_u128(2)], Found::Several(ref l) if l.len() == 2));
    }

    #[test]
    fn a_cluster_never_matches_servers_of_another_customer_or_site() {
        let guests = [guest(101, "web-1", Some(1), &["bc:24:11:00:00:01"])];
        let mut theirs = server(1, "web-1", Some(1), &["bc:24:11:00:00:01"]);
        theirs.customer = GLOBEX;
        theirs.site = BERLIN;
        for owner in [Owner::Customer(ACME), Owner::Site(PARIS)] {
            let owners = HashMap::from([("pve".to_owned(), owner)]);
            let found = match_servers(&[theirs.clone()], &guests, &owners, &HashSet::new());
            assert_eq!(found[&Uuid::from_u128(1)], Found::Nothing, "{owner:?}");
            let ours = server(2, "web-1", Some(1), &[]);
            let found = match_servers(&[ours], &guests, &owners, &HashSet::new());
            assert_eq!(
                found[&Uuid::from_u128(2)],
                one(101, Signal::Uuid),
                "{owner:?}"
            );
        }
        // A cluster no longer configured matches nothing.
        let found = match_servers(&[theirs], &guests, &HashMap::new(), &HashSet::new());
        assert_eq!(found[&Uuid::from_u128(1)], Found::Nothing);
        assert_eq!(
            serving(
                &HashMap::from([
                    ("a".into(), Owner::Customer(ACME)),
                    ("b".into(), Owner::Everyone)
                ]),
                GLOBEX,
                BERLIN
            ),
            ["b"]
        );
    }
}
