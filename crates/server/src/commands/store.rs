//! `commands` in PostgreSQL. Every state change is one guarded UPDATE, so
//! the dispatcher, results arriving on streams and the expiry sweep can run
//! concurrently: a transition from a state the row is no longer in does
//! nothing.

use std::time::Duration;

use chrono::{DateTime, Utc};
use monitorflow_common::proto::v1::CommandResult;
use serde_json::json;
use sqlx::{PgExecutor, PgPool};
use uuid::Uuid;

use super::{CommandRow, Outcome, State};
use crate::api::audit::{self, Event};

const COLUMNS: &str = "c.id, c.server_id, c.kind, c.payload, c.state, c.issued_at, c.expires_at,
     c.sent_at, c.started_at, c.finished_at, c.exit_code, c.stdout, c.stderr, c.error,
     c.requested_by, c.campaign_id";

const UNIQUE_VIOLATION: &str = "23505";

/// A command to queue.
#[derive(Debug, Clone)]
pub struct NewCommand {
    pub server_id: Uuid,
    pub kind: String,
    pub payload: Vec<u8>,
    /// From now to `expires_at`.
    pub ttl: Duration,
    pub requested_by: Option<Uuid>,
    pub campaign_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct CommandStore {
    pool: PgPool,
}

impl CommandStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Queues `new`; the dispatcher sends it once the agent is connected.
    pub async fn enqueue(&self, new: &NewCommand) -> Result<CommandRow, sqlx::Error> {
        Self::enqueue_in(&self.pool, new).await
    }

    /// [`Self::enqueue`] in a caller's transaction, to audit it with the
    /// insert.
    pub async fn enqueue_in(
        db: impl PgExecutor<'_>,
        new: &NewCommand,
    ) -> Result<CommandRow, sqlx::Error> {
        sqlx::query_as(&format!(
            "INSERT INTO commands AS c (server_id, kind, payload, expires_at, requested_by, campaign_id)
             VALUES ($1, $2, $3, now() + make_interval(secs => $4), $5, $6)
             RETURNING {COLUMNS}"
        ))
        .bind(new.server_id)
        .bind(&new.kind)
        .bind(&new.payload)
        .bind(new.ttl.as_secs_f64())
        .bind(new.requested_by)
        .bind(new.campaign_id)
        .fetch_one(db)
        .await
    }

