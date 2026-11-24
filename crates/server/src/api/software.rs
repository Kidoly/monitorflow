//! The software catalogue (ADR-0188): what campaigns and commands may
//! install on servers or remove from them. An entry is shared by every
//! customer (`customer_id` null), which only global administrators
//! change, or a customer's own, which its administrators change. Anyone
//! who sees a customer sees the shared entries and that customer's.

use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use monitorflow_common::proto::v1::PackageManager;
use monitorflow_common::software::{self, parse_action, MAX_PACKAGES};
use monitorflow_common::third_party;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgExecutor;
use uuid::Uuid;

use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::notifications::is_global_admin;
use super::sensors::{body, parse_id};
use super::{ApiError, ApiState};
use crate::campaign::{SoftwareItem, SoftwareSpec};

/// One entry of the catalogue.
#[derive(Debug, Clone, Serialize, sqlx::FromRow, utoipa::ToSchema)]
#[schema(as = SoftwareEntry)]
pub struct Entry {
    pub id: Uuid,
    /// The customer it belongs to; `null` when every customer shares it.
    pub customer_id: Option<Uuid>,
    /// What people call it, like `7-Zip`.
    pub name: String,
    /// `winget`, the only manager for now.
    pub manager: String,
    /// The manager's package ID, like `7zip.7zip`.
    pub package_id: String,
    /// The winget source it installs from, like `winget`.
    pub source: String,
    pub description: String,
    /// The user who added it; `null` once that user is deleted.
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Body of `GET /api/software`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = SoftwareCatalog)]
pub struct Catalog {
    /// Sorted by name.
    pub software: Vec<Entry>,
}

/// Body of `POST /api/software` and `PUT /api/software/{id}`.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[schema(as = SoftwareEntryRequest)]
#[serde(deny_unknown_fields)]
pub struct EntryRequest {
    /// The customer it belongs to; absent or `null` to share it with
    /// every customer, which takes a global administrator. Fixed once
    /// made.
    pub customer_id: Option<Uuid>,
    /// 1 to 128 characters.
    pub name: String,
    /// `winget`, the default.
    #[serde(default = "winget")]
    pub manager: String,
    /// A winget package ID: 2 to 8 segments joined by `.`, like
    /// `7zip.7zip`.
    pub package_id: String,
    /// The winget source, `winget` by default; never `msstore`.
    #[serde(default = "winget")]
    pub source: String,
    /// Up to 500 characters.
    #[serde(default)]
    pub description: String,
}

fn winget() -> String {
    "winget".into()
}

/// Query of `GET /api/software`.
#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct ListParams {
    /// Only the shared entries and this customer's.
    pub customer_id: Option<Uuid>,
}

/// What a campaign or a command installs or removes, by catalogue entry.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, utoipa::ToSchema)]
#[schema(as = SoftwareRequest)]
#[serde(deny_unknown_fields)]
pub struct SoftwareRequest {
    /// `install` or `remove`.
    pub action: String,
    /// Entries of the catalogue shared or of the customer, 1 to 100.
    pub catalog_ids: Vec<Uuid>,
}

impl EntryRequest {
    fn validate(&self) -> Result<(), ApiError> {
        let invalid = |m: &str| Err(ApiError::Invalid(m.into()));
        let name = self.name.trim();
        if name.is_empty() || name.chars().count() > 128 || name.chars().any(char::is_control) {
            return invalid("name must be 1 to 128 characters");
        }
        if third_party::parse(&self.manager) != Some(PackageManager::Winget) {
            return invalid("manager must be `winget`");
        }
        if !third_party::valid_id(PackageManager::Winget, &self.package_id) {
            return invalid("package_id is not a winget package ID, like `7zip.7zip`");
        }
        if !software::valid_source(&self.source) {
            return invalid("source must be a winget source name other than `msstore`");
        }
        if self.description.chars().count() > 500 {
            return invalid("description is at most 500 characters");
        }
        Ok(())
    }
}

const COLUMNS: &str = "id, customer_id, name, manager, package_id, source, description, \
     created_by, created_at, updated_at";

