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
use crate::campaign::{MsiItem, MsiPropertyItem, SoftwareItem, SoftwareSpec, MSI};
use crate::software_files::{read_product, Product, Received, UploadError, MAX_TOTAL_BYTES};

/// One entry of the catalogue.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[schema(as = SoftwareEntry)]
pub struct Entry {
    pub id: Uuid,
    /// The customer it belongs to; `null` when every customer shares it.
    pub customer_id: Option<Uuid>,
    /// What people call it, like `7-Zip`.
    pub name: String,
    /// `winget` or `msi` (Windows), `apt` (Debian and Ubuntu) or `dnf`
    /// (RHEL and its derivatives).
    pub manager: String,
    /// The manager's package ID, like `7zip.7zip`, the distribution's
    /// package name, like `nginx`, or for `msi` the ProductCode, like
    /// `{6F1C2E7A-3B9D-4C55-A0E2-8D4F7B1A9C30}`.
    pub package_id: String,
    /// The winget source it installs from, like `winget`; empty for apt
    /// and dnf.
    pub source: String,
    /// apt and dnf only: the mirror of the central server it installs
    /// from, which a campaign adds to the servers' sources first;
    /// `null` for the servers' own sources, or once the mirror is gone.
    pub mirror: Option<String>,
    /// `msi` only: the uploaded file; `null` for the other managers.
    pub file: Option<SoftwareFile>,
    /// `msi` only: public properties passed to `msiexec` at every
    /// install; empty for the other managers.
    pub properties: Vec<MsiPropertyItem>,
    pub description: String,
    /// The user who added it; `null` once that user is deleted.
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// An entry as stored.
#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    customer_id: Option<Uuid>,
    name: String,
    manager: String,
    package_id: String,
    source: String,
    mirror: Option<String>,
    file: Option<sqlx::types::Json<SoftwareFile>>,
    properties: sqlx::types::Json<Vec<MsiPropertyItem>>,
    description: String,
    created_by: Option<Uuid>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<Row> for Entry {
    fn from(row: Row) -> Self {
        Self {
            id: row.id,
            customer_id: row.customer_id,
            name: row.name,
            manager: row.manager,
            package_id: row.package_id,
            source: row.source,
            mirror: row.mirror,
            file: row.file.map(|f| f.0),
            properties: row.properties.0,
            description: row.description,
            created_by: row.created_by,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

/// An uploaded MSI file (ADR-0188), as `POST /api/software/files`
/// answers and an `msi` entry shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SoftwareFile {
    /// SHA-256 of the file, lower-case hex: what entries name it by.
    pub sha256: String,
    /// In bytes.
    pub size: u64,
    /// As the uploader named it; may be empty.
    pub file_name: String,
    /// Read from its Property table, upper case; `null` when it has none.
    pub product_code: Option<String>,
    pub product_name: String,
    pub product_version: String,
    pub manufacturer: String,
}

/// Query of `POST /api/software/files`.
#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct UploadParams {
    /// The file's name, for people only, like `tool-1.2.msi`.
    pub file_name: Option<String>,
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
    /// `winget`, the default, `msi`, `apt` or `dnf`.
    #[serde(default = "winget")]
    pub manager: String,
    /// For winget a package ID: 2 to 8 segments joined by `.`, like
    /// `7zip.7zip`. For apt and dnf a package name of the distribution:
    /// letters, digits and `.+_:-`, like `nginx`; never an option, a
    /// path, a glob or a group. For msi the ProductCode, an upper-case
    /// GUID between braces; absent or empty for the one read from the
    /// file.
    #[serde(default)]
    pub package_id: String,
    /// winget only: the source, `winget` by default; never `msstore`.
    /// Absent or empty for apt and dnf.
    #[serde(default)]
    pub source: Option<String>,
    /// apt and dnf only: the name of a mirror of the central server of
    /// the same family (ADR-0142) the package installs from. Absent for
    /// the servers' own sources.
    #[serde(default)]
    pub mirror: Option<String>,
    /// msi only: the SHA-256 of a file uploaded with `POST
    /// /api/software/files`, by the caller or named by an entry they see.
    #[serde(default)]
    pub file_sha256: Option<String>,
    /// msi only: up to 32 public properties passed to `msiexec`, each
    /// name once. Names are upper-case letters, digits and `_`, starting
    /// with a letter, never one that changes what msiexec does (`REBOOT`,
    /// `REMOVE`, `TRANSFORMS`, `PATCH`...); values are up to 256
    /// printable ASCII characters but `"` and `;`. They travel in the
    /// signed command, not on the mirror listeners.
    #[serde(default)]
    pub properties: Vec<MsiPropertyItem>,
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
    /// Checks the request; returns the source to store.
    fn validate(&self) -> Result<String, ApiError> {
        let invalid = |m: &str| Err(ApiError::Invalid(m.into()));
        let name = self.name.trim();
        if name.is_empty() || name.chars().count() > 128 || name.chars().any(char::is_control) {
            return invalid("name must be 1 to 128 characters");
        }
        if self.description.chars().count() > 500 {
            return invalid("description is at most 500 characters");
        }
        if self.manager != MSI && (self.file_sha256.is_some() || !self.properties.is_empty()) {
            return invalid("file_sha256 and properties are for msi packages");
        }
        if self.manager == MSI {
            return self.validate_msi().map(|()| String::new());
        }
        let manager =
            third_party::parse(&self.manager).filter(|m| software::is_software_manager(*m));
        let Some(manager) = manager else {
            return invalid("manager must be `winget`, `msi`, `apt` or `dnf`");
        };
        if manager == PackageManager::Winget {
            if !third_party::valid_id(manager, &self.package_id) {
                return invalid("package_id is not a winget package ID, like `7zip.7zip`");
            }
            let source = self.source.clone().unwrap_or_else(winget);
            if !software::valid_source(&source) {
                return invalid("source must be a winget source name other than `msstore`");
            }
            if self.mirror.is_some() {
                return invalid("mirror is for apt and dnf packages");
            }
            return Ok(source);
        }
        if !third_party::valid_id(manager, &self.package_id) {
            return invalid(
                "package_id is not a package name of the distribution: letters, digits and \
                 `.+_:-`, starting with a letter or digit, like `nginx`",
            );
        }
        if self.source.as_deref().is_some_and(|s| !s.is_empty()) {
            return invalid("source is for winget packages: apt and dnf install from the servers' sources or a mirror");
        }
        Ok(String::new())
    }

    fn validate_msi(&self) -> Result<(), ApiError> {
        let invalid = |m: &str| Err(ApiError::Invalid(m.into()));
        if !self
            .file_sha256
            .as_deref()
            .is_some_and(software::valid_sha256)
        {
            return invalid(
                "file_sha256 must be the SHA-256 of an uploaded file, 64 lower-case hex digits",
            );
        }
        if self.source.as_deref().is_some_and(|s| !s.is_empty()) || self.mirror.is_some() {
            return invalid("source and mirror are not for msi packages");
        }
        if !self.package_id.is_empty() && !software::valid_product_code(&self.package_id) {
            return invalid(
                "package_id must be a ProductCode, an upper-case GUID between braces, or empty \
                 for the file's own",
            );
        }
        if self.properties.len() > software::MAX_MSI_PROPERTIES {
            return invalid("at most 32 properties");
        }
        let mut names = std::collections::HashSet::new();
        for p in &self.properties {
            if !software::valid_msi_property(&p.name) {
                return Err(ApiError::Invalid(format!(
                    "`{}` is not a public property name, or is one MonitorFlow never sets",
                    p.name.escape_debug()
                )));
            }
            if !software::valid_msi_value(&p.value) {
                return Err(ApiError::Invalid(format!(
                    "the value of {} has a quote, a semicolon or a character that is not \
                     printable ASCII, or is over 256 characters",
                    p.name
                )));
            }
            if !names.insert(p.name.as_str()) {
                return Err(ApiError::Invalid(format!("{} is set twice", p.name)));
            }
        }
        Ok(())
    }
}

const COLUMNS: &str = "id, customer_id, name, manager, package_id, source, \
     (SELECT m.name FROM mirrors m WHERE m.id = mirror_id) AS mirror, \
     (SELECT jsonb_build_object('sha256', f.sha256, 'size', f.size, 'file_name', f.file_name, \
         'product_code', f.product_code, 'product_name', f.product_name, \
         'product_version', f.product_version, 'manufacturer', f.manufacturer) \
      FROM software_files f WHERE f.sha256 = file_sha256) AS file, \
     properties, description, created_by, created_at, updated_at";

/// The ID of the mirror `name` for packages of `manager`, refused unless
/// it exists, is of that family and is not being removed.
async fn mirror_id(
    db: impl PgExecutor<'_>,
    manager: &str,
    name: Option<&str>,
) -> Result<Option<Uuid>, ApiError> {
    let Some(name) = name else {
        return Ok(None);
    };
    let found: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM mirrors WHERE name = $1 AND family = $2 AND deleting_at IS NULL",
    )
    .bind(name)
    .bind(manager)
    .fetch_optional(db)
    .await?;
    found.map(Some).ok_or_else(|| {
        ApiError::Invalid(format!(
            "no {manager} mirror named `{}`",
            name.escape_debug()
        ))
    })
}

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
    let entry: Option<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM software_catalog WHERE id = $1"
    )))
    .bind(id)
    .fetch_optional(&state.pool)
    .await?;
    let entry = Entry::from(entry.ok_or(ApiError::NotFound)?);
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
    let software: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
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
    let software = software.into_iter().map(Entry::from).collect();
    Ok(Json(Catalog { software }))
}

