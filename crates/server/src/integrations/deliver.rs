//! Sends pending deliveries (ADR-0102). Runs on every server instance
//! (ADR-0089): a delivery is claimed by moving its `next_at` one
//! [`LEASE`] ahead in its own transaction, so two instances never send it
//! at once, and one that dies mid-send leaves it to be claimed again when
//! the lease runs out. At least once, never zero times.
//!
//! A delivery waits while an earlier one to the same endpoint about the
//! same subject is pending, so an incident's `resolved` never overtakes its
//! `opened`. Failures are retried after [`retry_delay`] until the delivery
//! is [`MAX_AGE`] old; an endpoint without a success for that long is
//! disabled, and administrators are told through their notification
//! channels.

use std::future::Future;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use serde_json::Value;
use sqlx::PgPool;
use tokio::task::JoinSet;
use tokio::time::{interval, MissedTickBehavior};
use uuid::Uuid;

use super::{host, Clients, Endpoint, Failure, StoredEvent, ON};
use crate::notify::Coverage;

/// Time between two looks for due deliveries.
pub const POLL: Duration = Duration::from_secs(2);
/// How long a claim lasts: well over a request's timeout.
pub const LEASE: Duration = Duration::from_secs(60);
/// Deliveries claimed per pass, sent together.
const BATCH: i64 = 20;
/// The first wait after a failure; each next one doubles, up to
/// [`LAST_RETRY`].
const FIRST_RETRY: Duration = Duration::from_secs(30);
const LAST_RETRY: Duration = Duration::from_secs(3600);
/// A delivery this old gives up after its next failure, and an endpoint
/// without a success for this long is disabled.
pub const MAX_AGE: TimeDelta = TimeDelta::hours(24);
/// Events are kept this long, for replays.
pub const KEEP_DAYS: i32 = 30;
/// Passes between two prunes: about hourly at the default poll.
const PRUNE_PASSES: u64 = 1800;

/// The wait before the next try after `attempts` tries.
pub fn retry_delay(attempts: i32) -> Duration {
    let doublings = attempts.saturating_sub(1).clamp(0, 16) as u32;
    FIRST_RETRY.saturating_mul(1 << doublings).min(LAST_RETRY)
}

/// A claimed delivery, with its endpoint and event.
#[derive(Debug, sqlx::FromRow)]
struct Claimed {
    id: i64,
    attempts: i32,
    replay: bool,
    endpoint_id: Uuid,
    name: String,
    url: String,
    secret: String,
    events: Vec<String>,
    allow_private: bool,
    event_id: Uuid,
    event: String,
    details: Value,
    event_created_at: DateTime<Utc>,
}

impl Claimed {
    fn endpoint(&self) -> Endpoint {
        Endpoint {
            id: self.endpoint_id,
            name: self.name.clone(),
            url: self.url.clone(),
            secret: crate::secrets::open(
                crate::secrets::Column::IntegrationEndpoints,
                &self.secret,
            )
            .map_err(|e| e.to_string()),
            events: self.events.clone(),
            // Not needed to send.
            coverage: Coverage::Everything,
            allow_private: self.allow_private,
            state: ON.into(),
        }
    }

    fn event(&self) -> StoredEvent {
        StoredEvent {
            id: self.event_id,
            event: self.event.clone(),
            details: self.details.clone(),
            created_at: self.event_created_at,
        }
    }
}

#[derive(Clone)]
pub struct Deliverer {
    pool: PgPool,
    /// Base URL of the web interface, for links.
    web_url: String,
    clients: Clients,
}

impl Deliverer {
    pub fn new(pool: PgPool, web_url: impl Into<String>) -> Self {
        Self {
            pool,
            web_url: web_url.into().trim_end_matches('/').to_owned(),
            clients: Clients::default(),
        }
    }

