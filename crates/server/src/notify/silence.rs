//! Silences (ADR-0079): what stays unsaid for a while, and what is said
//! once the silence is over.
//!
//! A silence never touches incidents. The dispatcher [`hold`]s the outbox
//! rows a silence covers, and [`release`] looks at them again when no
//! silence does: an incident gets one message, or none if nobody needs one
//! ([`settle`]). All of it lives in the database, so a restart loses
//! nothing.
//!
//! What a release sends is owed: chat pacing (ADR-0077) never gathers it
//! into a summary, and a channel still limiting after every try gets it
//! again at the next pass through [`requeue`], for that channel alone.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use sqlx::{PgConnection, PgExecutor, PgPool};
use uuid::Uuid;

use super::message::Event;

/// A row of `silences`, for the dispatcher.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Silence {
    pub id: Uuid,
    pub server_id: Option<Uuid>,
    pub relay_id: Option<Uuid>,
    pub site_id: Option<Uuid>,
    pub customer_id: Option<Uuid>,
    pub starts_at: DateTime<Utc>,
    /// When it is over: as planned, or earlier when ended by hand.
    pub ends_at: DateTime<Utc>,
}

/// The ID in `details[key].id`, like the server of an event.
fn id_at(details: &Value, key: &str) -> Option<Uuid> {
    details.get(key)?.get("id")?.as_str()?.parse().ok()
}

impl Silence {
    /// Whether `event` is about this silence's scope. An event of the whole
    /// server, like a failed backup, is only about everything.
    pub fn concerns(&self, event: &Event) -> bool {
        match (
            self.server_id,
            self.relay_id,
            self.site_id,
            self.customer_id,
        ) {
            (Some(server), ..) => id_at(&event.details, "server") == Some(server),
            (_, Some(relay), ..) => id_at(&event.details, "relay") == Some(relay),
            (_, _, Some(site), _) => id_at(&event.details, "site") == Some(site),
            (.., Some(customer)) => event.customer_id == Some(customer),
            _ => true,
        }
    }

    /// From its start, included, to its end, excluded.
    pub fn active(&self, now: DateTime<Utc>) -> bool {
        self.starts_at <= now && now < self.ends_at
    }

    /// Whether a pending `event` waits for this silence: it started by
    /// `now`, and the event was written before it ended. The second part
    /// covers a server that was down when the silence ended; the event goes
    /// through [`release`] like the others. An offline incident is dated
    /// from the last message, so the event's time is when it was written,
    /// not when it says it happened.
    pub fn holds(&self, event: &Event, now: DateTime<Utc>) -> bool {
        self.starts_at <= now && event.created_at < self.ends_at && self.concerns(event)
    }
}

/// Silences started by `now` and over after `after`, without those called
/// off before they started.
pub async fn load(
    db: impl PgExecutor<'_>,
    now: DateTime<Utc>,
    after: DateTime<Utc>,
) -> Result<Vec<Silence>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, server_id, relay_id, site_id, customer_id, starts_at,
                coalesce(ended_at, ends_at) AS ends_at
         FROM silences
         WHERE starts_at <= $1 AND coalesce(ended_at, ends_at) > GREATEST(starts_at, $2)",
    )
    .bind(now)
    .bind(after)
    .fetch_all(db)
    .await
}

/// The incident an event is about.
fn incident_of(event: &Event) -> Option<Uuid> {
    id_at(&event.details, "incident").filter(|_| event.event.starts_with("incident."))
}

/// The row of the incident's held `rows`, in order, to send once no silence
/// covers it; the others are dropped. `None` says nothing at all.
///
/// - Still open: one message with its state now, whatever happened during
///   the silence. Nothing open stays unsaid.
/// - Resolved, and someone was told it opened: the resolution, so that
///   nobody waits for it.
/// - Opened and resolved during the silence: nothing. It stays in the
///   history.
pub fn settle(rows: &[&Event], open: bool) -> Option<i64> {
    let told = !rows.iter().any(|e| e.event == "incident.opened");
    match (open, told) {
        (true, _) => rows.first().map(|e| e.id),
        (false, true) => rows
            .iter()
            .find(|e| e.event == "incident.resolved")
            .map(|e| e.id),
        (false, false) => None,
    }
}

