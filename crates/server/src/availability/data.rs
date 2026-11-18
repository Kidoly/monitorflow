//! What a report is computed from, read from PostgreSQL: the servers and
//! devices of the report's sites, their relays, the incidents and silences
//! that overlap the period, and the business services (ADR-0155).

use std::collections::{BTreeSet, HashMap};

use chrono::{DateTime, Utc};
use sqlx::PgConnection;
use uuid::Uuid;

use super::{Rule, Span, Spans};
use crate::notify::silence::CheckRef;
use crate::sensor_config::builtin_defaults;

/// A server or device of the report.
#[derive(Debug, Clone, PartialEq)]
pub struct Machine {
    pub id: Uuid,
    pub name: String,
    pub device: bool,
    pub site_id: Uuid,
    pub site_name: String,
    pub customer_id: Uuid,
    /// The relays it is reached through: an agent's relay, and the relays
    /// that run a device's checks. While one is offline it is unreachable.
    pub relays: Vec<Uuid>,
    pub created_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

/// An incident of a machine (`server_id`) or a relay (`relay_id`), with
/// the time it was critical.
#[derive(Debug, Clone, PartialEq)]
pub struct Incident {
    pub id: Uuid,
    pub server_id: Option<Uuid>,
    pub relay_id: Option<Uuid>,
    pub metric: String,
    pub instance: String,
    pub critical: Spans,
}

/// A silence, from when it counts: its start or its creation, whichever
/// is later, to its end or when it was ended.
#[derive(Debug, Clone, PartialEq)]
pub struct Silence {
    pub server_id: Option<Uuid>,
    pub relay_id: Option<Uuid>,
    pub site_id: Option<Uuid>,
    pub customer_id: Option<Uuid>,
    pub checks: Option<Vec<CheckRef>>,
    pub span: Span,
    pub reason: String,
    /// An occurrence of a maintenance window.
    pub window: bool,
}

/// What something depends on (ADR-0152): while it is down, what depends
/// on it is unreachable, which counts as down (ADR-0155).
#[derive(Debug, Clone, PartialEq)]
pub enum Parent {
    Relay(Uuid),
    /// A server, down while offline, or a device, down while a ping check
    /// of it is critical.
    Server(Uuid),
    /// One check of a server or device, down while critical.
    Check(Uuid, CheckRef),
}

/// A dependency set by someone: `server_id`, or its `check`, is behind
/// `parent`. A server's relay, and the relays running a device's checks,
/// are parents without one (`Machine::relays`).
#[derive(Debug, Clone, PartialEq)]
pub struct Dependency {
    pub server_id: Uuid,
    pub check: Option<CheckRef>,
    pub parent: Parent,
}

/// A member of a business service.
#[derive(Debug, Clone, PartialEq)]
pub struct Member {
    pub server_id: Uuid,
    pub check: Option<CheckRef>,
}

/// A business service.
#[derive(Debug, Clone, PartialEq)]
pub struct Service {
    pub id: Uuid,
    pub customer_id: Uuid,
    pub name: String,
    pub rule: Rule,
    pub target: Option<f64>,
    pub members: Vec<Member>,
}

/// Everything a report reads.
#[derive(Debug, Clone, Default)]
pub struct Data {
    pub machines: Vec<Machine>,
    pub relays: HashMap<Uuid, String>,
    pub incidents: Vec<Incident>,
    pub silences: Vec<Silence>,
    pub services: Vec<Service>,
    pub dependencies: Vec<Dependency>,
    /// Servers and devices outside the report that something of it
    /// depends on: read for their down time, never reported.
    pub parents: Vec<Machine>,
    /// Every check name the central server knows, to tell which check
    /// wrote a metric (ADR-0151).
    pub names: Vec<String>,
}

/// Which servers and services a report covers.
#[derive(Debug, Clone)]
pub struct Reach {
    /// The sites; `None` for all of them.
    pub sites: Option<Vec<Uuid>>,
    /// The customers whose services are included; `None` for all, empty
    /// for none.
    pub services_of: Option<Vec<Uuid>>,
}

/// An incident event: `(kind, severity, occurred_at)`.
pub type EventRow = (String, Option<String>, DateTime<Utc>);

/// The critical time of an incident from its events in order, until `end`
/// while still open.
pub fn critical_spans(opened_at: DateTime<Utc>, events: &[EventRow], end: DateTime<Utc>) -> Spans {
    let mut spans = Vec::new();
    let mut since: Option<DateTime<Utc>> = None;
    for (kind, severity, at) in events {
        let at = if kind == "opened" { opened_at } else { *at };
        let critical = severity.as_deref() == Some("critical");
        match (since, critical) {
            (None, true) => since = Some(at),
            (Some(start), false) => {
                spans.push((start, at));
                since = None;
            }
            _ => {}
        }
        if kind == "resolved" {
            break;
        }
    }
    if let Some(start) = since {
        spans.push((start, end));
    }
    Spans::from_spans(spans)
}

type MachineRow = (
    Uuid,
    String,
    String,
    Uuid,
    String,
    Uuid,
    Option<Uuid>,
    DateTime<Utc>,
    Option<DateTime<Utc>>,
);

type SilenceRow = (
    Option<Uuid>,
    Option<Uuid>,
    Option<Uuid>,
    Option<Uuid>,
    Option<sqlx::types::Json<Vec<CheckRef>>>,
    DateTime<Utc>,
    DateTime<Utc>,
    String,
    bool,
);

/// Reads what a report over `period` within `reach` needs; `now` ends what
/// is still open.
pub async fn load(
    db: &mut PgConnection,
    reach: &Reach,
    period: Span,
    now: DateTime<Utc>,
) -> Result<Data, sqlx::Error> {
    let (from, to) = period;
    let end = to.min(now);
    let (all_sites, sites) = match &reach.sites {
        None => (true, Vec::new()),
        Some(sites) => (false, sites.clone()),
    };
    let rows: Vec<MachineRow> = sqlx::query_as(
        "SELECT s.id, s.hostname, s.kind, s.site_id, si.name, si.customer_id, s.relay_id,
                s.created_at, s.revoked_at
         FROM servers s JOIN sites si ON si.id = s.site_id
         WHERE ($1 OR s.site_id = ANY($2)) AND s.created_at < $4
           AND (s.revoked_at IS NULL OR s.revoked_at > $3)
         ORDER BY si.name, s.hostname, s.id",
    )
    .bind(all_sites)
    .bind(&sites)
    .bind(from)
    .bind(end)
    .fetch_all(&mut *db)
    .await?;
    let ids: Vec<Uuid> = rows.iter().map(|r| r.0).collect();
    let machines = with_relays(db, rows).await?;
    let (dependencies, parents) = dependencies(db, &ids).await?;
    let relay_ids: Vec<Uuid> = machines
        .iter()
        .chain(&parents)
        .flat_map(|m| m.relays.iter().copied())
        .chain(dependencies.iter().filter_map(|d| match d.parent {
            Parent::Relay(relay) => Some(relay),
            _ => None,
        }))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let read: Vec<Uuid> = ids
        .iter()
        .copied()
        .chain(parents.iter().map(|m| m.id))
        .collect();
    let relays: HashMap<Uuid, String> =
        sqlx::query_as::<_, (Uuid, String)>("SELECT id, name FROM relays WHERE id = ANY($1)")
            .bind(&relay_ids)
            .fetch_all(&mut *db)
            .await?
            .into_iter()
            .collect();
    let incidents = incidents(db, &read, &relay_ids, from, end).await?;
    let silences: Vec<SilenceRow> = sqlx::query_as(
        "SELECT x.server_id, x.relay_id, x.site_id, x.customer_id, x.checks,
                GREATEST(x.starts_at, x.created_at), coalesce(x.ended_at, x.ends_at),
                x.reason, x.window_id IS NOT NULL
         FROM silences x
         WHERE GREATEST(x.starts_at, x.created_at) < $2
           AND coalesce(x.ended_at, x.ends_at) > $1
           AND coalesce(x.ended_at, x.ends_at) > GREATEST(x.starts_at, x.created_at)
         ORDER BY 6",
    )
    .bind(from)
    .bind(end)
    .fetch_all(&mut *db)
    .await?;
    let silences = silences
        .into_iter()
        .map(|r| Silence {
            server_id: r.0,
            relay_id: r.1,
            site_id: r.2,
            customer_id: r.3,
            checks: r.4.map(|c| c.0),
            span: (r.5, r.6),
            reason: r.7,
            window: r.8,
        })
        .collect();
    let services = services(db, reach).await?;
    let mut names: Vec<String> = sqlx::query_scalar("SELECT DISTINCT name FROM sensor_configs")
        .fetch_all(&mut *db)
        .await?;
    names.extend(builtin_defaults(0).into_iter().map(|spec| spec.name));
    names.sort_unstable();
    names.dedup();
    Ok(Data {
        machines,
        relays,
        incidents,
        silences,
        services,
        dependencies,
        parents,
        names,
    })
}

/// `rows` as machines, with the relays they are reached through: their
/// own, and those that run their checks.
async fn with_relays(
    db: &mut PgConnection,
    rows: Vec<MachineRow>,
) -> Result<Vec<Machine>, sqlx::Error> {
    let ids: Vec<Uuid> = rows.iter().map(|r| r.0).collect();
    let probes: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT DISTINCT server_id, probe_relay_id FROM sensor_configs
         WHERE server_id = ANY($1) AND probe_relay_id IS NOT NULL AND enabled",
    )
    .bind(&ids)
    .fetch_all(&mut *db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            let mut relays: BTreeSet<Uuid> = r.6.into_iter().collect();
            relays.extend(probes.iter().filter(|p| p.0 == r.0).map(|p| p.1));
            Machine {
                id: r.0,
                name: r.1,
                device: r.2 == "device",
                site_id: r.3,
                site_name: r.4,
                customer_id: r.5,
                relays: relays.into_iter().collect(),
                created_at: r.7,
                revoked_at: r.8,
            }
        })
        .collect())
}

