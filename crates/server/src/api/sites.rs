//! `GET /api/sites`: the sites a user may read, for site-level
//! configuration in the web interface; `PUT
//! /api/sites/{id}/agent-auto-update` sets the site's automatic update
//! override (ADR-0053, ADR-0055); offline alerts are set in
//! [`super::offline_alerts`].

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use super::access::authorize_admin;
use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::sensors::{body, parse_id};
use super::{ApiError, ApiState};
use crate::auto_update::store;

/// One site.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct SiteView {
    pub id: Uuid,
    pub name: String,
    pub server_count: i64,
    /// Enrolled relays of the site, revoked ones included (ADR-0029).
    pub relay_count: i64,
    pub customer_id: Uuid,
    pub customer_name: String,
    /// The site's automatic update override: `null` follows the
    /// server-wide setting (ADR-0055).
    pub agent_auto_update: Option<bool>,
    /// Whether servers and relays of the site open incidents when offline
    /// (ADR-0057).
    pub offline_alerts: bool,
    /// Whether automatic updates apply to the site, override or not.
    #[sqlx(skip)]
    pub agent_auto_update_effective: bool,
}

impl SiteView {
    fn with_default(mut self, enabled: bool) -> Self {
        self.agent_auto_update_effective = self.agent_auto_update.unwrap_or(enabled);
        self
    }
}

/// The server-wide default of sites that inherit it.
async fn auto_update_default(state: &ApiState) -> Result<bool, sqlx::Error> {
    let settings = store::settings(&state.pool, state.settings.auto_update_enabled).await?;
    Ok(settings.enabled)
}

/// Columns of [`SiteView`]; callers add `WHERE` before the grouping.
const SELECT_SITES: &str = "SELECT si.id, si.name, count(s.id) AS server_count,
                                   (SELECT count(*) FROM relays r WHERE r.site_id = si.id)
                                       AS relay_count,
                                   si.customer_id, c.name AS customer_name,
                                   si.agent_auto_update, si.offline_alerts
                            FROM sites si
                            JOIN customers c ON c.id = si.customer_id
                            LEFT JOIN servers s ON s.site_id = si.id";

/// The site `id`, without access checks.
pub(super) async fn fetch(state: &ApiState, id: Uuid) -> Result<Option<SiteView>, sqlx::Error> {
    let site: Option<SiteView> = sqlx::query_as(&format!(
        "{SELECT_SITES} WHERE si.id = $1 GROUP BY si.id, c.name"
    ))
    .bind(id)
    .fetch_optional(&state.pool)
    .await?;
    match site {
        Some(site) => Ok(Some(site.with_default(auto_update_default(state).await?))),
        None => Ok(None),
    }
}

/// Body of `GET /api/sites`, sorted by name.
#[derive(Debug, Serialize)]
pub struct SiteList {
    pub sites: Vec<SiteView>,
}

/// `GET /api/sites`. Needs at least one role. Not paginated: sites are
/// few, unlike servers.
pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<SiteList>, ApiError> {
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    let (all_sites, sites) = user.readable_sites().bind();
    let sites: Vec<SiteView> = sqlx::query_as(&format!(
        "{SELECT_SITES}
         WHERE $1 OR si.id = ANY($2)
         GROUP BY si.id, c.name
         ORDER BY si.name"
    ))
    .bind(all_sites)
    .bind(&sites)
    .fetch_all(&state.pool)
    .await?;
    let enabled = auto_update_default(&state).await?;
    let sites = sites.into_iter().map(|s| s.with_default(enabled)).collect();
    Ok(Json(SiteList { sites }))
}

/// Body of `PUT /api/sites/{id}/agent-auto-update`: `true` or `false`
/// overrides the server-wide setting, `null` inherits it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutoUpdateRequest {
    pub enabled: Option<bool>,
}

/// `PUT /api/sites/{id}/agent-auto-update`: administrators of the site's
/// customer turn automatic updates on or off for it, or make it inherit
/// the server-wide setting; audited.
pub async fn set_agent_auto_update(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<AutoUpdateRequest>, JsonRejection>,
) -> Result<Json<SiteView>, ApiError> {
    let id = parse_id(&id)?;
    let exists: Option<Uuid> = sqlx::query_scalar("SELECT id FROM sites WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?;
    authorize_admin(&user, exists)?;
    exists.ok_or(ApiError::NotFound)?;
    let request = body(request)?;
    let mut tx = state.pool.begin().await?;
    sqlx::query("UPDATE sites SET agent_auto_update = $2 WHERE id = $1")
        .bind(id)
        .bind(request.enabled)
        .execute(&mut *tx)
        .await?;
    let actor = user.id.to_string();
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action: "site.agent_auto_update",
            target: Some(("site", &id.to_string())),
            details: json!({ "enabled": request.enabled }),
        },
    )
    .await?;
    tx.commit().await?;
    fetch(&state, id).await?.map(Json).ok_or(ApiError::NotFound)
}
