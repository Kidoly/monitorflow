//! `GET /api/sites`: the sites a user may read, for site-level
//! configuration in the web interface.

use axum::extract::State;
use axum::Json;
use serde::Serialize;
use uuid::Uuid;

use super::auth::{CurrentUser, SiteScope};
use super::{ApiError, ApiState};

/// One site.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct SiteView {
    pub id: Uuid,
    pub name: String,
    pub server_count: i64,
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
    let (all_sites, sites) = match user.readable_sites() {
        SiteScope::None => return Err(ApiError::Forbidden),
        SiteScope::All => (true, Vec::new()),
        SiteScope::Sites(sites) => (false, sites),
    };
    let sites = sqlx::query_as(
        "SELECT si.id, si.name, count(s.id) AS server_count
         FROM sites si LEFT JOIN servers s ON s.site_id = si.id
         WHERE $1 OR si.id = ANY($2)
         GROUP BY si.id
         ORDER BY si.name",
    )
    .bind(all_sites)
    .bind(&sites)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(SiteList { sites }))
}