/// How many levels of parents are read: cycles are refused when set, this
/// only bounds the work.
const MAX_PARENT_LEVELS: usize = 16;

type DependencyRow = (
    Uuid,
    Option<String>,
    String,
    Option<Uuid>,
    Option<Uuid>,
    Option<String>,
    String,
);

/// The dependencies (ADR-0152) of `servers` and of their parents, level by
/// level, and the parents outside them. Nothing before the table exists.
async fn dependencies(
    db: &mut PgConnection,
    servers: &[Uuid],
) -> Result<(Vec<Dependency>, Vec<Machine>), sqlx::Error> {
    let exists: bool = sqlx::query_scalar("SELECT to_regclass('dependencies') IS NOT NULL")
        .fetch_one(&mut *db)
        .await?;
    if !exists {
        return Ok((Vec::new(), Vec::new()));
    }
    let check =
        |name: Option<String>, instance: String| name.map(|name| CheckRef { name, instance });
    let mut seen: BTreeSet<Uuid> = servers.iter().copied().collect();
    let mut frontier: Vec<Uuid> = servers.to_vec();
    let mut found = Vec::new();
    let mut outside = Vec::new();
    for _ in 0..MAX_PARENT_LEVELS {
        if frontier.is_empty() {
            break;
        }
        // SQL-safe: a constant query.
        let rows: Vec<DependencyRow> = sqlx::query_as(
            "SELECT server_id, check_name, check_instance, parent_server_id, parent_relay_id,
                    parent_check_name, parent_check_instance
             FROM dependencies WHERE server_id = ANY($1)",
        )
        .bind(&frontier)
        .fetch_all(&mut *db)
        .await?;
        frontier.clear();
        for (server, name, instance, parent_server, parent_relay, parent_name, parent_instance) in
            rows
        {
            let parent = match (parent_server, parent_relay) {
                (Some(id), _) => {
                    if seen.insert(id) {
                        frontier.push(id);
                        outside.push(id);
                    }
                    match check(parent_name, parent_instance) {
                        Some(c) => Parent::Check(id, c),
                        None => Parent::Server(id),
                    }
                }
                (None, Some(relay)) => Parent::Relay(relay),
                (None, None) => continue,
            };
            found.push(Dependency {
                server_id: server,
                check: check(name, instance),
                parent,
            });
        }
    }
    let rows: Vec<MachineRow> = sqlx::query_as(
        "SELECT s.id, s.hostname, s.kind, s.site_id, si.name, si.customer_id, s.relay_id,
                s.created_at, s.revoked_at
         FROM servers s JOIN sites si ON si.id = s.site_id WHERE s.id = ANY($1)",
    )
    .bind(&outside)
    .fetch_all(&mut *db)
    .await?;
    Ok((found, with_relays(db, rows).await?))
}