/// Whether a rendered message is owed by a release: it carries
/// `silence`, which only [`release`] writes.
pub fn owed(payload: &Value) -> bool {
    payload.get("silence").is_some()
}

/// The incidents with rows held back. A new event of one of them waits
/// with them, so that it never goes out before what they will say.
pub async fn held_incidents(db: impl PgExecutor<'_>) -> Result<HashSet<Uuid>, sqlx::Error> {
    let ids: Vec<Option<String>> = sqlx::query_scalar(
        "SELECT DISTINCT details #>> '{incident,id}' FROM notification_outbox
         WHERE held_at IS NOT NULL AND sent_at IS NULL AND event LIKE 'incident.%'",
    )
    .fetch_all(db)
    .await?;
    Ok(ids
        .into_iter()
        .flatten()
        .filter_map(|id| id.parse().ok())
        .collect())
}

/// Whether a pending `event` waits: held by a silence, or behind its
/// incident's held rows.
pub fn waits(event: &Event, silence: Option<&Silence>, held: &HashSet<Uuid>) -> bool {
    silence.is_some() || incident_of(event).is_some_and(|i| held.contains(&i))
}

/// Keeps `event` from being delivered. The incident's timeline says so
/// when `silence` holds the first of its rows.
pub async fn hold(
    db: &mut PgConnection,
    event: &Event,
    silence: Option<Uuid>,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    if let (Some(incident), Some(silence)) = (incident_of(event), silence) {
        sqlx::query(
            "INSERT INTO incident_events (incident_id, occurred_at, kind, severity, details)
             SELECT i.id, $2, 'silenced', i.severity, $3 FROM incidents i
             WHERE i.id = $1 AND NOT EXISTS (
                 SELECT 1 FROM notification_outbox o
                 WHERE o.held_at IS NOT NULL AND o.sent_at IS NULL
                   AND o.details #>> '{incident,id}' = i.id::text)",
        )
        .bind(incident)
        .bind(now)
        .bind(json!({ "silence_id": silence }))
        .execute(&mut *db)
        .await?;
    }
    sqlx::query("UPDATE notification_outbox SET held_at = $2 WHERE id = $1")
        .bind(event.id)
        .bind(now)
        .execute(&mut *db)
        .await?;
    Ok(())
}

/// Room for owed messages in one pass of the narrowest chat channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// Messages it may be sent in a pass without gathering.
    pub messages: usize,
    /// Lines its group messages list.
    pub lines: usize,
}

impl Budget {
    /// Messages for owed rows: `incidents` per customer, which go out in
    /// groups, and `others`, one each.
    fn needs(self, incidents: &HashMap<Option<Uuid>, usize>, others: usize) -> usize {
        let lines = self.lines.max(1);
        incidents
            .iter()
            .map(|(customer, n)| match customer {
                Some(_) => n.div_ceil(lines),
                None => *n,
            })
            .sum::<usize>()
            + others
    }
}

/// What a release does with one incident's held rows, or one other event.
enum Unit<'a> {
    Incident(Uuid, Vec<&'a Event>),
    Other(&'a Event),
}

impl Unit<'_> {
    fn rows(&self) -> Vec<&Event> {
        match self {
            Self::Incident(_, rows) => rows.clone(),
            Self::Other(event) => vec![event],
        }
    }
}

