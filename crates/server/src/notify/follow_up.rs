//! Follow-up (ADR-0082): when nobody acknowledges a critical incident, the
//! next step of its place's follow-up is told, after a wait.
//!
//! The first message is what the dispatcher always sends: the channels and
//! the person on call. A follow-up only adds steps after it, each "after N
//! minutes without acknowledgement, tell these people, schedules and
//! channels". The narrowest follow-up covering the incident's place applies;
//! without one, [`default_steps`]. Warnings never follow up.
//!
//! Where each incident stands lives in `incident_follow_ups`: the dispatcher
//! [`start`]s it when it sends a critical event, takes the steps that are
//! due ([`take_due`]) in its pass, and a step goes out like any event, in
//! groups and within the customer's window (ADR-0080). An acknowledgement,
//! the resolution, or the end of the critical state stops it; a silence
//! puts the next step off until after it ends.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{PgConnection, PgExecutor, PgPool, Row};
use uuid::Uuid;

use super::message::Event;
use super::on_call::{self, mail_channel, narrowness, Directory, Schedule};
use super::silence;
use super::{Channel, Coverage, Destination};

/// The wait of the default follow-up's one step.
pub const DEFAULT_AFTER_MINUTES: u32 = 15;
/// Most steps of one follow-up.
pub const MAX_STEPS: usize = 10;
/// Longest wait before a step: a day.
pub const MAX_AFTER_MINUTES: u32 = 1440;
/// How long the link in a personal message acknowledges: a night.
pub const LINK_LIFETIME: TimeDelta = TimeDelta::hours(12);

/// One step: after `after_minutes` without acknowledgement, counted from
/// the step before or from the first message, tell these.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub after_minutes: u32,
    /// Schedules whose person on call is told, or who covers their gap.
    #[serde(default)]
    pub on_call: Vec<Uuid>,
    #[serde(default)]
    pub users: Vec<Uuid>,
    #[serde(default)]
    pub channels: Vec<Uuid>,
    /// The nearest administrators of the incident's place who can be told:
    /// of its site, else its customer, else everything.
    #[serde(default)]
    pub administrators: bool,
}

/// What applies where nobody set a follow-up: critical alerts not
/// acknowledged within 15 minutes go to the nearest administrators, in
/// person, once.
pub fn default_steps() -> Vec<Step> {
    vec![Step {
        after_minutes: DEFAULT_AFTER_MINUTES,
        administrators: true,
        ..Step::default()
    }]
}

/// A row of `follow_up_policies`.
#[derive(Debug, Clone, PartialEq)]
pub struct Policy {
    pub id: Uuid,
    pub coverage: Coverage,
    pub steps: Vec<Step>,
}

/// Every follow-up set up.
pub async fn policies(db: impl PgExecutor<'_>) -> Result<Vec<Policy>, sqlx::Error> {
    let rows = sqlx::query("SELECT id, customer_id, site_id, steps FROM follow_up_policies")
        .fetch_all(db)
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let id: Uuid = row.try_get("id")?;
        let Some(coverage) =
            Coverage::from_columns(row.try_get("customer_id")?, row.try_get("site_id")?)
        else {
            continue;
        };
        match serde_json::from_value(row.try_get("steps")?) {
            Ok(steps) => out.push(Policy {
                id,
                coverage,
                steps,
            }),
            Err(error) => tracing::warn!(policy = %id, %error, "ignoring an unreadable follow-up"),
        }
    }
    Ok(out)
}

/// The follow-up of an incident at `customer` and `site`: the narrowest
/// that covers it; `None` for the default.
pub fn policy_for(
    policies: &[Policy],
    customer: Option<Uuid>,
    site: Option<Uuid>,
) -> Option<&Policy> {
    policies
        .iter()
        .filter(|p| p.coverage.includes(customer, site))
        .max_by_key(|p| narrowness(p.coverage))
}

/// The steps that apply at `customer` and `site`.
pub fn steps_for(policies: &[Policy], customer: Option<Uuid>, site: Option<Uuid>) -> Vec<Step> {
    policy_for(policies, customer, site).map_or_else(default_steps, |p| p.steps.clone())
}

