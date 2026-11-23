//! Update campaigns (ADR-0033): list, create, read with waves and
//! per-server status, start, pause, resume or stop, edit or delete
//! (ADR-0166), and run again once over, keeping past runs (ADR-0173).
//!
//! Campaigns span sites, so reading one needs a role on its customer as a
//! whole (global or customer grant); creating and acting need an
//! administrator role on it, as queuing a command does (ADR-0031). The
//! JSON is documented in ADR-0033, with pinned snapshots in ADR-0038, and
//! kept stable for the web interface.

use monitorflow_common::release::Version;
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
use crate::campaign::query::{self, ListFilter, Listed, TargetRow, WaveCounts, WaveRow};
use crate::campaign::{
    Action, CampaignError, CampaignState, CampaignStore, FailureThreshold, MajorUpgradeSpec,
    NewCampaign, RunSettings, SourcesPin, ThirdPartySpec, UpdateSpec, WaveSpec, Window,
};
use crate::commands::sources::{self, KeyLookupError};

/// Body of `POST /api/campaigns` and `PUT /api/campaigns/{id}`. Only
/// `customer_id`, `name` and a target (`site_id` and/or `server_ids`) are
/// required.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[schema(as = CampaignRequest)]
#[serde(deny_unknown_fields)]
pub struct CreateRequest {
    pub customer_id: Uuid,
    pub name: String,
    /// Every unrevoked server of this site of the customer.
    pub site_id: Option<Uuid>,
    /// These servers of the customer, besides those of `site_id`.
    #[serde(default)]
    pub server_ids: Vec<Uuid>,
    /// The batches, in order, up to 100; the servers left over make one
    /// last batch. Empty: one batch of every server.
    #[serde(default)]
    pub waves: Vec<WaveSpec>,
    /// Packages to upgrade, up to 1000; empty upgrades everything. Names
    /// are letters, digits and `.+_:-` not starting with `-`, or KB
    /// numbers and update IDs for Windows servers.
    #[serde(default)]
    pub packages: Vec<String>,
    /// Only security updates.
    #[serde(default)]
    pub security_only: bool,
    /// Whether the agent may reboot the server when an update needs it.
    #[serde(default)]
    pub allow_reboot: bool,
    /// Whether OS updates are included; true by default. Without them,
    /// only servers of a platform a selected manager is for are targets
    /// (ADR-0138).
    pub include_os: Option<bool>,
    /// Third-party managers to update after the OS, each once:
    /// `[{"manager": "winget" | "flatpak" | "snap", "packages": [id]}]`,
    /// empty `packages` for every pending app (ADR-0138).
    #[serde(default)]
    pub third_party: Vec<ThirdPartySpec>,
    /// Seconds a command may wait for its agent before it expires, 60 to
    /// 604800, default 3600; never past the end of the open window.
    pub command_ttl_s: Option<u64>,
    #[serde(default)]
    pub failure_threshold: FailureThreshold,
    #[serde(default)]
    pub windows: Vec<Window>,
    /// Snapshots to pin every server to during its update (ADR-0038).
    pub sources: Option<SourcesPin>,
    /// Update the agents to this version, like `0.3.1`, instead of
    /// packages (ADR-0053); takes no package options nor `sources`.
    pub agent_version: Option<String>,
    /// Move every server to the next release of its distribution instead
    /// (ADR-0182): `{"from": "debian 12", "to": "debian 13"}`, with
    /// `accept_limited_rollback` to upgrade servers that cannot get a
    /// snapshot first and `parallel` servers of a batch at once (1 by
    /// default). Takes no package options, `sources` nor `agent_version`.
    pub major_upgrade: Option<MajorUpgradeSpec>,
}

