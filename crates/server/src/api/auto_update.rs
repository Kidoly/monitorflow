//! Automatic updates (ADR-0055), for global administrators:
//!
//! - `GET /api/auto-update`: the settings, the release agents move to, the
//!   rollout's progress per site, and earlier rollouts.
//! - `GET /api/auto-update/{kind}/peers?state=…`: the agents or relays
//!   behind one of those totals, classified by the same
//!   [`Peer::state`](crate::auto_update::Peer::state).
//! - `PUT /api/settings/auto-update`: changes `enabled` (unless the
//!   configuration sets it), `paused` or `failure_threshold`, stored in
//!   `server_settings` like the other server-wide settings (ADR-0054);
//!   audited as `settings.auto_update` with the old and new values.
//! - `POST /api/auto-update/rollouts/{kind}/{version}/resume`: resumes a
//!   halted rollout; audited as `auto_update.resume`.

use std::collections::{BTreeMap, HashMap};
use std::str::FromStr;

use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::{DateTime, Utc};
use monitorflow_common::release::Version;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::sensors::body;
use super::server_update::require_global_admin;
use super::servers::page_limit;
use super::{ApiError, ApiState};
use crate::auto_update::policy::{self, Settings, SitePhase, Tries};
use crate::auto_update::store::{self, Kind, Rollout, RolloutState, Stored};
use crate::auto_update::{self, server_version, PeerState};

/// Earlier rollouts listed.
const HISTORY: i64 = 10;
/// Peers listed per page of `GET /api/auto-update/{kind}/peers`.
pub const DEFAULT_PEERS: u32 = 100;
pub const MAX_PEERS: u32 = 500;

/// Body of `GET /api/auto-update`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = AutoUpdateStatus)]
pub struct StatusView {
    pub settings: Settings,
    /// The central server's version; agents and relays never move past
    /// it.
    pub server_version: String,
    pub agents: KindView,
    /// Updated before the agents behind them.
    pub relays: KindView,
}

/// Automatic updates of one kind of peer.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = AutoUpdateKindView)]
pub struct KindView {
    /// The newest stable release the server holds; `null` when it holds
    /// none.
    pub latest: Option<String>,
    /// What peers move to: the newest stable release not newer than the
    /// server; `null` when there is none.
    pub target: Option<String>,
    /// The rollout of `target`, once its first update was queued.
    pub rollout: Option<Rollout>,
    /// The sum of `sites`.
    pub totals: Totals,
    /// Every site, by customer and site name.
    pub sites: Vec<SiteView>,
    /// The 10 newest rollouts, `rollout` included, each with where it
    /// ended.
    pub history: Vec<HistoryRollout>,
}

/// A rollout of the history and where it stands now.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = AutoUpdateHistoryRollout)]
pub struct HistoryRollout {
    #[serde(flatten)]
    pub rollout: Rollout,
    pub outcome: Outcome,
}

/// Where a rollout of the history stands. `state` only tells whether it
/// halted; a rollout of an older release stays `rolling` there forever.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[schema(as = AutoUpdateRolloutOutcome)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    /// The rollout of the target, with servers still waiting or updating.
    Rolling,
    /// The rollout of the target, halted after too many failures.
    Halted,
    /// The rollout of the target, with nothing left to update.
    Finished,
    /// No longer the target: a newer release replaced it, so nothing more
    /// is queued for it.
    Superseded,
}

/// Where `rollout` stands, given the current `target` and the totals of
/// its rollout.
pub fn outcome(rollout: &Rollout, target: Option<&Version>, totals: &Totals) -> Outcome {
    let is_target = target.is_some_and(|t| t.to_string() == rollout.version);
    if !is_target {
        Outcome::Superseded
    } else if rollout.state == RolloutState::Halted {
        Outcome::Halted
    } else if totals.waiting + totals.pending == 0 {
        Outcome::Finished
    } else {
        Outcome::Rolling
    }
}

