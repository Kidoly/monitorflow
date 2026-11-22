//! The history of the central server's updates (ADR-0172): each update
//! kept in `server_updates` by the ID of its request, copied from the
//! updater's status file whenever the server reads it.

use chrono::{DateTime, Utc};
use monitorflow_common::server_update::{UpdateRun, UpdateState};
use serde::Serialize;
use sqlx::PgPool;

/// State of a request the updater did not take yet.
pub const REQUESTED: &str = "requested";

/// The updater's word for `state`, as stored.
fn state_text(state: UpdateState) -> String {
    serde_json::to_value(state)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".into())
}

/// Records a request from the web interface, before the updater takes it.
pub async fn requested(
    pool: &PgPool,
    id: &str,
    from: &str,
    to: &str,
    requested_by: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO server_updates (id, from_version, to_version, requested_by, state, started_at)
         VALUES ($1, $2, $3, $4, $5, now())
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(id)
    .bind(from)
    .bind(to)
    .bind(requested_by)
    .bind(REQUESTED)
    .execute(pool)
    .await?;
    Ok(())
}

/// Copies `run` as the updater last wrote it. Who asked stays the request's
/// when there was one.
pub async fn record(pool: &PgPool, run: &UpdateRun) -> Result<(), sqlx::Error> {
    if run.id.is_empty() || run.to.is_empty() {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO server_updates
             (id, from_version, to_version, requested_by, state, error, started_at, finished_at)
         VALUES ($1, $2, $3, nullif($4, ''), $5, $6, $7, $8)
         ON CONFLICT (id) DO UPDATE SET
             from_version = coalesce(excluded.from_version, server_updates.from_version),
             requested_by = coalesce(server_updates.requested_by, excluded.requested_by),
             state = excluded.state,
             error = excluded.error,
             started_at = least(server_updates.started_at, excluded.started_at),
             finished_at = excluded.finished_at
         WHERE (server_updates.state, server_updates.error, server_updates.finished_at)
               IS DISTINCT FROM (excluded.state, excluded.error, excluded.finished_at)",
    )
    .bind(&run.id)
    .bind(&run.from)
    .bind(&run.to)
    .bind(&run.requested_by)
    .bind(state_text(run.state))
    .bind(&run.error)
    .bind(run.started_at)
    .bind(run.finished_at)
    .execute(pool)
    .await?;
    Ok(())
}

/// How an update of the history ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[schema(as = ServerUpdateResult)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Asked for or running now.
    InProgress,
    /// The new version runs.
    Succeeded,
    /// Nothing changed.
    Failed,
    /// The previous version runs again.
    RolledBack,
    /// Not recorded: from before the history, or its end was not seen.
    Unknown,
}

/// One update of the server, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[schema(as = ServerUpdateHistoryEntry)]
pub struct Entry {
    /// The ID of its request, `update.id` of `GET /api/v1/server-update`.
    pub id: String,
    /// `null` when not known.
    pub from: Option<String>,
    pub to: String,
    /// The user name of who asked; `null` when the updater started it
    /// (from the command line) or it is not known.
    pub requested_by: Option<String>,
    pub started_at: DateTime<Utc>,
    /// `null` while running, and when the end was not recorded.
    pub finished_at: Option<DateTime<Utc>>,
    pub result: Outcome,
    /// Why it failed or rolled back, in the updater's words.
    pub error: Option<String>,
    /// `result` was inferred: a later update started from the version it
    /// installed, or the server runs it.
    pub inferred: bool,
}

#[derive(sqlx::FromRow)]
struct Row {
    id: String,
    from_version: Option<String>,
    to_version: String,
    requested_by: Option<String>,
    state: String,
    error: Option<String>,
    started_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
}

/// The `limit` newest updates. `busy` is the request in progress, if any:
/// the updater's current run, or a request it has not taken yet.
pub async fn list(
    pool: &PgPool,
    running: &str,
    busy: Option<&str>,
    limit: i64,
) -> Result<Vec<Entry>, sqlx::Error> {
    // One more, to infer how the oldest listed ended.
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, from_version, to_version, requested_by, state, error, started_at, finished_at
         FROM server_updates ORDER BY started_at DESC, id DESC LIMIT $1",
    )
    .bind(limit + 1)
    .fetch_all(pool)
    .await?;
    Ok(entries(rows, running, busy, limit as usize))
}