/// The place of an incident, for whose roles and mail server count.
pub fn place(customer: Option<Uuid>, site: Option<Uuid>) -> Coverage {
    match (customer, site) {
        (_, Some(site)) => Coverage::Site(site),
        (Some(customer), None) => Coverage::Customer(customer),
        (None, None) => Coverage::Everything,
    }
}

/// Who one step tells.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Told {
    pub users: Vec<Uuid>,
    pub channels: Vec<Uuid>,
}

impl Told {
    pub fn is_empty(&self) -> bool {
        self.users.is_empty() && self.channels.is_empty()
    }
}

/// Who `step` tells at `at` about an incident at `customer` and `site`.
/// Only those who can be told and see the place: a schedule or channel
/// that does not cover it, or a person who cannot be told, is skipped. A
/// schedule in a gap tells who covers its gaps (ADR-0081).
pub fn recipients(
    step: &Step,
    schedules: &[Schedule],
    directory: &Directory,
    channels: &[Channel],
    (customer, site): (Option<Uuid>, Option<Uuid>),
    at: DateTime<Utc>,
) -> Told {
    let place = place(customer, site);
    let mail = mail_channel(channels, directory, place).is_some();
    let mut users: Vec<Uuid> = Vec::new();
    for schedule in schedules
        .iter()
        .filter(|s| step.on_call.contains(&s.id) && s.coverage.includes(customer, site))
    {
        let mail = mail_channel(channels, directory, schedule.coverage).is_some();
        users.extend(on_call::recipients(schedule, directory, mail, at));
    }
    users.extend(
        step.users
            .iter()
            .filter(|u| directory.problem(Some(**u), place, mail).is_none()),
    );
    if step.administrators {
        users.extend(directory.fallback(place, mail));
    }
    let mut seen = HashSet::new();
    users.retain(|u| seen.insert(*u));
    let channels = channels
        .iter()
        .filter(|c| !c.person && step.channels.contains(&c.id))
        .filter(|c| c.coverage.includes(customer, site))
        .map(|c| c.id)
        .collect();
    Told { users, channels }
}

/// A channel in a sentence, without its addresses or URL: "Slack #ops",
/// "an email channel", "a webhook".
pub fn label(destination: &Destination) -> String {
    match destination.chat() {
        Some((service, chat)) if chat.name.is_empty() => service.name().to_owned(),
        Some((service, chat)) => format!("{} {}", service.name(), chat.name),
        None => match destination {
            Destination::Email(_) => "an email channel".into(),
            _ => "a webhook".into(),
        },
    }
}

/// The names of `told`, people first, as the timeline and the screen say.
pub fn names(told: &Told, directory: &Directory, channels: &[Channel]) -> Vec<String> {
    told.users
        .iter()
        .filter_map(|u| directory.people.get(u).map(|p| p.username.clone()))
        .chain(told.channels.iter().filter_map(|id| {
            channels
                .iter()
                .find(|c| c.id == *id)
                .map(|c| label(&c.destination))
        }))
        .collect()
}

/// Whether `event` starts a follow-up: an incident told as critical.
fn starts(event: &Event) -> bool {
    matches!(
        event.event.as_str(),
        "incident.opened" | "incident.escalated"
    ) && event.details["incident"]["severity"] == "critical"
}

fn incident_of(event: &Event) -> Option<Uuid> {
    event.details["incident"]["id"].as_str()?.parse().ok()
}

