//! Servers synced as PSA assets (ADR-0164, ADR-0191): the settings, the
//! PSA's asset types and their fields, and each server's asset, proposed
//! matches included. Global administrators only.

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgExecutor;
use uuid::Uuid;

use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::psa::{connected, recorded, require_global_admin, RecordList};
use super::sensors::{body, parse_id};
use super::{ApiError, ApiState};
use crate::psa::assets::AssetType;
use crate::psa::halopsa::INVENTORY_NUMBER;
use crate::psa::{self, Connection};

/// Body of `GET` and `PUT /api/psa/asset-settings`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = PsaAssetSettings)]
pub struct AssetSettingsView {
    /// Servers of matched sites and customers are synced as PSA assets.
    pub asset_sync: bool,
    /// A removed server's asset is marked inactive.
    pub asset_inactive_on_removal: bool,
    /// The asset type of each system family; a family without one is not
    /// synced.
    pub types: Vec<AssetType>,
}

/// One family's asset type in `PUT /api/psa/asset-settings`.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[schema(as = PsaAssetTypeRequest)]
#[serde(deny_unknown_fields)]
pub struct AssetTypeRequest {
    /// `linux` or `windows`.
    pub family: String,
    /// An asset type of `GET /api/psa/asset-types`.
    pub type_id: i64,
    /// `inventory_number` (the asset tag) or a field id of
    /// `GET /api/psa/asset-types/{id}/fields`, as text; absent or `null`
    /// for nowhere.
    #[serde(default)]
    pub hostname_field: Option<String>,
    #[serde(default)]
    pub serial_field: Option<String>,
    #[serde(default)]
    pub manufacturer_field: Option<String>,
    #[serde(default)]
    pub model_field: Option<String>,
}

/// Body of `PUT /api/psa/asset-settings`.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[schema(as = PsaAssetSettingsRequest)]
#[serde(deny_unknown_fields)]
pub struct AssetSettingsRequest {
    pub asset_sync: bool,
    #[serde(default)]
    pub asset_inactive_on_removal: bool,
    /// Replaces every family's type; at most one per family.
    #[serde(default)]
    pub types: Vec<AssetTypeRequest>,
}

/// A server and its PSA asset.
#[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
#[schema(as = PsaAssetLink)]
pub struct AssetLinkView {
    pub server_id: Uuid,
    pub hostname: String,
    pub site_name: String,
    /// `proposed` (an existing asset found by serial number or host name,
    /// to confirm), `create` (a new asset at the next pass), `linked` or
    /// `failed`.
    pub state: String,
    /// The PSA asset; `null` until there is one.
    pub asset_id: Option<i64>,
    /// The asset's key field when found or created.
    pub asset_name: Option<String>,
    /// `serial`, `hostname`, `created` or `chosen`.
    pub matched_by: Option<String>,
    /// Whether the asset was marked inactive after the server's removal.
    pub deactivated: bool,
    /// Why the PSA refused, in its words; `null` otherwise.
    pub error: Option<String>,
    /// The last push; `null` before the first.
    pub pushed_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

/// Body of `GET /api/psa/asset-links`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = PsaAssetLinkList)]
pub struct AssetLinkList {
    /// Proposed matches first, then failures, then the most recent; at
    /// most 200.
    pub servers: Vec<AssetLinkView>,
    /// Servers by state, all of them.
    pub proposed: i64,
    pub linked: i64,
    pub failed: i64,
}

/// Body of `PUT /api/psa/asset-links/{server_id}`.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[schema(as = PsaAssetLinkRequest)]
#[serde(deny_unknown_fields)]
pub struct AssetLinkRequest {
    /// The server's asset: the proposed one to confirm it, another asset
    /// of the server's client, or `null` to create a new one at the next
    /// pass.
    #[serde(default)]
    pub asset_id: Option<i64>,
}

/// Body of `POST /api/psa/asset-links/sync`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = PsaAssetSync)]
pub struct SyncQueued {
    /// Linked servers to look at again at the next passes.
    pub queued: u64,
}

async fn settings(
    db: impl PgExecutor<'_>,
    connection: &Connection,
) -> Result<AssetSettingsView, ApiError> {
    let types = sqlx::query_as(
        "SELECT family, type_id, type_name, hostname_field, serial_field, manufacturer_field,
                model_field
         FROM psa_asset_types WHERE connection_id = $1 ORDER BY family",
    )
    .bind(connection.id)
    .fetch_all(db)
    .await?;
    Ok(AssetSettingsView {
        asset_sync: connection.assets.asset_sync,
        asset_inactive_on_removal: connection.assets.asset_inactive_on_removal,
        types,
    })
}

async fn audited(
    db: impl PgExecutor<'_>,
    user: &CurrentUser,
    action: &str,
    target: (&str, &str),
    details: serde_json::Value,
) -> Result<(), ApiError> {
    let actor = user.id.to_string();
    audit::record(
        db,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action,
            target: Some(target),
            details,
        },
    )
    .await?;
    Ok(())
}