/// Servers (or relays) by where they stand towards `target`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[schema(as = AutoUpdateTotals)]
pub struct Totals {
    /// On the target or newer.
    pub up_to_date: usize,
    /// Older and not tried yet.
    pub waiting: usize,
    /// Of `waiting`, agents whose relay runs an older version or is being
    /// updated.
    pub waiting_for_relay: usize,
    /// Updates queued and not finished.
    pub pending: usize,
    /// Updates of the target that succeeded.
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
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[schema(as = AutoUpdateSite)]
pub struct SiteView {
    pub id: Uuid,
    pub name: String,
    pub customer_name: String,
    /// The site's override: `null` inherits.
    pub auto_update: Option<bool>,
    /// Whether automatic updates apply to it.
    pub enabled: bool,
    /// `null` when automatic updates do not apply to it or it has
    /// nothing to update.
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
            match peer.state(target) {
                Some(PeerState::UpToDate) => site.up_to_date += 1,
                Some(PeerState::Waiting) => {
                    site.waiting += 1;
                    if peer.held_by_relay(target) {
                        site.waiting_for_relay += 1;
                    }
                }
                Some(PeerState::Pending) => site.pending += 1,
                Some(PeerState::Failed) => site.failed += 1,
                None => {}
            }
        }
        for site in &mut sites {
            let t = tries.get(&site.id).copied().unwrap_or_default();
            let mut view = totals.get(&site.id).copied().unwrap_or_default();
            view.succeeded = t.succeeded;
            site.totals = view;
            if site.enabled && (t != Tries::default() || view.waiting > 0) {
                site.phase = Some(t.phase());
            }
        }
    }
    let totals = sites.iter().fold(Totals::default(), |a, s| a.add(s.totals));
    let history = Rollout::recent(&state.pool, kind, HISTORY)
        .await?
        .into_iter()
        .map(|rollout| HistoryRollout {
            outcome: outcome(&rollout, target, &totals),
            rollout,
        })
        .collect();
    Ok(KindView {
        latest: latest.map(ToString::to_string),
        target: target.map(ToString::to_string),
        rollout,
        totals,
        sites,
        history,
    })
}

/// Query string of `GET /api/auto-update/{kind}/peers`.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PeersParams {
    /// The total to list.
    pub state: PeerState,
    /// Only the peers of this site.
    pub site_id: Option<Uuid>,
    /// 1 to 500, default 100.
    pub limit: Option<u32>,
    /// Peers to skip, default 0.
    pub offset: Option<u32>,
}

/// Body of `GET /api/auto-update/{kind}/peers`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = AutoUpdatePeerList)]
pub struct PeerList {
    /// What peers move to; `null` when there is none, and then nothing is
    /// listed.
    pub target: Option<String>,
    /// Peers in this state (and site), the matching total of
    /// `GET /api/auto-update`; `peers` holds one page of them.
    pub total: usize,
    /// Failed ones newest first, the others by customer, site and name.
    pub peers: Vec<PeerView>,
}

/// One agent or relay.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = AutoUpdatePeer)]
pub struct PeerView {
    /// The server's ID for an agent, the relay's for a relay.
    pub id: Uuid,
    /// The server's hostname or the relay's name.
    pub name: String,
    pub site_id: Uuid,
    pub site_name: String,
    pub customer_id: Uuid,
    pub customer_name: String,
    /// The version it reports; `null` before it reported one.
    pub version: Option<String>,
    pub state: PeerState,
    /// A waiting agent whose relay runs an older version or is being
    /// updated: it goes once its relay is on the target.
    pub waiting_for_relay: bool,
    /// Why its automatic update to the target failed; `null` otherwise.
    pub error: Option<String>,
    /// When its automatic update to the target finished; `null` when not
    /// tried or not finished.
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(sqlx::FromRow)]
struct PeerRow {
    id: Uuid,
    name: String,
    site_name: String,
    customer_id: Uuid,
    customer_name: String,
    error: Option<String>,
    finished_at: Option<DateTime<Utc>>,
}

