//! Update campaigns (ADR-0033): list, create, read with waves and
//! per-server status, and start, pause, resume or stop.
//!
//! Campaigns span sites, so reading one needs a role on its customer as a
//! whole (global or customer grant); creating and acting need an
//! administrator role on it, as queuing a command does (ADR-0031). The
//! JSON is documented in ADR-0033, with pinned snapshots in ADR-0038, and
//! kept stable for the web interface.

use std::collections::HashMap;
use std::time::Duration;

use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::auth::CurrentUser;
use super::commands::{DEFAULT_LIMIT, DEFAULT_TTL_S, MAX_LIMIT, MAX_TTL_S, MIN_TTL_S};
use super::customers::validate_name;
use super::sensors::{body, parse_id};
use super::servers::{decode_cursor, encode_cursor, page_limit};
use super::{ApiError, ApiState};
use crate::campaign::query::{self, ListFilter, Listed, WaveCounts};
use crate::campaign::{
    Action, CampaignError, CampaignState, CampaignStore, FailureThreshold, NewCampaign, SourcesPin,
    UpdateSpec, WaveSpec, Window,
};
use crate::commands::sources::{self, KeyLookupError};

/// Body of `POST /api/campaigns`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateRequest {
    pub customer_id: Uuid,
    pub name: String,
    pub site_id: Option<Uuid>,
    #[serde(default)]
    pub server_ids: Vec<Uuid>,
    #[serde(default)]
    pub waves: Vec<WaveSpec>,
    #[serde(default)]
    pub packages: Vec<String>,
    #[serde(default)]
    pub security_only: bool,
    #[serde(default)]
    pub allow_reboot: bool,
    /// [`MIN_TTL_S`] to [`MAX_TTL_S`], default [`DEFAULT_TTL_S`].
    pub command_ttl_s: Option<u64>,
    #[serde(default)]
    pub failure_threshold: FailureThreshold,
    #[serde(default)]
    pub windows: Vec<Window>,
    /// Snapshots to pin every server to during its update (ADR-0038).
    pub sources: Option<SourcesPin>,
}

/// Servers by status; `failed` includes expired commands and skipped
/// servers, `in_progress` queued, sent and running commands.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Progress {
    pub total: i64,
    pub pending: i64,
    pub in_progress: i64,
    pub succeeded: i64,
    pub failed: i64,
    pub cancelled: i64,
}

impl Progress {
    fn add(&mut self, c: &WaveCounts) {
        self.total += c.total;
        self.pending += c.pending;
        self.in_progress += c.in_progress;
        self.succeeded += c.succeeded;
        self.failed += c.failed;
        self.cancelled += c.cancelled;
    }
}

