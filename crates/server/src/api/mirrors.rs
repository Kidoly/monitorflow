//! Package mirrors (ADR-0030): listing, detail with snapshots and runs,
//! starting syncs and snapshots, adding a distribution of the catalog
//! (ADR-0067) and deleting a mirror (ADR-0075). Mirrors are global, so every route needs a global
//! administrator role.

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
use crate::mirror::cache;
use crate::mirror::catalog::{self, Disk, CATALOG};
use crate::mirror::jobs::{self, JobError, JobKind};
use crate::mirror::removal::{self, RemovalError, Users};
use crate::mirror::retention::{self, Kept, KEEP_SNAPSHOTS};
use crate::mirror::{
    signing, store, tools, Family, Mirror, Mirrors, Mode, Run, RunKind, RunState, Snapshot,
    StoreError, Trigger,
};

/// Snapshots and runs shown in a mirror's detail.
const DETAIL_SNAPSHOTS: i64 = 100;
const DETAIL_RUNS: i64 = 10;

/// A run without its output, as listings show it.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = MirrorRunSummary)]
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
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct MirrorView {
    #[serde(flatten)]
    pub mirror: Mirror,
    /// The newest published snapshot, which `current` serves; `null`
    /// before the first.
    pub current_snapshot: Option<String>,
    /// `null` before the first run.
    pub last_run: Option<RunSummary>,
}

/// Body of `GET /api/mirrors`, by name.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct MirrorList {
    pub mirrors: Vec<MirrorView>,
    /// Published snapshots each mirror keeps, on top of those in use.
    #[schema(value_type = u64)]
    pub keep_snapshots: usize,
}

/// Body of `GET /api/mirrors/{id}`: the newest snapshots and runs first,
/// runs with their output.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct MirrorDetail {
    #[serde(flatten)]
    pub mirror: MirrorView,
    /// Its 100 newest snapshots, pruned ones included.
    pub snapshots: Vec<SnapshotView>,
    /// Published snapshots each mirror keeps, on top of those in use.
    #[schema(value_type = u64)]
    pub keep_snapshots: usize,
    /// Its 10 latest runs, with their output.
    pub runs: Vec<Run>,
    /// What keeps it from being deleted.
    pub used_by: Users,
}

/// A snapshot and why it stays; `null` when the next pass of retention
/// removes it, or while it is created.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct SnapshotView {
    #[serde(flatten)]
    pub snapshot: Snapshot,
    /// Why it stays.
    pub kept: Option<Kept>,
}

/// Body of `POST /api/mirrors/{id}/snapshots`.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SnapshotRequest {
    /// 1 to 64 of A-Z, a-z, 0-9, `.`, `_` and `-`, not starting with a dot
    /// and not `current`; the UTC time such as `20261023T090000Z` when
    /// absent or `null`.
    pub name: Option<String>,
}

/// Whether mirrors can run on this install, and why not.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = MirrorTools)]
pub struct ToolsStatus {
    pub available: bool,
    /// Why not; `null` when available.
    pub problem: Option<String>,
}

/// One distribution of `GET /api/mirrors/catalog`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = CatalogDistribution)]
pub struct CatalogEntry {
    /// What `POST /api/v1/mirrors` takes, such as `debian-12`.
    pub id: &'static str,
    /// Its name on screen, such as `Debian 12`.
    pub name: &'static str,
    pub family: Family,
    /// About what its mirrors take on disk in full mode, in bytes.
    pub size_bytes: u64,
    /// About what they take on demand (ADR-0142): indexes and the packages
    /// a fleet typically installs, in bytes.
    pub on_demand_size_bytes: u64,
    /// What on demand needs at least: the indexes, in bytes.
    pub index_bytes: u64,
    /// Every mirror of the distribution exists.
    pub added: bool,
    /// Why it cannot be added in full mode now: too little free space.
    pub problem: Option<String>,
    /// Why it cannot be added on demand now: too little free space even
    /// for its indexes.
    pub on_demand_problem: Option<String>,
}

/// Body of `GET /api/mirrors/catalog`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = MirrorCatalog)]
pub struct Catalog {
    pub tools: ToolsStatus,
    /// The disk of `mirror_dir`; `null` when unknown.
    pub disk: Option<Disk>,
    pub distributions: Vec<CatalogEntry>,
}

/// Body of `GET /api/mirrors/cache` (ADR-0142).
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = MirrorCache)]
pub struct Cache {
    /// The cap in force, in bytes.
    pub cap_bytes: u64,
    /// The default: 100 GiB, or half the disk when smaller.
    pub default_cap_bytes: u64,
    /// Whether an administrator set the cap.
    pub cap_set: bool,
    /// What on-demand mirrors hold: indexes and packages, in bytes.
    pub used_bytes: u64,
    pub index_bytes: u64,
    pub package_bytes: u64,
    pub packages: u64,
    /// The last eviction could not get under the cap: campaigns and the
    /// current snapshots hold more. `false` before the first one.
    pub over_cap: bool,
}