/// The ProductCode of an msi entry of `request`, checking that `user` may
/// name its file: one they uploaded, or one an entry they see names
/// already. Locks the file's row; `None` for the other managers.
async fn msi_file(
    tx: &mut sqlx::PgConnection,
    user: &CurrentUser,
    request: &EntryRequest,
) -> Result<Option<String>, ApiError> {
    let Some(sha256) = request
        .file_sha256
        .as_deref()
        .filter(|_| request.manager == MSI)
    else {
        return Ok(None);
    };
    let (all, customers) = user.visible_customers().bind();
    let found: Option<(Option<String>, bool)> = sqlx::query_as(
        "SELECT f.product_code,
             $2 = ANY(f.uploaded_by) OR EXISTS (
                 SELECT 1 FROM software_catalog c WHERE c.file_sha256 = f.sha256
                   AND (c.customer_id IS NULL OR $3 OR c.customer_id = ANY($4)))
         FROM software_files f WHERE f.sha256 = $1 FOR UPDATE",
    )
    .bind(sha256)
    .bind(user.id)
    .bind(all)
    .bind(&customers)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((code, true)) = found else {
        return Err(ApiError::Invalid(
            "file_sha256 names no file you uploaded: upload it with POST /api/software/files"
                .into(),
        ));
    };
    if !request.package_id.is_empty() {
        return Ok(Some(request.package_id.clone()));
    }
    code.map(Some).ok_or_else(|| {
        ApiError::Invalid(
            "the file has no ProductCode in its Property table: give it as package_id".into(),
        )
    })
}