/// Whether `user` may change entries of `customer`: its administrators,
/// or global ones for shared entries.
fn authorize_change(user: &CurrentUser, customer: Option<Uuid>) -> Result<(), ApiError> {
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    match customer {
        None if is_global_admin(user) => Ok(()),
        None => Err(ApiError::Forbidden),
        Some(id) if !user.visible_customers().contains(id) => Err(ApiError::NotFound),
        Some(id) if user.administered_customers().contains(id) => Ok(()),
        Some(_) => Err(ApiError::Forbidden),
    }
}

/// The entry `id` if `user` sees it.
async fn visible(state: &ApiState, user: &CurrentUser, id: &str) -> Result<Entry, ApiError> {
    let id = parse_id(id)?;
    // SQL-safe: interpolates only the constant COLUMNS.
    let entry: Option<Entry> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM software_catalog WHERE id = $1"
    )))
    .bind(id)
    .fetch_optional(&state.pool)
    .await?;
    let entry = entry.ok_or(ApiError::NotFound)?;
    match entry.customer_id {
        Some(c) if !user.visible_customers().contains(c) => Err(ApiError::NotFound),
        _ if !user.can_read() => Err(ApiError::NotFound),
        _ => Ok(entry),
    }
}

/// A unique violation names the package taken already.
fn taken(error: sqlx::Error) -> ApiError {
    match &error {
        sqlx::Error::Database(e) if e.is_unique_violation() => {
            ApiError::Conflict("this package is in the catalogue already for that customer".into())
        }
        _ => error.into(),
    }
}

/// `GET /api/software`.
pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
    params: Result<Query<ListParams>, QueryRejection>,
) -> Result<Json<Catalog>, ApiError> {
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    let Query(params) = params.map_err(|e| ApiError::Invalid(e.body_text()))?;
    let (all, customers) = user.visible_customers().bind();
    // SQL-safe: interpolates only the constant COLUMNS.
    let software: Vec<Entry> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM software_catalog
         WHERE (customer_id IS NULL OR $1 OR customer_id = ANY($2))
           AND ($3::uuid IS NULL OR customer_id IS NULL OR customer_id = $3)
         ORDER BY lower(name), package_id, id"
    )))
    .bind(all)
    .bind(&customers)
    .bind(params.customer_id)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(Catalog { software }))
}

/// `POST /api/software`.
pub async fn create(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<EntryRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<Entry>), ApiError> {
    let request = body(request)?;
    authorize_change(&user, request.customer_id)?;
    request.validate()?;
    let mut tx = state.pool.begin().await?;
    // SQL-safe: interpolates only the constant COLUMNS.
    let entry: Entry = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "INSERT INTO software_catalog
             (customer_id, name, manager, package_id, source, description, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         RETURNING {COLUMNS}"
    )))
    .bind(request.customer_id)
    .bind(request.name.trim())
    .bind(&request.manager)
    .bind(&request.package_id)
    .bind(&request.source)
    .bind(request.description.trim())
    .bind(user.id)
    .fetch_one(&mut *tx)
    .await
    .map_err(taken)?;
    record(&mut *tx, &user, "software.create", &entry).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(entry)))
}

/// `PUT /api/software/{id}`: the whole entry again; its customer stays.
pub async fn update(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<EntryRequest>, JsonRejection>,
) -> Result<Json<Entry>, ApiError> {
    let current = visible(&state, &user, &id).await?;
    authorize_change(&user, current.customer_id)?;
    let request = body(request)?;
    if request.customer_id != current.customer_id {
        return Err(ApiError::Invalid(
            "customer_id cannot change: add the entry again for the other customer".into(),
        ));
    }
    request.validate()?;
    let mut tx = state.pool.begin().await?;
    // SQL-safe: interpolates only the constant COLUMNS.
    let entry: Entry = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "UPDATE software_catalog SET name = $2, manager = $3, package_id = $4, source = $5,
             description = $6, updated_at = now()
         WHERE id = $1
         RETURNING {COLUMNS}"
    )))
    .bind(current.id)
    .bind(request.name.trim())
    .bind(&request.manager)
    .bind(&request.package_id)
    .bind(&request.source)
    .bind(request.description.trim())
    .fetch_one(&mut *tx)
    .await
    .map_err(taken)?;
    record(&mut *tx, &user, "software.update", &entry).await?;
    tx.commit().await?;
    Ok(Json(entry))
}