/// Body of `PUT /api/mirrors/cache`.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[schema(as = SetMirrorCacheRequest)]
#[serde(deny_unknown_fields)]
pub struct SetCacheRequest {
    /// At least 1 GiB, in bytes; `null` for the default.
    pub cap_bytes: Option<u64>,
}

/// Body of `POST /api/mirrors`.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[schema(as = AddMirrorRequest)]
#[serde(deny_unknown_fields)]
pub struct AddRequest {
    /// An `id` of the catalog.
    pub distribution: String,
    /// `on_demand` (the default) or `full` (ADR-0142).
    #[serde(default)]
    pub mode: Option<Mode>,
}

/// Body of `PATCH /api/mirrors/{id}`.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[schema(as = UpdateMirrorRequest)]
#[serde(deny_unknown_fields)]
pub struct UpdateRequest {
    /// What the next sync downloads (ADR-0142).
    pub mode: Mode,
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
    Ok(Json(MirrorList {
        mirrors,
        keep_snapshots: KEEP_SNAPSHOTS,
    }))
}

async fn tools_status(mirrors: Option<&Mirrors>) -> ToolsStatus {
    let result = match mirrors {
        None => Err("mirrors are not enabled on this server".to_owned()),
        Some(mirrors) => match mirrors.runner().run(&tools::probe()).await {
            Ok(output) if output.success => Ok(()),
            Ok(output) => Err(format!("aptly failed: {}", output.status)),
            Err(error) => Err(error.to_string()),
        },
    };
    if let Err(error) = &result {
        tracing::warn!(%error, "the mirror tools are not available");
    }
    ToolsStatus {
        available: result.is_ok(),
        problem: result.err(),
    }
}

async fn disk(mirrors: Option<&Mirrors>) -> Option<Disk> {
    let root = mirrors?.layout().root().to_owned();
    tokio::task::spawn_blocking(move || catalog::disk(&root))
        .await
        .ok()?
}

async fn cache_view(state: &ApiState) -> Result<Cache, ApiError> {
    let mirrors = state
        .mirrors
        .as_ref()
        .ok_or_else(|| ApiError::Conflict("mirrors are not enabled on this server".into()))?;
    let cap = cache::cap(&state.pool, mirrors.layout()).await?;
    let store = mirrors.layout().store();
    let usage = tokio::task::spawn_blocking(move || store.usage())
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Cache {
        cap_bytes: cap.bytes,
        default_cap_bytes: cap.default_bytes,
        cap_set: cap.set,
        used_bytes: usage.total(),
        index_bytes: usage.index_bytes,
        package_bytes: usage.package_bytes,
        packages: usage.packages,
        over_cap: mirrors.last_eviction().is_some_and(|o| o.over_cap),
    })
}