/// Starts the follow-up of each critical incident `events` tell, sent at
/// `now`, unless acknowledged or already running; one that stopped when the
/// incident was no longer critical starts over. Stops those `events`
/// resolve. In the dispatcher's claim, so it holds what was sent.
pub async fn start(
    db: &mut PgConnection,
    events: &[Event],
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let resolved: Vec<Uuid> = events
        .iter()
        .filter(|e| e.event == "incident.resolved")
        .filter_map(incident_of)
        .collect();
    if !resolved.is_empty() {
        stop(&mut *db, &resolved, "resolved", now).await?;
    }
    if !events.iter().any(starts) {
        return Ok(());
    }
    let policies = policies(&mut *db).await?;
    for event in events.iter().filter(|e| starts(e)) {
        let (Some(incident), Some(first)) = (
            incident_of(event),
            steps_for(&policies, event.customer_id, event.site_id)
                .first()
                .map(|s| s.after_minutes),
        ) else {
            continue;
        };
        sqlx::query(
            "INSERT INTO incident_follow_ups (incident_id, started_at, next_at)
             SELECT i.id, $2, $2 + make_interval(mins => $3) FROM incidents i
             WHERE i.id = $1 AND i.state = 'open'
               AND NOT EXISTS (SELECT 1 FROM incident_acknowledgements a WHERE a.incident_id = i.id)
             ON CONFLICT (incident_id) DO UPDATE
             SET started_at = EXCLUDED.started_at, step = 0, next_at = EXCLUDED.next_at,
                 stopped_at = NULL, stop_reason = NULL
             WHERE incident_follow_ups.stop_reason = 'not_critical'",
        )
        .bind(incident)
        .bind(now)
        .bind(i32::try_from(first).unwrap_or(i32::MAX))
        .execute(&mut *db)
        .await?;
    }
    Ok(())
}

/// Stops the running follow-ups of `incidents` for `reason`.
pub async fn stop(
    db: impl PgExecutor<'_>,
    incidents: &[Uuid],
    reason: &str,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE incident_follow_ups SET next_at = NULL, stopped_at = $3, stop_reason = $2
         WHERE incident_id = ANY($1) AND next_at IS NOT NULL",
    )
    .bind(incidents)
    .bind(reason)
    .bind(now)
    .execute(db)
    .await?;
    Ok(())
}

/// A step due now: the event that tells it, already written to the outbox
/// as sent, and the step.
#[derive(Debug, Clone)]
pub struct Due {
    pub event: Event,
    pub step: Step,
}

/// The incident's state as a webhook payload's objects, like the trigger
/// writes for its events (ADR-0060). Shared with the API, which shows what
/// a link acknowledges.
pub const DETAILS: &str = "jsonb_build_object(
        'customer', p.customer, 'site', p.site,
        'server', (SELECT notification_ref(id, hostname) FROM servers WHERE id = i.server_id),
        'relay', (SELECT notification_ref(id, name) FROM relays WHERE id = i.relay_id),
        'incident', jsonb_build_object(
            'id', i.id, 'metric', i.metric, 'instance', i.instance, 'severity', i.severity,
            'value', i.last_value, 'peak_value', i.peak_value, 'reason', NULL,
            'opened_at', i.opened_at, 'resolved_at', i.resolved_at))";

