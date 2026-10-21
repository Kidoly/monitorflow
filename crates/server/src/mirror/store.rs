//! PostgreSQL access for mirrors, snapshots and runs, and the per-mirror
//! advisory lock that keeps one job per mirror at a time across processes
//! (ADR-0030).

use serde_json::json;
use sqlx::pool::PoolConnection;
use sqlx::{PgExecutor, PgPool, Postgres};
use uuid::Uuid;

use super::model::{Invalid, Mirror, MirrorSpec, Run, RunKind, Snapshot, Trigger};
use crate::api::audit::{self, Event};

const UNIQUE_VIOLATION: &str = "23505";

/// Error of a run interrupted by the death of the process that held it.
pub const INTERRUPTED: &str = "interrupted: the process running it stopped";

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Invalid(#[from] Invalid),
    #[error("a {0} with this name already exists")]
    Duplicate(&'static str),
    #[error("database query failed: {0}")]
    Database(#[from] sqlx::Error),
}

const MIRROR_COLUMNS: &str = "id, name, family, upstream_url, distribution, components,
     architectures, sync_interval_s, auto_snapshot, created_at";
const SNAPSHOT_COLUMNS: &str =
    "id, mirror_id, name, state, size_bytes, error, created_at, published_at";
const RUN_COLUMNS: &str = "id, mirror_id, kind, trigger, requested_by, state, snapshot_id,
     output, error, started_at, finished_at";

fn is_unique_violation(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::Database(e) if e.code().as_deref() == Some(UNIQUE_VIOLATION))
}

/// Validates and inserts `spec`, audited as `mirror.create` by `actor`
/// (`(kind, id)` as in the audit log).
pub async fn create(
    pool: &PgPool,
    spec: &MirrorSpec,
    actor: (&str, &str),
) -> Result<Mirror, StoreError> {
    spec.validate()?;
    let mut tx = pool.begin().await?;
    let mirror: Mirror = sqlx::query_as(&format!(
        "INSERT INTO mirrors (name, family, upstream_url, distribution, components,
                              architectures, sync_interval_s, auto_snapshot)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING {MIRROR_COLUMNS}"
    ))
    .bind(&spec.name)
    .bind(spec.family)
    .bind(&spec.upstream_url)
    .bind(&spec.distribution)
    .bind(&spec.components)
    .bind(&spec.architectures)
    .bind(spec.sync_interval_s)
    .bind(spec.auto_snapshot)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| {
        if is_unique_violation(&e) {
            StoreError::Duplicate("mirror")
        } else {
            e.into()
        }
    })?;
    audit::record(
        &mut *tx,
        Event {
            actor_kind: actor.0,
            actor_id: actor.1,
            action: "mirror.create",
            target: Some(("mirror", &mirror.id.to_string())),
            details: json!({
                "name": mirror.name,
                "family": mirror.family,
                "upstream_url": mirror.upstream_url,
            }),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(mirror)
}

/// Every mirror, by name.
pub async fn list(db: impl PgExecutor<'_>) -> Result<Vec<Mirror>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {MIRROR_COLUMNS} FROM mirrors ORDER BY name"
    ))
    .fetch_all(db)
    .await
}

pub async fn get(db: impl PgExecutor<'_>, id: Uuid) -> Result<Option<Mirror>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {MIRROR_COLUMNS} FROM mirrors WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(db)
    .await
}

pub async fn by_name(db: impl PgExecutor<'_>, name: &str) -> Result<Option<Mirror>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {MIRROR_COLUMNS} FROM mirrors WHERE name = $1"
    ))
    .bind(name)
    .fetch_optional(db)
    .await
}

/// Mirrors with an interval whose last sync run started at least that long
/// ago, or never ran, and that have nothing running.
pub async fn due(db: impl PgExecutor<'_>) -> Result<Vec<Mirror>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {MIRROR_COLUMNS} FROM mirrors m
         WHERE sync_interval_s IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM mirror_runs r
                           WHERE r.mirror_id = m.id AND r.state = 'running')
           AND COALESCE((SELECT max(started_at) FROM mirror_runs r
                         WHERE r.mirror_id = m.id AND r.kind = 'sync'), '-infinity')
               <= now() - make_interval(secs => sync_interval_s)
         ORDER BY name"
    ))
    .fetch_all(db)
    .await
}

