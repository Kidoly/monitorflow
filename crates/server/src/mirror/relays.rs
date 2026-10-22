//! Mirrors served by relays (ADR-0037): each relay reports on its own
//! stream the URL its agents reach its mirror listener at and the
//! snapshots it holds; source configuration for a server attached to that
//! relay uses the relay's URL when it holds every mirror named.

use chrono::{DateTime, Utc};
use monitorflow_common::config::valid_mirror_url;
use monitorflow_common::mirror::{is_mirror_name, is_snapshot_name};
use monitorflow_common::proto::v1::RelayMirrors;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::Family;

/// Most mirrors in one report.
pub const MAX_MIRRORS: usize = 1024;
/// Most snapshots of one mirror in one report.
pub const MAX_SNAPSHOTS: usize = 64;

/// A mirror a relay serves.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct ServedMirror {
    pub family: Family,
    pub name: String,
    /// The snapshot `current` names on the relay.
    pub current: Option<String>,
    /// Complete snapshots, `current` included.
    pub snapshots: Vec<String>,
}

/// What a relay serves, as it last reported it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Served {
    /// Base URL of its mirror listener, without the trailing slash.
    pub url: String,
    pub mirrors: Vec<ServedMirror>,
}

impl Served {
    /// Whether the relay holds `snapshot` of the mirror `family`/`name`:
    /// a snapshot it lists, or `current` when it has one.
    pub fn holds(&self, family: Family, name: &str, snapshot: &str) -> bool {
        self.mirrors.iter().any(|m| {
            m.family == family
                && m.name == name
                && if snapshot == crate::commands::sources::CURRENT {
                    m.current.is_some()
                } else {
                    m.snapshots.iter().any(|s| s == snapshot)
                }
        })
    }
}

/// Checks a report from a relay. `Ok(None)` when it serves no mirrors.
/// Errors say why in one line, for the log.
pub fn check(report: &RelayMirrors) -> Result<Option<Served>, String> {
    if report.public_url.is_empty() {
        return Ok(None);
    }
    if !valid_mirror_url(&report.public_url) {
        return Err(
            "public_url must be an http or https URL without user, query or fragment".into(),
        );
    }
    if report.mirrors.len() > MAX_MIRRORS {
        return Err(format!("more than {MAX_MIRRORS} mirrors"));
    }
    let mut mirrors: Vec<ServedMirror> = Vec::with_capacity(report.mirrors.len());
    for m in &report.mirrors {
        let family = m
            .family
            .parse::<Family>()
            .map_err(|_| format!("unknown family `{}`", m.family.escape_debug()))?;
        if !is_mirror_name(&m.name) {
            return Err(format!("invalid mirror name `{}`", m.name.escape_debug()));
        }
        if mirrors
            .iter()
            .any(|o| o.family == family && o.name == m.name)
        {
            return Err(format!("mirror `{}` is repeated", m.name));
        }
        if m.snapshots.len() > MAX_SNAPSHOTS {
            return Err(format!(
                "mirror `{}` lists more than {MAX_SNAPSHOTS} snapshots",
                m.name
            ));
        }
        let current = Some(m.current.clone()).filter(|c| !c.is_empty());
        if let Some(bad) = current
            .iter()
            .chain(&m.snapshots)
            .find(|s| !is_snapshot_name(s))
        {
            return Err(format!("invalid snapshot name `{}`", bad.escape_debug()));
        }
        mirrors.push(ServedMirror {
            family,
            name: m.name.clone(),
            current,
            snapshots: m.snapshots.clone(),
        });
    }
    Ok(Some(Served {
        url: report.public_url.trim_end_matches('/').to_owned(),
        mirrors,
    }))
}

