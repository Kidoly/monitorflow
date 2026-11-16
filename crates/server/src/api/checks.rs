//! How many servers run each check, over the sites the user reads: the
//! "Most used" row of the check catalogue in the web interface.
//!
//! A server row counts its server; a site row counts every server of the
//! site that is not removed. A server with its own copy of a site check
//! counts twice, which a ranking does not mind. Turned-off rows count
//! nothing.

use axum::extract::State;
use axum::Json;
use serde::Serialize;

use super::auth::CurrentUser;
use super::{ApiError, ApiState};

/// Most entries returned; the catalogue shows a handful.
pub const MAX_USAGE: i64 = 200;

/// One check and how many servers run it.
#[derive(Debug, Serialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct CheckUsage {
    /// The kind of check, like `port`, `systemd` or `plugin`.
    pub kind: String,
    /// The sensor name, like `port_443` or `service_nginx`.
    pub name: String,
    /// For kind `plugin`, the plugin it runs.
    pub plugin: Option<String>,
    /// Servers that run it, not removed ones.
    pub servers: i64,
}

/// Body of `GET /api/checks/usage`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = CheckUsageList)]
pub struct UsageList {
    /// Most servers first.
    pub checks: Vec<CheckUsage>,
}

/// `GET /api/checks/usage`.
pub async fn usage(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<UsageList>, ApiError> {
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    let (all, sites) = user.readable_sites().bind();
    let rows: Vec<(String, String, Option<String>, i64)> = sqlx::query_as(
        "WITH counted AS (
             SELECT sc.kind, sc.name, sc.params->>'plugin' AS plugin,
                    CASE WHEN sc.server_id IS NOT NULL THEN 1
                         ELSE (SELECT count(*) FROM servers m
                               WHERE m.site_id = sc.site_id AND m.revoked_at IS NULL)
                    END AS servers
             FROM sensor_configs sc
             LEFT JOIN servers s ON s.id = sc.server_id
             WHERE sc.enabled
               AND (sc.server_id IS NULL OR s.revoked_at IS NULL)
               AND ($1 OR COALESCE(sc.site_id, s.site_id) = ANY($2))
         )
         SELECT kind, name, plugin, sum(servers)::bigint AS servers
         FROM counted GROUP BY kind, name, plugin
         HAVING sum(servers) > 0
         ORDER BY servers DESC, kind, name
         LIMIT $3",
    )
    .bind(all)
    .bind(&sites)
    .bind(MAX_USAGE)
    .fetch_all(&state.pool)
    .await?;
    let checks = rows
        .into_iter()
        .map(|(kind, name, plugin, servers)| CheckUsage {
            kind,
            name,
            plugin,
            servers,
        })
        .collect();
    Ok(Json(UsageList { checks }))
}
