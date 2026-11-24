//! The hardware and software inventory (ADR-0187): a server's latest
//! inventory and its changes, the warranty people type in, and the search
//! across the fleet, as JSON or CSV.
//!
//! - `GET /api/servers/{id}/inventory`
//! - `GET /api/servers/{id}/inventory/changes`
//! - `PUT /api/servers/{id}/warranty`
//! - `GET /api/inventory`

use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use super::access::authorize;
use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::sensors::parse_id;
use super::servers::{decode_cursor, encode_cursor, page_limit};
use super::{ApiError, ApiState};
use crate::server_inventory::search::{self, Hit, KINDS};

/// Longest warranty note, in characters.
pub const MAX_NOTE: usize = 500;

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct InventoryDates {
    /// When the agent collected it, by its clock.
    pub collected_at: DateTime<Utc>,
    /// When the server first received an inventory.
    pub first_at: DateTime<Utc>,
    /// When its content last changed.
    pub changed_at: DateTime<Utc>,
    /// The last report saying it is still current; agents send one at
    /// least daily.
    pub checked_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct ItemView {
    /// `system`, `processor`, `memory`, `disk`, `nic`, `software`,
    /// `service`, `feature` or `licence`.
    pub kind: String,
    /// Unique within its kind, like `sda` or a program's name.
    pub key: String,
    /// Text fields; the names of each kind are in the description.
    #[schema(value_type = Object)]
    pub fields: Value,
}

#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Warranty {
    /// The last day covered; `null` when unknown.
    pub ends_on: Option<NaiveDate>,
    /// Like the contract or the level of service; `null` when none.
    pub note: Option<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct WarrantyView {
    pub ends_on: Option<NaiveDate>,
    pub note: Option<String>,
    pub updated_at: DateTime<Utc>,
}

/// What a PSA asset receives (ADR-0164): the host name and, from the
/// inventory, the maker, model and serial number.
#[derive(Debug, Default, Serialize, utoipa::ToSchema)]
pub struct AssetFacts {
    pub hostname: String,
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub serial: Option<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ServerInventory {
    /// `null` until the agent sent an inventory (agents before 0.12).
    pub reported: Option<InventoryDates>,
    /// Every item, by kind then key. Installed Linux packages are not
    /// items: see `GET /api/v1/servers/{id}/packages`.
    pub items: Vec<ItemView>,
    /// Entries of the server's package list (Linux packages, or Windows
    /// updates).
    pub package_count: i64,
    pub warranty: Option<WarrantyView>,
    pub asset: AssetFacts,
}

/// The site of the server `id`, and its host name.
async fn server_site(state: &ApiState, id: Uuid) -> Result<Option<(Uuid, String)>, ApiError> {
    Ok(
        sqlx::query_as("SELECT site_id, hostname FROM servers WHERE id = $1")
            .bind(id)
            .fetch_optional(&state.pool)
            .await?,
    )
}

/// `GET /api/servers/{id}/inventory`.
pub async fn server(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<ServerInventory>, ApiError> {
    let id = parse_id(&id)?;
    let row = server_site(&state, id).await?;
    authorize(&user, row.as_ref().map(|r| r.0), false)?;
    let (_, hostname) = row.ok_or(ApiError::NotFound)?;
    type Dates = (DateTime<Utc>, DateTime<Utc>, DateTime<Utc>, DateTime<Utc>);
    let dates: Option<Dates> = sqlx::query_as(
        "SELECT collected_at, first_at, changed_at, checked_at FROM server_inventories
         WHERE server_id = $1",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?;
    let mut items: Vec<ItemView> =
        sqlx::query_as("SELECT kind, key, fields FROM server_inventory_items WHERE server_id = $1")
            .bind(id)
            .fetch_all(&state.pool)
            .await?;
    let kinds = monitorflow_common::inventory::KINDS;
    let order = |kind: &str| kinds.iter().position(|k| *k == kind).unwrap_or(kinds.len());
    items.sort_by(|a, b| {
        (order(&a.kind), a.key.to_lowercase()).cmp(&(order(&b.kind), b.key.to_lowercase()))
    });
    let package_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM server_packages WHERE server_id = $1")
            .bind(id)
            .fetch_one(&state.pool)
            .await?;
    let warranty: Option<(Option<NaiveDate>, Option<String>, DateTime<Utc>)> = sqlx::query_as(
        "SELECT ends_on, note, updated_at FROM server_warranties WHERE server_id = $1",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?;
    let asset = asset_facts(hostname, &items);
    Ok(Json(ServerInventory {
        reported: dates.map(
            |(collected_at, first_at, changed_at, checked_at)| InventoryDates {
                collected_at,
                first_at,
                changed_at,
                checked_at,
            },
        ),
        items,
        package_count,
        warranty: warranty.map(|(ends_on, note, updated_at)| WarrantyView {
            ends_on,
            note,
            updated_at,
        }),
        asset,
    }))
}

/// The asset facts of a server from its items: the hook the PSA's asset
/// sync starts from (ADR-0164, ADR-0187).
pub fn asset_facts(hostname: String, items: &[ItemView]) -> AssetFacts {
    let system = items.iter().find(|i| i.kind == "system");
    let field = |name: &str| {
        system
            .and_then(|s| s.fields.get(name))
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    AssetFacts {
        hostname,
        manufacturer: field("manufacturer"),
        model: field("model"),
        serial: field("serial").or_else(|| field("chassis_serial")),
    }
}

#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct ChangesParams {
    /// 1 to 500, default 100.
    pub limit: Option<u32>,
    /// `next_cursor` of the previous page.
    pub cursor: Option<String>,
}

#[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct ChangeView {
    pub id: i64,
    pub at: DateTime<Utc>,
    /// An item kind, or `package` for the package list.
    pub kind: String,
    pub key: String,
    /// `added`, `removed` or `changed`.
    pub change: String,
    /// The fields before; `null` when added.
    #[schema(value_type = Option<Object>)]
    pub before: Option<Value>,
    /// The fields after; `null` when removed.
    #[schema(value_type = Option<Object>)]
    pub after: Option<Value>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ChangeList {
    /// Newest first.
    pub changes: Vec<ChangeView>,
    pub next_cursor: Option<String>,
}

/// `GET /api/servers/{id}/inventory/changes`.
pub async fn changes(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    params: Result<Query<ChangesParams>, QueryRejection>,
) -> Result<Json<ChangeList>, ApiError> {
    let id = parse_id(&id)?;
    let row = server_site(&state, id).await?;
    authorize(&user, row.map(|r| r.0), false)?;
    let Query(params) = params.map_err(|e| ApiError::Invalid(e.body_text()))?;
    let limit = page_limit(params.limit, 100, 500)?;
    let before: Option<i64> = params.cursor.as_deref().map(decode_cursor).transpose()?;
    let mut changes: Vec<ChangeView> = sqlx::query_as(
        "SELECT id, at, kind, key, change, before, after FROM server_inventory_changes
         WHERE server_id = $1 AND ($2::bigint IS NULL OR id < $2)
         ORDER BY id DESC LIMIT $3",
    )
    .bind(id)
    .bind(before)
    .bind(i64::from(limit) + 1)
    .fetch_all(&state.pool)
    .await?;
    let next_cursor = if changes.len() > limit as usize {
        changes.truncate(limit as usize);
        changes.last().map(|c| encode_cursor(&c.id))
    } else {
        None
    };
    Ok(Json(ChangeList {
        changes,
        next_cursor,
    }))
}

/// `PUT /api/servers/{id}/warranty`: sets the warranty, or removes it
/// when both fields are `null`.
pub async fn set_warranty(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<Warranty>, JsonRejection>,
) -> Result<Json<Option<WarrantyView>>, ApiError> {
    let id = parse_id(&id)?;
    let row = server_site(&state, id).await?;
    authorize(&user, row.map(|r| r.0), true)?;
    let Json(mut warranty) = request.map_err(|e| ApiError::Invalid(e.body_text()))?;
    warranty.note = warranty
        .note
        .map(|n| n.trim().to_owned())
        .filter(|n| !n.is_empty());
    if warranty
        .note
        .as_ref()
        .is_some_and(|n| n.chars().count() > MAX_NOTE)
    {
        return Err(ApiError::Invalid(format!(
            "The note is at most {MAX_NOTE} characters."
        )));
    }
    let mut tx = state.pool.begin().await?;
    let view = if warranty.ends_on.is_none() && warranty.note.is_none() {
        sqlx::query("DELETE FROM server_warranties WHERE server_id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        None
    } else {
        let updated_at: DateTime<Utc> = sqlx::query_scalar(
            "INSERT INTO server_warranties (server_id, ends_on, note, updated_by)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (server_id) DO UPDATE SET ends_on = EXCLUDED.ends_on,
                 note = EXCLUDED.note, updated_by = EXCLUDED.updated_by, updated_at = now()
             RETURNING updated_at",
        )
        .bind(id)
        .bind(warranty.ends_on)
        .bind(&warranty.note)
        .bind(user.id)
        .fetch_one(&mut *tx)
        .await?;
        Some(WarrantyView {
            ends_on: warranty.ends_on,
            note: warranty.note.clone(),
            updated_at,
        })
    };
    let actor = user.id.to_string();
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action: "server.warranty",
            target: Some(("server", &id.to_string())),
            details: json!({ "ends_on": warranty.ends_on, "note": warranty.note }),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(Json(view))
}

#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct SearchParams {
    /// `software` (the default), `system`, `processor`, `memory`, `disk`,
    /// `nic`, `service`, `feature` or `licence`.
    pub kind: Option<String>,
    /// Text the name contains, case aside; for hardware, any field. At
    /// least 2 characters for `software`, unless `server_id` is given.
    pub q: Option<String>,
    /// `true`: the name is `q` exactly, case aside.
    pub exact: Option<bool>,
    /// Only versions strictly below this one, compared the way dpkg does.
    pub version_below: Option<String>,
    /// Only versions at or above this one.
    pub version_at_least: Option<String>,
    pub site_id: Option<Uuid>,
    pub server_id: Option<Uuid>,
    /// 1 to 5000, default 500.
    pub limit: Option<u32>,
    /// `json` (the default) or `csv`, a file to download with every match
    /// up to 100,000.
    pub format: Option<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct SearchResult {
    /// By host name, then name.
    pub hits: Vec<Hit>,
    /// More matched than `limit`, or than the 100,000 rows a search reads.
    pub truncated: bool,
}

/// `GET /api/inventory`: the fleet search.
pub async fn search(
    State(state): State<ApiState>,
    user: CurrentUser,
    params: Result<Query<SearchParams>, QueryRejection>,
) -> Result<Response, ApiError> {
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    let Query(params) = params.map_err(|e| ApiError::Invalid(e.body_text()))?;
    let kind = params.kind.unwrap_or_else(|| "software".into());
    if !KINDS.contains(&kind.as_str()) {
        return Err(ApiError::Invalid(format!(
            "The kind is one of {}.",
            KINDS.join(", ")
        )));
    }
    let text = params.q.unwrap_or_default().trim().to_owned();
    if kind == "software" && params.server_id.is_none() && text.chars().count() < 2 {
        return Err(ApiError::Invalid(
            "Searching software needs at least 2 characters in q.".into(),
        ));
    }
    if text.len() > 200 {
        return Err(ApiError::Invalid("q is at most 200 bytes.".into()));
    }
    let csv = match params.format.as_deref() {
        None | Some("json") => false,
        Some("csv") => true,
        Some(_) => return Err(ApiError::Invalid("The format is `json` or `csv`.".into())),
    };
    let limit = if csv {
        search::MAX_SCANNED as u32
    } else {
        page_limit(params.limit, 500, 5000)?
    };
    let version = |v: Option<String>| v.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
    let query = search::Query {
        kind,
        text,
        exact: params.exact.unwrap_or(false),
        below: version(params.version_below),
        at_least: version(params.version_at_least),
        site: params.site_id,
        server: params.server_id,
        scope: user.readable_sites(),
        limit: limit as usize,
    };
    let (hits, truncated) = search::run(&state.pool, &query).await?;
    if !csv {
        return Ok(Json(SearchResult { hits, truncated }).into_response());
    }
    let mut response = search::csv(&hits).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/csv; charset=utf-8"),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment; filename=\"inventory.csv\""),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_facts_come_from_the_system_item() {
        let items = vec![
            ItemView {
                kind: "disk".into(),
                key: "sda".into(),
                fields: json!({"serial": "DISK1"}),
            },
            ItemView {
                kind: "system".into(),
                key: "system".into(),
                fields: json!({"manufacturer": "Dell Inc.", "model": "PowerEdge R650",
                               "chassis_serial": "7XK2LM3"}),
            },
        ];
        let facts = asset_facts("web-1".into(), &items);
        assert_eq!(facts.manufacturer.as_deref(), Some("Dell Inc."));
        assert_eq!(facts.serial.as_deref(), Some("7XK2LM3"));
        assert_eq!(asset_facts("x".into(), &[]).model, None);
    }
}