    /// Commands of `server_id`, newest first, issued before `before`
    /// (`issued_at`, `id`) when set.
    pub async fn list(
        &self,
        server_id: Uuid,
        before: Option<(DateTime<Utc>, Uuid)>,
        limit: i64,
    ) -> Result<Vec<CommandRow>, sqlx::Error> {
        let (before_at, before_id) = before.unzip();
        sqlx::query_as(&format!(
            "SELECT {COLUMNS} FROM commands c
             WHERE c.server_id = $1
               AND ($2::timestamptz IS NULL OR (c.issued_at, c.id) < ($2, $3))
             ORDER BY c.issued_at DESC, c.id DESC
             LIMIT $4"
        ))
        .bind(server_id)
        .bind(before_at)
        .bind(before_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
    }

    pub async fn get(&self, id: Uuid) -> Result<Option<CommandRow>, sqlx::Error> {
        sqlx::query_as(&format!("SELECT {COLUMNS} FROM commands c WHERE c.id = $1"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await
    }

    /// What the agent `peer_id` should have now: its command in flight
    /// (`sent` or `running`), else its oldest unexpired queued one.
    pub async fn next(&self, peer_id: &str) -> Result<Option<CommandRow>, sqlx::Error> {
        sqlx::query_as(&format!(
            "SELECT {COLUMNS} FROM commands c
             JOIN servers s ON s.id = c.server_id
             WHERE s.peer_id = $1 AND s.revoked_at IS NULL
               AND (c.state IN ('sent', 'running')
                    OR (c.state = 'queued' AND c.expires_at > now()))
             ORDER BY c.state = 'queued', c.issued_at, c.id
             LIMIT 1"
        ))
        .bind(peer_id)
        .fetch_optional(&self.pool)
        .await
    }

    /// Moves a queued, unexpired command to `sent`. False when it is no
    /// longer queued or another command of the server is in flight.
    pub async fn mark_sent(&self, id: Uuid) -> Result<bool, sqlx::Error> {
        let updated = sqlx::query(
            "UPDATE commands SET state = 'sent', sent_at = now()
             WHERE id = $1 AND state = 'queued' AND expires_at > now()",
        )
        .bind(id)
        .execute(&self.pool)
        .await;
        match updated {
            Ok(done) => Ok(done.rows_affected() == 1),
            Err(sqlx::Error::Database(e)) if e.code().as_deref() == Some(UNIQUE_VIOLATION) => {
                Ok(false)
            }
            Err(e) => Err(e),
        }
    }

    /// Records `result`, sent by the agent `peer_id`, on its own command
    /// only. Returns the new state, or `None` when nothing changed: an
    /// unknown or foreign command, or one already final. Final results are
    /// audited in the same transaction.
    pub async fn record(
        &self,
        peer_id: &str,
        result: &CommandResult,
    ) -> Result<Option<State>, sqlx::Error> {
        let Ok(id) = Uuid::parse_str(&result.command_id) else {
            return Ok(None);
        };
        let (state, ran, exit_code, stdout, stderr, error) = match Outcome::of(result) {
            Outcome::Running => {
                let updated = sqlx::query(
                    "UPDATE commands c SET state = 'running', started_at = now()
                     FROM servers s
                     WHERE c.id = $1 AND s.id = c.server_id AND s.peer_id = $2 AND c.state = 'sent'",
                )
                .bind(id)
                .bind(peer_id)
                .execute(&self.pool)
                .await?;
                return Ok((updated.rows_affected() == 1).then_some(State::Running));
            }
            Outcome::Final {
                state,
                ran,
                exit_code,
                stdout,
                stderr,
                error,
            } => (state, ran, exit_code, stdout, stderr, error),
        };
        let mut tx = self.pool.begin().await?;
        // A late result replaces `expired`: the agent only starts a command
        // before its expiry (ADR-0031).
        let updated: Option<(String, Uuid)> = sqlx::query_as(
            "UPDATE commands c SET state = $3, exit_code = $4, stdout = $5, stderr = $6,
                 error = $7, finished_at = now(),
                 started_at = CASE WHEN $8 THEN COALESCE(c.started_at, now()) ELSE c.started_at END
             FROM servers s
             WHERE c.id = $1 AND s.id = c.server_id AND s.peer_id = $2
               AND c.state IN ('sent', 'running', 'expired')
             RETURNING c.kind, c.server_id",
        )
        .bind(id)
        .bind(peer_id)
        .bind(state)
        .bind(exit_code)
        .bind(&stdout)
        .bind(&stderr)
        .bind(&error)
        .bind(ran)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((kind, server_id)) = updated else {
            return Ok(None);
        };
        audit::record(
            &mut *tx,
            Event {
                actor_kind: "peer",
                actor_id: peer_id,
                action: "command.finish",
                target: Some(("command", &id.to_string())),
                details: json!({
                    "kind": kind,
                    "server_id": server_id,
                    "state": state.as_str(),
                    "exit_code": exit_code,
                }),
            },
        )
        .await?;
        tx.commit().await?;
        Ok(Some(state))
    }

    /// Ends commands that will not complete: queued ones past their
    /// expiry, sent ones `sent_grace` past it, and running ones that
    /// started `run_timeout` ago. Returns how many changed.
    pub async fn expire(
        &self,
        sent_grace: Duration,
        run_timeout: Duration,
    ) -> Result<u64, sqlx::Error> {
        let done = sqlx::query(
            "UPDATE commands SET finished_at = now(),
                 state = CASE WHEN state = 'running' THEN 'failed' ELSE 'expired' END,
                 error = CASE state
                     WHEN 'running' THEN 'no result from the agent'
                     WHEN 'sent' THEN 'the agent did not start it before expiry'
                     ELSE 'not delivered before expiry' END
             WHERE (state = 'queued' AND expires_at <= now())
                OR (state = 'sent' AND expires_at <= now() - make_interval(secs => $1))
                OR (state = 'running' AND started_at <= now() - make_interval(secs => $2))",
        )
        .bind(sent_grace.as_secs_f64())
        .bind(run_timeout.as_secs_f64())
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected())
    }
}