/// The incidents of `servers` and `relays` that were open at some point
/// between `from` and `end`, with their critical time.
async fn incidents(
    db: &mut PgConnection,
    servers: &[Uuid],
    relays: &[Uuid],
    from: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<Vec<Incident>, sqlx::Error> {
    type Row = (
        Uuid,
        Option<Uuid>,
        Option<Uuid>,
        String,
        String,
        DateTime<Utc>,
    );
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, server_id, relay_id, metric, instance, opened_at FROM incidents
         WHERE (server_id = ANY($1) OR relay_id = ANY($2))
           AND opened_at < $4 AND (resolved_at IS NULL OR resolved_at > $3)
         ORDER BY opened_at, id",
    )
    .bind(servers)
    .bind(relays)
    .bind(from)
    .bind(end)
    .fetch_all(&mut *db)
    .await?;
    let ids: Vec<Uuid> = rows.iter().map(|r| r.0).collect();
    let events: Vec<(Uuid, String, Option<String>, DateTime<Utc>)> = sqlx::query_as(
        "SELECT incident_id, kind, severity, occurred_at FROM incident_events
         WHERE incident_id = ANY($1) ORDER BY incident_id, id",
    )
    .bind(&ids)
    .fetch_all(&mut *db)
    .await?;
    let mut by_incident: HashMap<Uuid, Vec<EventRow>> = HashMap::new();
    for (id, kind, severity, at) in events {
        by_incident
            .entry(id)
            .or_default()
            .push((kind, severity, at));
    }
    Ok(rows
        .into_iter()
        .map(|r| Incident {
            critical: critical_spans(r.5, by_incident.get(&r.0).map_or(&[], Vec::as_slice), end),
            id: r.0,
            server_id: r.1,
            relay_id: r.2,
            metric: r.3,
            instance: r.4,
        })
        .filter(|i| !i.critical.is_empty())
        .collect())
}

