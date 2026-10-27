//! Automatic updates (ADR-0055), for global administrators:
//!
//! - `GET /api/auto-update`: the settings, the release agents move to, the
//!   rollout's progress per site, and earlier rollouts.
//! - `PUT /api/auto-update`: changes `enabled` (unless the configuration
//!   sets it), `paused` or `failure_threshold`; audited as
//!   `auto_update.settings`.
//! - `POST /api/auto-update/rollouts/{kind}/{version}/resume`: resumes a
//!   halted rollout; audited as `auto_update.resume`.

use std::collections::BTreeMap;
use std::str::FromStr;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::Json;
use chrono::{DateTime, Utc};
use monitorflow_common::release::Version;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::mirrors::require_global_admin;
use super::sensors::body;
use super::{ApiError, ApiState};
use crate::auto_update::policy::{self, Settings, SitePhase, Tries};
use crate::auto_update::store::{self, Kind, Rollout};
use crate::auto_update::{self, server_version};

/// Earlier rollouts listed.
const HISTORY: i64 = 10;

/// Body of `GET /api/auto-update`.
#[derive(Debug, Serialize)]
pub struct StatusView {
    pub settings: Settings,
    pub updated_at: DateTime<Utc>,
    /// Peers never move past it.
    pub server_version: String,
    pub agents: KindView,
    /// Updated before the agents behind them.
    pub relays: KindView,
}

