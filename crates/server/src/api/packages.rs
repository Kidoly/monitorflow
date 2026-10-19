//! `GET /api/servers/{id}/packages` (ADR-0031): the server's latest package
//! report, read-only.

use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::{DateTime, Utc};
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

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListParams {
    /// Only packages with an upgrade available.
    pub upgradable: Option<bool>,
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct PackageView {
    pub name: String,
    pub arch: String,
    pub version: String,
    /// `null` when no upgrade is available.
    pub upgradable_version: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PackageList {
    /// `null` until the agent sent a report.
    pub reboot_required: Option<bool>,
    /// When the agent collected the report, by its clock.
    pub reported_at: Option<DateTime<Utc>>,
    /// Packages by name and architecture.
    pub packages: Vec<PackageView>,
    pub next_cursor: Option<String>,
}

pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    params: Result<Query<ListParams>, QueryRejection>,
) -> Result<Json<PackageList>, ApiError> {
    let id = parse_id(&id)?;
    let row: Option<(uuid::Uuid, Option<bool>, Option<DateTime<Utc>>)> = sqlx::query_as(
        "SELECT site_id, reboot_required, packages_reported_at FROM servers WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?;
    authorize(&user, row.map(|r| r.0), false)?;
    let (_, reboot_required, reported_at) = row.ok_or(ApiError::NotFound)?;
    let Query(params) = params.map_err(|e| ApiError::Invalid(e.body_text()))?;
    let limit = page_limit(params.limit, DEFAULT_LIMIT, MAX_LIMIT)?;
    let after: Option<(String, String)> =
        params.cursor.as_deref().map(decode_cursor).transpose()?;
    let (after_name, after_arch) = after.unzip();
    let mut packages: Vec<PackageView> = sqlx::query_as(
        "SELECT name, arch, version, upgradable_version FROM server_packages
         WHERE server_id = $1
           AND (NOT $2 OR upgradable_version IS NOT NULL)
           AND ($3::text IS NULL OR (name, arch) > ($3, $4))
         ORDER BY name, arch
         LIMIT $5",
    )
    .bind(id)
    .bind(params.upgradable.unwrap_or(false))
    .bind(after_name)
    .bind(after_arch)
    .bind(i64::from(limit) + 1)
    .fetch_all(&state.pool)
    .await?;
    let next_cursor = if packages.len() > limit as usize {
        packages.truncate(limit as usize);
        packages.last().map(|p| encode_cursor(&(&p.name, &p.arch)))
    } else {
        None
    };
    Ok(Json(PackageList {
        reboot_required,
        reported_at,
        packages,
        next_cursor,
    }))
}