/// One campaign, as listed.
#[derive(Debug, Serialize)]
pub struct CampaignView {
    pub id: Uuid,
    pub customer_id: Uuid,
    pub customer_name: String,
    pub name: String,
    pub site_id: Option<Uuid>,
    pub state: CampaignState,
    pub error: Option<String>,
    pub spec: UpdateSpec,
    pub command_ttl_s: i32,
    pub failure_threshold: FailureThreshold,
    pub windows: Vec<Window>,
    /// The pinned snapshots, if any (ADR-0038).
    pub sources: Option<SourcesPin>,
    pub wave_count: usize,
    pub current_wave: Option<i32>,
    pub progress: Progress,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
pub struct CampaignList {
    /// Newest first.
    pub campaigns: Vec<CampaignView>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TargetView {
    pub server_id: Uuid,
    pub peer_id: String,
    pub hostname: String,
    pub site_id: Uuid,
    pub status: &'static str,
    pub command_id: Option<Uuid>,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
    pub finished_at: Option<DateTime<Utc>>,
    /// With pinned snapshots: the `configure_sources` command before the
    /// update and the command restoring the sources after it.
    pub sources_command_id: Option<Uuid>,
    pub restore_command_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
pub struct WaveView {
    pub index: i32,
    /// `pending`, `running` or `done`.
    pub state: &'static str,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub progress: Progress,
    pub servers: Vec<TargetView>,
}

/// `GET /api/campaigns/{id}` and the answer of every write.
#[derive(Debug, Serialize)]
pub struct CampaignDetail {
    #[serde(flatten)]
    pub campaign: CampaignView,
    pub waves: Vec<WaveView>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListParams {
    pub customer_id: Option<Uuid>,
    pub state: Option<CampaignState>,
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

impl From<CampaignError> for ApiError {
    fn from(error: CampaignError) -> Self {
        match error {
            CampaignError::Invalid(message) => Self::Invalid(message),
            CampaignError::NotFound => Self::NotFound,
            e @ CampaignError::Transition { .. } => Self::Conflict(e.to_string()),
            CampaignError::Database(e) => Self::Database(e),
        }
    }
}

/// 404 unless the user reads `customer` as a whole; then, with `admin`,
/// 403 unless it administers it.
fn authorize_customer(user: &CurrentUser, customer: Uuid, admin: bool) -> Result<(), ApiError> {
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    if !user.visible_customers().contains(customer) {
        return Err(ApiError::NotFound);
    }
    if admin && !user.administered_customers().contains(customer) {
        return Err(ApiError::Forbidden);
    }
    Ok(())
}

/// Builds the views of `rows` with their windows and counts.
async fn views(state: &ApiState, rows: Vec<Listed>) -> Result<Vec<CampaignView>, ApiError> {
    let ids: Vec<Uuid> = rows.iter().map(|r| r.campaign.id).collect();
    let mut windows: HashMap<Uuid, Vec<Window>> = HashMap::new();
    for (id, window) in query::windows(&state.pool, &ids).await? {
        windows.entry(id).or_default().push(window);
    }
    let mut pins: HashMap<Uuid, Vec<_>> = HashMap::new();
    for (id, mirror) in query::pins(&state.pool, &ids).await? {
        pins.entry(id).or_default().push(mirror);
    }
    let mut counts: HashMap<Uuid, (Progress, usize)> = HashMap::new();
    for wave in query::counts(&state.pool, &ids).await? {
        let entry = counts.entry(wave.campaign_id).or_default();
        entry.0.add(&wave);
        entry.1 += 1;
    }
    Ok(rows
        .into_iter()
        .map(
            |Listed {
                 campaign: c,
                 customer_name,
             }| {
                let (progress, wave_count) = counts.remove(&c.id).unwrap_or_default();
                CampaignView {
                    spec: c.spec(),
                    failure_threshold: c.threshold(),
                    windows: windows.remove(&c.id).unwrap_or_default(),
                    sources: c.sources_mode.map(|mode| SourcesPin {
                        mode,
                        mirrors: pins.remove(&c.id).unwrap_or_default(),
                    }),
                    wave_count,
                    progress,
                    id: c.id,
                    customer_id: c.customer_id,
                    customer_name,
                    name: c.name,
                    site_id: c.site_id,
                    state: c.state,
                    error: c.error,
                    command_ttl_s: c.command_ttl_s,
                    current_wave: c.current_wave,
                    created_by: c.created_by,
                    created_at: c.created_at,
                    started_at: c.started_at,
                    finished_at: c.finished_at,
                }
            },
        )
        .collect())
}

/// The detail of the campaign `id`, which the caller authorized.
async fn detail_of(state: &ApiState, id: Uuid) -> Result<CampaignDetail, ApiError> {
    let row = query::get(&state.pool, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let campaign = views(state, vec![row])
        .await?
        .pop()
        .ok_or(ApiError::NotFound)?;
    let counts = query::counts(&state.pool, &[id]).await?;
    let mut targets = query::targets(&state.pool, id)
        .await?
        .into_iter()
        .peekable();
    let mut waves = Vec::new();
    for wave in query::waves(&state.pool, id).await? {
        let mut progress = Progress::default();
        if let Some(c) = counts.iter().find(|c| c.wave == wave.position) {
            progress.add(c);
        }
        let mut servers = Vec::new();
        while let Some(t) = targets.next_if(|t| t.wave == wave.position) {
            servers.push(TargetView {
                status: t.status(),
                server_id: t.server_id,
                peer_id: t.peer_id,
                hostname: t.hostname,
                site_id: t.site_id,
                command_id: t.command_id,
                exit_code: t.exit_code,
                error: t.error,
                finished_at: t.finished_at,
                sources_command_id: t.sources_command_id,
                restore_command_id: t.restore_command_id,
            });
        }
        let state = match (wave.started_at, wave.finished_at) {
            (_, Some(_)) => "done",
            (Some(_), None) => "running",
            (None, None) => "pending",
        };
        waves.push(WaveView {
            index: wave.position,
            state,
            started_at: wave.started_at,
            finished_at: wave.finished_at,
            progress,
            servers,
        });
    }
    Ok(CampaignDetail { campaign, waves })
}

/// `GET /api/campaigns`.
pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
    params: Result<Query<ListParams>, QueryRejection>,
) -> Result<Json<CampaignList>, ApiError> {
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    let Query(params) = params.map_err(|e| ApiError::Invalid(e.body_text()))?;
    let limit = page_limit(params.limit, DEFAULT_LIMIT, MAX_LIMIT)?;
    let filter = ListFilter {
        customers: user.visible_customers().bind(),
        customer_id: params.customer_id,
        state: params.state,
        before: params.cursor.as_deref().map(decode_cursor).transpose()?,
        limit: i64::from(limit) + 1,
    };
    let mut rows = query::list(&state.pool, &filter).await?;
    let next_cursor = if rows.len() > limit as usize {
        rows.truncate(limit as usize);
        rows.last()
            .map(|r| encode_cursor(&(r.campaign.created_at, r.campaign.id)))
    } else {
        None
    };
    let campaigns = views(&state, rows).await?;
    Ok(Json(CampaignList {
        campaigns,
        next_cursor,
    }))
}

/// `POST /api/campaigns`.
pub async fn create(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<CreateRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<CampaignDetail>), ApiError> {
    let request = body(request)?;
    authorize_customer(&user, request.customer_id, true)?;
    validate_name(&request.name).map_err(|e| ApiError::Invalid(e.to_string()))?;
    let ttl = request.command_ttl_s.unwrap_or(DEFAULT_TTL_S);
    if !(MIN_TTL_S..=MAX_TTL_S).contains(&ttl) {
        return Err(ApiError::Invalid(format!(
            "command_ttl_s must be between {MIN_TTL_S} and {MAX_TTL_S}"
        )));
    }
    if request.site_id.is_none() && request.server_ids.is_empty() {
        return Err(ApiError::Invalid("give a site_id or server_ids".into()));
    }
    if request.sources.is_some() {
        check_sources(&state).await?;
    }
    let new = NewCampaign {
        customer_id: request.customer_id,
        name: request.name,
        site_id: request.site_id,
        server_ids: request.server_ids,
        waves: request.waves,
        spec: UpdateSpec {
            packages: request.packages,
            security_only: request.security_only,
            allow_reboot: request.allow_reboot,
        },
        command_ttl: Duration::from_secs(ttl),
        failure_threshold: request.failure_threshold,
        windows: request.windows,
        sources: request.sources,
        created_by: Some(user.id),
    };
    let actor = user.id.to_string();
    let row = CampaignStore::new(state.pool.clone())
        .create(&new, ("user", &actor))
        .await?;
    tracing::info!(user = %user.id, campaign = %row.id, "campaign created");
    Ok((StatusCode::CREATED, Json(detail_of(&state, row.id).await?)))
}

/// 409 unless the server can build `configure_sources` commands, as
/// `POST /api/servers/{id}/sources` (ADR-0035).
async fn check_sources(state: &ApiState) -> Result<(), ApiError> {
    if state.settings.mirror_public_url.is_none() {
        return Err(ApiError::Conflict(
            "mirror_public_url is not configured".into(),
        ));
    }
    let mirrors = state
        .mirrors
        .as_ref()
        .ok_or_else(|| ApiError::Conflict("mirrors are not enabled on this server".into()))?;
    sources::archive_key(mirrors.runner(), mirrors.layout())
        .await
        .map_err(|error| match error {
            KeyLookupError::Key(key) => ApiError::Conflict(key.to_string()),
            other => ApiError::Internal(other.to_string()),
        })?;
    Ok(())
}

/// `GET /api/campaigns/{id}`.
pub async fn detail(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<CampaignDetail>, ApiError> {
    let id = parse_id(&id)?;
    let customer = customer_of(&state, &user, id).await?;
    authorize_customer(&user, customer, false)?;
    Ok(Json(detail_of(&state, id).await?))
}

/// The customer of the campaign `id`; 403 for users without any role and
/// 404 for unknown IDs, before revealing anything.
async fn customer_of(state: &ApiState, user: &CurrentUser, id: Uuid) -> Result<Uuid, ApiError> {
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    sqlx::query_scalar("SELECT customer_id FROM campaigns WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or(ApiError::NotFound)
}

async fn act(
    state: ApiState,
    user: CurrentUser,
    id: &str,
    action: Action,
) -> Result<Json<CampaignDetail>, ApiError> {
    let id = parse_id(id)?;
    let customer = customer_of(&state, &user, id).await?;
    authorize_customer(&user, customer, true)?;
    let actor = user.id.to_string();
    let row = CampaignStore::new(state.pool.clone())
        .act(id, action, ("user", &actor))
        .await?;
    if row.state.is_active() {
        if let Some(runner) = &state.campaigns {
            runner.wake();
        }
    }
    tracing::info!(user = %user.id, campaign = %id, action = action.as_str(), state = %row.state, "campaign action");
    Ok(Json(detail_of(&state, id).await?))
}

/// `POST /api/campaigns/{id}/start`.
pub async fn start(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<CampaignDetail>, ApiError> {
    act(state, user, &id, Action::Start).await
}

/// `POST /api/campaigns/{id}/pause`.
pub async fn pause(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<CampaignDetail>, ApiError> {
    act(state, user, &id, Action::Pause).await
}

/// `POST /api/campaigns/{id}/resume`.
pub async fn resume(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<CampaignDetail>, ApiError> {
    act(state, user, &id, Action::Resume).await
}

/// `POST /api/campaigns/{id}/stop`.
pub async fn stop(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<CampaignDetail>, ApiError> {
    act(state, user, &id, Action::Stop).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn requests_parse_with_defaults() {
        let request: CreateRequest = serde_json::from_value(json!({
            "customer_id": Uuid::nil(), "name": "october", "site_id": Uuid::nil(),
            "waves": [{"size": 1}, {"percent": 50}],
            "windows": [{"kind": "weekly", "weekdays": [6], "start": "22:00", "end": "04:00",
                         "timezone": "Europe/Paris"}]
        }))
        .unwrap();
        assert_eq!(request.waves, [WaveSpec::Size(1), WaveSpec::Percent(50)]);
        assert_eq!(request.failure_threshold, FailureThreshold::default());
        assert!(request.server_ids.is_empty() && !request.allow_reboot);
        assert_eq!(request.sources, None);
        let pinned: CreateRequest = serde_json::from_value(json!({
            "customer_id": Uuid::nil(), "name": "x",
            "sources": {"mode": "replace", "mirrors": [{"name": "debian-12", "snapshot": "s1"}]}
        }))
        .unwrap();
        assert_eq!(pinned.sources.unwrap().mirrors[0].snapshot, "s1");
        assert!(serde_json::from_value::<CreateRequest>(json!({
            "customer_id": Uuid::nil(), "name": "x", "extra": 1
        }))
        .is_err());
        assert!(serde_json::from_value::<CreateRequest>(json!({
            "customer_id": Uuid::nil(), "name": "x", "waves": [{"size": 1, "percent": 2}]
        }))
        .is_err());
    }

    #[test]
    fn errors_map_to_statuses() {
        let status = |e: CampaignError| ApiError::from(e).status();
        assert_eq!(
            status(CampaignError::Invalid("x".into())),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(status(CampaignError::NotFound), StatusCode::NOT_FOUND);
        let conflict = CampaignError::Transition {
            action: "pause",
            state: CampaignState::Draft,
        };
        assert_eq!(
            ApiError::from(conflict).to_string(),
            "cannot pause a campaign that is draft"
        );
    }

    #[test]
    fn progress_adds_waves() {
        let mut p = Progress::default();
        let wave = WaveCounts {
            total: 3,
            pending: 1,
            in_progress: 1,
            succeeded: 1,
            ..WaveCounts::default()
        };
        p.add(&wave);
        p.add(&wave);
        assert_eq!((p.total, p.pending, p.succeeded, p.failed), (6, 2, 2, 0));
    }
}