    /// Sends due deliveries every `poll` until `stop` resolves.
    pub async fn run(self, poll: Duration, stop: impl Future<Output = ()>) {
        tokio::pin!(stop);
        let mut tick = interval(poll);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut passes = 0u64;
        loop {
            tokio::select! {
                _ = &mut stop => return,
                _ = tick.tick() => {}
            }
            if let Err(error) = self.pass().await {
                tracing::warn!(%error, "could not read integration deliveries; will retry");
            }
            if passes.is_multiple_of(PRUNE_PASSES) {
                if let Err(error) = self.prune().await {
                    tracing::warn!(%error, "could not prune integration events");
                }
            }
            passes += 1;
        }
    }

    /// Claims due deliveries, sends them together and records each
    /// outcome; how many were claimed.
    pub async fn pass(&self) -> Result<usize, sqlx::Error> {
        let claimed = self.claim().await?;
        let count = claimed.len();
        let mut sends = JoinSet::new();
        for delivery in claimed {
            let this = self.clone();
            sends.spawn(async move {
                let outcome = this
                    .clients
                    .post(
                        &delivery.endpoint(),
                        &delivery.event(),
                        &this.web_url,
                        delivery.replay,
                    )
                    .await;
                this.record(&delivery, outcome).await
            });
        }
        while let Some(done) = sends.join_next().await {
            match done {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    tracing::warn!(%error, "could not record an integration delivery; it will be sent again")
                }
                Err(error) => tracing::warn!(%error, "an integration delivery task failed"),
            }
        }
        Ok(count)
    }

    /// Due deliveries of endpoints that are on, the first pending one of
    /// each subject only, moved one lease ahead and counted as an attempt.
    async fn claim(&self) -> Result<Vec<Claimed>, sqlx::Error> {
        sqlx::query_as(
            "WITH due AS (
                 SELECT d.id FROM integration_deliveries d
                 JOIN integration_endpoints e ON e.id = d.endpoint_id
                 WHERE d.state = 'pending' AND d.next_at <= now() AND e.state = 'on'
                   AND NOT EXISTS (
                       SELECT 1 FROM integration_deliveries p
                       WHERE p.endpoint_id = d.endpoint_id AND p.subject = d.subject
                         AND p.state = 'pending' AND p.id < d.id)
                 ORDER BY d.id LIMIT $1
                 FOR UPDATE OF d SKIP LOCKED)
             UPDATE integration_deliveries d
             SET next_at = now() + make_interval(secs => $2), attempts = d.attempts + 1,
                 last_attempt_at = now()
             FROM due, integration_endpoints e, integration_events v
             WHERE d.id = due.id AND e.id = d.endpoint_id AND v.seq = d.event_seq
             RETURNING d.id, d.attempts, d.replay, e.id AS endpoint_id, e.name, e.url, e.secret,
                       e.events, e.allow_private, v.id AS event_id, v.event, v.details,
                       v.created_at AS event_created_at",
        )
        .bind(BATCH)
        .bind(LEASE.as_secs_f64())
        .fetch_all(&self.pool)
        .await
    }

    /// Writes the outcome of an attempt, unless another instance claimed
    /// the delivery since (its attempt count moved on). After a failure,
    /// disables the endpoint once it has failed for [`MAX_AGE`] without a
    /// success, and tells the administrators.
    async fn record(
        &self,
        delivery: &Claimed,
        outcome: Result<u16, Failure>,
    ) -> Result<(), sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        match outcome {
            Ok(status) => {
                sqlx::query(
                    "UPDATE integration_deliveries
                     SET state = 'delivered', finished_at = now(), last_status = $3,
                         last_error = NULL
                     WHERE id = $1 AND attempts = $2 AND state = 'pending'",
                )
                .bind(delivery.id)
                .bind(delivery.attempts)
                .bind(i32::from(status))
                .execute(&mut *tx)
                .await?;
                sqlx::query(
                    "UPDATE integration_endpoints SET failing_since = NULL, last_success_at = now()
                     WHERE id = $1",
                )
                .bind(delivery.endpoint_id)
                .execute(&mut *tx)
                .await?;
            }
            Err(failure) => {
                tracing::info!(
                    endpoint = %delivery.endpoint_id,
                    event = %delivery.event_id,
                    attempts = delivery.attempts,
                    error = %failure.message,
                    "an integration delivery failed"
                );
                sqlx::query(
                    "UPDATE integration_deliveries
                     SET state = CASE WHEN created_at <= now() - make_interval(secs => $3)
                                      THEN 'failed' ELSE 'pending' END,
                         finished_at = CASE WHEN created_at <= now() - make_interval(secs => $3)
                                            THEN now() END,
                         next_at = now() + make_interval(secs => $4),
                         last_status = $5, last_error = $6
                     WHERE id = $1 AND attempts = $2 AND state = 'pending'",
                )
                .bind(delivery.id)
                .bind(delivery.attempts)
                .bind(MAX_AGE.as_seconds_f64())
                .bind(retry_delay(delivery.attempts).as_secs_f64())
                .bind(failure.status.map(i32::from))
                .bind(&failure.message)
                .execute(&mut *tx)
                .await?;
                sqlx::query(
                    "UPDATE integration_endpoints SET failing_since = coalesce(failing_since, now())
                     WHERE id = $1",
                )
                .bind(delivery.endpoint_id)
                .execute(&mut *tx)
                .await?;
                self.disable_if_failing(&mut tx, delivery, &failure.message)
                    .await?;
            }
        }
        tx.commit().await
    }

    /// Turns the endpoint off once it has failed for [`MAX_AGE`] without a
    /// success, and writes `integration.disabled` for the notification
    /// channels covering it; once, as only an endpoint that is on changes.
    async fn disable_if_failing(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        delivery: &Claimed,
        error: &str,
    ) -> Result<(), sqlx::Error> {
        let disabled = sqlx::query(
            "UPDATE integration_endpoints SET state = 'disabled'
             WHERE id = $1 AND state = 'on'
               AND failing_since <= now() - make_interval(secs => $2)",
        )
        .bind(delivery.endpoint_id)
        .bind(MAX_AGE.as_seconds_f64())
        .execute(&mut **tx)
        .await?;
        if disabled.rows_affected() == 0 {
            return Ok(());
        }
        tracing::warn!(endpoint = %delivery.endpoint_id, "disabled an integration endpoint after a day of failures");
        sqlx::query(
            "INSERT INTO notification_outbox (event, customer_id, details)
             SELECT 'integration.disabled', c.id, jsonb_build_object(
                 'occurred_at', now(),
                 'customer', notification_ref(c.id, c.name),
                 'site', notification_ref(s.id, s.name),
                 'integration', jsonb_build_object('id', e.id, 'name', e.name,
                                                   'host', $2::text, 'error', $3::text))
             FROM integration_endpoints e
             LEFT JOIN sites s ON s.id = e.site_id
             LEFT JOIN customers c ON c.id = coalesce(e.customer_id, s.customer_id)
             WHERE e.id = $1",
        )
        .bind(delivery.endpoint_id)
        .bind(host(&delivery.url))
        .bind(error)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    /// Deletes events older than [`KEEP_DAYS`], with their deliveries.
    pub async fn prune(&self) -> Result<u64, sqlx::Error> {
        let done = sqlx::query(
            "DELETE FROM integration_events WHERE created_at < now() - make_interval(days => $1)",
        )
        .bind(KEEP_DAYS)
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retries_double_from_thirty_seconds_up_to_an_hour() {
        let waits: Vec<u64> = (1..=10).map(|n| retry_delay(n).as_secs()).collect();
        assert_eq!(waits, [30, 60, 120, 240, 480, 960, 1920, 3600, 3600, 3600]);
        assert_eq!(retry_delay(0).as_secs(), 30);
        assert_eq!(retry_delay(i32::MAX).as_secs(), 3600);
    }

    #[test]
    fn a_day_of_retries_is_about_thirty_attempts() {
        let mut elapsed = Duration::ZERO;
        let mut attempts = 0;
        while elapsed < MAX_AGE.to_std().unwrap() {
            attempts += 1;
            elapsed += retry_delay(attempts);
        }
        assert!((25..=35).contains(&attempts), "{attempts}");
    }
}