async fn services(db: &mut PgConnection, reach: &Reach) -> Result<Vec<Service>, sqlx::Error> {
    let (all, customers) = match &reach.services_of {
        None => (true, Vec::new()),
        Some(customers) => (false, customers.clone()),
    };
    type Row = (Uuid, Uuid, String, String, Option<i32>, Option<f64>);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, customer_id, name, rule, at_least, target FROM business_services
         WHERE $1 OR customer_id = ANY($2) ORDER BY name, id",
    )
    .bind(all)
    .bind(&customers)
    .fetch_all(&mut *db)
    .await?;
    let ids: Vec<Uuid> = rows.iter().map(|r| r.0).collect();
    let members: Vec<(Uuid, Uuid, Option<String>, String)> = sqlx::query_as(
        "SELECT service_id, server_id, check_name, check_instance FROM business_service_members
         WHERE service_id = ANY($1) ORDER BY service_id, position, server_id, check_name",
    )
    .bind(&ids)
    .fetch_all(&mut *db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Service {
            members: members
                .iter()
                .filter(|m| m.0 == r.0)
                .map(|m| Member {
                    server_id: m.1,
                    check: m.2.clone().map(|name| CheckRef {
                        name,
                        instance: m.3.clone(),
                    }),
                })
                .collect(),
            id: r.0,
            customer_id: r.1,
            name: r.2,
            rule: match (r.3.as_str(), r.4) {
                ("at_least", Some(n)) => Rule::AtLeast(u32::try_from(n).unwrap_or(1)),
                _ => Rule::All,
            },
            target: r.5,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(m: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_790_000_000 + m * 60, 0).unwrap()
    }

    fn e(kind: &str, severity: Option<&str>, m: i64) -> (String, Option<String>, DateTime<Utc>) {
        (kind.into(), severity.map(Into::into), t(m))
    }

    #[test]
    fn only_critical_time_counts() {
        let events = [
            e("opened", Some("warning"), 0),
            e("escalated", Some("critical"), 10),
            e("silenced", Some("critical"), 12),
            e("deescalated", Some("warning"), 20),
            e("escalated", Some("critical"), 30),
            e("resolved", None, 45),
        ];
        let spans = critical_spans(t(0), &events, t(100));
        assert_eq!(spans, Spans::from_spans([(t(10), t(20)), (t(30), t(45))]));
    }

    #[test]
    fn an_open_incident_counts_until_the_end() {
        // An offline incident is dated from the last message.
        let events = [e("opened", Some("critical"), 5)];
        let spans = critical_spans(t(2), &events, t(60));
        assert_eq!(spans, Spans::one(t(2), t(60)));
        assert!(critical_spans(t(0), &[e("opened", Some("warning"), 0)], t(60)).is_empty());
    }
}
