//! Dependencies and the facts about their parents, from PostgreSQL
//! (ADR-0152).
//!
//! A parent is down while:
//! - an agent's or relay's offline incident is open (ADR-0072);
//! - a server or device has a critical incident open from a ping check;
//! - a check has a critical incident open.
//!
//! It is in doubt while an agent or relay whose offline alerts are on has
//! no stream, or while the critical condition of such a check holds but
//! has not lasted its duration yet (`alert_states`).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use monitorflow_common::tls::PeerRole;
use sqlx::PgExecutor;
use uuid::Uuid;

use super::holds::Down;
use super::{CheckRef, Edge, Facts, Holds, Node, RECENT};
use crate::alerting::offline::OFFLINE_METRIC;
use crate::notify::silence::check_covers;
use crate::sensor_config::builtin_defaults;

/// Who has a stream now. Without the registry, an agent or relay counts
/// as without one when nothing was recorded from it for
/// [`SILENT_AFTER`].
#[derive(Debug, Clone, Copy)]
pub enum Streams<'a> {
    Live(&'a HashSet<(PeerRole, String)>),
    LastSeen,
}

/// Without the registry: `last_seen_at` is recorded to within a minute and
/// heartbeats come every 30 seconds.
pub const SILENT_AFTER: chrono::TimeDelta = chrono::TimeDelta::minutes(2);

impl Streams<'_> {
    fn silent(
        &self,
        role: PeerRole,
        peer_id: &str,
        last_seen: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> bool {
        match self {
            Self::Live(online) => !online.contains(&(role, peer_id.to_owned())),
            Self::LastSeen => now - last_seen >= SILENT_AFTER,
        }
    }
}

fn check_node(server: Uuid, name: Option<String>, instance: String) -> Node {
    match name {
        Some(name) => Node::Check(server, name, instance),
        None => Node::Server(server),
    }
}

/// Every dependency between servers and relays that are not removed, and
/// the automatic ones: a server behind its relay, a check behind the relay
/// that runs it.
pub async fn edges(db: &mut sqlx::PgConnection) -> Result<Vec<Edge>, sqlx::Error> {
    type Line = (
        Uuid,
        Uuid,
        Option<String>,
        String,
        Option<Uuid>,
        Option<Uuid>,
        Option<String>,
        String,
    );
    let lines: Vec<Line> = sqlx::query_as(
        "SELECT d.id, d.server_id, d.check_name, d.check_instance, d.parent_server_id,
                d.parent_relay_id, d.parent_check_name, d.parent_check_instance
         FROM dependencies d
         JOIN servers s ON s.id = d.server_id
         LEFT JOIN servers p ON p.id = d.parent_server_id
         LEFT JOIN relays r ON r.id = d.parent_relay_id
         WHERE s.revoked_at IS NULL
           AND (d.parent_server_id IS NULL OR p.revoked_at IS NULL)
           AND (d.parent_relay_id IS NULL OR r.revoked_at IS NULL)",
    )
    .fetch_all(&mut *db)
    .await?;
    let mut edges: Vec<Edge> = lines
        .into_iter()
        .filter_map(
            |(
                id,
                server,
                check,
                instance,
                parent_server,
                parent_relay,
                parent_check,
                parent_instance,
            )| {
                let parent = match (parent_server, parent_relay) {
                    (Some(p), _) => check_node(p, parent_check, parent_instance),
                    (None, Some(r)) => Node::Relay(r),
                    (None, None) => return None,
                };
                Some(Edge {
                    id: Some(id),
                    child: check_node(server, check, instance),
                    parent,
                })
            },
        )
        .collect();
    let behind_relays: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT s.id, s.relay_id FROM servers s JOIN relays r ON r.id = s.relay_id
         WHERE s.revoked_at IS NULL AND r.revoked_at IS NULL",
    )
    .fetch_all(&mut *db)
    .await?;
    edges.extend(
        behind_relays
            .into_iter()
            .map(|(server, relay)| Edge::automatic(Node::Server(server), Node::Relay(relay))),
    );
    let run_by_relays: Vec<(Uuid, String, String, Uuid)> = sqlx::query_as(
        "SELECT c.server_id, c.name, c.instance, c.probe_relay_id
         FROM sensor_configs c
         JOIN servers s ON s.id = c.server_id
         JOIN relays r ON r.id = c.probe_relay_id
         WHERE c.probe = 'relay' AND c.enabled AND s.revoked_at IS NULL AND r.revoked_at IS NULL",
    )
    .fetch_all(&mut *db)
    .await?;
    edges.extend(
        run_by_relays
            .into_iter()
            .map(|(server, name, instance, relay)| {
                Edge::automatic(Node::Check(server, name, instance), Node::Relay(relay))
            }),
    );
    Ok(edges)
}

/// Every check name the central server knows.
pub async fn check_names(db: impl PgExecutor<'_>) -> Result<Vec<String>, sqlx::Error> {
    let mut names: Vec<String> = sqlx::query_scalar("SELECT DISTINCT name FROM sensor_configs")
        .fetch_all(db)
        .await?;
    names.extend(builtin_defaults(0).into_iter().map(|spec| spec.name));
    names.sort();
    names.dedup();
    Ok(names)
}