/// Takes the steps due at `now`, writes each as a sent outbox row, and
/// moves its follow-up on to the next step or stops it. A customer is told
/// at most once a `window` (ADR-0080): a step waits for it, unless the
/// customer is in `claimed`, sent to in this pass already. An incident
/// acknowledged, resolved or no longer critical stops instead; one that a
/// silence covers waits for the silence to end, plus the step's wait.
/// What is taken is told at most once, as the outbox is.
pub async fn take_due(
    pool: &PgPool,
    now: DateTime<Utc>,
    window: std::time::Duration,
    claimed: &HashSet<Uuid>,
) -> Result<Vec<Due>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    // SQL-safe: interpolates only the constant DETAILS.
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT f.incident_id, f.step, f.started_at, i.state, i.severity, p.customer_id,
                (p.site ->> 'id')::uuid AS site_id,
                EXISTS (SELECT 1 FROM incident_acknowledgements a
                        WHERE a.incident_id = i.id) AS acknowledged,
                {DETAILS} AS details
         FROM incident_follow_ups f
         JOIN incidents i ON i.id = f.incident_id
         CROSS JOIN LATERAL notification_place(i.server_id, i.relay_id) p
         WHERE f.next_at <= $1
         ORDER BY f.next_at, f.incident_id
         FOR UPDATE OF f SKIP LOCKED"
    )))
    .bind(now)
    .fetch_all(&mut *tx)
    .await?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let recent: HashSet<Uuid> = sqlx::query_scalar(
        "SELECT DISTINCT customer_id FROM notification_outbox
         WHERE sent_at > $1 - make_interval(secs => $2) AND held_at IS NULL
           AND channel_id IS NULL AND user_id IS NULL AND customer_id IS NOT NULL",
    )
    .bind(now)
    .bind(window.as_secs_f64())
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .collect();
    let policies = policies(&mut *tx).await?;
    let silences = silence::load(&mut *tx, now, now).await?;
    let mut due = Vec::new();
    for row in rows {
        let incident: Uuid = row.try_get("incident_id")?;
        let customer: Option<Uuid> = row.try_get("customer_id")?;
        let site: Option<Uuid> = row.try_get("site_id")?;
        if customer.is_some_and(|c| recent.contains(&c) && !claimed.contains(&c)) {
            continue;
        }
        let stopped = if row.try_get::<String, _>("state")? != "open" {
            Some("resolved")
        } else if row.try_get("acknowledged")? {
            Some("acknowledged")
        } else if row.try_get::<String, _>("severity")? != "critical" {
            Some("not_critical")
        } else {
            None
        };
        if let Some(reason) = stopped {
            stop(&mut *tx, &[incident], reason, now).await?;
            continue;
        }
        let step = usize::try_from(row.try_get::<i32, _>("step")?).unwrap_or(usize::MAX);
        let steps = steps_for(&policies, customer, site);
        let Some(current) = steps.get(step) else {
            stop(&mut *tx, &[incident], "finished", now).await?;
            continue;
        };
        let started_at: DateTime<Utc> = row.try_get("started_at")?;
        let mut event = Event {
            id: 0,
            event: "incident.opened".into(),
            customer_id: customer,
            site_id: site,
            details: row.try_get("details")?,
            created_at: now,
        };
        let silenced_until = silences
            .iter()
            .filter(|s| s.active(now) && s.concerns(&event))
            .map(|s| s.ends_at)
            .max();
        if let Some(end) = silenced_until {
            sqlx::query("UPDATE incident_follow_ups SET next_at = $2 WHERE incident_id = $1")
                .bind(incident)
                .bind(end + minutes(current.after_minutes))
                .execute(&mut *tx)
                .await?;
            continue;
        }
        event.details["occurred_at"] = json!(now);
        event.details["follow_up"] = json!({
            "step": step + 1,
            "steps": steps.len(),
            "minutes": (now - started_at).num_minutes().max(0),
        });
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO notification_outbox (event, customer_id, details, created_at, sent_at)
             VALUES ($1, $2, $3, $4, $4) RETURNING id",
        )
        .bind(&event.event)
        .bind(customer)
        .bind(&event.details)
        .bind(now)
        .fetch_one(&mut *tx)
        .await?;
        event.id = id;
        match steps.get(step + 1) {
            Some(next) => {
                sqlx::query(
                    "UPDATE incident_follow_ups SET step = step + 1, next_at = $2
                     WHERE incident_id = $1",
                )
                .bind(incident)
                .bind(now + minutes(next.after_minutes))
                .execute(&mut *tx)
                .await?;
            }
            None => {
                sqlx::query(
                    "UPDATE incident_follow_ups
                     SET step = step + 1, next_at = NULL, stopped_at = $2, stop_reason = 'finished'
                     WHERE incident_id = $1",
                )
                .bind(incident)
                .bind(now)
                .execute(&mut *tx)
                .await?;
            }
        }
        due.push(Due {
            event,
            step: current.clone(),
        });
    }
    tx.commit().await?;
    Ok(due)
}

pub fn minutes(n: u32) -> TimeDelta {
    TimeDelta::minutes(i64::from(n))
}

