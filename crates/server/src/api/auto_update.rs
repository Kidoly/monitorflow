//! Automatic updates (ADR-0055), for global administrators:
//!
//! - `GET /api/auto-update`: the settings, the release agents move to, the
//!   rollout's progress per site, and earlier rollouts.
//! - `PUT /api/settings/auto-update`: changes `enabled` (unless the
//!   configuration sets it), `paused` or `failure_threshold`, stored in
//!   `server_settings` like the other server-wide settings (ADR-0054);
//!   audited as `settings.auto_update` with the old and new values.
//! - `POST /api/auto-update/rollouts/{kind}/{version}/resume`: resumes a
//!   halted rollout; audited as `auto_update.resume`.

use std::collections::BTreeMap;
use std::str::FromStr;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::Json;
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
use crate::auto_update::store::{self, Kind, Rollout, Stored};
use crate::auto_update::{self, server_version};

/// Earlier rollouts listed.
const HISTORY: i64 = 10;

/// Body of `GET /api/auto-update`.
#[derive(Debug, Serialize)]
pub struct StatusView {
    pub settings: Settings,
    /// Peers never move past it.
    pub server_version: String,
    pub agents: KindView,
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
    pub pending: usize,
    pub succeeded: usize,
    /// Since the rollout started or was resumed.
    pub failed: usize,
}

/// One site.
#[derive(Debug, Serialize)]
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
    let settings = store::settings(&state.pool, state.settings.auto_update_enabled).await?;
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
    let mut sites: Vec<SiteView> = rows
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
    let mut rollout = None;
    if let Some(target) = &target {
        rollout = Rollout::load(&state.pool, Kind::Agent, target).await?;
        let peers = store::peers(
            &state.pool,
            target,
            rollout.as_ref().map(|r| r.counted_from),
        )
        .await?;
        let tries = auto_update::tries(&peers);
        let mut totals: BTreeMap<Uuid, Totals> = BTreeMap::new();
        for peer in &peers {
            let site = totals.entry(peer.site_id).or_default();
            if peer.tried.is_none() {
                if peer.needs(target) {
                    site.waiting += 1;
                } else if peer
                    .agent_version
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
    let totals = sites.iter().fold(Totals::default(), |a, s| Totals {
        up_to_date: a.up_to_date + s.totals.up_to_date,
        waiting: a.waiting + s.totals.waiting,
        pending: a.pending + s.totals.pending,
        succeeded: a.succeeded + s.totals.succeeded,
        failed: a.failed + s.totals.failed,
    });
    let history = Rollout::recent(&state.pool, Kind::Agent, HISTORY).await?;
    Ok(Json(StatusView {
        settings,
        server_version: ceiling.to_string(),
        agents: KindView {
            latest: latest.map(|v| v.to_string()),
            target: target.map(|v| v.to_string()),
            rollout,
            totals,
            sites,
            history,
        },
    }))
}

/// Body of `PUT /api/settings/auto-update`: the fields to change.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsRequest {
    pub enabled: Option<bool>,
    pub paused: Option<bool>,
    pub failure_threshold: Option<u32>,
}

/// `GET /api/settings/auto-update`: the settings as they apply.
pub async fn get_settings(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<Settings>, ApiError> {
    require_global_admin(&user)?;
    Ok(Json(
        store::settings(&state.pool, state.settings.auto_update_enabled).await?,
    ))
}

/// `PUT /api/settings/auto-update`.
pub async fn set_settings(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<SettingsRequest>, JsonRejection>,
) -> Result<Json<Settings>, ApiError> {
    require_global_admin(&user)?;
    let request = body(request)?;
    if request.enabled.is_some() && state.settings.auto_update_enabled.is_some() {
        return Err(ApiError::Conflict(
            "automatic updates are turned on or off by the server's configuration (auto_update.enabled)"
                .into(),
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
    // Locks the row, so concurrent changes audit the right old values.
    let old: Stored = sqlx::query_as(
        "SELECT auto_update_enabled, auto_update_paused, auto_update_failure_threshold
         FROM server_settings FOR UPDATE",
    )
    .fetch_optional(&mut *tx)
    .await?
    .unwrap_or_default();
    let new = Stored {
        enabled: request.enabled.or(old.enabled),
        paused: request.paused.or(old.paused),
        failure_threshold: request
            .failure_threshold
            .map(|t| t as i32)
            .or(old.failure_threshold),
    };
    sqlx::query(
        "INSERT INTO server_settings (singleton, auto_update_enabled, auto_update_paused,
                                      auto_update_failure_threshold)
         VALUES (true, $1, $2, $3)
         ON CONFLICT (singleton) DO UPDATE
         SET auto_update_enabled = $1, auto_update_paused = $2, auto_update_failure_threshold = $3",
    )
    .bind(new.enabled)
    .bind(new.paused)
    .bind(new.failure_threshold)
    .execute(&mut *tx)
    .await?;
    let actor = user.id.to_string();
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action: "settings.auto_update",
            target: Some(("settings", "auto_update")),
            details: json!({ "old": old, "new": new }),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(Json(new.effective(state.settings.auto_update_enabled)))
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