/// Marks `sha256` as named by nothing from now, for the sweep to check:
/// it clears the mark again while something still names it.
async fn released(db: impl PgExecutor<'_>, sha256: Option<&str>) -> Result<(), sqlx::Error> {
    if let Some(sha256) = sha256 {
        sqlx::query("UPDATE software_files SET unreferenced_since = now() WHERE sha256 = $1")
            .bind(sha256)
            .execute(db)
            .await?;
    }
    Ok(())
}

/// `POST /api/software/files`: an MSI file, as the request body. Kept
/// once by SHA-256; whoever uploads it may then add it to the catalogue.
pub async fn upload(
    State(state): State<ApiState>,
    user: CurrentUser,
    params: Result<Query<UploadParams>, QueryRejection>,
    file: axum::body::Body,
) -> Result<(StatusCode, Json<SoftwareFile>), ApiError> {
    let administers_some = match user.administered_customers() {
        crate::api::access::Scope::All => true,
        crate::api::access::Scope::Only(ids) => !ids.is_empty(),
    };
    if !user.can_read() || !(is_global_admin(&user) || administers_some) {
        return Err(ApiError::Forbidden);
    }
    let Query(params) = params.map_err(|e| ApiError::Invalid(e.body_text()))?;
    let file_name = clean_file_name(params.file_name.as_deref().unwrap_or_default());
    let store = state.settings.software_files.clone();
    let received = store.receive(file).await.map_err(|e| match e {
        UploadError::Io(error) => ApiError::Internal(format!("storing an MSI file: {error}")),
        other => ApiError::Invalid(other.to_string()),
    })?;
    let temp = received.temp.clone();
    let product = tokio::task::spawn_blocking(move || read_product(&temp))
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let product = match product {
        Ok(product) => product,
        Err(error) => {
            let _ = tokio::fs::remove_file(&received.temp).await;
            return Err(ApiError::Invalid(error));
        }
    };
    let stored = store_file(&state, &user, &store, &received, &product, &file_name).await;
    // Kept by `store_file`, or not wanted: either way the temporary file goes.
    let _ = tokio::fs::remove_file(&received.temp).await;
    stored.map(|file| (StatusCode::CREATED, Json(file)))
}

