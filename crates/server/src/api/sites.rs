//! `GET /api/sites`: the sites a user may read, for site-level
//! configuration in the web interface.

use axum::extract::State;
use axum::Json;
use serde::Serialize;
use uuid::Uuid;

use super::auth::CurrentUser;
use super::{ApiError, ApiState};

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
}

/// Columns of [`SiteView`]; callers add `WHERE` before the grouping.
const SELECT_SITES: &str = "SELECT si.id, si.name, count(s.id) AS server_count,
                                   (SELECT count(*) FROM relays r WHERE r.site_id = si.id)
                                       AS relay_count,
                                   si.customer_id, c.name AS customer_name
                            FROM sites si
                            JOIN customers c ON c.id = si.customer_id
                            LEFT JOIN servers s ON s.site_id = si.id";

/// The site `id`, without access checks.
pub(super) async fn fetch(pool: &sqlx::PgPool, id: Uuid) -> Result<Option<SiteView>, sqlx::Error> {
    sqlx::query_as(&format!(
        "{SELECT_SITES} WHERE si.id = $1 GROUP BY si.id, c.name"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
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
    let sites = sqlx::query_as(&format!(
        "{SELECT_SITES}
         WHERE $1 OR si.id = ANY($2)
         GROUP BY si.id, c.name
         ORDER BY si.name"
    ))
    .bind(all_sites)
    .bind(&sites)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(SiteList { sites }))
}