/// Servers by status; `failed` includes expired commands and skipped
/// servers, `in_progress` queued, sent and running commands.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[schema(as = CampaignProgress)]
pub struct Progress {
    pub total: i64,
    /// Servers without a command yet.
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
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = Campaign)]
pub struct CampaignView {
    pub id: Uuid,
    pub customer_id: Uuid,
    pub customer_name: String,
    pub name: String,
    /// The site named at creation; `null` when only `server_ids` were.
    pub site_id: Option<Uuid>,
    pub state: CampaignState,
    /// Why it stopped or failed; `null` otherwise.
    pub error: Option<String>,
    /// What every package update installs (unused by an agent update).
    pub spec: UpdateSpec,
    /// Seconds a command may wait for its agent.
    pub command_ttl_s: i32,
    pub failure_threshold: FailureThreshold,
    pub windows: Vec<Window>,
    /// The pinned snapshots, if any (ADR-0038).
    pub sources: Option<SourcesPin>,
    /// Set for an agent update campaign (ADR-0053).
    pub agent_version: Option<String>,
    /// Set for a major upgrade campaign (ADR-0182).
    pub major_upgrade: Option<MajorUpgradeSpec>,
    /// The `server_ids` of the request, as last written; `null` for a
    /// campaign made before they were kept (ADR-0166).
    pub server_ids: Option<Vec<Uuid>>,
    /// The `waves` of the request, as last written; `null` for a campaign
    /// made before they were kept. `waves` holds the batches they made.
    pub wave_specs: Option<Vec<WaveSpec>>,
    /// Number of batches.
    pub wave_count: usize,
    /// Index of the batch in progress, from 0; `null` before the first is
    /// queued.
    pub current_wave: Option<i32>,
    pub progress: Progress,
    /// How many earlier runs it has, read with `/campaigns/{id}/runs`
    /// (ADR-0173).
    pub previous_runs: i64,
    /// The user who made it; `null` once that user is deleted.
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

/// Body of `GET /api/campaigns`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = CampaignList)]
pub struct CampaignList {
    /// Newest first.
    pub campaigns: Vec<CampaignView>,
    /// Pass as `cursor` for the next page; `null` on the last one.
    pub next_cursor: Option<String>,
}

/// One server of a batch.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = CampaignServer)]
pub struct TargetView {
    pub server_id: Uuid,
    /// The agent's name.
    pub peer_id: String,
    pub hostname: String,
    pub site_id: Uuid,
    /// `pending` (no command yet), `skipped` (no command, `error` says
    /// why), or the command's state: `queued`, `sent`, `running`,
    /// `succeeded`, `failed`, `expired`, `cancelled`. A failed restore of
    /// pinned sources makes it `failed`.
    pub status: &'static str,
    /// The update command, see `/commands/{id}`; `null` before it is
    /// queued.
    pub command_id: Option<Uuid>,
    /// The update's exit code once it ran, else `null`.
    pub exit_code: Option<i32>,
    pub error: Option<String>,
    pub finished_at: Option<DateTime<Utc>>,
    /// With pinned snapshots: the `configure_sources` command before the
    /// update and the command restoring the sources after it.
    pub sources_command_id: Option<Uuid>,
    pub restore_command_id: Option<Uuid>,
    /// The local snapshot taken before the update, if any (ADR-0094).
    pub snapshot_command_id: Option<Uuid>,
    /// In a major upgrade: where the server's upgrade is, as its agent
    /// says (`starting`, `sources`, `refreshing`, `upgrading`,
    /// `full_upgrade`, `rebooting`, `done`, `failed`), and its Proxmox
    /// snapshot (`taking`, `taken`, `limited`, `failed`) (ADR-0182). A
    /// started upgrade has `status` `upgrading` until the server is back
    /// on the new release with its checks as before.
    pub upgrade_phase: Option<String>,
    pub vm_snapshot: Option<String>,
}

/// One batch.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = CampaignWave)]
pub struct WaveView {
    /// From 0.
    pub index: i32,
    /// `pending`, `running` or `done`.
    pub state: &'static str,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub progress: Progress,
    pub servers: Vec<TargetView>,
}

/// `GET /api/campaigns/{id}` and the answer of every write.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = CampaignDetail)]
pub struct CampaignDetail {
    #[serde(flatten)]
    pub campaign: CampaignView,
    /// The batches, in order.
    pub waves: Vec<WaveView>,
}

/// Query string of `GET /api/campaigns`. Every filter is optional.
#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct ListParams {
    pub customer_id: Option<Uuid>,
    pub state: Option<CampaignState>,
    /// 1 to 500, default 100.
    pub limit: Option<u32>,
    /// `next_cursor` of the previous page.
    pub cursor: Option<String>,
}

impl From<CampaignError> for ApiError {
    fn from(error: CampaignError) -> Self {
        match error {
            CampaignError::Invalid(message) => Self::Invalid(message),
            CampaignError::NotFound => Self::NotFound,
            e @ CampaignError::Transition { .. } => Self::Conflict(e.to_string()),
            CampaignError::Busy(message) => Self::Conflict(message),
            CampaignError::Database(e) => Self::Database(e),
        }
    }
}

/// 404 unless the user reads `customer` as a whole; then, with `admin`,
/// 403 unless it administers it.
pub(super) fn authorize_customer(
    user: &CurrentUser,
    customer: Uuid,
    admin: bool,
) -> Result<(), ApiError> {
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
    let runs: HashMap<Uuid, i64> = query::run_counts(&state.pool, &ids)
        .await?
        .into_iter()
        .collect();
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
                    agent_version: c.agent_version,
                    major_upgrade: c.major_upgrade.map(|m| m.0),
                    server_ids: c.requested_server_ids,
                    wave_specs: c.requested_waves.map(|w| w.0),
                    wave_count,
                    progress,
                    previous_runs: runs.get(&c.id).copied().unwrap_or(0),
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
    let targets = query::targets(&state.pool, id).await?;
    let waves = query::waves(&state.pool, id).await?;
    Ok(CampaignDetail {
        campaign,
        waves: wave_views(waves, &counts, targets),
    })
}

