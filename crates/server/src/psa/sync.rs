//! Tickets from incidents (ADR-0164, ADR-0166): reads the incident events
//! of `integration_events` through `psa_deliveries`, its own delivery
//! state, so it is fed from every place incidents come from, at least
//! once and in order per incident.
//!
//! `incident.opened` (or `incident.escalated` to the least severity that
//! opens tickets) creates one ticket for the PSA client of the incident's
//! site, else of its customer, else the connection's default client, with
//! the priority the map gives its severity and the server's asset when it
//! has one (ADR-0191). A silence covering the incident holds the ticket
//! until it ends, as it holds notifications: then the ticket is opened if
//! the incident is still open, and never if it opened and resolved during
//! the silence. `incident.escalated` of an incident with a ticket adds a
//! note and raises the priority to the critical one. `incident.resolved`
//! adds a private note and moves the ticket to the connection's resolved
//! status, unless someone closed it in the PSA already. The leader runs
//! it: a delivery is claimed by moving `next_at` a lease ahead, as
//! integration deliveries are. A 4xx from the PSA is a setup error, kept
//! on the incident's ticket and the connection, not retried; anything else
//! is retried for a day.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use serde_json::Value;
use sqlx::PgPool;
use tokio::time::{interval, MissedTickBehavior};
use uuid::Uuid;

use super::halopsa::{Limiter, NewTicket};
use super::{Connection, Halo, HaloError};
use crate::integrations::deliver::retry_delay;
use crate::notify::message::Event;
use crate::notify::silence;

/// Time between two looks for due deliveries.
pub const POLL: Duration = Duration::from_secs(5);
/// How long a claim lasts: well over a few requests' timeouts.
const LEASE: Duration = Duration::from_secs(120);
/// Deliveries claimed per pass, handled one after another.
const BATCH: i64 = 20;
/// A delivery this old gives up after its next failure.
const MAX_AGE: TimeDelta = TimeDelta::hours(24);
/// Longest ticket summary sent.
const MAX_SUMMARY: usize = 200;

#[derive(Debug, sqlx::FromRow)]
struct Claimed {
    id: i64,
    attempts: i32,
    created_at: DateTime<Utc>,
    event: String,
    details: Value,
    customer_id: Option<Uuid>,
    site_id: Option<Uuid>,
    event_at: DateTime<Utc>,
}

/// What became of one delivery.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Done,
    /// Try again later: no answer, a rate limit or a server error.
    Retry(String),
    /// A silence holds it until then; not an attempt.
    Hold(DateTime<Utc>),
}

/// A held delivery is looked at again this long after its silence ends.
const AFTER_SILENCE: TimeDelta = TimeDelta::seconds(5);

/// The text of an incident for the PSA: built from MonitorFlow's own
/// fields only, never the AI's text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncidentText {
    pub summary: String,
    pub details: String,
}

fn text<'a>(details: &'a Value, path: &[&str]) -> &'a str {
    path.iter()
        .fold(details, |v, k| &v[*k])
        .as_str()
        .unwrap_or_default()
}

/// The ticket's summary and details for an incident event's `details`.
/// The details end with the incident's id, which a retry searches for.
pub fn incident_text(details: &Value, web_url: &str) -> IncidentText {
    let id = text(details, &["incident", "id"]);
    let severity = match text(details, &["incident", "severity"]) {
        "critical" => "Critical",
        "warning" => "Warning",
        other => other,
    };
    let metric = text(details, &["incident", "metric"]);
    let instance = text(details, &["incident", "instance"]);
    let measure = if instance.is_empty() {
        metric.to_owned()
    } else {
        format!("{metric} ({instance})")
    };
    let server = text(details, &["server", "name"]);
    let place = if server.is_empty() {
        text(details, &["relay", "name"])
    } else {
        server
    };
    let mut summary = format!("{severity}: {measure} on {place}");
    if summary.chars().count() > MAX_SUMMARY {
        summary = summary.chars().take(MAX_SUMMARY - 1).collect::<String>() + "…";
    }
    let value = &details["incident"]["value"];
    let mut lines = vec![
        format!("MonitorFlow opened an incident on {place}."),
        format!("Measure: {measure}"),
    ];
    if !value.is_null() {
        lines.push(format!("Value: {value}"));
    }
    let site = text(details, &["site", "name"]);
    if !site.is_empty() {
        lines.push(format!("Site: {site}"));
    }
    let opened = text(details, &["incident", "opened_at"]);
    if !opened.is_empty() {
        lines.push(format!("Opened at: {opened}"));
    }
    lines.push(format!("Details: {web_url}/incidents/view/?id={id}"));
    lines.push(format!("MonitorFlow incident: {id}"));
    IncidentText {
        summary,
        details: lines.join("\n"),
    }
}

