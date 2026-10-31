//! Offline incidents (ADR-0057): a built-in rule on the registry's
//! liveness. A server or relay that has had no stream for
//! [`OfflineSettings::after`] opens a critical incident with the reserved
//! metric [`OFFLINE_METRIC`]; it resolves when the peer reconnects or is
//! muted. A removed peer is not watched any more: [`resolve_removed`]
//! closes all of its incidents, with no word of being back online.
//!
//! One task checks every peer every [`OfflineSettings::poll`], from the
//! database and the registry, so a restart neither loses nor duplicates an
//! incident. It owns the offline rows of `incidents`, as the threshold
//! engine owns the others.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use chrono::{DateTime, Utc};
use monitorflow_common::tls::PeerRole;
use serde_json::json;
use sqlx::{PgExecutor, PgPool};
use tokio::time::{interval, MissedTickBehavior};
use uuid::Uuid;

use crate::grpc::Registry;

/// The metric of offline incidents; sensor metrics all start with
/// `monitorflow_`, so it cannot meet a threshold's.
pub const OFFLINE_METRIC: &str = "offline";

#[derive(Debug, Clone)]
pub struct OfflineSettings {
    /// Silence before an incident opens: ten missed heartbeats at the
    /// default 30 s, longer than an agent restart or a server update.
    pub after: Duration,
    /// Time between two checks.
    pub poll: Duration,
}

impl Default for OfflineSettings {
    fn default() -> Self {
        Self {
            after: Duration::from_secs(5 * 60),
            poll: Duration::from_secs(30),
        }
    }
}

/// A server or relay row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Peer {
    Server(Uuid),
    Relay(Uuid),
}

/// What a check knows of one peer.
#[derive(Debug, Clone, PartialEq)]
pub struct Watched {
    pub peer: Peer,
    pub peer_id: String,
    /// The server's assigned relay; `None` for relays.
    pub relay: Option<Uuid>,
    /// Last time recorded, or the enrollment without one.
    pub last_seen: DateTime<Utc>,
    /// Muted for the site or the peer: no incident may be open.
    pub muted: bool,
    /// The open offline incident.
    pub incident: Option<Uuid>,
}

/// A change a check makes.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Opens an incident dated from the peer's last message.
    Open { since: DateTime<Utc> },
    /// `reason` is `back_online` or `muted`.
    Resolve {
        id: Uuid,
        at: DateTime<Utc>,
        reason: &'static str,
    },
}

/// The changes for `peers`. `online` maps the role and ID of each
/// connected peer to the start of its stream. Nobody counts as silent for
/// longer than since `started`, the start of this server, so its own
/// downtime opens nothing. A server behind a relay that is offline opens
/// nothing either: the relay's incident says it.
pub fn decide(
    peers: &[Watched],
    online: &HashMap<(PeerRole, String), DateTime<Utc>>,
    now: DateTime<Utc>,
    started: DateTime<Utc>,
    after: Duration,
) -> Vec<(Peer, Action)> {
    let connected = |p: &Watched| {
        let role = match p.peer {
            Peer::Server(_) => PeerRole::Agent,
            Peer::Relay(_) => PeerRole::Relay,
        };
        online.get(&(role, p.peer_id.clone())).copied()
    };
    let relays_down: HashSet<Uuid> = peers
        .iter()
        .filter_map(|p| match p.peer {
            Peer::Relay(id) if !p.muted && connected(p).is_none() => Some(id),
            _ => None,
        })
        .collect();
    let after = chrono::Duration::from_std(after).unwrap_or(chrono::Duration::MAX);
    peers
        .iter()
        .filter_map(|p| {
            let since = connected(p);
            let action = match (p.incident, since, p.muted) {
                (Some(id), Some(at), _) => Action::Resolve {
                    id,
                    at,
                    reason: "back_online",
                },
                (Some(id), None, true) => Action::Resolve {
                    id,
                    at: now,
                    reason: "muted",
                },
                (None, None, false)
                    if now - p.last_seen.max(started) >= after
                        && !p.relay.is_some_and(|r| relays_down.contains(&r)) =>
                {
                    Action::Open { since: p.last_seen }
                }
                _ => return None,
            };
            Some((p.peer, action))
        })
        .collect()
}