/// The batches `waves` with their `counts` and servers, `targets` in
/// batch order.
fn wave_views(
    waves: Vec<WaveRow>,
    counts: &[WaveCounts],
    targets: Vec<TargetRow>,
) -> Vec<WaveView> {
    let mut targets = targets.into_iter().peekable();
    let mut views = Vec::new();
    for wave in waves {
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
                snapshot_command_id: t.snapshot_command_id,
                upgrade_phase: t.upgrade_phase,
                vm_snapshot: t.vm_snapshot_state,
            });
        }
        let state = match (wave.started_at, wave.finished_at) {
            (_, Some(_)) => "done",
            (Some(_), None) => "running",
            (None, None) => "pending",
        };
        views.push(WaveView {
            index: wave.position,
            state,
            started_at: wave.started_at,
            finished_at: wave.finished_at,
            progress,
            servers,
        });
    }
    views
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

/// Checks `request` and makes it a [`NewCampaign`] by `user`, who must
/// administer its customer.
async fn new_campaign(
    state: &ApiState,
    user: &CurrentUser,
    request: CreateRequest,
) -> Result<NewCampaign, ApiError> {
    authorize_customer(user, request.customer_id, true)?;
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
        check_sources(state).await?;
    }
    let agent_version = request
        .agent_version
        .as_deref()
        .map(str::parse::<Version>)
        .transpose()
        .map_err(|_| ApiError::Invalid("agent_version must be like 0.2.0".into()))?;
    Ok(NewCampaign {
        customer_id: request.customer_id,
        name: request.name,
        site_id: request.site_id,
        server_ids: request.server_ids,
        waves: request.waves,
        spec: UpdateSpec {
            packages: request.packages,
            security_only: request.security_only,
            allow_reboot: request.allow_reboot,
            include_os: request.include_os.unwrap_or(true),
            third_party: request.third_party,
        },
        command_ttl: Duration::from_secs(ttl),
        failure_threshold: request.failure_threshold,
        windows: request.windows,
        sources: request.sources,
        agent_version,
        major_upgrade: request.major_upgrade,
        created_by: Some(user.id),
    })
}

/// Refuses a major upgrade of servers that cannot get a snapshot first,
/// unless the request accepts it, naming them (ADR-0182).
async fn check_rollback(
    state: &ApiState,
    user: &CurrentUser,
    new: &NewCampaign,
) -> Result<(), ApiError> {
    let Some(major) = &new.major_upgrade else {
        return Ok(());
    };
    if major.accept_limited_rollback {
        return Ok(());
    }
    let targets = CampaignStore::new(state.pool.clone())
        .resolve_targets(new)
        .await?;
    let servers = super::major_upgrades::readiness(
        state,
        user,
        &super::major_upgrades::Filter::default(),
        Some(&targets),
    )
    .await?;
    let limited: Vec<String> = servers
        .iter()
        .filter(|s| !s.rollback.available)
        .map(|s| match &s.rollback.detail {
            Some(why) => format!("{} ({why})", s.hostname),
            None => s.hostname.clone(),
        })
        .collect();
    if limited.is_empty() {
        return Ok(());
    }
    Err(ApiError::Invalid(format!(
        "rollback is limited on {}; set accept_limited_rollback to upgrade them without a snapshot",
        limited.join(", ")
    )))
}

/// `POST /api/campaigns`.
pub async fn create(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<CreateRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<CampaignDetail>), ApiError> {
    let request = body(request)?;
    let new = new_campaign(&state, &user, request).await?;
    check_rollback(&state, &user, &new).await?;
    let actor = user.id.to_string();
    let row = CampaignStore::new(state.pool.clone())
        .create(&new, ("user", &actor))
        .await?;
    tracing::info!(user = %user.id, campaign = %row.id, "campaign created");
    Ok((StatusCode::CREATED, Json(detail_of(&state, row.id).await?)))
}