/// Records `received` and moves it into place, within the store's bound.
async fn store_file(
    state: &ApiState,
    user: &CurrentUser,
    store: &crate::software_files::Store,
    received: &Received,
    product: &Product,
    file_name: &str,
) -> Result<SoftwareFile, ApiError> {
    let mut tx = state.pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('software_files'))")
        .execute(&mut *tx)
        .await?;
    let size = i64::try_from(received.size).map_err(|_| ApiError::Invalid("too large".into()))?;
    let known: Option<String> =
        sqlx::query_scalar("SELECT file_name FROM software_files WHERE sha256 = $1")
            .bind(&received.sha256)
            .fetch_optional(&mut *tx)
            .await?;
    if known.is_some() {
        sqlx::query(
            "UPDATE software_files SET
                 uploaded_by = CASE WHEN $2 = ANY(uploaded_by) THEN uploaded_by
                                    ELSE array_append(uploaded_by, $2) END,
                 file_name = CASE WHEN file_name = '' THEN $3 ELSE file_name END
             WHERE sha256 = $1",
        )
        .bind(&received.sha256)
        .bind(user.id)
        .bind(file_name)
        .execute(&mut *tx)
        .await?;
    } else {
        let total: i64 =
            sqlx::query_scalar("SELECT coalesce(sum(size), 0)::bigint FROM software_files")
                .fetch_one(&mut *tx)
                .await?;
        if u64::try_from(total).unwrap_or(0) + received.size > MAX_TOTAL_BYTES {
            return Err(ApiError::Conflict(format!(
                "the central server keeps at most {} GiB of MSI files; remove entries nothing \
                 needs, and their files go 8 days later",
                MAX_TOTAL_BYTES >> 30
            )));
        }
        sqlx::query(
            "INSERT INTO software_files (sha256, size, product_code, product_name,
                 product_version, manufacturer, file_name, uploaded_by)
             VALUES ($1, $2, $3, $4, $5, $6, $7, ARRAY[$8]::uuid[])",
        )
        .bind(&received.sha256)
        .bind(size)
        .bind(&product.product_code)
        .bind(&product.product_name)
        .bind(&product.product_version)
        .bind(&product.manufacturer)
        .bind(file_name)
        .bind(user.id)
        .execute(&mut *tx)
        .await?;
        let (store, kept) = (store.clone(), received.clone());
        tokio::task::spawn_blocking(move || store.keep(&kept))
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?
            .map_err(|e| ApiError::Internal(format!("storing an MSI file: {e}")))?;
    }
    let file: SoftwareFile = sqlx::query_scalar::<_, sqlx::types::Json<SoftwareFile>>(
        "SELECT jsonb_build_object('sha256', sha256, 'size', size, 'file_name', file_name,
             'product_code', product_code, 'product_name', product_name,
             'product_version', product_version, 'manufacturer', manufacturer)
         FROM software_files WHERE sha256 = $1",
    )
    .bind(&received.sha256)
    .fetch_one(&mut *tx)
    .await?
    .0;
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "user",
            actor_id: &user.id.to_string(),
            action: "software.upload",
            target: Some(("software_file", &file.sha256)),
            details: json!({
                "size": file.size,
                "file_name": file_name,
                "product_code": file.product_code,
                "product_name": file.product_name,
                "product_version": file.product_version,
            }),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(file)
}

