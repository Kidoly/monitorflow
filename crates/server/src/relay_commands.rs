//! Commands to relays (ADR-0055): only `self_update`, sent on the relay's
//! own stream, one in flight per relay, signed like agents' commands
//! (ADR-0031).
//!
//! One task for the whole server, like the agents' dispatcher: it looks at
//! a relay when its stream opens, when a command is queued for it, when one
//! ends, and every [`POLL`] at every connected relay, after expiring stale
//! commands. A command in flight that was not sent on the relay's current
//! connection is sent again: the relay answers from its journal, or from
//! its update status after the restart the update caused.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use monitorflow_common::command::{self, SELF_UPDATE};
use monitorflow_common::proto::v1::envelope::Payload;
use monitorflow_common::proto::v1::{Command, CommandResult};
use monitorflow_common::tls::PeerRole;
use serde::Serialize;
use sqlx::{PgConnection, PgPool};
use tokio::sync::mpsc;
use tokio::time::{interval, timeout, MissedTickBehavior};
use uuid::Uuid;

use crate::ca::CertificateAuthority;
use crate::commands::Outcome;
#[cfg(test)]
use crate::commands::{MAX_ERROR, MAX_OUTPUT};
use crate::grpc::Registry;

pub const POLL: Duration = Duration::from_secs(10);
/// A command that started and sent no result this long is failed: longer
/// than any health timeout plus the restart.
const RUN_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);
const WAKE_QUEUE: usize = 256;
const SEND_TIMEOUT: Duration = Duration::from_secs(1);

/// One relay command, as the API shows it: a self-update (ADR-0055).
#[derive(Debug, Clone, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct RelayCommand {
    pub id: Uuid,
    pub relay_id: Uuid,
    /// The release asked for.
    pub version: String,
    /// `queued`, `sent` or `running` while in flight; then `succeeded`,
    /// `failed`, `expired` (the relay did not take it within 24 hours) or
    /// `cancelled`.
    #[schema(example = "succeeded")]
    pub state: String,
    /// The user who asked; `null` for automatic updates, or when that user
    /// was deleted.
    pub requested_by: Option<Uuid>,
    pub issued_at: DateTime<Utc>,
    /// When it expires if the relay has not taken it.
    pub expires_at: DateTime<Utc>,
    /// `null` while in flight.
    pub finished_at: Option<DateTime<Utc>>,
    /// What the updater printed, when it reported.
    pub stdout: Option<String>,
    /// Why it failed; `null` otherwise.
    pub error: Option<String>,
}

const COLUMNS: &str =
    "id, relay_id, version, state, requested_by, issued_at, expires_at, finished_at, stdout, error";

/// Queues a `self_update` to `version` for `relay_id`; `None` when the
/// relay already has a command in flight.
pub async fn enqueue_in(
    conn: &mut PgConnection,
    relay_id: Uuid,
    version: &str,
    payload: &[u8],
    ttl: Duration,
    requested_by: Option<Uuid>,
) -> Result<Option<RelayCommand>, sqlx::Error> {
    // SQL-safe: interpolates only the constant COLUMNS.
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "INSERT INTO relay_commands (relay_id, kind, payload, version, expires_at, requested_by)
         VALUES ($1, $2, $3, $4, now() + make_interval(secs => $5), $6)
         ON CONFLICT (relay_id) WHERE state IN ('queued', 'sent', 'running') DO NOTHING
         RETURNING {COLUMNS}"
    )))
    .bind(relay_id)
    .bind(SELF_UPDATE)
    .bind(payload)
    .bind(version)
    .bind(ttl.as_secs_f64())
    .bind(requested_by)
    .fetch_optional(&mut *conn)
    .await
}

/// The newest commands of `relay_id`, at most `limit`.
pub async fn recent(
    pool: &PgPool,
    relay_id: Uuid,
    limit: i64,
) -> Result<Vec<RelayCommand>, sqlx::Error> {
    // SQL-safe: interpolates only the constant COLUMNS.
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM relay_commands WHERE relay_id = $1
         ORDER BY issued_at DESC LIMIT $2"
    )))
    .bind(relay_id)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// The latest command of each of `relay_ids` that has one.
pub async fn latest(pool: &PgPool, relay_ids: &[Uuid]) -> Result<Vec<RelayCommand>, sqlx::Error> {
    // SQL-safe: interpolates only the constant COLUMNS.
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT DISTINCT ON (relay_id) {COLUMNS} FROM relay_commands
         WHERE relay_id = ANY($1)
         ORDER BY relay_id, issued_at DESC"
    )))
    .bind(relay_ids)
    .fetch_all(pool)
    .await
}

/// What a result stores: its state, then, once final, its exit code,
/// output and error, cut and cleaned as agents' results are, NUL and
/// invalid UTF-8 included, which PostgreSQL text refuses.
struct Stored {
    state: &'static str,
    final_state: bool,
    exit_code: i32,
    stdout: Option<String>,
    stderr: Option<String>,
    error: Option<String>,
}

