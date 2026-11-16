//! `GET /api/servers/{id}/third-party` (ADR-0138): the third-party
//! package managers of the server's latest report and the apps they
//! list, read-only.

use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};

use super::access::authorize;
use super::auth::CurrentUser;
use super::sensors::parse_id;
use super::servers::{decode_cursor, encode_cursor, page_limit};
use super::{ApiError, ApiState};

/// Page size without `limit`.
pub const DEFAULT_LIMIT: u32 = 500;
/// Largest accepted `limit`.
pub const MAX_LIMIT: u32 = 5000;

#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct ListParams {
    /// `winget`, `flatpak` or `snap`: only that manager's apps.
    pub manager: Option<String>,
    /// `true`: only apps with an update pending.
    pub pending: Option<bool>,
    /// 1 to 5000, default 500.
    pub limit: Option<u32>,
    /// `next_cursor` of the previous page.
    pub cursor: Option<String>,
}

/// One third-party package manager of the server.
#[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
#[schema(as = ThirdPartyManager)]
pub struct ManagerView {
    /// `winget`, `flatpak` or `snap`.
    pub manager: String,
    /// `available`, `absent` (not installed) or `unusable` (installed,
    /// but its apps could not be listed; see `error`).
    pub state: String,
    /// The manager's version, when known.
    pub version: Option<String>,
    /// Why it is unusable, one line.
    pub error: Option<String>,
    /// Apps it lists as installed.
    pub installed: i64,
    /// Of those, apps with an update pending.
    pub pending: i64,
}

/// One app a third-party manager installed.
#[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
#[schema(as = ThirdPartyPackage)]
pub struct PackageView {
    /// `winget`, `flatpak` or `snap`.
    pub manager: String,
    /// What the manager's update takes: winget package identifier,
    /// Flatpak ref, snap name.
    pub id: String,
    /// Display name, `null` when the manager gives none.
    pub name: Option<String>,
    /// The installed version, as the manager writes it.
    pub version: String,
    /// The version an update would install; `null` when none is pending.
    pub available_version: Option<String>,
    /// `machine`, the only scope updates touch, or `user` (the agent's
    /// own account; listed only).
    pub scope: String,
    /// Where updates come from: winget source, Flatpak remote, snap
    /// channel; `null` when unknown.
    pub origin: Option<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = ThirdPartyList)]
pub struct List {
    /// False until the agent reports third-party managers: an agent or
    /// relay before ADR-0138. `managers` and `packages` are then empty.
    pub reported: bool,
    /// Every third-party manager of the server's platform, present or
    /// not, by name.
    pub managers: Vec<ManagerView>,
    /// Apps by manager, ID and scope.
    pub packages: Vec<PackageView>,
    /// Pass as `cursor` for the next page; `null` on the last one.
    pub next_cursor: Option<String>,
}

pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    params: Result<Query<ListParams>, QueryRejection>,
) -> Result<Json<List>, ApiError> {
    let id = parse_id(&id)?;
    let site: Option<uuid::Uuid> = sqlx::query_scalar("SELECT site_id FROM servers WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?;
    authorize(&user, site, false)?;
    site.ok_or(ApiError::NotFound)?;
    let Query(params) = params.map_err(|e| ApiError::Invalid(e.body_text()))?;
    if let Some(manager) = &params.manager {
        if monitorflow_common::third_party::parse(manager).is_none() {
            return Err(ApiError::Invalid(
                "manager must be winget, flatpak or snap".into(),
            ));
        }
    }
    let limit = page_limit(params.limit, DEFAULT_LIMIT, MAX_LIMIT)?;
    let after: Option<(String, String, String)> =
        params.cursor.as_deref().map(decode_cursor).transpose()?;
    let managers: Vec<ManagerView> = sqlx::query_as(
        "SELECT m.manager, m.state, m.version, m.error,
             count(p.package_id) AS installed,
             count(p.available_version) AS pending
         FROM server_package_managers m
         LEFT JOIN server_third_party_packages p
             ON p.server_id = m.server_id AND p.manager = m.manager
         WHERE m.server_id = $1
         GROUP BY m.manager, m.state, m.version, m.error
         ORDER BY m.manager",
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await?;
    let (after_manager, after_id, after_scope) = match after {
        Some((m, i, s)) => (Some(m), Some(i), Some(s)),
        None => (None, None, None),
    };
    let mut packages: Vec<PackageView> = sqlx::query_as(
        "SELECT manager, package_id AS id, name, version, available_version, scope, origin
         FROM server_third_party_packages
         WHERE server_id = $1
           AND ($2::text IS NULL OR manager = $2)
           AND (NOT $3 OR available_version IS NOT NULL)
           AND ($4::text IS NULL OR (manager, package_id, scope) > ($4, $5, $6))
         ORDER BY manager, package_id, scope
         LIMIT $7",
    )
    .bind(id)
    .bind(params.manager.as_deref())
    .bind(params.pending.unwrap_or(false))
    .bind(after_manager)
    .bind(after_id)
    .bind(after_scope)
    .bind(i64::from(limit) + 1)
    .fetch_all(&state.pool)
    .await?;
    let next_cursor = if packages.len() > limit as usize {
        packages.truncate(limit as usize);
        packages
            .last()
            .map(|p| encode_cursor(&(&p.manager, &p.id, &p.scope)))
    } else {
        None
    };
    Ok(Json(List {
        reported: !managers.is_empty(),
        managers,
        packages,
        next_cursor,
    }))
}