/// Whether `severity` reaches `least`, `warning` or `critical`.
pub fn reaches(severity: &str, least: &str) -> bool {
    match least {
        "warning" => matches!(severity, "warning" | "critical"),
        _ => severity == "critical",
    }
}

#[derive(Clone)]
pub struct Syncer {
    pool: PgPool,
    /// Base URL of the web interface, for links in tickets.
    web_url: String,
    limiter: Arc<Limiter>,
}

impl Syncer {
    pub fn new(pool: PgPool, web_url: impl Into<String>) -> Self {
        Self::with_limiter(pool, web_url, Limiter::shared())
    }

    pub fn with_limiter(pool: PgPool, web_url: impl Into<String>, limiter: Arc<Limiter>) -> Self {
        Self {
            pool,
            web_url: web_url.into().trim_end_matches('/').to_owned(),
            limiter,
        }
    }

    /// Handles due deliveries every `poll` until `stop` resolves.
    pub async fn run(self, poll: Duration, stop: impl Future<Output = ()>) {
        tokio::pin!(stop);
        let mut tick = interval(poll);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = &mut stop => return,
                _ = tick.tick() => {}
            }
            if let Err(error) = self.pass().await {
                tracing::warn!(%error, "could not read PSA deliveries; will retry");
            }
        }
    }

    /// Claims due deliveries and handles them in order; how many were
    /// claimed.
    pub async fn pass(&self) -> Result<usize, sqlx::Error> {
        let claimed = self.claim().await?;
        if claimed.is_empty() {
            return Ok(0);
        }
        let Some(connection) = super::load(&self.pool).await? else {
            self.finish_all(&claimed).await?;
            return Ok(claimed.len());
        };
        let halo = connection
            .credentials()
            .map(|c| Halo::new(c).with_limiter(self.limiter.clone()));
        for delivery in &claimed {
            let outcome = match (&halo, connection.enabled) {
                (_, false) => Ok(Outcome::Done),
                (Err(error), true) => Ok(Outcome::Retry(error.message.clone())),
                (Ok(halo), true) => self.handle(&connection, halo, delivery).await,
            };
            match outcome {
                Ok(outcome) => self.record(delivery, outcome).await?,
                Err(error) => {
                    tracing::warn!(%error, delivery = delivery.id, "could not handle a PSA delivery; it will be tried again");
                }
            }
        }
        Ok(claimed.len())
    }

    /// Due deliveries, the first pending one of each incident only, moved
    /// one lease ahead and counted as an attempt.
    async fn claim(&self) -> Result<Vec<Claimed>, sqlx::Error> {
        sqlx::query_as(
            "WITH due AS (
                 SELECT d.id FROM psa_deliveries d
                 WHERE d.state = 'pending' AND d.next_at <= now()
                   AND NOT EXISTS (
                       SELECT 1 FROM psa_deliveries p
                       WHERE p.subject = d.subject AND p.state = 'pending' AND p.id < d.id)
                 ORDER BY d.id LIMIT $1
                 FOR UPDATE OF d SKIP LOCKED)
             UPDATE psa_deliveries d
             SET next_at = now() + make_interval(secs => $2), attempts = d.attempts + 1
             FROM due, integration_events v
             WHERE d.id = due.id AND v.seq = d.event_seq
             RETURNING d.id, d.attempts, d.created_at, v.event, v.details, v.customer_id,
                       v.site_id, v.created_at AS event_at",
        )
        .bind(BATCH)
        .bind(LEASE.as_secs_f64())
        .fetch_all(&self.pool)
        .await
    }

    async fn finish_all(&self, claimed: &[Claimed]) -> Result<(), sqlx::Error> {
        for delivery in claimed {
            self.record(delivery, Outcome::Done).await?;
        }
        Ok(())
    }

    /// Writes the outcome, unless another instance claimed the delivery
    /// since. A retry past [`MAX_AGE`] gives up.
    async fn record(&self, delivery: &Claimed, outcome: Outcome) -> Result<(), sqlx::Error> {
        match outcome {
            Outcome::Hold(until) => {
                sqlx::query(
                    "UPDATE psa_deliveries SET next_at = $3, attempts = $2 - 1, last_error = NULL
                     WHERE id = $1 AND attempts = $2 AND state = 'pending'",
                )
                .bind(delivery.id)
                .bind(delivery.attempts)
                .bind(until + AFTER_SILENCE)
                .execute(&self.pool)
                .await?;
            }
            Outcome::Done => {
                sqlx::query(
                    "UPDATE psa_deliveries SET state = 'done', finished_at = now(), last_error = NULL
                     WHERE id = $1 AND attempts = $2 AND state = 'pending'",
                )
                .bind(delivery.id)
                .bind(delivery.attempts)
                .execute(&self.pool)
                .await?;
            }
            Outcome::Retry(error) => {
                let give_up = Utc::now() - delivery.created_at > MAX_AGE;
                sqlx::query(
                    "UPDATE psa_deliveries
                     SET state = CASE WHEN $3 THEN 'failed' ELSE 'pending' END,
                         finished_at = CASE WHEN $3 THEN now() END,
                         next_at = now() + make_interval(secs => $4), last_error = $5
                     WHERE id = $1 AND attempts = $2 AND state = 'pending'",
                )
                .bind(delivery.id)
                .bind(delivery.attempts)
                .bind(give_up)
                .bind(retry_delay(delivery.attempts).as_secs_f64())
                .bind(&error)
                .execute(&self.pool)
                .await?;
                if give_up {
                    let incident = incident_id(&delivery.details);
                    sqlx::query(
                        "UPDATE psa_tickets SET state = 'failed', error = $2, updated_at = now()
                         WHERE incident_id = $1 AND state = 'creating'",
                    )
                    .bind(incident)
                    .bind(&error)
                    .execute(&self.pool)
                    .await?;
                }
            }
        }
        Ok(())
    }

    async fn handle(
        &self,
        connection: &Connection,
        halo: &Halo,
        delivery: &Claimed,
    ) -> Result<Outcome, sqlx::Error> {
        let Some(incident) = incident_id(&delivery.details) else {
            return Ok(Outcome::Done);
        };
        match delivery.event.as_str() {
            "incident.opened" | "incident.escalated" => {
                self.open(connection, halo, incident, delivery).await
            }
            "incident.resolved" => {
                self.close(connection, halo, incident, &delivery.details)
                    .await
            }
            _ => Ok(Outcome::Done),
        }
    }

    /// The outcome of a PSA request: a 4xx is kept as the ticket's and the
    /// connection's error and ends the delivery; the rest is retried.
    async fn failed(
        &self,
        connection: &Connection,
        incident: Uuid,
        error: HaloError,
    ) -> Result<Outcome, sqlx::Error> {
        super::record_failure(&self.pool, connection.id, &error.message).await?;
        if error.retryable() {
            sqlx::query(
                "UPDATE psa_tickets SET error = $2, updated_at = now() WHERE incident_id = $1",
            )
            .bind(incident)
            .bind(&error.message)
            .execute(&self.pool)
            .await?;
            return Ok(Outcome::Retry(error.message));
        }
        tracing::warn!(%incident, error = %error.message, "the PSA refused a ticket request");
        sqlx::query(
            "UPDATE psa_tickets SET state = CASE WHEN state = 'creating' THEN 'failed' ELSE state END,
                 error = $2, updated_at = now()
             WHERE incident_id = $1",
        )
        .bind(incident)
        .bind(&error.message)
        .execute(&self.pool)
        .await?;
        Ok(Outcome::Done)
    }

    /// Where the incident's ticket goes: the PSA client and site of its
    /// site, else its customer's client, else the connection's default.
    async fn destination(
        &self,
        connection: &Connection,
        details: &Value,
    ) -> Result<Option<(i64, Option<i64>)>, sqlx::Error> {
        if let Ok(site) = text(details, &["site", "id"]).parse::<Uuid>() {
            let link: Option<(i64, Option<i64>)> = sqlx::query_as(
                "SELECT client_id, halo_site_id FROM psa_site_links
                 WHERE site_id = $1 AND connection_id = $2
                 UNION ALL
                 SELECT cl.client_id, NULL FROM psa_customer_links cl
                 JOIN sites s ON s.customer_id = cl.customer_id
                 WHERE s.id = $1 AND cl.connection_id = $2
                 LIMIT 1",
            )
            .bind(site)
            .bind(connection.id)
            .fetch_optional(&self.pool)
            .await?;
            if link.is_some() {
                return Ok(link);
            }
        }
        let default = &connection.default_client;
        Ok(default
            .default_client_id
            .map(|client| (client, default.default_site_id)))
    }

    /// The end of the latest active silence covering the delivery's event.
    async fn silenced_until(
        &self,
        delivery: &Claimed,
    ) -> Result<Option<DateTime<Utc>>, sqlx::Error> {
        let now = Utc::now();
        let mut db = self.pool.acquire().await?;
        let silences = silence::load(&mut db, now, now).await?;
        let event = Event {
            id: delivery.id,
            event: delivery.event.clone(),
            customer_id: delivery.customer_id,
            site_id: delivery.site_id,
            details: delivery.details.clone(),
            created_at: delivery.event_at,
        };
        Ok(silences
            .iter()
            .filter(|s| s.active(now) && s.concerns(&event))
            .map(|s| s.ends_at)
            .max())
    }

    async fn open(
        &self,
        connection: &Connection,
        halo: &Halo,
        incident: Uuid,
        delivery: &Claimed,
    ) -> Result<Outcome, sqlx::Error> {
        let details = &delivery.details;
        let severity = text(details, &["incident", "severity"]);
        if !reaches(severity, &connection.tickets.min_severity) {
            return Ok(Outcome::Done);
        }
        let current: Option<(String, Option<i64>)> =
            sqlx::query_as("SELECT state, ticket_id FROM psa_tickets WHERE incident_id = $1")
                .bind(incident)
                .fetch_optional(&self.pool)
                .await?;
        let (retrying, held) = match current.as_ref().map(|(state, _)| state.as_str()) {
            None => (false, false),
            Some("creating") => (true, false),
            Some("held") => (false, true),
            Some("open") if delivery.event == "incident.escalated" => {
                let ticket = current.as_ref().and_then(|(_, t)| *t);
                return self
                    .escalate(connection, halo, incident, ticket, severity)
                    .await;
            }
            // Open, closed, failed, skipped or ended: one ticket per incident.
            Some(_) => return Ok(Outcome::Done),
        };
        if !retrying {
            if let Some(until) = self.silenced_until(delivery).await? {
                sqlx::query(
                    "INSERT INTO psa_tickets (incident_id, connection_id, state, error)
                     VALUES ($1, $2, 'held', $3)
                     ON CONFLICT (incident_id) DO UPDATE SET state = 'held', error = $3,
                         updated_at = now()",
                )
                .bind(incident)
                .bind(connection.id)
                .bind(format!(
                    "Held by a silence until {}.",
                    until.format("%Y-%m-%d %H:%M UTC")
                ))
                .execute(&self.pool)
                .await?;
                return Ok(Outcome::Hold(until));
            }
            if held {
                let open: bool = sqlx::query_scalar(
                    "SELECT coalesce((SELECT state = 'open' FROM incidents WHERE id = $1), false)",
                )
                .bind(incident)
                .fetch_one(&self.pool)
                .await?;
                if !open {
                    sqlx::query(
                        "UPDATE psa_tickets SET state = 'skipped', updated_at = now(),
                             error = 'The incident opened and resolved during a silence.'
                         WHERE incident_id = $1",
                    )
                    .bind(incident)
                    .execute(&self.pool)
                    .await?;
                    return Ok(Outcome::Done);
                }
                sqlx::query("DELETE FROM psa_tickets WHERE incident_id = $1 AND state = 'held'")
                    .bind(incident)
                    .execute(&self.pool)
                    .await?;
            }
        }
        let Some((client, halo_site)) = self.destination(connection, details).await? else {
            let site_name = text(details, &["site", "name"]);
            let reason = if site_name.is_empty() {
                "The incident has no site, so no HaloPSA client, and no default client is set."
                    .to_owned()
            } else {
                format!(
                    "The site {site_name} is not matched to a HaloPSA client, and no default client is set."
                )
            };
            sqlx::query(
                "INSERT INTO psa_tickets (incident_id, connection_id, state, error)
                 VALUES ($1, $2, 'skipped', $3) ON CONFLICT (incident_id) DO NOTHING",
            )
            .bind(incident)
            .bind(connection.id)
            .bind(reason)
            .execute(&self.pool)
            .await?;
            return Ok(Outcome::Done);
        };
        if !retrying {
            sqlx::query(
                "INSERT INTO psa_tickets (incident_id, connection_id, state)
                 VALUES ($1, $2, 'creating') ON CONFLICT (incident_id) DO NOTHING",
            )
            .bind(incident)
            .bind(connection.id)
            .execute(&self.pool)
            .await?;
        } else {
            // An earlier try may have created it before its answer was lost.
            match halo.find_ticket(client, &incident.to_string()).await {
                Ok(Some(ticket)) => return self.opened(connection, incident, ticket).await,
                Ok(None) => {}
                Err(error) => return self.failed(connection, incident, error).await,
            }
        }
        let text = incident_text(details, &self.web_url);
        let asset: Option<i64> = match text_id(details, "server") {
            Some(server) => sqlx::query_scalar(
                "SELECT asset_id FROM psa_asset_links
                 WHERE server_id = $1 AND connection_id = $2 AND state = 'linked'",
            )
            .bind(server)
            .bind(connection.id)
            .fetch_optional(&self.pool)
            .await?
            .flatten(),
            None => None,
        };
        let ticket = NewTicket {
            summary: text.summary,
            details: text.details,
            client_id: client,
            site_id: halo_site,
            ticket_type_id: connection.tickets.ticket_type_id,
            category: connection.tickets.category.clone(),
            priority_id: connection.tickets.priority(severity),
            asset_id: asset,
        };
        match halo.create_ticket(&ticket).await {
            Ok(id) => self.opened(connection, incident, id).await,
            Err(error) => self.failed(connection, incident, error).await,
        }
    }

    async fn opened(
        &self,
        connection: &Connection,
        incident: Uuid,
        ticket: i64,
    ) -> Result<Outcome, sqlx::Error> {
        sqlx::query(
            "UPDATE psa_tickets SET state = 'open', ticket_id = $2, error = NULL, updated_at = now()
             WHERE incident_id = $1",
        )
        .bind(incident)
        .bind(ticket)
        .execute(&self.pool)
        .await?;
        super::record_success(&self.pool, connection.id, None).await?;
        tracing::info!(%incident, ticket, "opened a PSA ticket for an incident");
        Ok(Outcome::Done)
    }

    /// An incident with a ticket became critical: a note, and the critical
    /// priority when the map has one.
    async fn escalate(
        &self,
        connection: &Connection,
        halo: &Halo,
        incident: Uuid,
        ticket: Option<i64>,
        severity: &str,
    ) -> Result<Outcome, sqlx::Error> {
        let Some(ticket) = ticket.filter(|_| severity == "critical") else {
            return Ok(Outcome::Done);
        };
        if let Err(error) = halo
            .add_note(ticket, "MonitorFlow: the incident is now critical.")
            .await
        {
            return self.failed(connection, incident, error).await;
        }
        if let Some(priority) = connection.tickets.critical_priority_id {
            if let Err(error) = halo.set_priority(ticket, priority).await {
                return self.failed(connection, incident, error).await;
            }
        }
        super::record_success(&self.pool, connection.id, None).await?;
        Ok(Outcome::Done)
    }

    async fn close(
        &self,
        connection: &Connection,
        halo: &Halo,
        incident: Uuid,
        details: &Value,
    ) -> Result<Outcome, sqlx::Error> {
        let row: Option<(Option<i64>, Option<i64>)> = sqlx::query_as(
            "SELECT ticket_id, psa_status_id FROM psa_tickets
             WHERE incident_id = $1 AND state = 'open'",
        )
        .bind(incident)
        .fetch_optional(&self.pool)
        .await?;
        let Some((Some(ticket), status_now)) = row else {
            return Ok(Outcome::Done);
        };
        // Never move a ticket someone closed in the PSA: that could reopen it.
        let closed_there =
            status_now.is_some_and(|s| connection.tickets.closed_status_ids.contains(&s));
        let resolved = text(details, &["incident", "resolved_at"]);
        let note = if resolved.is_empty() {
            "MonitorFlow: the incident is resolved.".to_owned()
        } else {
            format!("MonitorFlow: the incident is resolved ({resolved}).")
        };
        if let Err(error) = halo.add_note(ticket, &note).await {
            return self.failed(connection, incident, error).await;
        }
        if let Some(status) = connection
            .tickets
            .resolved_status_id
            .filter(|_| !closed_there)
        {
            if let Err(error) = halo.set_status(ticket, status).await {
                return self.failed(connection, incident, error).await;
            }
        }
        sqlx::query(
            "UPDATE psa_tickets SET state = 'closed', closed_at = now(), error = NULL,
                 updated_at = now()
             WHERE incident_id = $1",
        )
        .bind(incident)
        .execute(&self.pool)
        .await?;
        super::record_success(&self.pool, connection.id, None).await?;
        Ok(Outcome::Done)
    }
}