impl Stored {
    fn of(result: &CommandResult) -> Self {
        let some = |text: String| Some(text).filter(|t| !t.is_empty());
        match Outcome::of(result) {
            Outcome::Running => Self {
                state: "running",
                final_state: false,
                exit_code: result.exit_code,
                stdout: None,
                stderr: None,
                error: None,
            },
            Outcome::Final {
                state,
                exit_code,
                stdout,
                stderr,
                error,
                ..
            } => Self {
                state: state.as_str(),
                final_state: true,
                exit_code,
                stdout: some(stdout),
                stderr: some(stderr),
                error,
            },
        }
    }
}

/// Records `result`, sent by the relay `peer_id`, on its own command in
/// flight only. Returns the command's version and new state, or `None`
/// when nothing changed.
pub async fn record_result(
    pool: &PgPool,
    peer_id: &str,
    result: &CommandResult,
) -> Result<Option<(String, &'static str)>, sqlx::Error> {
    let Ok(id) = Uuid::parse_str(&result.command_id) else {
        return Ok(None);
    };
    let stored = Stored::of(result);
    let version = sqlx::query_scalar::<_, String>(
        "UPDATE relay_commands c
         SET state = $3,
             started_at = coalesce(c.started_at, now()),
             finished_at = CASE WHEN $4 THEN now() END,
             exit_code = CASE WHEN $4 THEN $5 END,
             stdout = CASE WHEN $4 THEN $6 END,
             stderr = CASE WHEN $4 THEN $7 END,
             error = CASE WHEN $4 THEN $8 END
         FROM relays r
         WHERE c.id = $1 AND r.id = c.relay_id AND r.peer_id = $2
           AND c.state IN ('sent', 'running')
         RETURNING c.version",
    )
    .bind(id)
    .bind(peer_id)
    .bind(stored.state)
    .bind(stored.final_state)
    .bind(stored.exit_code)
    .bind(stored.stdout)
    .bind(stored.stderr)
    .bind(stored.error)
    .fetch_optional(pool)
    .await?;
    Ok(version.map(|v| (v, stored.state)))
}

/// Handle shared by relay streams, the API and automatic updates.
#[derive(Debug, Clone)]
pub struct RelayCommands {
    pool: PgPool,
    wake: mpsc::Sender<String>,
}

impl RelayCommands {
    /// Starts the sender, which runs until every handle is dropped.
    pub fn spawn(pool: PgPool, registry: Registry, ca: Arc<CertificateAuthority>) -> Self {
        let (wake, rx) = mpsc::channel(WAKE_QUEUE);
        let sender = Sender {
            pool: pool.clone(),
            registry,
            ca,
            wake: rx,
            delivered: HashMap::new(),
        };
        tokio::spawn(sender.run());
        Self { pool, wake }
    }

    /// Looks at the relay `peer_id`: its stream opened or a command was
    /// queued for it.
    pub fn dispatch(&self, peer_id: &str) {
        if self.wake.try_send(peer_id.to_owned()).is_err() {
            tracing::debug!(
                relay_id = peer_id,
                "relay dispatch queue full; the next poll sends it"
            );
        }
    }

    /// Records a result from the relay `peer_id`. Failures are logged:
    /// the command stays in flight, is sent again and answered again.
    pub async fn record(&self, peer_id: &str, result: CommandResult) {
        let command_id = result.command_id.as_str();
        match record_result(&self.pool, peer_id, &result).await {
            Ok(Some((version, state))) => {
                tracing::info!(relay_id = peer_id, command_id, %version, state, "relay command result");
                if state != "running" {
                    self.dispatch(peer_id);
                }
            }
            Ok(None) => tracing::debug!(
                relay_id = peer_id,
                command_id,
                "ignoring a result for no relay command in flight"
            ),
            Err(error) => {
                tracing::warn!(relay_id = peer_id, command_id, %error, "could not record a relay command result")
            }
        }
    }
}

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    kind: String,
    payload: Vec<u8>,
    state: String,
    issued_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

struct Sender {
    pool: PgPool,
    registry: Registry,
    ca: Arc<CertificateAuthority>,
    wake: mpsc::Receiver<String>,
    /// The command last sent to each relay, and on which connection.
    delivered: HashMap<String, (u64, Uuid)>,
}