/// `GET /api/psa/asset-types`: the PSA's asset types.
pub async fn asset_types(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<RecordList>, ApiError> {
    require_global_admin(&user)?;
    let (connection, halo) = connected(&state).await?;
    let records = recorded(&state, &connection, halo.asset_types().await).await?;
    Ok(Json(RecordList { records }))
}

/// `GET /api/psa/asset-types/{id}/fields`: the fields of one asset type.
pub async fn asset_fields(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<RecordList>, ApiError> {
    require_global_admin(&user)?;
    let (connection, halo) = connected(&state).await?;
    let records = recorded(&state, &connection, halo.asset_fields(id).await).await?;
    Ok(Json(RecordList { records }))
}

/// `GET /api/psa/asset-settings`.
pub async fn get_settings(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<AssetSettingsView>, ApiError> {
    require_global_admin(&user)?;
    let connection = psa::load(&state.pool)
        .await?
        .ok_or_else(|| ApiError::Conflict("Set up the HaloPSA connection first.".into()))?;
    Ok(Json(settings(&state.pool, &connection).await?))
}

/// A field target checked against the type's fields.
fn target(value: Option<String>, fields: &[i64]) -> Result<Option<String>, ApiError> {
    let Some(value) = value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    if value == INVENTORY_NUMBER {
        return Ok(Some(value));
    }
    match value.parse::<i64>() {
        Ok(id) if fields.contains(&id) => Ok(Some(id.to_string())),
        _ => Err(ApiError::Invalid(format!(
            "The asset type has no field {value}: choose `inventory_number` or one of its fields."
        ))),
    }
}

/// `PUT /api/psa/asset-settings`: whether servers are synced, and each
/// family's asset type and fields, checked against the PSA's lists;
/// audited as `psa.connection.assets`.
pub async fn put_settings(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<AssetSettingsRequest>, JsonRejection>,
) -> Result<Json<AssetSettingsView>, ApiError> {
    require_global_admin(&user)?;
    let request = body(request)?;
    let (connection, halo) = connected(&state).await?;
    let mut families: Vec<&str> = Vec::new();
    for kind in &request.types {
        if !["linux", "windows"].contains(&kind.family.as_str()) {
            return Err(ApiError::Invalid(
                "`family` is `linux` or `windows`.".into(),
            ));
        }
        if families.contains(&kind.family.as_str()) {
            return Err(ApiError::Invalid(format!(
                "Choose one asset type for {}.",
                kind.family
            )));
        }
        families.push(&kind.family);
    }
    if request.asset_sync && request.types.is_empty() {
        return Err(ApiError::Invalid(
            "Choose an asset type for Linux or Windows servers before turning the sync on.".into(),
        ));
    }
    let known = if request.types.is_empty() {
        Vec::new()
    } else {
        recorded(&state, &connection, halo.asset_types().await).await?
    };
    let mut types = Vec::new();
    for kind in request.types {
        let found = known
            .iter()
            .find(|t| t.id == kind.type_id)
            .ok_or_else(|| ApiError::Invalid("HaloPSA has no such asset type.".into()))?;
        let fields: Vec<i64> = recorded(&state, &connection, halo.asset_fields(found.id).await)
            .await?
            .into_iter()
            .map(|f| f.id)
            .collect();
        types.push(AssetType {
            family: kind.family,
            type_id: found.id,
            type_name: found.name.clone(),
            hostname_field: target(kind.hostname_field, &fields)?,
            serial_field: target(kind.serial_field, &fields)?,
            manufacturer_field: target(kind.manufacturer_field, &fields)?,
            model_field: target(kind.model_field, &fields)?,
        });
    }
    let mut tx = state.pool.begin().await?;
    sqlx::query(
        "UPDATE psa_connections SET asset_sync = $2, asset_inactive_on_removal = $3,
             updated_at = now()
         WHERE id = $1",
    )
    .bind(connection.id)
    .bind(request.asset_sync)
    .bind(request.asset_inactive_on_removal)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM psa_asset_types WHERE connection_id = $1")
        .bind(connection.id)
        .execute(&mut *tx)
        .await?;
    for kind in &types {
        sqlx::query(
            "INSERT INTO psa_asset_types (connection_id, family, type_id, type_name,
                 hostname_field, serial_field, manufacturer_field, model_field)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(connection.id)
        .bind(&kind.family)
        .bind(kind.type_id)
        .bind(&kind.type_name)
        .bind(&kind.hostname_field)
        .bind(&kind.serial_field)
        .bind(&kind.manufacturer_field)
        .bind(&kind.model_field)
        .execute(&mut *tx)
        .await?;
    }
    // New fields or types: every linked asset is pushed again.
    // An empty hash differs from any, without linking back a second time.
    sqlx::query("UPDATE psa_asset_links SET pushed_hash = '' WHERE pushed_hash IS NOT NULL")
        .execute(&mut *tx)
        .await?;
    audited(
        &mut *tx,
        &user,
        "psa.connection.assets",
        ("psa_connection", psa::HALOPSA),
        json!({
            "asset_sync": request.asset_sync,
            "asset_inactive_on_removal": request.asset_inactive_on_removal,
            "types": types.iter().map(|t| json!({"family": t.family, "type_id": t.type_id})).collect::<Vec<_>>(),
        }),
    )
    .await?;
    let saved = psa::load(&mut *tx).await?.ok_or(ApiError::NotFound)?;
    let view = settings(&mut *tx, &saved).await?;
    tx.commit().await?;
    Ok(Json(view))
}

const LINKS: &str = "SELECT l.server_id, s.hostname, si.name AS site_name, l.state, l.asset_id,
        l.asset_name, l.matched_by, l.deactivated, l.error, l.pushed_at, l.updated_at
    FROM psa_asset_links l
    JOIN servers s ON s.id = l.server_id
    JOIN sites si ON si.id = s.site_id";

/// `GET /api/psa/asset-links`: servers and their assets, proposed matches
/// first.
pub async fn asset_links(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<AssetLinkList>, ApiError> {
    require_global_admin(&user)?;
    // SQL-safe: interpolates only the constant LINKS.
    let servers = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{LINKS} ORDER BY CASE l.state WHEN 'proposed' THEN 0 WHEN 'failed' THEN 1
             WHEN 'create' THEN 2 ELSE 3 END, l.updated_at DESC, l.server_id
         LIMIT 200"
    )))
    .fetch_all(&state.pool)
    .await?;
    let (proposed, linked, failed): (i64, i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE state = 'proposed'), count(*) FILTER (WHERE state = 'linked'),
                count(*) FILTER (WHERE state = 'failed')
         FROM psa_asset_links",
    )
    .fetch_one(&state.pool)
    .await?;
    Ok(Json(AssetLinkList {
        servers,
        proposed,
        linked,
        failed,
    }))
}