/// Replaces what the relay `peer_id` serves with `served`, unless a report
/// received after `received_at` is already stored. Returns whether it was
/// stored.
pub async fn record(
    pool: &PgPool,
    peer_id: &str,
    received_at: DateTime<Utc>,
    served: Option<&Served>,
) -> Result<bool, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let relay: Option<Uuid> = sqlx::query_scalar(
        "UPDATE relays SET mirror_url = $2, mirror_reported_at = $3
         WHERE peer_id = $1 AND revoked_at IS NULL
           AND (mirror_reported_at IS NULL OR mirror_reported_at < $3)
         RETURNING id",
    )
    .bind(peer_id)
    .bind(served.map(|s| &s.url))
    .bind(received_at)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(relay) = relay else {
        return Ok(false);
    };
    sqlx::query("DELETE FROM relay_mirrors WHERE relay_id = $1")
        .bind(relay)
        .execute(&mut *tx)
        .await?;
    for m in served.map_or(&[][..], |s| &s.mirrors) {
        sqlx::query(
            "INSERT INTO relay_mirrors (relay_id, family, name, current, snapshots)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(relay)
        .bind(m.family)
        .bind(&m.name)
        .bind(&m.current)
        .bind(&m.snapshots)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(true)
}

/// What the relay the server `server_id` connects through serves, if that
/// relay is not revoked and reported a mirror URL.
pub async fn for_server(
    db: &mut PgConnection,
    server_id: Uuid,
) -> Result<Option<Served>, sqlx::Error> {
    let found: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT r.id, r.mirror_url FROM servers s
         JOIN relays r ON r.id = s.relay_id
         WHERE s.id = $1 AND r.revoked_at IS NULL AND r.mirror_url IS NOT NULL",
    )
    .bind(server_id)
    .fetch_optional(&mut *db)
    .await?;
    let Some((relay, url)) = found else {
        return Ok(None);
    };
    let mirrors = sqlx::query_as(
        "SELECT family, name, current, snapshots FROM relay_mirrors
         WHERE relay_id = $1 ORDER BY family, name",
    )
    .bind(relay)
    .fetch_all(&mut *db)
    .await?;
    Ok(Some(Served { url, mirrors }))
}

#[cfg(test)]
mod tests {
    use monitorflow_common::proto::v1;

    use super::*;

    fn served(family: &str, name: &str, current: &str, snapshots: &[&str]) -> v1::ServedMirror {
        v1::ServedMirror {
            family: family.into(),
            name: name.into(),
            current: current.into(),
            snapshots: snapshots.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    fn report(url: &str, mirrors: Vec<v1::ServedMirror>) -> RelayMirrors {
        RelayMirrors {
            public_url: url.into(),
            mirrors,
        }
    }

    #[test]
    fn reports_are_checked() {
        assert_eq!(
            check(&report("", vec![served("apt", "x", "s1", &[])])),
            Ok(None)
        );
        let ok = check(&report(
            "http://relay.example:7480/",
            vec![
                served("apt", "debian-12", "s2", &["s1", "s2"]),
                served("dnf", "rocky-9", "", &["s1"]),
            ],
        ))
        .unwrap()
        .unwrap();
        assert_eq!(ok.url, "http://relay.example:7480");
        assert_eq!(ok.mirrors[0].current.as_deref(), Some("s2"));
        assert_eq!(ok.mirrors[1].current, None);
        assert!(ok.holds(Family::Apt, "debian-12", "current"));
        assert!(ok.holds(Family::Apt, "debian-12", "s1"));
        assert!(!ok.holds(Family::Apt, "debian-12", "s3"));
        assert!(!ok.holds(Family::Dnf, "debian-12", "s1"));
        assert!(ok.holds(Family::Dnf, "rocky-9", "s1"));
        assert!(!ok.holds(Family::Dnf, "rocky-9", "current"));

        let url = "http://relay.example:7480";
        let many: Vec<String> = (0..=MAX_SNAPSHOTS).map(|i| format!("s{i}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        for bad in [
            report("ftp://relay.example", vec![]),
            report("http://user@relay.example", vec![]),
            report(url, vec![served("rpm", "x", "", &[])]),
            report(url, vec![served("apt", "../x", "", &[])]),
            report(url, vec![served("apt", "x", "..", &[])]),
            report(url, vec![served("apt", "x", "", &["a/b"])]),
            report(
                url,
                vec![served("apt", "x", "", &[]), served("apt", "x", "", &[])],
            ),
            report(url, vec![served("apt", "x", "", &many)]),
        ] {
            assert!(check(&bad).is_err(), "{bad:?}");
        }
    }
}