/// Every server and relay that is not removed.
pub async fn load(pool: &PgPool) -> Result<Vec<Watched>, sqlx::Error> {
    type Line = (
        bool,
        Uuid,
        String,
        Option<Uuid>,
        DateTime<Utc>,
        bool,
        Option<Uuid>,
    );
    let lines: Vec<Line> = sqlx::query_as(
        "SELECT false, s.id, s.peer_id, s.relay_id, COALESCE(s.last_seen_at, s.created_at),
                si.offline_alerts AND s.offline_alerts, i.id
         FROM servers s
         JOIN sites si ON si.id = s.site_id
         LEFT JOIN incidents i ON i.server_id = s.id AND i.metric = 'offline'
                              AND i.state = 'open'
         WHERE s.revoked_at IS NULL
         UNION ALL
         SELECT true, r.id, r.peer_id, NULL, COALESCE(r.last_seen_at, r.created_at),
                si.offline_alerts AND r.offline_alerts, i.id
         FROM relays r
         JOIN sites si ON si.id = r.site_id
         LEFT JOIN incidents i ON i.relay_id = r.id AND i.state = 'open'
         WHERE r.revoked_at IS NULL",
    )
    .fetch_all(pool)
    .await?;
    Ok(lines
        .into_iter()
        .map(
            |(relay, id, peer_id, via, last_seen, enabled, incident)| Watched {
                peer: if relay {
                    Peer::Relay(id)
                } else {
                    Peer::Server(id)
                },
                peer_id,
                relay: via,
                last_seen,
                muted: !enabled,
                incident,
            },
        )
        .collect())
}

/// Resolves every open incident, of any metric, of removed servers and
/// relays, with the reason `revoked`. Removal calls it in its transaction;
/// each check calls it again, for a threshold incident a last sample
/// opened just after. Returns the number resolved.
pub async fn resolve_removed(db: impl PgExecutor<'_>) -> Result<u64, sqlx::Error> {
    let done = sqlx::query(
        "WITH closed AS (
             UPDATE incidents SET state = 'resolved', resolved_at = GREATEST(now(), opened_at)
             WHERE state = 'open'
               AND (server_id IN (SELECT id FROM servers WHERE revoked_at IS NOT NULL)
                    OR relay_id IN (SELECT id FROM relays WHERE revoked_at IS NOT NULL))
             RETURNING id, resolved_at)
         INSERT INTO incident_events (incident_id, occurred_at, kind, details)
         SELECT id, resolved_at, 'resolved', '{\"reason\": \"revoked\"}'::jsonb FROM closed",
    )
    .execute(db)
    .await?;
    Ok(done.rows_affected())
}

/// Writes `action` with its event in one transaction. Returns the
/// incident's ID, or `None` when another writer got there first.
pub async fn apply(
    pool: &PgPool,
    peer: Peer,
    action: &Action,
    after: Duration,
) -> Result<Option<Uuid>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let (id, at, kind, severity, reason) = match *action {
        Action::Open { since } => {
            let (server, relay) = match peer {
                Peer::Server(id) => (Some(id), None),
                Peer::Relay(id) => (None, Some(id)),
            };
            let id: Option<Uuid> = sqlx::query_scalar(
                "INSERT INTO incidents (server_id, relay_id, metric, instance, severity, opened_at,
                                        last_value, last_value_at, peak_value, threshold)
                 VALUES ($1, $2, 'offline', '', 'critical', $3, 0, $3, 0, $4)
                 ON CONFLICT DO NOTHING
                 RETURNING id",
            )
            .bind(server)
            .bind(relay)
            .bind(since)
            .bind(json!({ "metric": OFFLINE_METRIC, "after_s": after.as_secs() }))
            .fetch_optional(&mut *tx)
            .await?;
            (id, since, "opened", Some("critical"), "no_heartbeat")
        }
        Action::Resolve { id, at, reason } => {
            let id: Option<Uuid> = sqlx::query_scalar(
                "UPDATE incidents SET state = 'resolved', resolved_at = GREATEST($2, opened_at)
                 WHERE id = $1 AND state = 'open'
                 RETURNING id",
            )
            .bind(id)
            .bind(at)
            .fetch_optional(&mut *tx)
            .await?;
            (id, at, "resolved", None, reason)
        }
    };
    let Some(id) = id else {
        return Ok(None);
    };
    sqlx::query(
        "INSERT INTO incident_events (incident_id, occurred_at, kind, severity, details)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(at)
    .bind(kind)
    .bind(severity)
    .bind(json!({ "reason": reason }))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Some(id))
}

/// One check: loads the peers, decides and writes. Returns the changes
/// written.
pub async fn check(
    pool: &PgPool,
    online: &HashMap<(PeerRole, String), DateTime<Utc>>,
    started: DateTime<Utc>,
    settings: &OfflineSettings,
) -> Result<Vec<(Peer, Action)>, sqlx::Error> {
    let removed = resolve_removed(pool).await?;
    if removed > 0 {
        tracing::info!(removed, "resolved the incidents of removed peers");
    }
    let peers = load(pool).await?;
    let mut written = Vec::new();
    for (peer, action) in decide(&peers, online, Utc::now(), started, settings.after) {
        if let Some(id) = apply(pool, peer, &action, settings.after).await? {
            match &action {
                Action::Open { since } => {
                    tracing::warn!(?peer, %id, %since, "offline incident opened")
                }
                Action::Resolve { reason, .. } => {
                    tracing::info!(?peer, %id, reason, "offline incident resolved")
                }
            }
            written.push((peer, action));
        }
    }
    Ok(written)
}

