//! Package mirrors (ADR-0030): listing, detail with snapshots and runs,
//! and starting syncs and snapshots. Mirrors are global, so every route
//! needs a global administrator role.

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::access::Scope;
use super::auth::CurrentUser;
use super::sensors::{body, parse_id};
use super::{ApiError, ApiState};
use crate::mirror::jobs::{self, JobError, JobKind};
use crate::mirror::{store, Mirror, Run, RunKind, RunState, Snapshot, Trigger};

/// Snapshots and runs shown in a mirror's detail.
const DETAIL_SNAPSHOTS: i64 = 100;
const DETAIL_RUNS: i64 = 10;

/// A run without its output, as listings show it.
#[derive(Debug, Serialize)]
pub struct RunSummary {
    pub id: Uuid,
    pub kind: RunKind,
    pub trigger: Trigger,
    pub requested_by: Option<String>,
    pub state: RunState,
    pub snapshot_id: Option<Uuid>,
    pub error: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

impl From<Run> for RunSummary {
    fn from(run: Run) -> Self {
        Self {
            id: run.id,
            kind: run.kind,
            trigger: run.trigger,
            requested_by: run.requested_by,
            state: run.state,
            snapshot_id: run.snapshot_id,
            error: run.error,
            started_at: run.started_at,
            finished_at: run.finished_at,
        }
    }
}

/// One mirror with the name of its `current` snapshot and its last run.
#[derive(Debug, Serialize)]
pub struct MirrorView {
    #[serde(flatten)]
    pub mirror: Mirror,
    pub current_snapshot: Option<String>,
    pub last_run: Option<RunSummary>,
}

/// Body of `GET /api/mirrors`, by name.
#[derive(Debug, Serialize)]
pub struct MirrorList {
    pub mirrors: Vec<MirrorView>,
}

/// Body of `GET /api/mirrors/{id}`: the newest snapshots and runs first,
/// runs with their output.
#[derive(Debug, Serialize)]
pub struct MirrorDetail {
    #[serde(flatten)]
    pub mirror: MirrorView,
    pub snapshots: Vec<Snapshot>,
    pub runs: Vec<Run>,
}

/// Body of `POST /api/mirrors/{id}/snapshots`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotRequest {
    pub name: Option<String>,
}

pub(super) fn require_global_admin(user: &CurrentUser) -> Result<(), ApiError> {
    if user.administered_customers() != Scope::All {
        return Err(ApiError::Forbidden);
    }
    Ok(())
}

async fn view(state: &ApiState, mirror: Mirror) -> Result<MirrorView, ApiError> {
    let current = store::latest_published(&state.pool, mirror.id).await?;
    let last_run = store::runs(&state.pool, mirror.id, 1).await?.pop();
    Ok(MirrorView {
        mirror,
        current_snapshot: current.map(|s| s.name),
        last_run: last_run.map(Into::into),
    })
}

async fn find(state: &ApiState, id: &str) -> Result<Mirror, ApiError> {
    store::get(&state.pool, parse_id(id)?)
        .await?
        .ok_or(ApiError::NotFound)
}

/// `GET /api/mirrors`.
pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<MirrorList>, ApiError> {
    require_global_admin(&user)?;
    let mut mirrors = Vec::new();
    for mirror in store::list(&state.pool).await? {
        mirrors.push(view(&state, mirror).await?);
    }
    Ok(Json(MirrorList { mirrors }))
}

/// `GET /api/mirrors/{id}`.
pub async fn detail(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<MirrorDetail>, ApiError> {
    require_global_admin(&user)?;
    let mirror = find(&state, &id).await?;
    let snapshots = store::snapshots(&state.pool, mirror.id, DETAIL_SNAPSHOTS).await?;
    let runs = store::runs(&state.pool, mirror.id, DETAIL_RUNS).await?;
    Ok(Json(MirrorDetail {
        mirror: view(&state, mirror).await?,
        snapshots,
        runs,
    }))
}

/// `POST /api/mirrors/{id}/sync`: 202 with the started run.
pub async fn sync(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<RunSummary>), ApiError> {
    start(&state, &user, &id, JobKind::Sync).await
}

/// `POST /api/mirrors/{id}/snapshots`: 202 with the started run.
pub async fn snapshot(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<SnapshotRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<RunSummary>), ApiError> {
    require_global_admin(&user)?;
    let name = body(request)?.name;
    start(&state, &user, &id, JobKind::Snapshot(name)).await
}

/// Starts the job in the background; it outlives the request.
async fn start(
    state: &ApiState,
    user: &CurrentUser,
    id: &str,
    kind: JobKind,
) -> Result<(StatusCode, Json<RunSummary>), ApiError> {
    require_global_admin(user)?;
    let mirror = find(state, id).await?;
    let mirrors = state
        .mirrors
        .as_ref()
        .ok_or_else(|| ApiError::Internal("mirrors are not configured".into()))?;
    let actor = user.id.to_string();
    let requested_by = format!("user:{actor}");
    let job = mirrors
        .begin(&mirror, kind, Trigger::Manual, Some(&requested_by))
        .await
        .map_err(|e| match e {
            JobError::Busy | JobError::NotSynced => ApiError::Conflict(e.to_string()),
            JobError::Invalid(invalid) => ApiError::Invalid(invalid.to_string()),
            JobError::Database(e) => ApiError::Database(e),
        })?;
    let run = job.run().clone();
    if let Err(error) = jobs::audit_request(&state.pool, ("user", &actor), &mirror, &run).await {
        // The job holds the lock and a running row: run it anyway, the
        // row records who asked.
        tracing::error!(%error, run = %run.id, "cannot audit a mirror job");
    }
    tracing::info!(user = %user.id, mirror = %mirror.name, run = %run.id, "mirror job requested");
    tokio::spawn(job.execute());
    Ok((StatusCode::ACCEPTED, Json(run.into())))
}