/// Lets go of the held rows no silence covers any more, marked with
/// `silence` in their details. A row sent goes back to the outbox as new:
/// the silence may have lasted longer than events are kept pending.
///
/// `budget` is how many owed messages may be pending at once, the room a
/// paced chat channel has in one pass (ADR-0077); `None` without one.
/// Incidents of one customer go out in group messages (ADR-0080), so they
/// count one message per [`Budget::lines`]. What does not fit stays held,
/// whole, for a later pass, so that pacing never has to gather or drop it.
/// Returns how many rows will be sent.
pub async fn release(
    pool: &PgPool,
    now: DateTime<Utc>,
    budget: Option<Budget>,
) -> Result<usize, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let held: Vec<Event> = sqlx::query_as(
        "SELECT id, event, customer_id, site_id, details, created_at FROM notification_outbox
         WHERE held_at IS NOT NULL AND sent_at IS NULL
         ORDER BY id FOR UPDATE SKIP LOCKED",
    )
    .fetch_all(&mut *tx)
    .await?;
    if held.is_empty() {
        return Ok(0);
    }
    let silences = load(&mut *tx, now, now).await?;
    // Owed rows not sent yet, waiting for their window or a channel.
    let mut owed_incidents: HashMap<Option<Uuid>, usize> = HashMap::new();
    let mut owed_others = 0;
    if budget.is_some() {
        let pending: Vec<(Option<Uuid>, bool)> = sqlx::query_as(
            "SELECT customer_id, event LIKE 'incident.%' FROM notification_outbox
             WHERE sent_at IS NULL AND held_at IS NULL AND details ? 'silence'",
        )
        .fetch_all(&mut *tx)
        .await?;
        for (customer, incident) in pending {
            if incident {
                *owed_incidents.entry(customer).or_default() += 1;
            } else {
                owed_others += 1;
            }
        }
    }
    let mut incidents: BTreeMap<Uuid, Vec<&Event>> = BTreeMap::new();
    let mut others = Vec::new();
    for event in held
        .iter()
        .filter(|e| !silences.iter().any(|s| s.active(now) && s.concerns(e)))
    {
        match incident_of(event) {
            Some(incident) => incidents.entry(incident).or_default().push(event),
            None => others.push(event),
        }
    }

    type Line = (Uuid, bool, f64, f64, String);
    let ids: Vec<Uuid> = incidents.keys().copied().collect();
    let states: HashMap<Uuid, Line> = sqlx::query_as::<_, Line>(
        "SELECT id, state = 'open', last_value, peak_value, severity
         FROM incidents WHERE id = ANY($1)",
    )
    .bind(&ids)
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .map(|line| (line.0, line))
    .collect();

    let approvals: Vec<Uuid> = others
        .iter()
        .filter_map(|e| id_at(&e.details, "approval"))
        .collect();
    let waiting: HashSet<Uuid> = sqlx::query_scalar(
        "SELECT id FROM remediation_pending_commands
         WHERE id = ANY($1) AND state = 'pending'",
    )
    .bind(&approvals)
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .collect();

    // Oldest first, so that what waits longest goes first.
    let mut units: Vec<Unit> = incidents
        .iter()
        .map(|(incident, rows)| Unit::Incident(*incident, rows.clone()))
        .chain(others.iter().map(|e| Unit::Other(e)))
        .collect();
    units.sort_by_key(|u| u.rows().first().map(|e| e.id));

    let mut send: Vec<(i64, Value)> = Vec::new();
    let mut settled: Vec<i64> = Vec::new();
    for unit in &units {
        let chosen = match unit {
            Unit::Incident(incident, rows) => {
                let state = states.get(incident);
                let open = state.is_some_and(|s| s.1);
                settle(rows, open)
                    .and_then(|id| rows.iter().find(|e| e.id == id))
                    .map(|event| {
                        let mut details = event.details.clone();
                        if let (true, Some((.., value, peak, severity))) = (open, state) {
                            details["incident"]["value"] = json!(value);
                            details["incident"]["peak_value"] = json!(peak);
                            details["incident"]["severity"] = json!(severity);
                        }
                        (event.id, details)
                    })
            }
            // A command nobody can approve any more needs no message; the
            // rest happened once and nothing else will tell it.
            Unit::Other(event) => match id_at(&event.details, "approval") {
                Some(approval) if !waiting.contains(&approval) => None,
                _ => Some((event.id, event.details.clone())),
            },
        };
        if let (Some(budget), true) = (budget, chosen.is_some()) {
            let customer = unit.rows().first().and_then(|e| e.customer_id);
            let (mut incidents, mut others) = (owed_incidents.clone(), owed_others);
            match unit {
                Unit::Incident(..) => *incidents.entry(customer).or_default() += 1,
                Unit::Other(_) => others += 1,
            }
            if budget.needs(&incidents, others) > budget.messages {
                continue;
            }
            (owed_incidents, owed_others) = (incidents, others);
        }
        if let Unit::Incident(incident, _) = unit {
            sqlx::query(
                "INSERT INTO incident_events (incident_id, occurred_at, kind, severity, details)
                 SELECT id, $2, 'silence_ended', severity, $3 FROM incidents WHERE id = $1",
            )
            .bind(incident)
            .bind(now)
            .bind(json!({ "notified": chosen.is_some() }))
            .execute(&mut *tx)
            .await?;
        }
        settled.extend(unit.rows().iter().map(|e| e.id));
        send.extend(chosen);
    }

    let sent: HashSet<i64> = send.iter().map(|(id, _)| *id).collect();
    let dropped: Vec<i64> = settled
        .into_iter()
        .filter(|id| !sent.contains(id))
        .collect();
    for (id, details) in &mut send {
        details["silence"] = json!({ "ended_at": now });
        sqlx::query(
            "UPDATE notification_outbox SET held_at = NULL, created_at = $2, details = $3
             WHERE id = $1",
        )
        .bind(*id)
        .bind(now)
        .bind(&*details)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query("UPDATE notification_outbox SET sent_at = $2 WHERE id = ANY($1)")
        .bind(&dropped)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(send.len())
}