/// Starts the task, which checks until the process exits.
pub fn spawn(pool: PgPool, registry: Registry, settings: OfflineSettings) {
    let started = Utc::now();
    tokio::spawn(async move {
        let mut tick = interval(settings.poll);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let online = registry
                .connected()
                .into_iter()
                .map(|p| ((p.role, p.id), p.connected_at))
                .collect();
            if let Err(error) = check(&pool, &online, started, &settings).await {
                tracing::warn!(%error, "could not check offline peers; will retry");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const AFTER: Duration = Duration::from_secs(300);

    fn at(minute: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000 + minute * 60, 0).unwrap()
    }

    fn peer(peer: Peer, id: &str, last_seen: i64) -> Watched {
        Watched {
            peer,
            peer_id: id.into(),
            relay: None,
            last_seen: at(last_seen),
            muted: false,
            incident: None,
        }
    }

    fn online(peers: &[(PeerRole, &str)]) -> HashMap<(PeerRole, String), DateTime<Utc>> {
        peers
            .iter()
            .map(|&(role, id)| ((role, id.to_owned()), at(9)))
            .collect()
    }

    #[test]
    fn opens_after_the_silence_and_not_before() {
        let a = Peer::Server(Uuid::from_u128(1));
        let peers = [peer(a, "a", 0)];
        let none = online(&[]);
        assert!(decide(&peers, &none, at(4), at(-60), AFTER).is_empty());
        assert_eq!(
            decide(&peers, &none, at(5), at(-60), AFTER),
            [(a, Action::Open { since: at(0) })]
        );
        // The server's own downtime does not count.
        assert!(decide(&peers, &none, at(12), at(8), AFTER).is_empty());
        assert_eq!(decide(&peers, &none, at(13), at(8), AFTER).len(), 1);
        // Connected, or muted, opens nothing.
        let up = online(&[(PeerRole::Agent, "a")]);
        assert!(decide(&peers, &up, at(60), at(-60), AFTER).is_empty());
        let muted = [Watched {
            muted: true,
            ..peer(a, "a", 0)
        }];
        assert!(decide(&muted, &none, at(60), at(-60), AFTER).is_empty());
        // A relay with the agent's ID is another peer.
        let relay = online(&[(PeerRole::Relay, "a")]);
        assert_eq!(decide(&peers, &relay, at(60), at(-60), AFTER).len(), 1);
    }

    #[test]
    fn resolves_on_reconnect_mute_or_revocation() {
        let a = Peer::Relay(Uuid::from_u128(1));
        let id = Uuid::from_u128(9);
        let open = Watched {
            incident: Some(id),
            ..peer(a, "a", 0)
        };
        let none = online(&[]);
        assert!(decide(std::slice::from_ref(&open), &none, at(60), at(-60), AFTER).is_empty());
        let up = online(&[(PeerRole::Relay, "a")]);
        assert_eq!(
            decide(std::slice::from_ref(&open), &up, at(60), at(-60), AFTER),
            [(
                a,
                Action::Resolve {
                    id,
                    at: at(9),
                    reason: "back_online"
                }
            )]
        );
        let muted = Watched {
            muted: true,
            ..open.clone()
        };
        assert_eq!(
            decide(&[muted], &none, at(60), at(-60), AFTER),
            [(
                a,
                Action::Resolve {
                    id,
                    at: at(60),
                    reason: "muted"
                }
            )]
        );
    }

    #[test]
    fn a_down_relay_speaks_for_its_servers() {
        let relay_id = Uuid::from_u128(1);
        let relay = peer(Peer::Relay(relay_id), "r", 0);
        let behind = Watched {
            relay: Some(relay_id),
            ..peer(Peer::Server(Uuid::from_u128(2)), "a", 0)
        };
        let peers = [relay.clone(), behind.clone()];
        let none = online(&[]);
        assert_eq!(
            decide(&peers, &none, at(10), at(-60), AFTER),
            [(relay.peer, Action::Open { since: at(0) })]
        );
        // Up relay, down agent: the agent's own incident.
        let up = online(&[(PeerRole::Relay, "r")]);
        assert_eq!(
            decide(&peers, &up, at(10), at(-60), AFTER),
            [(behind.peer, Action::Open { since: at(0) })]
        );
        // A muted relay no longer covers them.
        let muted = Watched {
            muted: true,
            ..relay
        };
        assert_eq!(
            decide(&[muted, behind.clone()], &none, at(10), at(-60), AFTER),
            [(behind.peer, Action::Open { since: at(0) })]
        );
    }
}