/// `GET /api/mirrors/cache`.
pub async fn cache(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<Cache>, ApiError> {
    require_global_admin(&user)?;
    Ok(Json(cache_view(&state).await?))
}

/// `PUT /api/mirrors/cache`: the cap, then an eviction at once when it
/// went down.
pub async fn set_cache(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<SetCacheRequest>, JsonRejection>,
) -> Result<Json<Cache>, ApiError> {
    require_global_admin(&user)?;
    let bytes = body(request)?.cap_bytes;
    if bytes.is_some_and(|b| b < cache::MIN_CAP) {
        return Err(ApiError::Invalid("cap_bytes must be at least 1 GiB".into()));
    }
    let mirrors = state
        .mirrors
        .clone()
        .ok_or_else(|| ApiError::Conflict("mirrors are not enabled on this server".into()))?;
    cache::set_cap(&state.pool, bytes, ("user", &user.id.to_string())).await?;
    if let Err(error) = cache::evict_now(&mirrors).await {
        tracing::warn!(%error, "cannot evict down to the new cap");
    }
    Ok(Json(cache_view(&state).await?))
}

/// `GET /api/mirrors/catalog`.
pub async fn catalog(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<Catalog>, ApiError> {
    require_global_admin(&user)?;
    let existing = store::list(&state.pool).await?;
    let mirrors = state.mirrors.as_ref();
    let (tools, disk) = tokio::join!(tools_status(mirrors), disk(mirrors));
    let distributions = CATALOG
        .iter()
        .map(|distribution| CatalogEntry {
            id: distribution.id,
            name: distribution.name,
            family: distribution.family,
            size_bytes: distribution.size_bytes,
            on_demand_size_bytes: distribution.on_demand_bytes,
            index_bytes: distribution.index_bytes,
            added: distribution
                .specs()
                .iter()
                .all(|spec| existing.iter().any(|m| m.name == spec.name)),
            problem: disk.and_then(|d| distribution.too_big(d.free_bytes)),
            on_demand_problem: disk
                .and_then(|d| distribution.too_big_in(Mode::OnDemand, d.free_bytes)),
        })
        .collect();
    Ok(Json(Catalog {
        tools,
        disk,
        distributions,
    }))
}

/// `POST /api/mirrors`: 201 with the mirrors created, which the scheduler
/// starts syncing within a minute. Creates the signing key on first use.
pub async fn add(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<AddRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<MirrorList>), ApiError> {
    require_global_admin(&user)?;
    let request = body(request)?;
    let (id, mode) = (request.distribution, request.mode.unwrap_or(Mode::OnDemand));
    let distribution = catalog::find(&id)
        .ok_or_else(|| ApiError::Invalid(format!("unknown distribution `{id}`")))?;
    let mirrors = state.mirrors.as_ref();
    let status = tools_status(mirrors).await;
    if let Some(problem) = status.problem {
        return Err(ApiError::Conflict(problem));
    }
    let mirrors = mirrors.expect("the tools answered");
    let existing = store::list(&state.pool).await?;
    let missing: Vec<_> = distribution
        .specs_in(mode)
        .into_iter()
        .filter(|spec| !existing.iter().any(|m| m.name == spec.name))
        .collect();
    if missing.is_empty() {
        return Err(ApiError::Conflict(format!(
            "{} is already added",
            distribution.name
        )));
    }
    let room = disk(Some(mirrors)).await;
    if let Some(problem) = room.and_then(|d| distribution.too_big_in(mode, d.free_bytes)) {
        return Err(ApiError::Conflict(problem));
    }
    let layout = mirrors.layout().clone();
    tokio::task::spawn_blocking(move || layout.prepare())
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    signing::ensure(mirrors.runner(), mirrors.layout())
        .await
        .map_err(|e| ApiError::Conflict(e.to_string()))?;
    let actor = user.id.to_string();
    let mut created = Vec::with_capacity(missing.len());
    for spec in &missing {
        match store::create(&state.pool, spec, ("user", &actor)).await {
            Ok(mirror) => created.push(view(&state, mirror).await?),
            // A second click, or the same request from elsewhere.
            Err(StoreError::Duplicate(_)) => {}
            Err(StoreError::Invalid(invalid)) => {
                return Err(ApiError::Invalid(invalid.to_string()))
            }
            Err(StoreError::Database(e)) => return Err(ApiError::Database(e)),
        }
    }
    tracing::info!(user = %user.id, %id, mirrors = created.len(), "distribution added to the mirrors");
    Ok((
        StatusCode::CREATED,
        Json(MirrorList {
            mirrors: created,
            keep_snapshots: KEEP_SNAPSHOTS,
        }),
    ))
}

/// `PATCH /api/mirrors/{id}`: the mode of its next sync (ADR-0142),
/// audited as `mirror.mode`. Refused while it is being removed.
pub async fn update(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<UpdateRequest>, JsonRejection>,
) -> Result<Json<MirrorView>, ApiError> {
    require_global_admin(&user)?;
    let mode = body(request)?.mode;
    let mirror = find(&state, &id).await?;
    if mirror.deleting_at.is_some() {
        return Err(ApiError::Conflict("this mirror is being removed".into()));
    }
    let mirror =
        store::set_mode(&state.pool, &mirror, mode, ("user", &user.id.to_string())).await?;
    Ok(Json(view(&state, mirror).await?))
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
    let layout = state.mirrors.as_ref().map(Mirrors::layout);
    let kept = retention::kept(&state.pool, layout, &mirror)
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(%error, mirror = %mirror.name, "cannot tell why snapshots are kept");
            Default::default()
        });
    let snapshots = snapshots
        .into_iter()
        .map(|snapshot| SnapshotView {
            kept: kept.get(&snapshot.id).copied(),
            snapshot,
        })
        .collect();
    let runs = store::runs(&state.pool, mirror.id, DETAIL_RUNS).await?;
    let mut conn = state.pool.acquire().await?;
    // Deleting checks again and refuses in doubt.
    let used_by = removal::users(&mut conn, &mirror)
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(%error, mirror = %mirror.name, "cannot tell what uses a mirror");
            Users::default()
        });
    drop(conn);
    Ok(Json(MirrorDetail {
        mirror: view(&state, mirror).await?,
        snapshots,
        keep_snapshots: KEEP_SNAPSHOTS,
        runs,
        used_by,
    }))
}

/// `DELETE /api/mirrors/{id}`: 202 with the mirror once it is being
/// deleted, which ends when its detail answers 404; 409 saying what still
/// uses it. Asking again tries again at once after a failure.
pub async fn delete(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<MirrorView>), ApiError> {
    require_global_admin(&user)?;
    let id = parse_id(&id)?;
    let mirrors = state
        .mirrors
        .as_ref()
        .ok_or_else(|| ApiError::Internal("mirrors are not configured".into()))?;
    let actor = user.id.to_string();
    let mirror = removal::request(&state.pool, id, ("user", &actor))
        .await
        .map_err(|e| match e {
            RemovalError::NotFound => ApiError::NotFound,
            RemovalError::InUse(_) => ApiError::Conflict(e.to_string()),
            RemovalError::Check(error) => ApiError::Conflict(error.to_string()),
            RemovalError::Database(e) => ApiError::Database(e),
        })?;
    removal::start(mirrors.clone());
    Ok((StatusCode::ACCEPTED, Json(view(&state, mirror).await?)))
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
            JobError::Busy | JobError::NotSynced | JobError::Going => {
                ApiError::Conflict(e.to_string())
            }
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