/// What is known now of the parents of `edges`.
pub async fn facts(
    db: &mut sqlx::PgConnection,
    edges: &[Edge],
    streams: Streams<'_>,
    now: DateTime<Utc>,
) -> Result<Facts, sqlx::Error> {
    let names = Arc::new(check_names(&mut *db).await?);
    let parents: HashSet<&Node> = edges.iter().map(|e| &e.parent).collect();
    let relays: Vec<Uuid> = parents
        .iter()
        .filter_map(|n| match n {
            Node::Relay(id) => Some(*id),
            _ => None,
        })
        .collect();
    let mut servers: Vec<Uuid> = parents.iter().filter_map(|n| n.server()).collect();
    servers.sort();
    servers.dedup();
    let mut facts = Facts {
        names: names.clone(),
        ..Facts::default()
    };
    if relays.is_empty() && servers.is_empty() {
        return Ok(facts);
    }

    // Which checks of each parent server are ping checks.
    let pings: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT server_id, name, instance FROM sensor_configs
         WHERE server_id = ANY($1) AND kind = 'ping' AND enabled",
    )
    .bind(&servers)
    .fetch_all(&mut *db)
    .await?;
    let mut ping_checks: HashMap<Uuid, Vec<CheckRef>> = HashMap::new();
    for (server, name, instance) in pings {
        ping_checks
            .entry(server)
            .or_default()
            .push(CheckRef { name, instance });
    }
    let check_parents: Vec<(Uuid, CheckRef, &Node)> = parents
        .iter()
        .filter_map(|n| Some((n.server()?, n.check_ref()?, *n)))
        .collect();
    // The nodes a series of `server` makes down or in doubt.
    let nodes_of = |server: Uuid, metric: &str, instance: &str| -> Vec<Node> {
        if metric == OFFLINE_METRIC {
            return vec![Node::Server(server)];
        }
        let mut nodes = Vec::new();
        if ping_checks
            .get(&server)
            .into_iter()
            .flatten()
            .any(|c| check_covers(c, &names, metric, instance))
        {
            nodes.push(Node::Server(server));
        }
        nodes.extend(
            check_parents
                .iter()
                .filter(|(s, c, _)| *s == server && check_covers(c, &names, metric, instance))
                .map(|(.., n)| (*n).clone()),
        );
        nodes
    };

    type Incident = (
        Uuid,
        Option<Uuid>,
        Option<Uuid>,
        String,
        String,
        DateTime<Utc>,
        Option<DateTime<Utc>>,
    );
    let incidents: Vec<Incident> = sqlx::query_as(
        "SELECT id, server_id, relay_id, metric, instance, opened_at, resolved_at
         FROM incidents
         WHERE (state = 'open' OR resolved_at > $3)
           AND (relay_id = ANY($1) OR server_id = ANY($2))
           AND (metric = 'offline' OR severity = 'critical')
           AND hidden_by IS NULL
         ORDER BY opened_at",
    )
    .bind(&relays)
    .bind(&servers)
    .bind(now - RECENT)
    .fetch_all(&mut *db)
    .await?;
    for (id, server, relay, metric, instance, opened, resolved) in incidents {
        let nodes = match (server, relay) {
            (_, Some(relay)) => vec![Node::Relay(relay)],
            (Some(server), None) => nodes_of(server, &metric, &instance),
            (None, None) => Vec::new(),
        };
        for node in nodes {
            match resolved {
                None => {
                    facts.down.entry(node).or_insert(Down {
                        incident: id,
                        since: opened,
                    });
                }
                Some(at) => {
                    let last = facts.recovered.entry(node).or_insert(at);
                    *last = (*last).max(at);
                }
            }
        }
    }
    let down: HashSet<Node> = facts.down.keys().cloned().collect();
    facts.recovered.retain(|node, _| !down.contains(node));

    // Agents and relays without a stream, whose offline incident may come.
    let peers: Vec<(bool, Uuid, String, DateTime<Utc>)> = sqlx::query_as(
        "SELECT false, s.id, s.peer_id, COALESCE(s.last_seen_at, s.created_at)
         FROM servers s JOIN sites si ON si.id = s.site_id
         WHERE s.id = ANY($2) AND s.kind = 'agent' AND NOT s.awaiting_agent
           AND s.revoked_at IS NULL AND si.offline_alerts AND s.offline_alerts
         UNION ALL
         SELECT true, r.id, r.peer_id, COALESCE(r.last_seen_at, r.created_at)
         FROM relays r JOIN sites si ON si.id = r.site_id
         WHERE r.id = ANY($1) AND r.revoked_at IS NULL AND si.offline_alerts AND r.offline_alerts",
    )
    .bind(&relays)
    .bind(&servers)
    .fetch_all(&mut *db)
    .await?;
    for (relay, id, peer_id, last_seen) in peers {
        let (role, node) = if relay {
            (PeerRole::Relay, Node::Relay(id))
        } else {
            (PeerRole::Agent, Node::Server(id))
        };
        if !down.contains(&node) && streams.silent(role, &peer_id, last_seen, now) {
            facts.doubt.insert(node);
        }
    }

    // Critical conditions that have not lasted their duration yet.
    let pending: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT server_id, metric, instance FROM alert_states
         WHERE server_id = ANY($1) AND critical_since IS NOT NULL
           AND severity IS DISTINCT FROM 'critical'",
    )
    .bind(&servers)
    .fetch_all(&mut *db)
    .await?;
    for (server, metric, instance) in pending {
        for node in nodes_of(server, &metric, &instance) {
            if !down.contains(&node) {
                facts.doubt.insert(node);
            }
        }
    }
    Ok(facts)
}

/// The holds now.
pub async fn holds(
    db: &mut sqlx::PgConnection,
    streams: Streams<'_>,
) -> Result<Holds, sqlx::Error> {
    let now = Utc::now();
    let edges = edges(&mut *db).await?;
    let facts = facts(&mut *db, &edges, streams, now).await?;
    Ok(Holds::new(&edges, facts, now))
}
