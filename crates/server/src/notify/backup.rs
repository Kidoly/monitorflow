//! Failed backups (ADR-0060; backups are ADR-0058): the updater reports them in
//! `status.json`, which no trigger can see, so the dispatcher copies a
//! failure into the outbox. One outage notifies once, not at each retry.

use std::path::Path;

use monitorflow_common::server_update::backup::{BackupState, Backups};
use monitorflow_common::server_update::{read_small, Status, MAX_STATE_FILE, STATUS_FILE};
use serde_json::{json, Value};
use sqlx::PgPool;

/// Longest error in a notification.
const MAX_ERROR: usize = 500;

/// The outbox details of `backups` when its last run failed.
pub fn failure(backups: &Backups) -> Option<Value> {
    let run = backups
        .last
        .as_ref()
        .filter(|r| r.state == BackupState::Failed)?;
    let error: Option<String> = run
        .error
        .as_ref()
        .map(|e| e.chars().take(MAX_ERROR).collect());
    Some(json!({
        "occurred_at": run.finished_at.unwrap_or(run.started_at),
        "backup": {
            "id": run.id,
            "kind": run.kind,
            "error": error,
            "last_success_at": backups.last_success_at,
        },
    }))
}

/// Queues the failure of `backups`, if any and not told already; returns
/// whether it did. Server-wide: every customer's data is in a backup.
pub async fn record(pool: &PgPool, backups: &Backups) -> Result<bool, sqlx::Error> {
    let Some(details) = failure(backups) else {
        return Ok(false);
    };
    let done = sqlx::query(
        "INSERT INTO notification_outbox (event, details) VALUES ('backup.failed', $1)
         ON CONFLICT ((coalesce(details #>> '{backup,last_success_at}', '')))
             WHERE event = 'backup.failed' DO NOTHING",
    )
    .bind(details)
    .execute(pool)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Reads the updater's status in `dir` and records a failure. A status
/// that cannot be read says nothing: the backups page shows that.
pub async fn check(pool: &PgPool, dir: &Path) -> Result<bool, sqlx::Error> {
    let path = dir.join(STATUS_FILE);
    let status = tokio::task::spawn_blocking(move || {
        read_small(&path, MAX_STATE_FILE)
            .ok()
            .flatten()
            .and_then(|bytes| serde_json::from_slice::<Status>(&bytes).ok())
    })
    .await
    .ok()
    .flatten();
    match status {
        Some(status) => record(pool, &status.backups).await,
        None => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use monitorflow_common::server_update::backup::{BackupKind, BackupRun};

    use super::*;

    fn run(state: BackupState, error: Option<String>) -> Backups {
        Backups {
            last: Some(BackupRun {
                id: "b1".into(),
                kind: BackupKind::Daily,
                requested_by: String::new(),
                state,
                error,
                warning: None,
                file: None,
                started_at: Utc::now(),
                finished_at: None,
            }),
            ..Backups::default()
        }
    }

    #[test]
    fn only_a_failed_run_is_a_failure() {
        assert_eq!(failure(&Backups::default()), None);
        assert_eq!(failure(&run(BackupState::Running, None)), None);
        assert_eq!(failure(&run(BackupState::Succeeded, None)), None);
        let details = failure(&run(BackupState::Failed, Some("x".repeat(9000)))).unwrap();
        assert_eq!(details["backup"]["kind"], "daily");
        assert_eq!(
            details["backup"]["error"].as_str().unwrap().len(),
            MAX_ERROR
        );
        assert_eq!(details["backup"]["last_success_at"], Value::Null);
    }
}