fn incident_id(details: &Value) -> Option<Uuid> {
    text(details, &["incident", "id"]).parse().ok()
}

fn text_id(details: &Value, key: &str) -> Option<Uuid> {
    text(details, &[key, "id"]).parse().ok()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_least_severity_decides() {
        assert!(reaches("critical", "critical"));
        assert!(!reaches("warning", "critical"));
        assert!(reaches("warning", "warning"));
        assert!(reaches("critical", "warning"));
        assert!(!reaches("", "warning"));
    }

    #[test]
    fn tickets_say_what_where_and_link_back() {
        let id = Uuid::nil();
        let details = json!({
            "incident": {"id": id, "severity": "critical", "metric": "disk_used_percent",
                         "instance": "/var", "value": 97.5, "opened_at": "2026-11-22T10:00:00Z"},
            "server": {"id": "s", "name": "web-01"},
            "site": {"id": "x", "name": "paris"},
        });
        let text = incident_text(&details, "https://mf.example.com");
        assert_eq!(text.summary, "Critical: disk_used_percent (/var) on web-01");
        assert!(text.details.contains("Value: 97.5"), "{}", text.details);
        assert!(text.details.contains("Site: paris"));
        assert!(text
            .details
            .contains(&format!("https://mf.example.com/incidents/view/?id={id}")));
        assert!(text
            .details
            .ends_with(&format!("MonitorFlow incident: {id}")));
    }

    #[test]
    fn long_summaries_are_cut() {
        let details = json!({"incident": {"severity": "warning", "metric": "x".repeat(500)},
                             "relay": {"name": "relay-1"}});
        let summary = incident_text(&details, "").summary;
        assert_eq!(summary.chars().count(), MAX_SUMMARY);
        assert!(summary.starts_with("Warning: xxx"));
    }
}