/// Automatic updates of one kind of peer.
#[derive(Debug, Serialize)]
pub struct KindView {
    /// The newest stable release the server holds.
    pub latest: Option<String>,
    /// What peers move to: `latest`, unless the server is older.
    pub target: Option<String>,
    /// The rollout of `target`, once its first update was queued.
    pub rollout: Option<Rollout>,
    pub totals: Totals,
    pub sites: Vec<SiteView>,
    /// The newest rollouts, `rollout` included.
    pub history: Vec<Rollout>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Totals {
    /// On the target or newer.
    pub up_to_date: usize,
    /// Older and not tried yet.
    pub waiting: usize,
    /// Of `waiting`, agents whose relay runs an older version or is being
    /// updated.
    pub waiting_for_relay: usize,
    pub pending: usize,
    pub succeeded: usize,
    /// Since the rollout started or was resumed.
    pub failed: usize,
}

impl Totals {
    fn add(self, other: Self) -> Self {
        Self {
            up_to_date: self.up_to_date + other.up_to_date,
            waiting: self.waiting + other.waiting,
            waiting_for_relay: self.waiting_for_relay + other.waiting_for_relay,
            pending: self.pending + other.pending,
            succeeded: self.succeeded + other.succeeded,
            failed: self.failed + other.failed,
        }
    }
}

/// One site.
#[derive(Debug, Clone, Serialize)]
pub struct SiteView {
    pub id: Uuid,
    pub name: String,
    pub customer_name: String,
    /// The site's override: `null` inherits.
    pub auto_update: Option<bool>,
    /// Whether automatic updates apply to it.
    pub enabled: bool,
    pub phase: Option<SitePhase>,
    pub totals: Totals,
}

#[derive(sqlx::FromRow)]
struct SiteRow {
    id: Uuid,
    name: String,
    customer_name: String,
    auto_update: Option<bool>,
}

/// `GET /api/auto-update`.
pub async fn status(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<StatusView>, ApiError> {
    require_global_admin(&user)?;
    let (settings, updated_at) =
        store::settings(&state.pool, state.settings.auto_update_enabled).await?;
    let held = state.settings.agent_releases.held().await;
    let ceiling = server_version();
    let latest = held.iter().filter(|v| v.pre.is_empty()).max().cloned();
    let target = policy::target(&held, &ceiling).cloned();
    let rows: Vec<SiteRow> = sqlx::query_as(
        "SELECT si.id, si.name, c.name AS customer_name, si.agent_auto_update AS auto_update
         FROM sites si JOIN customers c ON c.id = si.customer_id
         ORDER BY c.name, si.name",
    )
    .fetch_all(&state.pool)
    .await?;
    let sites: Vec<SiteView> = rows
        .into_iter()
        .map(|row| SiteView {
            enabled: settings.site_enabled(row.auto_update),
            id: row.id,
            name: row.name,
            customer_name: row.customer_name,
            auto_update: row.auto_update,
            phase: None,
            totals: Totals::default(),
        })
        .collect();
    let mut views = Vec::new();
    for kind in [Kind::Agent, Kind::Relay] {
        let view = kind_view(
            &state,
            kind,
            latest.as_ref(),
            target.as_ref(),
            sites.clone(),
        )
        .await?;
        views.push(view);
    }
    let relays = views.pop().expect("two kinds");
    let agents = views.pop().expect("two kinds");
    Ok(Json(StatusView {
        settings,
        updated_at,
        server_version: ceiling.to_string(),
        agents,
        relays,
    }))
}

/// The progress of `kind` towards `target`, per site.
async fn kind_view(
    state: &ApiState,
    kind: Kind,
    latest: Option<&Version>,
    target: Option<&Version>,
    mut sites: Vec<SiteView>,
) -> Result<KindView, ApiError> {
    let mut rollout = None;
    if let Some(target) = target {
        rollout = Rollout::load(&state.pool, kind, target).await?;
        let counted_from = rollout.as_ref().map(|r| r.counted_from);
        let peers = store::peers(&state.pool, kind, target, counted_from).await?;
        let tries = auto_update::tries(&peers);
        let mut totals: BTreeMap<Uuid, Totals> = BTreeMap::new();
        for peer in &peers {
            let site = totals.entry(peer.site_id).or_default();
            if peer.tried.is_none() {
                if peer.needs(target) {
                    site.waiting += 1;
                    if peer.held_by_relay(target) {
                        site.waiting_for_relay += 1;
                    }
                } else if peer
                    .version
                    .as_deref()
                    .and_then(|v| Version::from_str(v).ok())
                    .is_some_and(|v| v >= *target)
                {
                    site.up_to_date += 1;
                }
            } else if peer.tried.as_deref() == Some("succeeded") {
                site.up_to_date += 1;
            }
        }
        for site in &mut sites {
            let t = tries.get(&site.id).copied().unwrap_or_default();
            let mut view = totals.get(&site.id).copied().unwrap_or_default();
            (view.pending, view.succeeded, view.failed) = (t.pending, t.succeeded, t.failed);
            site.totals = view;
            if site.enabled && (t != Tries::default() || view.waiting > 0) {
                site.phase = Some(t.phase());
            }
        }
    }
    let totals = sites.iter().fold(Totals::default(), |a, s| a.add(s.totals));
    let history = Rollout::recent(&state.pool, kind, HISTORY).await?;
    Ok(KindView {
        latest: latest.map(ToString::to_string),
        target: target.map(ToString::to_string),
        rollout,
        totals,
        sites,
        history,
    })
}

/// Body of `PUT /api/auto-update`: the fields to change.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsRequest {
    pub enabled: Option<bool>,
    pub paused: Option<bool>,
    pub failure_threshold: Option<u32>,
}

/// `PUT /api/auto-update`.
pub async fn update(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<SettingsRequest>, JsonRejection>,
) -> Result<Json<StatusView>, ApiError> {
    require_global_admin(&user)?;
    let request = body(request)?;
    if request.enabled.is_some() && state.settings.auto_update_enabled.is_some() {
        return Err(ApiError::Conflict(
            "auto_update.enabled is set in the server's configuration".into(),
        ));
    }
    if let Some(threshold) = request.failure_threshold {
        if !(1..=policy::MAX_FAILURE_THRESHOLD).contains(&threshold) {
            return Err(ApiError::Invalid(format!(
                "failure_threshold must be 1 to {}",
                policy::MAX_FAILURE_THRESHOLD
            )));
        }
    }
    let mut tx = state.pool.begin().await?;
    sqlx::query(
        "UPDATE auto_update_settings
         SET enabled = coalesce($1, enabled), paused = coalesce($2, paused),
             failure_threshold = coalesce($3, failure_threshold),
             updated_at = now(), updated_by = $4",
    )
    .bind(request.enabled)
    .bind(request.paused)
    .bind(request.failure_threshold.map(|t| t as i32))
    .bind(user.id)
    .execute(&mut *tx)
    .await?;
    let actor = user.id.to_string();
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action: "auto_update.settings",
            target: None,
            details: json!({
                "enabled": request.enabled,
                "paused": request.paused,
                "failure_threshold": request.failure_threshold,
            }),
        },
    )
    .await?;
    tx.commit().await?;
    status(State(state), user).await
}

/// `POST /api/auto-update/rollouts/{kind}/{version}/resume`.
pub async fn resume(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path((kind, version)): Path<(String, String)>,
) -> Result<Json<Rollout>, ApiError> {
    require_global_admin(&user)?;
    let kind = Kind::from_str(&kind).map_err(|()| ApiError::NotFound)?;
    let mut tx = state.pool.begin().await?;
    let Some(rollout) = Rollout::resume(&mut tx, kind, &version).await? else {
        return Err(ApiError::Conflict("this rollout is not halted".into()));
    };
    let actor = user.id.to_string();
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action: "auto_update.resume",
            target: Some(("release", &version)),
            details: json!({ "kind": kind.as_str(), "version": version }),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(Json(rollout))
}