/// `DELETE /api/software/{id}`. Campaigns and commands made from it keep
/// what they resolved.
pub async fn delete(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let current = visible(&state, &user, &id).await?;
    authorize_change(&user, current.customer_id)?;
    let mut tx = state.pool.begin().await?;
    sqlx::query("DELETE FROM software_catalog WHERE id = $1")
        .bind(current.id)
        .execute(&mut *tx)
        .await?;
    record(&mut *tx, &user, "software.delete", &current).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn record(
    db: impl PgExecutor<'_>,
    user: &CurrentUser,
    action: &str,
    entry: &Entry,
) -> Result<(), sqlx::Error> {
    audit::record(
        db,
        Event {
            actor_kind: "user",
            actor_id: &user.id.to_string(),
            action,
            target: Some(("software", &entry.id.to_string())),
            details: json!({
                "customer_id": entry.customer_id,
                "name": entry.name,
                "manager": entry.manager,
                "package_id": entry.package_id,
                "source": entry.source,
            }),
        },
    )
    .await
}

/// What `request` installs or removes on servers of `customer`, resolved
/// from the catalogue now: entries shared or of that customer, each once.
pub async fn resolve(
    db: impl PgExecutor<'_>,
    customer: Uuid,
    request: &SoftwareRequest,
) -> Result<SoftwareSpec, ApiError> {
    if parse_action(&request.action).is_none() {
        return Err(ApiError::Invalid(
            "software.action must be `install` or `remove`".into(),
        ));
    }
    if request.catalog_ids.is_empty() || request.catalog_ids.len() > MAX_PACKAGES {
        return Err(ApiError::Invalid(format!(
            "software.catalog_ids takes 1 to {MAX_PACKAGES} entries"
        )));
    }
    let rows: Vec<(Uuid, String, String, String, String)> = sqlx::query_as(
        "SELECT id, name, manager, package_id, source FROM software_catalog
         WHERE id = ANY($1) AND (customer_id IS NULL OR customer_id = $2)",
    )
    .bind(&request.catalog_ids)
    .bind(customer)
    .fetch_all(db)
    .await?;
    let mut packages = Vec::new();
    for id in &request.catalog_ids {
        let Some((catalog_id, name, manager, package_id, source)) =
            rows.iter().find(|r| r.0 == *id).cloned()
        else {
            return Err(ApiError::Invalid(format!("unknown catalogue entry {id}")));
        };
        if packages
            .iter()
            .any(|p: &SoftwareItem| p.catalog_id == catalog_id)
        {
            return Err(ApiError::Invalid(format!(
                "catalogue entry {id} is listed twice"
            )));
        }
        packages.push(SoftwareItem {
            catalog_id,
            name,
            manager,
            id: package_id,
            source,
        });
    }
    let spec = SoftwareSpec {
        action: request.action.clone(),
        packages,
    };
    spec.validate().map_err(ApiError::Invalid)?;
    Ok(spec)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(value: serde_json::Value) -> EntryRequest {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn entries_default_to_winget_and_are_checked() {
        let ok = request(json!({"name": "7-Zip", "package_id": "7zip.7zip"}));
        assert_eq!(
            (ok.manager.as_str(), ok.source.as_str()),
            ("winget", "winget")
        );
        assert!(ok.validate().is_ok());
        for bad in [
            json!({"name": "", "package_id": "7zip.7zip"}),
            json!({"name": "x", "package_id": "7zip"}),
            json!({"name": "x", "package_id": "7zip.7zip", "manager": "snap"}),
            json!({"name": "x", "package_id": "7zip.7zip", "source": "msstore"}),
            json!({"name": "x", "package_id": "7zip.7zip", "description": "d".repeat(501)}),
        ] {
            assert!(request(bad.clone()).validate().is_err(), "{bad}");
        }
        assert!(
            serde_json::from_value::<EntryRequest>(json!({"name": "x", "package_id": "a.b",
            "version": "1"}))
            .is_err()
        );
    }
}