async fn asset_link(db: impl PgExecutor<'_>, server: Uuid) -> Result<AssetLinkView, ApiError> {
    // SQL-safe: interpolates only the constant LINKS.
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{LINKS} WHERE l.server_id = $1"
    )))
    .bind(server)
    .fetch_optional(db)
    .await?
    .ok_or(ApiError::NotFound)
}

/// `PUT /api/psa/asset-links/{server_id}`: confirms the proposed asset,
/// chooses another asset of the server's client, or asks for a new one;
/// audited as `psa.asset.link`.
pub async fn put_asset_link(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(server): Path<String>,
    request: Result<Json<AssetLinkRequest>, JsonRejection>,
) -> Result<Json<AssetLinkView>, ApiError> {
    require_global_admin(&user)?;
    let server = parse_id(&server)?;
    let request = body(request)?;
    let (connection, halo) = connected(&state).await?;
    let client: Option<Option<i64>> = sqlx::query_scalar(
        "SELECT coalesce(sl.client_id, cl.client_id)
         FROM servers s JOIN sites si ON si.id = s.site_id
         LEFT JOIN psa_site_links sl ON sl.site_id = s.site_id
         LEFT JOIN psa_customer_links cl ON cl.customer_id = si.customer_id
         WHERE s.id = $1",
    )
    .bind(server)
    .fetch_optional(&state.pool)
    .await?;
    let client = client.ok_or(ApiError::NotFound)?.ok_or_else(|| {
        ApiError::Conflict(
            "The server's site and customer are not matched to a HaloPSA client.".into(),
        )
    })?;
    let (state_name, asset, matched_by, name) = match request.asset_id {
        None => ("create", None, None, None),
        Some(id) => {
            let (found, owner) = recorded(&state, &connection, halo.asset(id).await)
                .await?
                .ok_or_else(|| ApiError::Invalid("HaloPSA has no such asset.".into()))?;
            if owner.is_some_and(|o| o != client) {
                return Err(ApiError::Invalid(
                    "That HaloPSA asset belongs to another client than the server's.".into(),
                ));
            }
            let name = found.key_field.clone().or(found.inventory_number.clone());
            ("linked", Some(found.id), Some("chosen"), name)
        }
    };
    let mut tx = state.pool.begin().await?;
    let taken: Option<String> = match asset {
        Some(id) => {
            sqlx::query_scalar(
                "SELECT s.hostname FROM psa_asset_links l JOIN servers s ON s.id = l.server_id
             WHERE l.connection_id = $1 AND l.asset_id = $2 AND l.server_id <> $3",
            )
            .bind(connection.id)
            .bind(id)
            .bind(server)
            .fetch_optional(&mut *tx)
            .await?
        }
        None => None,
    };
    if let Some(other) = taken {
        return Err(ApiError::Conflict(format!(
            "That HaloPSA asset is already the asset of {other}."
        )));
    }
    // Confirming the proposed asset keeps how it was found.
    sqlx::query(
        "INSERT INTO psa_asset_links (server_id, connection_id, asset_id, state, matched_by, asset_name)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (server_id) DO UPDATE SET connection_id = $2, asset_id = $3, state = $4,
             matched_by = CASE WHEN psa_asset_links.state = 'proposed'
                                    AND psa_asset_links.asset_id = $3
                               THEN psa_asset_links.matched_by ELSE $5 END,
             asset_name = coalesce($6, psa_asset_links.asset_name),
             pushed_hash = NULL, pushed_at = NULL, error = NULL, deactivated = false,
             updated_at = now()",
    )
    .bind(server)
    .bind(connection.id)
    .bind(asset)
    .bind(state_name)
    .bind(matched_by)
    .bind(name)
    .execute(&mut *tx)
    .await?;
    let target = server.to_string();
    audited(
        &mut *tx,
        &user,
        "psa.asset.link",
        ("server", &target),
        json!({ "asset_id": asset }),
    )
    .await?;
    let link = asset_link(&mut *tx, server).await?;
    tx.commit().await?;
    Ok(Json(link))
}