/// Owed rows `ids` again, for `channel` only: it was still limiting when
/// their turn came. A copy keeps the time of its release, so a channel
/// limiting for a whole day loses it like any event that old (ADR-0060).
pub async fn requeue(
    db: impl PgExecutor<'_>,
    ids: &[i64],
    channel: Uuid,
) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query(
        "INSERT INTO notification_outbox (event, customer_id, details, created_at, channel_id)
         SELECT event, customer_id, details, created_at, $2 FROM notification_outbox
         WHERE id = ANY($1) ORDER BY id",
    )
    .bind(ids)
    .bind(channel)
    .execute(db)
    .await?
    .rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SERVER: Uuid = Uuid::from_u128(1);
    const RELAY: Uuid = Uuid::from_u128(2);
    const SITE: Uuid = Uuid::from_u128(3);
    const CUSTOMER: Uuid = Uuid::from_u128(4);

    fn at(minute: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000 + minute * 60, 0).unwrap()
    }

    fn silence() -> Silence {
        Silence {
            id: Uuid::from_u128(9),
            server_id: None,
            relay_id: None,
            site_id: None,
            customer_id: None,
            starts_at: at(0),
            ends_at: at(60),
        }
    }

    fn event(id: i64, kind: &str) -> Event {
        Event {
            id,
            event: kind.into(),
            customer_id: Some(CUSTOMER),
            site_id: Some(SITE),
            details: json!({
                "server": {"id": SERVER, "name": "web-1"},
                "site": {"id": SITE, "name": "paris"},
                "customer": {"id": CUSTOMER, "name": "acme"},
            }),
            created_at: at(30),
        }
    }

    #[test]
    fn a_silence_runs_from_its_start_to_just_before_its_end() {
        let s = silence();
        assert!(!s.active(at(-1)));
        assert!(s.active(at(0)));
        assert!(s.active(at(59)));
        assert!(!s.active(at(60)));
    }

    #[test]
    fn it_holds_what_is_written_before_it_ends_once_it_started() {
        let s = silence();
        let written = |minute| Event {
            created_at: at(minute),
            ..event(1, "incident.opened")
        };
        // Upcoming: holds nothing.
        assert!(!s.holds(&written(-5), at(-1)));
        // Written just before the start, still pending at the start.
        assert!(s.holds(&written(-1), at(0)));
        assert!(s.holds(&written(30), at(30)));
        // Written during it, found after it ended: goes through release.
        assert!(s.holds(&written(59), at(61)));
        // Written at the end or after: the silence is over.
        assert!(!s.holds(&written(60), at(60)));
        assert!(!s.holds(&written(61), at(61)));
    }

    #[test]
    fn each_scope_matches_its_own_events() {
        let opened = event(1, "incident.opened");
        let other = Uuid::from_u128(99);
        let scoped = |server, relay, site, customer| Silence {
            server_id: server,
            relay_id: relay,
            site_id: site,
            customer_id: customer,
            ..silence()
        };
        assert!(silence().concerns(&opened));
        assert!(scoped(Some(SERVER), None, None, None).concerns(&opened));
        assert!(!scoped(Some(other), None, None, None).concerns(&opened));
        assert!(!scoped(None, Some(RELAY), None, None).concerns(&opened));
        assert!(scoped(None, None, Some(SITE), None).concerns(&opened));
        assert!(!scoped(None, None, Some(other), None).concerns(&opened));
        assert!(scoped(None, None, None, Some(CUSTOMER)).concerns(&opened));
        assert!(!scoped(None, None, None, Some(other)).concerns(&opened));

        let relay = Event {
            details: json!({
                "server": null,
                "relay": {"id": RELAY, "name": "gw"},
                "site": {"id": SITE, "name": "paris"},
            }),
            ..opened.clone()
        };
        assert!(scoped(None, Some(RELAY), None, None).concerns(&relay));
        assert!(!scoped(Some(SERVER), None, None, None).concerns(&relay));
        assert!(scoped(None, None, Some(SITE), None).concerns(&relay));

        // A failed backup concerns the whole server: only everything.
        let backup = Event {
            customer_id: None,
            details: json!({"backup": {"id": other}}),
            ..event(2, "backup.failed")
        };
        assert!(silence().concerns(&backup));
        assert!(!scoped(None, None, Some(SITE), None).concerns(&backup));
        assert!(!scoped(None, None, None, Some(CUSTOMER)).concerns(&backup));
    }

    #[test]
    fn new_events_wait_behind_their_incidents_held_rows() {
        let opened = Event {
            details: json!({"incident": {"id": SERVER}}),
            ..event(1, "incident.opened")
        };
        let none = HashSet::new();
        let held = HashSet::from([SERVER]);
        assert!(!waits(&opened, None, &none));
        assert!(waits(&opened, None, &held));
        assert!(waits(&opened, Some(&silence()), &none));
        // Only incidents wait behind incidents.
        let backup = Event {
            details: json!({"incident": {"id": SERVER}}),
            ..event(2, "backup.failed")
        };
        assert!(!waits(&backup, None, &held));
        assert!(owed(&json!({"silence": {"ended_at": "x"}})));
        assert!(!owed(&json!({"event": "incident.opened"})));
    }

    #[test]
    fn an_open_incident_is_told_once() {
        let opened = event(1, "incident.opened");
        let escalated = event(2, "incident.escalated");
        assert_eq!(settle(&[&opened], true), Some(1));
        assert_eq!(settle(&[&opened, &escalated], true), Some(1));
        // Told before the silence, worse during it.
        assert_eq!(settle(&[&escalated], true), Some(2));
    }

    #[test]
    fn a_resolved_incident_is_told_only_to_those_told_it_opened() {
        let opened = event(1, "incident.opened");
        let escalated = event(2, "incident.escalated");
        let resolved = event(3, "incident.resolved");
        // Opened and resolved during the silence.
        assert_eq!(settle(&[&opened, &resolved], false), None);
        assert_eq!(settle(&[&opened, &escalated, &resolved], false), None);
        // Told before the silence, resolved during it.
        assert_eq!(settle(&[&resolved], false), Some(3));
        assert_eq!(settle(&[&escalated, &resolved], false), Some(3));
    }

    #[test]
    fn a_release_counts_messages_not_rows() {
        let budget = Budget {
            messages: 4,
            lines: 10,
        };
        let incidents = |counts: &[(Option<Uuid>, usize)]| counts.iter().copied().collect();
        // One customer's 25 incidents are three group messages.
        assert_eq!(budget.needs(&incidents(&[(Some(CUSTOMER), 25)]), 0), 3);
        assert_eq!(budget.needs(&incidents(&[(Some(CUSTOMER), 1)]), 0), 1);
        // Two customers never share one; other events and incidents of no
        // customer go one each.
        let two = incidents(&[(Some(CUSTOMER), 3), (Some(SITE), 3), (None, 2)]);
        assert_eq!(budget.needs(&two, 1), 5);
    }
}