/// Rows newest first to entries, inferring how unrecorded ones ended.
fn entries(rows: Vec<Row>, running: &str, busy: Option<&str>, limit: usize) -> Vec<Entry> {
    let mut list = Vec::with_capacity(rows.len());
    for (i, row) in rows.iter().enumerate().take(limit) {
        let recorded = match row.state.as_str() {
            "succeeded" => Some(Outcome::Succeeded),
            "failed" => Some(Outcome::Failed),
            "rolled_back" => Some(Outcome::RolledBack),
            _ if busy == Some(row.id.as_str()) => Some(Outcome::InProgress),
            _ => None,
        };
        // The update just after it (in time) started from what it installed.
        let followed = i
            .checked_sub(1)
            .and_then(|newer| rows.get(newer))
            .is_some_and(|newer| newer.from_version.as_deref() == Some(row.to_version.as_str()));
        let runs_now = i == 0 && row.to_version == running;
        let (result, inferred) = match recorded {
            Some(result) => (result, false),
            None if followed || runs_now => (Outcome::Succeeded, true),
            None => (Outcome::Unknown, false),
        };
        list.push(Entry {
            id: row.id.clone(),
            from: row.from_version.clone(),
            to: row.to_version.clone(),
            requested_by: row.requested_by.clone().filter(|name| !name.is_empty()),
            started_at: row.started_at,
            finished_at: row.finished_at,
            result,
            error: row.error.clone(),
            inferred,
        });
    }
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, from: &str, to: &str, state: &str) -> Row {
        Row {
            id: id.into(),
            from_version: Some(from.into()),
            to_version: to.into(),
            requested_by: Some("admin".into()),
            state: state.into(),
            error: None,
            started_at: Utc::now(),
            finished_at: None,
        }
    }

    fn results(rows: Vec<Row>, running: &str, busy: Option<&str>) -> Vec<(Outcome, bool)> {
        entries(rows, running, busy, 10)
            .into_iter()
            .map(|e| (e.result, e.inferred))
            .collect()
    }

    #[test]
    fn recorded_results_stand_and_others_are_inferred() {
        let rows = vec![
            row("e", "0.9.0", "0.9.1", "succeeded"),
            row("d", "0.8.0", "0.9.0", "unknown"),
            row("c", "0.8.0", "0.8.1", "rolled_back"),
            row("b", "0.7.0", "0.8.0", "unknown"),
            row("a", "0.6.0", "0.7.0", "unknown"),
        ];
        use Outcome::*;
        assert_eq!(
            results(rows, "0.9.1", None),
            [
                (Succeeded, false),
                (Succeeded, true),
                (RolledBack, false),
                // 0.8.1 started from the 0.8.0 it installed.
                (Succeeded, true),
                (Succeeded, true),
            ]
        );
    }

    #[test]
    fn the_newest_is_in_progress_or_succeeded_when_the_server_runs_it() {
        use Outcome::*;
        let rows = || {
            vec![
                row("b", "0.9.0", "0.9.1", "requested"),
                row("a", "0.8.0", "0.9.0", "unknown"),
            ]
        };
        assert_eq!(
            results(rows(), "0.9.0", Some("b")),
            [(InProgress, false), (Succeeded, true)]
        );
        assert_eq!(
            results(rows(), "0.9.1", None),
            [(Succeeded, true), (Succeeded, true)]
        );
        // Never taken, and the server still runs the older one.
        assert_eq!(
            results(rows(), "0.9.0", None),
            [(Unknown, false), (Succeeded, true)]
        );
    }

    #[test]
    fn states_are_stored_in_the_updaters_words() {
        assert_eq!(state_text(UpdateState::RolledBack), "rolled_back");
        assert_eq!(state_text(UpdateState::CachingAgents), "caching_agents");
    }
}
