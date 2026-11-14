//! What each server's agent last saw running on its machine (ADR-0134):
//! running services, listening ports and data file systems, for check
//! recommendations. The report comes from the agent and is untrusted:
//! [`bound`] keeps only well-formed entries, within fixed caps, and counts
//! what it dropped. One row per server, replaced by each report.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use monitorflow_common::proto::v1::DiscoveryReport;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

/// Caps, the same the agent applies.
pub const MAX_SERVICES: usize = 256;
pub const MAX_PORTS: usize = 128;
pub const MAX_MOUNTS: usize = 64;
const MAX_UNIT_LEN: usize = 255;
const MAX_PATH_LEN: usize = 256;
const MAX_FS_TYPE_LEN: usize = 32;
const MAX_HOST_LEN: usize = 253;

/// A report once bounded, as stored.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Discovered {
    pub services: Vec<String>,
    pub ports: Vec<Port>,
    pub mounts: Vec<Mount>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Port {
    pub port: u16,
    pub loopback_only: bool,
    /// A check on 127.0.0.1 reaches it.
    pub local: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mount {
    pub path: String,
    pub fs_type: String,
    /// The file server of a network share; empty for a local disk.
    pub source_host: String,
}

/// A systemd service name (ADR-0021's charset), ending in `.service`.
pub fn valid_unit(unit: &str) -> bool {
    unit.len() <= MAX_UNIT_LEN
        && unit.len() > ".service".len()
        && unit.ends_with(".service")
        && !unit.starts_with(['-', '.', '@'])
        && unit
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b":_.\\@-".contains(&b))
}

/// A host name or an IP address, as a port check takes it (ADR-0021).
pub fn valid_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= MAX_HOST_LEN
        && (host.parse::<std::net::IpAddr>().is_ok()
            || host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)))
        && !host.starts_with(['-', '.'])
}

fn valid_path(path: &str) -> bool {
    path.starts_with('/') && path.len() <= MAX_PATH_LEN && !path.chars().any(char::is_control)
}

fn valid_fs_type(fs_type: &str) -> bool {
    !fs_type.is_empty()
        && fs_type.len() <= MAX_FS_TYPE_LEN
        && fs_type
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._".contains(&b))
}

/// The well-formed part of `report`, within the caps, and how many
/// entries were dropped.
pub fn bound(report: &DiscoveryReport) -> (Discovered, usize) {
    let mut dropped = 0;
    let mut keep = |ok: bool| {
        dropped += usize::from(!ok);
        ok
    };
    let services: BTreeSet<&str> = report
        .services
        .iter()
        .map(String::as_str)
        .filter(|s| keep(valid_unit(s)))
        .collect();
    let mut seen = BTreeSet::new();
    let ports: Vec<Port> = report
        .ports
        .iter()
        .filter(|p| keep(u16::try_from(p.port).is_ok_and(|n| n > 0) && seen.insert(p.port)))
        .map(|p| Port {
            port: p.port as u16,
            loopback_only: p.loopback_only,
            local: p.local,
        })
        .collect();
    let mut paths = BTreeSet::new();
    let mounts: Vec<Mount> = report
        .mounts
        .iter()
        .filter(|m| {
            keep(
                valid_path(&m.path)
                    && valid_fs_type(&m.fs_type)
                    && (m.source_host.is_empty() || valid_host(&m.source_host))
                    && paths.insert(m.path.as_str()),
            )
        })
        .map(|m| Mount {
            path: m.path.clone(),
            fs_type: m.fs_type.clone(),
            source_host: m.source_host.clone(),
        })
        .collect();
    let over = |n: usize, cap: usize| n.saturating_sub(cap);
    dropped += over(services.len(), MAX_SERVICES)
        + over(ports.len(), MAX_PORTS)
        + over(mounts.len(), MAX_MOUNTS);
    let mut ports = ports;
    ports.sort_by_key(|p| p.port);
    ports.truncate(MAX_PORTS);
    let mut mounts = mounts;
    mounts.truncate(MAX_MOUNTS);
    let discovered = Discovered {
        services: services
            .into_iter()
            .take(MAX_SERVICES)
            .map(str::to_owned)
            .collect(),
        ports,
        mounts,
    };
    (discovered, dropped)
}