/// Who was told of each of `incidents` before: people in person, and
/// channels a follow-up step told. Their resolution goes to them too.
pub async fn told_before(
    db: impl PgExecutor<'_>,
    incidents: &[Uuid],
) -> Result<HashMap<Uuid, Told>, sqlx::Error> {
    let rows: Vec<(Uuid, Option<Uuid>, Option<Uuid>)> = sqlx::query_as(
        "SELECT incident_id, user_id, channel_id FROM incident_told
         WHERE incident_id = ANY($1) ORDER BY told_at",
    )
    .bind(incidents)
    .fetch_all(db)
    .await?;
    let mut out: HashMap<Uuid, Told> = HashMap::new();
    for (incident, user, channel) in rows {
        let told = out.entry(incident).or_default();
        told.users.extend(user);
        told.channels.extend(channel);
    }
    Ok(out)
}

/// Records that `told` heard of `incident`.
pub async fn record_told(
    db: &mut PgConnection,
    incident: Uuid,
    told: &Told,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO incident_told (incident_id, user_id, told_at)
         SELECT $1, u, $3 FROM unnest($2::uuid[]) AS u
         ON CONFLICT (incident_id, user_id) WHERE user_id IS NOT NULL DO NOTHING",
    )
    .bind(incident)
    .bind(&told.users)
    .bind(now)
    .execute(&mut *db)
    .await?;
    sqlx::query(
        "INSERT INTO incident_told (incident_id, channel_id, told_at)
         SELECT $1, c, $3 FROM unnest($2::uuid[]) AS c
         ON CONFLICT (incident_id, channel_id) WHERE channel_id IS NOT NULL DO NOTHING",
    )
    .bind(incident)
    .bind(&told.channels)
    .bind(now)
    .execute(&mut *db)
    .await?;
    Ok(())
}

/// Writes on the incident's timeline that step `step` of `steps` told
/// `names`, or nobody.
pub async fn record_step(
    db: &mut PgConnection,
    event: &Event,
    names: &[String],
) -> Result<(), sqlx::Error> {
    let Some(incident) = incident_of(event) else {
        return Ok(());
    };
    let follow_up = &event.details["follow_up"];
    sqlx::query(
        "INSERT INTO incident_events (incident_id, occurred_at, kind, severity, details)
         SELECT id, $2, 'followed_up', severity, $3 FROM incidents WHERE id = $1",
    )
    .bind(incident)
    .bind(event.created_at)
    .bind(json!({
        "step": follow_up["step"],
        "steps": follow_up["steps"],
        "minutes": follow_up["minutes"],
        "told": names,
    }))
    .execute(&mut *db)
    .await?;
    Ok(())
}

/// Acknowledges the open incidents among `incidents` not acknowledged yet,
/// as `user` (`username`), from `via`, `page` or `link`: records it, writes
/// it on the timeline and stops their follow-up. Returns those it
/// acknowledged; the others were resolved, or someone was first.
pub async fn acknowledge(
    db: &mut PgConnection,
    incidents: &[Uuid],
    (user, username): (Uuid, &str),
    via: &str,
    now: DateTime<Utc>,
) -> Result<Vec<Uuid>, sqlx::Error> {
    let done: Vec<Uuid> = sqlx::query_scalar(
        "INSERT INTO incident_acknowledgements (incident_id, user_id, username, acknowledged_at, via)
         SELECT id, $2, $3, $4, $5 FROM incidents WHERE id = ANY($1) AND state = 'open'
         ORDER BY id
         ON CONFLICT (incident_id) DO NOTHING
         RETURNING incident_id",
    )
    .bind(incidents)
    .bind(user)
    .bind(username)
    .bind(now)
    .bind(via)
    .fetch_all(&mut *db)
    .await?;
    sqlx::query(
        "INSERT INTO incident_events (incident_id, occurred_at, kind, severity, details)
         SELECT id, $2, 'acknowledged', severity,
                jsonb_build_object('user_id', $3::uuid, 'username', $4::text, 'via', $5::text)
         FROM incidents WHERE id = ANY($1) ORDER BY id",
    )
    .bind(&done)
    .bind(now)
    .bind(user)
    .bind(username)
    .bind(via)
    .execute(&mut *db)
    .await?;
    stop(&mut *db, &done, "acknowledged", now).await?;
    Ok(done)
}

/// Whether a rendered message or an event's details are a follow-up step.
pub fn is_step(details: &Value) -> bool {
    details.get("follow_up").is_some()
}

#[cfg(test)]
mod tests;
