//! Delivers the outbox: claims pending events, renders them, and sends
//! each to every channel that wants it, a few tries per delivery. At most
//! once: a claimed event is not sent again after a restart.

use std::future::Future;
use std::time::Duration;

use sqlx::PgPool;
use tokio::task::JoinSet;
use tokio::time::{interval, sleep, MissedTickBehavior};

use super::message::{render, Event, Message};
use super::send::{deliver, http_client, SendError};
use super::{channels, record_outcome, Channel};

/// Time between two looks at the outbox.
pub const POLL: Duration = Duration::from_secs(5);
/// Events claimed per pass.
const BATCH: i64 = 100;
/// Events older than this when claimed are dropped: after a long outage a
/// flood of stale alerts helps no one.
const MAX_AGE: chrono::TimeDelta = chrono::TimeDelta::hours(24);
/// Delivered events are kept this long, then deleted.
const KEEP_DAYS: i32 = 7;

#[derive(Clone)]
pub struct Dispatcher {
    pool: PgPool,
    /// Base URL of the web interface, for links.
    web_url: String,
    http: reqwest::Client,
    /// Waits before the second and later tries of a delivery.
    retries: Vec<Duration>,
}

impl Dispatcher {
    pub fn new(pool: PgPool, web_url: impl Into<String>) -> Self {
        Self {
            pool,
            web_url: web_url.into().trim_end_matches('/').to_owned(),
            http: http_client(),
            retries: vec![Duration::from_secs(2), Duration::from_secs(10)],
        }
    }

    /// Without waits between tries, for tests.
    pub fn with_retries(mut self, retries: Vec<Duration>) -> Self {
        self.retries = retries;
        self
    }

    /// Delivers the outbox every `poll` until `stop` resolves.
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
                tracing::warn!(%error, "could not read the notification outbox; will retry");
            }
            passes += 1;
            // About hourly at the default poll.
            if passes % 720 == 1 {
                if let Err(error) = self.prune().await {
                    tracing::warn!(%error, "could not prune the notification outbox");
                }
            }
        }
    }

    /// Claims and delivers pending events; returns how many were claimed.
    pub async fn pass(&self) -> Result<usize, sqlx::Error> {
        let mut events: Vec<Event> = sqlx::query_as(
            "UPDATE notification_outbox SET sent_at = now()
             WHERE id IN (SELECT id FROM notification_outbox WHERE sent_at IS NULL
                          ORDER BY id LIMIT $1 FOR UPDATE SKIP LOCKED)
             RETURNING id, event, customer_id, details, created_at",
        )
        .bind(BATCH)
        .fetch_all(&self.pool)
        .await?;
        if events.is_empty() {
            return Ok(0);
        }
        events.sort_by_key(|e| e.id);
        let claimed = events.len();
        let oldest = chrono::Utc::now() - MAX_AGE;
        let (fresh, stale): (Vec<_>, Vec<_>) =
            events.into_iter().partition(|e| e.created_at >= oldest);
        if !stale.is_empty() {
            tracing::warn!(
                events = stale.len(),
                "dropped notifications older than a day"
            );
        }
        let channels = channels(&self.pool).await?;
        let mut tasks = JoinSet::new();
        for channel in channels {
            let messages: Vec<Message> = fresh
                .iter()
                .filter(|e| channel.wants(e.customer_id))
                .map(|e| render(e, &self.web_url))
                .collect();
            if messages.is_empty() {
                continue;
            }
            let this = self.clone();
            tasks.spawn(async move { this.deliver_all(&channel, &messages).await });
        }
        while tasks.join_next().await.is_some() {}
        Ok(claimed)
    }

    /// Sends `messages` in order. After a delivery failed every try, the
    /// rest get one try each, so a dead channel does not hold the pass.
    async fn deliver_all(&self, channel: &Channel, messages: &[Message]) {
        let mut last = Ok(());
        let mut sent = 0usize;
        for message in messages {
            let retries = if last.is_ok() { &self.retries[..] } else { &[] };
            last = self.deliver_with_retries(channel, message, retries).await;
            match &last {
                Ok(()) => sent += 1,
                Err(error) => {
                    tracing::warn!(channel = %channel.id, kind = channel.destination.kind(), %error, "notification not delivered")
                }
            }
        }
        if sent > 0 {
            tracing::info!(channel = %channel.id, sent, failed = messages.len() - sent, "notifications delivered");
        }
        let error = last.err().map(|e| e.0);
        if let Err(db) = record_outcome(&self.pool, channel.id, error.as_deref()).await {
            tracing::warn!(channel = %channel.id, error = %db, "could not record a notification outcome");
        }
    }

    async fn deliver_with_retries(
        &self,
        channel: &Channel,
        message: &Message,
        retries: &[Duration],
    ) -> Result<(), SendError> {
        let mut result = deliver(channel, message, &self.http).await;
        for wait in retries {
            if result.is_ok() {
                break;
            }
            sleep(*wait).await;
            result = deliver(channel, message, &self.http).await;
        }
        result
    }

    /// Sends a test to `channel` once and records the outcome.
    pub async fn test(&self, channel: &Channel) -> Result<Result<(), SendError>, sqlx::Error> {
        let result = deliver(channel, &render(&Event::test(), &self.web_url), &self.http).await;
        let error = result.as_ref().err().map(|e| e.0.as_str());
        record_outcome(&self.pool, channel.id, error).await?;
        Ok(result)
    }

    /// Deletes events delivered more than a week ago.
    pub async fn prune(&self) -> Result<u64, sqlx::Error> {
        Ok(sqlx::query(
            "DELETE FROM notification_outbox
             WHERE sent_at < now() - make_interval(days => $1)",
        )
        .bind(KEEP_DAYS)
        .execute(&self.pool)
        .await?
        .rows_affected())
    }
}