/// Stores `discovered` for the agent `peer_id`, unless it is removed;
/// false when no server row took it.
pub async fn record(
    pool: &PgPool,
    peer_id: &str,
    received_at: DateTime<Utc>,
    discovered: &Discovered,
    dropped: usize,
) -> Result<bool, sqlx::Error> {
    let report = serde_json::to_value(discovered).expect("plain data serializes");
    let stored = sqlx::query(
        "INSERT INTO server_discovery (server_id, report, reported_at, dropped)
         SELECT id, $2, $3, $4 FROM servers WHERE peer_id = $1 AND revoked_at IS NULL
         ON CONFLICT (server_id) DO UPDATE
             SET report = EXCLUDED.report, reported_at = EXCLUDED.reported_at,
                 dropped = EXCLUDED.dropped",
    )
    .bind(peer_id)
    .bind(report)
    .bind(received_at)
    .bind(i32::try_from(dropped).unwrap_or(i32::MAX))
    .execute(pool)
    .await?;
    Ok(stored.rows_affected() > 0)
}

/// The latest report of server `id` and when it came.
pub async fn load(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<(Discovered, DateTime<Utc>)>, sqlx::Error> {
    let row: Option<(serde_json::Value, DateTime<Utc>)> =
        sqlx::query_as("SELECT report, reported_at FROM server_discovery WHERE server_id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(report, at)| (serde_json::from_value(report).unwrap_or_default(), at)))
}

#[cfg(test)]
mod tests {
    use monitorflow_common::proto::v1::{DiscoveredMount, DiscoveredPort};

    use super::*;

    fn port(port: u32) -> DiscoveredPort {
        DiscoveredPort {
            port,
            loopback_only: false,
            local: true,
        }
    }

    fn mount(path: &str, fs_type: &str, host: &str) -> DiscoveredMount {
        DiscoveredMount {
            path: path.into(),
            fs_type: fs_type.into(),
            source_host: host.into(),
        }
    }

    #[test]
    fn keeps_what_is_well_formed_and_counts_the_rest() {
        let report = DiscoveryReport {
            collected_at_unix_ms: 1,
            services: [
                "ssh.service",
                "postgresql@16-main.service",
                "ssh.service",
                "cron.timer",
                "-x.service",
                "a b.service",
                "<script>.service",
                ".service",
            ]
            .map(String::from)
            .to_vec(),
            ports: vec![port(5432), port(0), port(70_000), port(5432), port(22)],
            mounts: vec![
                mount("/", "ext4", ""),
                mount("/mnt/backup", "nfs4", "nas.lan"),
                mount("/mnt/v6", "nfs", "fd00::5"),
                mount("relative", "ext4", ""),
                mount("/bad\nline", "ext4", ""),
                mount("/x", "EXT4", ""),
                mount("/y", "cifs", "files.lan/../etc"),
                mount("/", "xfs", ""),
            ],
        };
        let (found, dropped) = bound(&report);
        assert_eq!(
            found.services,
            ["postgresql@16-main.service", "ssh.service"]
        );
        assert_eq!(
            found.ports.iter().map(|p| p.port).collect::<Vec<_>>(),
            [22, 5432]
        );
        let paths: Vec<_> = found.mounts.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(paths, ["/", "/mnt/backup", "/mnt/v6"]);
        // The duplicate service is merged, not counted.
        assert_eq!(dropped, 5 + 3 + 5);
    }

    #[test]
    fn caps_each_list() {
        let report = DiscoveryReport {
            services: (0..300).map(|i| format!("u{i:03}.service")).collect(),
            ports: (1..=200).map(port).collect(),
            mounts: (0..100)
                .map(|i| mount(&format!("/m{i}"), "ext4", ""))
                .collect(),
            ..DiscoveryReport::default()
        };
        let (found, dropped) = bound(&report);
        assert_eq!(
            (found.services.len(), found.ports.len(), found.mounts.len()),
            (MAX_SERVICES, MAX_PORTS, MAX_MOUNTS)
        );
        assert_eq!(dropped, 44 + 72 + 36);
    }
}