/// `PUT /api/campaigns/{id}`: the whole campaign again. While a run is
/// under way only its name, windows and threshold may differ; once over,
/// it is prepared for its next run (ADR-0166, ADR-0173).
pub async fn update(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<CreateRequest>, JsonRejection>,
) -> Result<Json<CampaignDetail>, ApiError> {
    let id = parse_id(&id)?;
    let customer = customer_of(&state, &user, id).await?;
    authorize_customer(&user, customer, true)?;
    let request = body(request)?;
    let new = new_campaign(&state, &user, request).await?;
    check_rollback(&state, &user, &new).await?;
    let actor = user.id.to_string();
    let row = CampaignStore::new(state.pool.clone())
        .update(id, &new, ("user", &actor))
        .await?;
    if row.state.is_active() {
        if let Some(runner) = &state.campaigns {
            runner.wake();
        }
    }
    tracing::info!(user = %user.id, campaign = %id, "campaign edited");
    Ok(Json(detail_of(&state, id).await?))
}

/// `DELETE /api/campaigns/{id}`: a draft or a finished campaign
/// (ADR-0166).
pub async fn delete(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let id = parse_id(&id)?;
    let customer = customer_of(&state, &user, id).await?;
    authorize_customer(&user, customer, true)?;
    let actor = user.id.to_string();
    CampaignStore::new(state.pool.clone())
        .delete(id, ("user", &actor))
        .await?;
    tracing::info!(user = %user.id, campaign = %id, "campaign deleted");
    Ok(StatusCode::NO_CONTENT)
}

/// Which servers `POST /api/campaigns/{id}/relaunch` runs again.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, utoipa::ToSchema)]
#[schema(as = CampaignRelaunchServers)]
#[serde(rename_all = "lowercase")]
pub enum RelaunchServers {
    /// Every target of the campaign, planned again from its settings.
    #[default]
    All,
    /// Only the servers that failed in the last run, in one batch.
    Failed,
}

/// Body of `POST /api/campaigns/{id}/relaunch`.
#[derive(Debug, Default, Deserialize, utoipa::ToSchema)]
#[schema(as = CampaignRelaunchRequest)]
#[serde(deny_unknown_fields)]
pub struct RelaunchRequest {
    /// `all` by default.
    #[serde(default)]
    pub servers: RelaunchServers,
}

/// `POST /api/campaigns/{id}/relaunch`: a finished campaign runs again
/// with its current settings; its last run stays in the history
/// (ADR-0173).
pub async fn relaunch(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<RelaunchRequest>, JsonRejection>,
) -> Result<Json<CampaignDetail>, ApiError> {
    let id = parse_id(&id)?;
    let customer = customer_of(&state, &user, id).await?;
    authorize_customer(&user, customer, true)?;
    let RelaunchRequest { servers } = body(request)?;
    let actor = user.id.to_string();
    CampaignStore::new(state.pool.clone())
        .relaunch(id, servers == RelaunchServers::Failed, ("user", &actor))
        .await?;
    if let Some(runner) = &state.campaigns {
        runner.wake();
    }
    tracing::info!(user = %user.id, campaign = %id, ?servers, "campaign relaunched");
    Ok(Json(detail_of(&state, id).await?))
}

/// One past run of a campaign (ADR-0173).
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = CampaignRun)]
pub struct RunView {
    /// 1 for the first run.
    pub number: i32,
    /// `stopped`, `completed` or `failed`.
    pub state: CampaignState,
    /// Why it stopped or failed; `null` otherwise.
    pub error: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: DateTime<Utc>,
    /// The settings it ran with.
    pub settings: RunSettings,
    pub progress: Progress,
    /// Its batches and servers, as they ended.
    pub waves: Vec<WaveView>,
}

/// Body of `GET /api/campaigns/{id}/runs`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = CampaignRuns)]
pub struct RunList {
    /// Newest first. The run in progress or last finished is the campaign
    /// itself, not listed here.
    pub runs: Vec<RunView>,
}

/// `GET /api/campaigns/{id}/runs`.
pub async fn runs(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<RunList>, ApiError> {
    let id = parse_id(&id)?;
    let customer = customer_of(&state, &user, id).await?;
    authorize_customer(&user, customer, false)?;
    let mut runs = Vec::new();
    for run in query::runs(&state.pool, id).await? {
        let (waves, counts, targets) = query::run_detail(&state.pool, id, run.number).await?;
        let mut progress = Progress::default();
        counts.iter().for_each(|c| progress.add(c));
        runs.push(RunView {
            number: run.number,
            state: run.state,
            error: run.error,
            started_at: run.started_at,
            finished_at: run.finished_at,
            settings: run.settings.0,
            progress,
            waves: wave_views(waves, &counts, targets),
        });
    }
    Ok(Json(RunList { runs }))
}

/// 409 unless the server can build `configure_sources` commands, as
/// `POST /api/servers/{id}/sources` (ADR-0035).
async fn check_sources(state: &ApiState) -> Result<(), ApiError> {
    if state.settings.live.mirror_public_url().is_none() {
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
        assert_eq!(
            status(CampaignError::Busy("in flight".into())),
            StatusCode::CONFLICT
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