/// `DELETE /api/psa/asset-links/{server_id}`: forgets the server's asset
/// in MonitorFlow only; the next pass matches it again. 204, 404 when it
/// had none; audited as `psa.asset.unlink`.
pub async fn delete_asset_link(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(server): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_global_admin(&user)?;
    let server = parse_id(&server)?;
    let mut tx = state.pool.begin().await?;
    let removed: Option<Option<i64>> =
        sqlx::query_scalar("DELETE FROM psa_asset_links WHERE server_id = $1 RETURNING asset_id")
            .bind(server)
            .fetch_optional(&mut *tx)
            .await?;
    let asset = removed.ok_or(ApiError::NotFound)?;
    let target = server.to_string();
    audited(
        &mut *tx,
        &user,
        "psa.asset.unlink",
        ("server", &target),
        json!({ "asset_id": asset }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/psa/asset-links/sync`: every linked or failed server is
/// pushed again at the next passes, without waiting for the hour.
pub async fn sync_now(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<SyncQueued>, ApiError> {
    require_global_admin(&user)?;
    connected(&state).await?;
    let queued = sqlx::query(
        "UPDATE psa_asset_links SET pushed_at = NULL,
             pushed_hash = CASE WHEN pushed_hash IS NULL THEN NULL ELSE '' END
         WHERE state IN ('linked', 'failed')",
    )
    .execute(&state.pool)
    .await?
    .rows_affected();
    Ok(Json(SyncQueued { queued }))
}