/// The mirror's snapshots, newest first.
pub async fn snapshots(
    db: impl PgExecutor<'_>,
    mirror_id: Uuid,
    limit: i64,
) -> Result<Vec<Snapshot>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {SNAPSHOT_COLUMNS} FROM mirror_snapshots WHERE mirror_id = $1
         ORDER BY created_at DESC, name DESC LIMIT $2"
    ))
    .bind(mirror_id)
    .bind(limit)
    .fetch_all(db)
    .await
}

/// The newest published snapshot: what `current` points at.
pub async fn latest_published(
    db: impl PgExecutor<'_>,
    mirror_id: Uuid,
) -> Result<Option<Snapshot>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {SNAPSHOT_COLUMNS} FROM mirror_snapshots
         WHERE mirror_id = $1 AND state = 'published'
         ORDER BY published_at DESC LIMIT 1"
    ))
    .bind(mirror_id)
    .fetch_optional(db)
    .await
}

/// The mirror's runs, newest first.
pub async fn runs(
    db: impl PgExecutor<'_>,
    mirror_id: Uuid,
    limit: i64,
) -> Result<Vec<Run>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {RUN_COLUMNS} FROM mirror_runs WHERE mirror_id = $1
         ORDER BY started_at DESC LIMIT $2"
    ))
    .bind(mirror_id)
    .bind(limit)
    .fetch_all(db)
    .await
}

pub async fn run(db: impl PgExecutor<'_>, id: Uuid) -> Result<Option<Run>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {RUN_COLUMNS} FROM mirror_runs WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(db)
    .await
}

/// Proof that this process runs the only job of one mirror. Held on a
/// dedicated connection that is closed, not returned to the pool, when the
/// lock goes away: closing the session releases the advisory lock even if
/// the job panics.
pub struct MirrorLock {
    conn: PoolConnection<Postgres>,
    mirror_id: Uuid,
}

impl std::fmt::Debug for MirrorLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MirrorLock")
            .field("mirror_id", &self.mirror_id)
            .finish()
    }
}

/// The advisory lock key of a mirror, from its ID.
const LOCK_KEY: &str = "hashtextextended('monitorflow.mirror:' || $1::text, 0)";

impl MirrorLock {
    /// The lock of `mirror_id`, or `None` while another session holds it.
    pub async fn try_acquire(pool: &PgPool, mirror_id: Uuid) -> Result<Option<Self>, sqlx::Error> {
        let mut conn = pool.acquire().await?;
        conn.close_on_drop();
        let locked: bool = sqlx::query_scalar(&format!("SELECT pg_try_advisory_lock({LOCK_KEY})"))
            .bind(mirror_id)
            .fetch_one(&mut *conn)
            .await?;
        Ok(locked.then_some(Self { conn, mirror_id }))
    }

    pub fn mirror_id(&self) -> Uuid {
        self.mirror_id
    }

    /// Marks the mirror's stale `running` runs failed: their process lost
    /// the lock, so it is gone. Then inserts a new running run.
    pub async fn start_run(
        &mut self,
        kind: RunKind,
        trigger: Trigger,
        requested_by: Option<&str>,
    ) -> Result<Run, sqlx::Error> {
        sqlx::query(
            "UPDATE mirror_runs SET state = 'failed', error = $2, finished_at = now()
             WHERE mirror_id = $1 AND state = 'running'",
        )
        .bind(self.mirror_id)
        .bind(INTERRUPTED)
        .execute(&mut *self.conn)
        .await?;
        sqlx::query(
            "UPDATE mirror_snapshots SET state = 'failed', error = $2
             WHERE mirror_id = $1 AND state = 'creating'",
        )
        .bind(self.mirror_id)
        .bind(INTERRUPTED)
        .execute(&mut *self.conn)
        .await?;
        sqlx::query_as(&format!(
            "INSERT INTO mirror_runs (mirror_id, kind, trigger, requested_by)
             VALUES ($1, $2, $3, $4) RETURNING {RUN_COLUMNS}"
        ))
        .bind(self.mirror_id)
        .bind(kind)
        .bind(trigger)
        .bind(requested_by)
        .fetch_one(&mut *self.conn)
        .await
    }