impl Sender {
    async fn run(mut self) {
        let mut tick = interval(POLL);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                peer = self.wake.recv() => match peer {
                    Some(peer) => self.dispatch(&peer).await,
                    None => return,
                },
                _ = tick.tick() => self.poll().await,
            }
        }
    }

    async fn poll(&mut self) {
        if let Err(error) = self.expire().await {
            tracing::warn!(%error, "could not expire relay commands; will retry");
        }
        let relays: Vec<String> = self
            .registry
            .connected()
            .into_iter()
            .filter(|p| p.role == PeerRole::Relay)
            .map(|p| p.id)
            .collect();
        self.delivered.retain(|id, _| relays.contains(id));
        for id in relays {
            self.dispatch(&id).await;
        }
    }

    /// Unsent past their expiry, or running too long.
    async fn expire(&self) -> Result<(), sqlx::Error> {
        sqlx::query(
            "UPDATE relay_commands
             SET state = CASE WHEN state = 'running' THEN 'failed' ELSE 'expired' END,
                 finished_at = now(),
                 error = CASE WHEN state = 'running'
                              THEN 'no result from the relay in time' ELSE error END
             WHERE (state IN ('queued', 'sent') AND expires_at < now())
                OR (state = 'running' AND started_at < now() - make_interval(secs => $1))",
        )
        .bind(RUN_TIMEOUT.as_secs_f64())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn dispatch(&mut self, peer_id: &str) {
        let Some(peer) = self.registry.get(PeerRole::Relay, peer_id) else {
            return;
        };
        let row: Option<Row> = match sqlx::query_as(
            "SELECT c.id, c.kind, c.payload, c.state, c.issued_at, c.expires_at
             FROM relay_commands c JOIN relays r ON r.id = c.relay_id
             WHERE r.peer_id = $1 AND r.revoked_at IS NULL
               AND c.state IN ('queued', 'sent', 'running')
             ORDER BY c.issued_at LIMIT 1",
        )
        .bind(peer_id)
        .fetch_optional(&self.pool)
        .await
        {
            Ok(row) => row,
            Err(error) => {
                tracing::warn!(relay_id = peer_id, %error, "could not load relay commands; will retry");
                return;
            }
        };
        let Some(row) = row else { return };
        if row.state == "queued" {
            let marked = sqlx::query(
                "UPDATE relay_commands SET state = 'sent', sent_at = now()
                 WHERE id = $1 AND state = 'queued' AND expires_at > now()",
            )
            .bind(row.id)
            .execute(&self.pool)
            .await;
            if !matches!(marked, Ok(r) if r.rows_affected() == 1) {
                return;
            }
        } else if self.delivered.get(peer_id) == Some(&(peer.connection, row.id)) {
            return;
        }
        if self.send(peer_id, &row).await {
            self.delivered
                .insert(peer_id.to_owned(), (peer.connection, row.id));
        }
    }

    async fn send(&self, peer_id: &str, row: &Row) -> bool {
        let mut command = Command {
            command_id: row.id.to_string(),
            kind: row.kind.clone(),
            payload: row.payload.clone(),
            signature: Vec::new(),
            issued_at_unix_ms: row.issued_at.timestamp_millis(),
            expires_at_unix_ms: row.expires_at.timestamp_millis(),
            agent_id: peer_id.to_owned(),
        };
        command.signature = match self.ca.sign_command(&command::signed_bytes(&command)) {
            Ok(signature) => signature,
            Err(error) => {
                tracing::error!(command_id = %row.id, %error, "could not sign a relay command");
                return false;
            }
        };
        let envelope = self.registry.envelope(Payload::Command(command));
        let sending = self.registry.send(PeerRole::Relay, peer_id, envelope);
        match timeout(SEND_TIMEOUT, sending).await {
            Ok(Ok(())) => {
                tracing::info!(relay_id = peer_id, command_id = %row.id, "relay command sent");
                true
            }
            _ => {
                tracing::debug!(relay_id = peer_id, "relay command not sent; will retry");
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use monitorflow_common::proto::v1::CommandStatus;

    use super::*;

    fn result(status: CommandStatus, exit_code: i32) -> CommandResult {
        CommandResult {
            status: status as i32,
            exit_code,
            ..CommandResult::default()
        }
    }

    #[test]
    fn results_map_to_states() {
        let state = |status, code| Stored::of(&result(status, code)).state;
        assert_eq!(state(CommandStatus::Running, 0), "running");
        assert_eq!(state(CommandStatus::Succeeded, 0), "succeeded");
        assert_eq!(state(CommandStatus::Failed, 1), "failed");
        assert_eq!(state(CommandStatus::Rejected, -1), "failed");
        assert_eq!(state(CommandStatus::Expired, 0), "expired");
        assert_eq!(state(CommandStatus::Unspecified, 0), "succeeded");
        assert_eq!(state(CommandStatus::Unspecified, 3), "failed");
    }

    #[test]
    fn output_keeps_the_end_without_nul() {
        let mut done = result(CommandStatus::Failed, 1);
        assert_eq!(Stored::of(&done).stdout, None);
        done.stdout = vec![b'x'; MAX_OUTPUT + 10];
        done.stderr = b"bad\0byte".to_vec();
        done.error = "e".repeat(MAX_ERROR + 10);
        let stored = Stored::of(&done);
        assert_eq!(stored.stdout.unwrap().len(), MAX_OUTPUT);
        assert_eq!(stored.stderr.as_deref(), Some("bad\u{FFFD}byte"));
        assert_eq!(stored.error.unwrap().len(), MAX_ERROR);
        let running = Stored::of(&CommandResult {
            stdout: b"partial".to_vec(),
            ..result(CommandStatus::Running, 0)
        });
        assert_eq!((running.final_state, running.stdout), (false, None));
    }
}