/// The last component of `name`, without control characters, at most 255
/// characters.
fn clean_file_name(name: &str) -> String {
    name.rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .take(255)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// `POST /api/software`.
pub async fn create(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<EntryRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<Entry>), ApiError> {
    let request = body(request)?;
    authorize_change(&user, request.customer_id)?;
    let source = request.validate()?;
    let mut tx = state.pool.begin().await?;
    let mirror = mirror_id(&mut *tx, &request.manager, request.mirror.as_deref()).await?;
    let code = msi_file(&mut tx, &user, &request).await?;
    let package_id = code.as_deref().unwrap_or(&request.package_id);
    let file = code.as_ref().and(request.file_sha256.as_deref());
    // SQL-safe: interpolates only the constant COLUMNS.
    let entry: Row = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "INSERT INTO software_catalog
             (customer_id, name, manager, package_id, source, description, created_by, mirror_id,
              file_sha256, properties)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
         RETURNING {COLUMNS}"
    )))
    .bind(request.customer_id)
    .bind(request.name.trim())
    .bind(&request.manager)
    .bind(package_id)
    .bind(&source)
    .bind(request.description.trim())
    .bind(user.id)
    .bind(mirror)
    .bind(file)
    .bind(sqlx::types::Json(&request.properties))
    .fetch_one(&mut *tx)
    .await
    .map_err(taken)?;
    let entry = Entry::from(entry);
    if let Some(sha256) = file {
        sqlx::query("UPDATE software_files SET unreferenced_since = NULL WHERE sha256 = $1")
            .bind(sha256)
            .execute(&mut *tx)
            .await?;
    }
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
    let source = request.validate()?;
    let mut tx = state.pool.begin().await?;
    let mirror = mirror_id(&mut *tx, &request.manager, request.mirror.as_deref()).await?;
    let code = msi_file(&mut tx, &user, &request).await?;
    let package_id = code.as_deref().unwrap_or(&request.package_id);
    let file = code.as_ref().and(request.file_sha256.as_deref());
    // SQL-safe: interpolates only the constant COLUMNS.
    let entry: Row = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "UPDATE software_catalog SET name = $2, manager = $3, package_id = $4, source = $5,
             description = $6, mirror_id = $7, file_sha256 = $8, properties = $9,
             updated_at = now()
         WHERE id = $1
         RETURNING {COLUMNS}"
    )))
    .bind(current.id)
    .bind(request.name.trim())
    .bind(&request.manager)
    .bind(package_id)
    .bind(&source)
    .bind(request.description.trim())
    .bind(mirror)
    .bind(file)
    .bind(sqlx::types::Json(&request.properties))
    .fetch_one(&mut *tx)
    .await
    .map_err(taken)?;
    let entry = Entry::from(entry);
    let before = current.file.as_ref().map(|f| f.sha256.as_str());
    if before != file {
        released(&mut *tx, before).await?;
    }
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
    released(&mut *tx, current.file.as_ref().map(|f| f.sha256.as_str())).await?;
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
                "mirror": entry.mirror,
                "file_sha256": entry.file.as_ref().map(|f| &f.sha256),
                "properties": entry.properties.iter().map(|p| &p.name).collect::<Vec<_>>(),
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
    type Resolved = (
        Uuid,
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<i64>,
        sqlx::types::Json<Vec<MsiPropertyItem>>,
    );
    let rows: Vec<Resolved> = sqlx::query_as(
        "SELECT c.id, c.name, c.manager, c.package_id, c.source, m.name, f.sha256, f.size,
             c.properties
         FROM software_catalog c
         LEFT JOIN mirrors m ON m.id = c.mirror_id AND m.deleting_at IS NULL
         LEFT JOIN software_files f ON f.sha256 = c.file_sha256
         WHERE c.id = ANY($1) AND (c.customer_id IS NULL OR c.customer_id = $2)",
    )
    .bind(&request.catalog_ids)
    .bind(customer)
    .fetch_all(db)
    .await?;
    let mut packages = Vec::new();
    for id in &request.catalog_ids {
        let Some((catalog_id, name, manager, package_id, source, mirror, sha256, size, properties)) =
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
            mirror,
            msi: sha256.zip(size).map(|(sha256, size)| MsiItem {
                sha256,
                size: u64::try_from(size).unwrap_or(0),
                properties: properties.0,
            }),
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
        assert_eq!(ok.manager.as_str(), "winget");
        assert_eq!(ok.validate().unwrap(), "winget");
        let apt = request(
            json!({"name": "nginx", "manager": "apt", "package_id": "nginx",
                                 "mirror": "nginx-stable"}),
        );
        assert_eq!(apt.validate().unwrap(), "");
        let dnf = request(
            json!({"name": "httpd", "manager": "dnf", "package_id": "httpd",
                                 "source": ""}),
        );
        assert_eq!(dnf.validate().unwrap(), "");
        for bad in [
            json!({"name": "", "package_id": "7zip.7zip"}),
            json!({"name": "x", "package_id": "7zip"}),
            json!({"name": "x", "package_id": "7zip.7zip", "manager": "snap"}),
            json!({"name": "x", "package_id": "7zip.7zip", "source": "msstore"}),
            json!({"name": "x", "package_id": "7zip.7zip", "description": "d".repeat(501)}),
            json!({"name": "x", "package_id": "7zip.7zip", "mirror": "m"}),
            json!({"name": "x", "manager": "apt", "package_id": "-y"}),
            json!({"name": "x", "manager": "apt", "package_id": "/tmp/x.deb"}),
            json!({"name": "x", "manager": "dnf", "package_id": "@core"}),
            json!({"name": "x", "manager": "apt", "package_id": "nginx", "source": "winget"}),
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