    /// Unlocks and closes the connection.
    pub async fn release(mut self) {
        let unlocked = sqlx::query(&format!("SELECT pg_advisory_unlock({LOCK_KEY})"))
            .bind(self.mirror_id)
            .execute(&mut *self.conn)
            .await;
        if let Err(error) = unlocked {
            // Closing the connection below releases it anyway.
            tracing::warn!(%error, mirror_id = %self.mirror_id, "cannot release the mirror lock");
        }
        let _ = self.conn.close().await;
    }
}

/// Replaces the stored output of a running run, so it shows progress.
pub async fn set_output(
    db: impl PgExecutor<'_>,
    run_id: Uuid,
    output: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE mirror_runs SET output = $2 WHERE id = $1 AND state = 'running'")
        .bind(run_id)
        .bind(output)
        .execute(db)
        .await?;
    Ok(())
}

/// Ends a run: succeeded without `error`, failed with it.
pub async fn finish_run(
    db: impl PgExecutor<'_>,
    run_id: Uuid,
    output: &str,
    error: Option<&str>,
    snapshot_id: Option<Uuid>,
) -> Result<Run, sqlx::Error> {
    sqlx::query_as(&format!(
        "UPDATE mirror_runs
         SET state = CASE WHEN $3::text IS NULL THEN 'succeeded' ELSE 'failed' END,
             output = $2, error = $3, snapshot_id = COALESCE($4, snapshot_id),
             finished_at = now()
         WHERE id = $1 RETURNING {RUN_COLUMNS}"
    ))
    .bind(run_id)
    .bind(output)
    .bind(error)
    .bind(snapshot_id)
    .fetch_one(db)
    .await
}

/// Inserts a snapshot in state `creating`, or takes over a failed one of
/// the same name for a retry. Needs the mirror's lock.
pub async fn insert_snapshot(lock: &mut MirrorLock, name: &str) -> Result<Snapshot, StoreError> {
    super::model::validate_snapshot_name(name)?;
    let row: Option<Snapshot> = sqlx::query_as(&format!(
        "INSERT INTO mirror_snapshots (mirror_id, name) VALUES ($1, $2)
         ON CONFLICT (mirror_id, name) DO UPDATE
             SET state = 'creating', error = NULL, size_bytes = NULL, created_at = now()
             WHERE mirror_snapshots.state = 'failed'
         RETURNING {SNAPSHOT_COLUMNS}"
    ))
    .bind(lock.mirror_id)
    .bind(name)
    .fetch_optional(&mut *lock.conn)
    .await?;
    row.ok_or(StoreError::Duplicate("snapshot"))
}

pub async fn publish_snapshot(
    db: impl PgExecutor<'_>,
    id: Uuid,
    size_bytes: Option<i64>,
) -> Result<Snapshot, sqlx::Error> {
    sqlx::query_as(&format!(
        "UPDATE mirror_snapshots SET state = 'published', size_bytes = $2, published_at = now()
         WHERE id = $1 RETURNING {SNAPSHOT_COLUMNS}"
    ))
    .bind(id)
    .bind(size_bytes)
    .fetch_one(db)
    .await
}

pub async fn fail_snapshot(
    db: impl PgExecutor<'_>,
    id: Uuid,
    error: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE mirror_snapshots SET state = 'failed', error = $2 WHERE id = $1")
        .bind(id)
        .bind(error)
        .execute(db)
        .await?;
    Ok(())
}