/// `GET /api/auto-update/{kind}/peers`.
pub async fn peers(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(kind): Path<String>,
    params: Result<Query<PeersParams>, QueryRejection>,
) -> Result<Json<PeerList>, ApiError> {
    require_global_admin(&user)?;
    let kind = Kind::from_str(&kind).map_err(|()| ApiError::NotFound)?;
    let Query(params) = params.map_err(|r| ApiError::Invalid(r.body_text()))?;
    let limit = page_limit(params.limit, DEFAULT_PEERS, MAX_PEERS)? as usize;
    let offset = params.offset.unwrap_or(0) as usize;
    let held = state.settings.agent_releases.held().await;
    let Some(target) = policy::target(&held, &server_version()).cloned() else {
        return Ok(Json(PeerList {
            target: None,
            total: 0,
            peers: Vec::new(),
        }));
    };
    let counted_from = Rollout::load(&state.pool, kind, &target)
        .await?
        .map(|r| r.counted_from);
    let matching: Vec<_> = store::peers(&state.pool, kind, &target, counted_from)
        .await?
        .into_iter()
        .filter(|p| p.state(&target) == Some(params.state))
        .filter(|p| params.site_id.is_none_or(|site| p.site_id == site))
        .collect();
    let ids: Vec<Uuid> = matching.iter().map(|p| p.id).collect();
    let sql = match kind {
        Kind::Agent => {
            "SELECT s.id, s.hostname AS name, si.name AS site_name, c.id AS customer_id,
                    c.name AS customer_name, cmd.error, cmd.finished_at
             FROM servers s
             JOIN sites si ON si.id = s.site_id
             JOIN customers c ON c.id = si.customer_id
             LEFT JOIN agent_auto_updates a ON a.server_id = s.id AND a.version = $2
             LEFT JOIN commands cmd ON cmd.id = a.command_id
             WHERE s.id = ANY($1)"
        }
        Kind::Relay => {
            "SELECT r.id, r.name, si.name AS site_name, c.id AS customer_id,
                    c.name AS customer_name, cmd.error, cmd.finished_at
             FROM relays r
             JOIN sites si ON si.id = r.site_id
             JOIN customers c ON c.id = si.customer_id
             LEFT JOIN relay_auto_updates a ON a.relay_id = r.id AND a.version = $2
             LEFT JOIN relay_commands cmd ON cmd.id = a.command_id
             WHERE r.id = ANY($1)"
        }
    };
    let mut rows: HashMap<Uuid, PeerRow> = sqlx::query_as::<_, PeerRow>(sql)
        .bind(&ids)
        .bind(target.to_string())
        .fetch_all(&state.pool)
        .await?
        .into_iter()
        .map(|row| (row.id, row))
        .collect();
    let total = matching.len();
    let mut peers: Vec<PeerView> = matching
        .into_iter()
        .filter_map(|peer| {
            let row = rows.remove(&peer.id)?;
            let failed = params.state == PeerState::Failed;
            Some(PeerView {
                id: peer.id,
                name: row.name,
                site_id: peer.site_id,
                site_name: row.site_name,
                customer_id: row.customer_id,
                customer_name: row.customer_name,
                waiting_for_relay: params.state == PeerState::Waiting
                    && peer.held_by_relay(&target),
                version: peer.version,
                state: params.state,
                error: row.error.filter(|_| failed),
                finished_at: row.finished_at,
            })
        })
        .collect();
    peers.sort_by(|a, b| {
        let newest = match params.state {
            PeerState::Failed => b.finished_at.cmp(&a.finished_at),
            _ => std::cmp::Ordering::Equal,
        };
        newest
            .then_with(|| a.customer_name.cmp(&b.customer_name))
            .then_with(|| a.site_name.cmp(&b.site_name))
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.id.cmp(&b.id))
    });
    let peers = peers.into_iter().skip(offset).take(limit).collect();
    Ok(Json(PeerList {
        target: Some(target.to_string()),
        total,
        peers,
    }))
}

/// Body of `PUT /api/settings/auto-update`: the fields to change.
#[derive(Debug, Default, Deserialize, utoipa::ToSchema)]
#[schema(as = AutoUpdateSettingsRequest)]
#[serde(deny_unknown_fields)]
pub struct SettingsRequest {
    /// The default of sites that inherit it; refused while the
    /// configuration sets it. Absent keeps it.
    pub enabled: Option<bool>,
    /// Nothing automatic is queued while paused. Absent keeps it.
    pub paused: Option<bool>,
    /// 1 to 1000. Absent keeps it.
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

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    fn rollout(version: &str, state: RolloutState) -> Rollout {
        Rollout {
            kind: Kind::Agent,
            version: version.into(),
            state,
            started_at: Utc::now(),
            halted_at: None,
            reason: None,
            counted_from: Utc::now(),
        }
    }

    #[test]
    fn older_rollouts_are_superseded_and_the_target_finishes() {
        let target = Version::from_str("1.2.0").unwrap();
        let busy = Totals {
            waiting: 2,
            ..Totals::default()
        };
        let done = Totals {
            up_to_date: 5,
            ..Totals::default()
        };
        let older = rollout("1.1.0", RolloutState::Rolling);
        assert_eq!(outcome(&older, Some(&target), &busy), Outcome::Superseded);
        let halted = rollout("1.1.0", RolloutState::Halted);
        assert_eq!(outcome(&halted, Some(&target), &busy), Outcome::Superseded);
        let current = rollout("1.2.0", RolloutState::Rolling);
        assert_eq!(outcome(&current, Some(&target), &busy), Outcome::Rolling);
        assert_eq!(outcome(&current, Some(&target), &done), Outcome::Finished);
        let stopped = rollout("1.2.0", RolloutState::Halted);
        assert_eq!(outcome(&stopped, Some(&target), &done), Outcome::Halted);
        assert_eq!(outcome(&current, None, &done), Outcome::Superseded);
    }
}
