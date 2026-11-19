//! Lookups, MIB files and the names of SNMP objects for custom SNMP
//! checks (ADR-0157).
//!
//! Lookups and MIBs belong to a customer, like business services: anyone
//! who reads a site of the customer reads them (a check of the site uses
//! them), and operators and above of the whole customer change them.
//! Built-in lookups and the library of common objects are for everyone
//! and never change.

use std::collections::BTreeMap;

use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use monitorflow_snmp::custom::{
    oid_text, parse_oid, Lookup, LookupEntry, State as LookupState, ValueKind,
};
use monitorflow_snmp::{library, mib};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgConnection;
use uuid::Uuid;

use super::access::Scope;
use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::sensors::body;
use super::{ApiError, ApiState};

/// Largest body of an upload: 20 files of 1 MiB, written as JSON text.
pub const MAX_UPLOAD: usize = (mib::MAX_FILES + 1) * (mib::MAX_FILE + mib::MAX_FILE / 4);
/// Most objects one search returns.
const MAX_FOUND: usize = 200;

/// Query string of the lists.
#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct ListParams {
    /// Only those of this customer, and the built-in ones.
    pub customer_id: Option<Uuid>,
}

/// One lookup: built in, or a customer's own.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[schema(as = SnmpLookup)]
pub struct LookupView {
    /// `builtin:<name>`, or the lookup's ID.
    pub id: String,
    pub builtin: bool,
    /// `null` for a built-in lookup.
    pub customer_id: Option<Uuid>,
    pub name: String,
    #[schema(value_type = Vec<Object>)]
    pub entries: Vec<LookupEntry>,
    /// The state of a value no entry names: `ok`, `warning` or `critical`.
    #[schema(value_type = String)]
    pub otherwise: LookupState,
    /// How many checks name it.
    pub used_by: i64,
    pub updated_at: Option<DateTime<Utc>>,
    /// Whether this user may change or remove it.
    pub can_change: bool,
}

/// Body of `GET /api/snmp-lookups`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = SnmpLookupList)]
pub struct LookupList {
    /// The built-in ones first, then by name.
    pub lookups: Vec<LookupView>,
}

/// Body of `POST /api/snmp-lookups` and `PUT /api/snmp-lookups/{id}`.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[schema(as = SnmpLookupRequest)]
#[serde(deny_unknown_fields)]
pub struct LookupRequest {
    /// May be left out when there is only one customer; a change keeps it.
    #[serde(default)]
    pub customer_id: Option<Uuid>,
    /// 1 to 64 characters, unique per customer.
    pub name: String,
    /// 1 to 100 entries, each `{"value": 1, "name": "OK", "state": "ok"}`,
    /// a range `{"value": 10, "max": 19, ...}` or a text `{"text": "on", ...}`.
    #[schema(value_type = Vec<Object>)]
    pub entries: Vec<LookupEntry>,
    /// `warning` by default.
    #[serde(default)]
    #[schema(value_type = Option<String>)]
    pub otherwise: Option<LookupState>,
}

/// A customer for a change: the one named, or the only one; the user must
/// change it as a whole.
async fn customer_to_change(
    db: &mut PgConnection,
    user: &CurrentUser,
    named: Option<Uuid>,
) -> Result<Uuid, ApiError> {
    let customer = match named {
        Some(id) => id,
        None => {
            let ids: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM customers ORDER BY name")
                .fetch_all(&mut *db)
                .await?;
            match ids.as_slice() {
                [only] => *only,
                _ => return Err(ApiError::Invalid("Choose the customer.".into())),
            }
        }
    };
    if !readable_customers(db, user).await?.contains(customer) {
        return Err(ApiError::NotFound);
    }
    if !user.modifiable_customers().contains(customer) {
        return Err(ApiError::Forbidden);
    }
    Ok(customer)
}

/// Customers the user reads a site of, or is granted as a whole.
pub async fn readable_customers(
    db: &mut PgConnection,
    user: &CurrentUser,
) -> Result<Scope, ApiError> {
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    if user.visible_customers() == Scope::All || user.readable_sites() == Scope::All {
        return Ok(Scope::All);
    }
    let (_, sites) = user.readable_sites().bind();
    let mut ids: Vec<Uuid> =
        sqlx::query_scalar("SELECT DISTINCT customer_id FROM sites WHERE id = ANY($1)")
            .bind(&sites)
            .fetch_all(&mut *db)
            .await?;
    if let Scope::Only(more) = user.visible_customers() {
        ids.extend(more);
    }
    ids.sort();
    ids.dedup();
    Ok(Scope::Only(ids))
}

fn filter(params: &ListParams, scope: &Scope) -> Result<(), ApiError> {
    match params.customer_id {
        Some(c) if !scope.contains(c) => Err(ApiError::NotFound),
        _ => Ok(()),
    }
}

/// How many checks of devices in `sites` name each lookup, by reference.
/// Built-in lookups are shared by every customer: a person counts only
/// the checks they may read.
async fn usage(db: &mut PgConnection, sites: &Scope) -> Result<BTreeMap<String, i64>, sqlx::Error> {
    let (all, ids) = sites.bind();
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT p.value, count(*)
         FROM sensor_configs c LEFT JOIN servers s ON s.id = c.server_id,
              jsonb_each_text(c.params) p
         WHERE c.kind = 'snmp_custom' AND p.key ~ '^lookup[0-9]+$'
           AND ($1 OR coalesce(c.site_id, s.site_id) = ANY($2))
         GROUP BY p.value",
    )
    .bind(all)
    .bind(&ids)
    .fetch_all(&mut *db)
    .await?;
    Ok(rows.into_iter().collect())
}

fn builtin_views(used: &BTreeMap<String, i64>) -> Vec<LookupView> {
    library::builtin_lookups()
        .into_iter()
        .map(|b| LookupView {
            id: b.id.to_owned(),
            builtin: true,
            customer_id: None,
            name: b.name.to_owned(),
            entries: b.lookup.entries,
            otherwise: b.lookup.otherwise,
            used_by: used.get(b.id).copied().unwrap_or(0),
            updated_at: None,
            can_change: false,
        })
        .collect()
}

type LookupRow = (Uuid, Uuid, String, serde_json::Value, DateTime<Utc>);

fn own_view(user: &CurrentUser, used: &BTreeMap<String, i64>, row: LookupRow) -> LookupView {
    let (id, customer, name, definition, updated_at) = row;
    let lookup: Lookup = serde_json::from_value(definition).unwrap_or_default();
    LookupView {
        used_by: used.get(&id.to_string()).copied().unwrap_or(0),
        id: id.to_string(),
        builtin: false,
        customer_id: Some(customer),
        name,
        entries: lookup.entries,
        otherwise: lookup.otherwise,
        updated_at: Some(updated_at),
        can_change: user.modifiable_customers().contains(customer),
    }
}

/// `GET /api/snmp-lookups`.
pub async fn list_lookups(
    State(state): State<ApiState>,
    user: CurrentUser,
    params: Result<Query<ListParams>, QueryRejection>,
) -> Result<Json<LookupList>, ApiError> {
    let Query(params) = params.map_err(|r| ApiError::Invalid(r.body_text()))?;
    let mut conn = state.pool.acquire().await?;
    let scope = readable_customers(&mut conn, &user).await?;
    filter(&params, &scope)?;
    let (all, ids) = scope.bind();
    let rows: Vec<LookupRow> = sqlx::query_as(
        "SELECT id, customer_id, name, definition, updated_at FROM snmp_lookups
         WHERE ($1 OR customer_id = ANY($2)) AND ($3::uuid IS NULL OR customer_id = $3)
         ORDER BY lower(name), id",
    )
    .bind(all)
    .bind(&ids)
    .bind(params.customer_id)
    .fetch_all(&mut *conn)
    .await?;
    let used = usage(&mut conn, &user.readable_sites()).await?;
    let mut lookups = builtin_views(&used);
    lookups.extend(rows.into_iter().map(|r| own_view(&user, &used, r)));
    Ok(Json(LookupList { lookups }))
}

async fn own_row(
    db: &mut PgConnection,
    user: &CurrentUser,
    id: Uuid,
) -> Result<LookupRow, ApiError> {
    let row: Option<LookupRow> = sqlx::query_as(
        "SELECT id, customer_id, name, definition, updated_at FROM snmp_lookups WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&mut *db)
    .await?;
    let row = row.ok_or(ApiError::NotFound)?;
    if !readable_customers(db, user).await?.contains(row.1) {
        return Err(ApiError::NotFound);
    }
    Ok(row)
}

/// `GET /api/snmp-lookups/{id}`: a built-in one too.
pub async fn get_lookup(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<LookupView>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let used = usage(&mut conn, &user.readable_sites()).await?;
    if id.starts_with("builtin:") {
        readable_customers(&mut conn, &user).await?;
        return builtin_views(&used)
            .into_iter()
            .find(|l| l.id == id)
            .map(Json)
            .ok_or(ApiError::NotFound);
    }
    let id = Uuid::parse_str(&id).map_err(|_| ApiError::NotFound)?;
    let row = own_row(&mut conn, &user, id).await?;
    Ok(Json(own_view(&user, &used, row)))
}

fn checked(request: &LookupRequest) -> Result<(String, Lookup), ApiError> {
    let name = request.name.trim().to_owned();
    if name.is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
        return Err(ApiError::Invalid(
            "Give the lookup a name of 1 to 64 characters.".into(),
        ));
    }
    let lookup = Lookup {
        entries: request.entries.clone(),
        otherwise: request.otherwise.unwrap_or(LookupState::Warning),
    };
    lookup.validate().map_err(ApiError::Invalid)?;
    Ok((name, lookup))
}

fn conflict(error: sqlx::Error, name: &str) -> ApiError {
    match &error {
        sqlx::Error::Database(e) if e.constraint() == Some("snmp_lookups_name_key") => {
            ApiError::Conflict(format!("There is already a lookup named {name}."))
        }
        _ => error.into(),
    }
}

async fn record(
    db: &mut PgConnection,
    user: &CurrentUser,
    action: &str,
    target: (&str, String),
    details: serde_json::Value,
) -> Result<(), sqlx::Error> {
    let actor = user.id.to_string();
    audit::record(
        &mut *db,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action,
            target: Some((target.0, &target.1)),
            details,
        },
    )
    .await
}

/// `POST /api/snmp-lookups`.
pub async fn create_lookup(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<LookupRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<LookupView>), ApiError> {
    let request = body(request)?;
    let mut tx = state.pool.begin().await?;
    let customer = customer_to_change(&mut tx, &user, request.customer_id).await?;
    let (name, lookup) = checked(&request)?;
    let definition = serde_json::to_value(&lookup).map_err(|e| ApiError::Invalid(e.to_string()))?;
    let row: LookupRow = sqlx::query_as(
        "INSERT INTO snmp_lookups (customer_id, name, definition, created_by) VALUES ($1, $2, $3, $4)
         RETURNING id, customer_id, name, definition, updated_at",
    )
    .bind(customer)
    .bind(&name)
    .bind(&definition)
    .bind(user.id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| conflict(e, &name))?;
    let details = json!({"customer_id": customer, "name": name, "entries": lookup.entries.len()});
    record(
        &mut tx,
        &user,
        "snmp_lookup.create",
        ("snmp_lookup", row.0.to_string()),
        details,
    )
    .await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(own_view(&user, &BTreeMap::new(), row)),
    ))
}

/// `PUT /api/snmp-lookups/{id}`: a built-in one cannot change.
pub async fn update_lookup(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<LookupRequest>, JsonRejection>,
) -> Result<Json<LookupView>, ApiError> {
    if id.starts_with("builtin:") {
        return Err(ApiError::Invalid(
            "A built-in lookup cannot change: make one of your own.".into(),
        ));
    }
    let id = Uuid::parse_str(&id).map_err(|_| ApiError::NotFound)?;
    let request = body(request)?;
    let mut tx = state.pool.begin().await?;
    let before = own_row(&mut tx, &user, id).await?;
    if request.customer_id.is_some_and(|c| c != before.1) {
        return Err(ApiError::Invalid(
            "A lookup stays with its customer.".into(),
        ));
    }
    customer_to_change(&mut tx, &user, Some(before.1)).await?;
    let (name, lookup) = checked(&request)?;
    let definition = serde_json::to_value(&lookup).map_err(|e| ApiError::Invalid(e.to_string()))?;
    let row: LookupRow = sqlx::query_as(
        "UPDATE snmp_lookups SET name = $2, definition = $3, updated_at = now() WHERE id = $1
         RETURNING id, customer_id, name, definition, updated_at",
    )
    .bind(id)
    .bind(&name)
    .bind(&definition)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| conflict(e, &name))?;
    let details = json!({"before": {"name": before.2}, "after": {"name": name, "entries": lookup.entries.len()}});
    record(
        &mut tx,
        &user,
        "snmp_lookup.update",
        ("snmp_lookup", id.to_string()),
        details,
    )
    .await?;
    let used = usage(&mut tx, &user.readable_sites()).await?;
    tx.commit().await?;
    Ok(Json(own_view(&user, &used, row)))
}

/// `DELETE /api/snmp-lookups/{id}`: 409 while a check names it.
pub async fn delete_lookup(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    if id.starts_with("builtin:") {
        return Err(ApiError::Invalid(
            "A built-in lookup cannot be removed.".into(),
        ));
    }
    let id = Uuid::parse_str(&id).map_err(|_| ApiError::NotFound)?;
    let mut tx = state.pool.begin().await?;
    let before = own_row(&mut tx, &user, id).await?;
    customer_to_change(&mut tx, &user, Some(before.1)).await?;
    // Locks the row, so a check naming it cannot slip in meanwhile.
    sqlx::query("SELECT 1 FROM snmp_lookups WHERE id = $1 FOR UPDATE")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let used = usage(&mut tx, &Scope::All)
        .await?
        .get(&id.to_string())
        .copied()
        .unwrap_or(0);
    if used > 0 {
        return Err(ApiError::Conflict(format!(
            "{used} check(s) use this lookup: change them first."
        )));
    }
    sqlx::query("DELETE FROM snmp_lookups WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    record(
        &mut tx,
        &user,
        "snmp_lookup.delete",
        ("snmp_lookup", id.to_string()),
        json!({"name": before.2}),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// One uploaded MIB module.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = SnmpMib)]
pub struct MibView {
    pub id: Uuid,
    pub customer_id: Uuid,
    /// Its module name, like `TEST-NAS-MIB`.
    pub module: String,
    pub file_name: String,
    pub objects: i64,
    pub uploaded_at: DateTime<Utc>,
    pub can_change: bool,
}

/// Body of `GET /api/snmp-mibs`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = SnmpMibList)]
pub struct MibList {
    /// By module name.
    pub mibs: Vec<MibView>,
}

/// One file of an upload.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[schema(as = SnmpMibFile)]
#[serde(deny_unknown_fields)]
pub struct MibFile {
    /// Its file name, for the list and errors.
    pub name: String,
    /// The file's text, at most 1 MiB.
    pub text: String,
}

/// Body of `POST /api/snmp-mibs`.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[schema(as = SnmpMibUpload)]
#[serde(deny_unknown_fields)]
pub struct MibUpload {
    #[serde(default)]
    pub customer_id: Option<Uuid>,
    /// 1 to 20 files; modules that import one another may come together.
    pub files: Vec<MibFile>,
}

/// What an upload added: each module, and the enumerations it offers as
/// lookups.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = SnmpMibUploaded)]
pub struct Uploaded {
    pub mibs: Vec<MibView>,
    /// For "Make a lookup": the objects with named values, with the states
    /// their names suggest.
    pub offered_lookups: Vec<OfferedLookup>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = SnmpOfferedLookup)]
pub struct OfferedLookup {
    /// The object or type it comes from, like `raidStatus`.
    pub name: String,
    pub module: String,
    #[schema(value_type = Vec<Object>)]
    pub entries: Vec<LookupEntry>,
}

type MibRow = (Uuid, Uuid, String, String, i64, DateTime<Utc>);

const MIB_SELECT: &str = "SELECT m.id, m.customer_id, m.module, m.file_name,
        (SELECT count(*) FROM snmp_mib_objects o WHERE o.mib_id = m.id), m.uploaded_at
    FROM snmp_mibs m";

fn mib_view(user: &CurrentUser, r: MibRow) -> MibView {
    MibView {
        can_change: user.modifiable_customers().contains(r.1),
        id: r.0,
        customer_id: r.1,
        module: r.2,
        file_name: r.3,
        objects: r.4,
        uploaded_at: r.5,
    }
}

/// `GET /api/snmp-mibs`.
pub async fn list_mibs(
    State(state): State<ApiState>,
    user: CurrentUser,
    params: Result<Query<ListParams>, QueryRejection>,
) -> Result<Json<MibList>, ApiError> {
    let Query(params) = params.map_err(|r| ApiError::Invalid(r.body_text()))?;
    let mut conn = state.pool.acquire().await?;
    let scope = readable_customers(&mut conn, &user).await?;
    filter(&params, &scope)?;
    let (all, ids) = scope.bind();
    // SQL-safe: interpolates only the constant MIB_SELECT.
    let rows: Vec<MibRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{MIB_SELECT} WHERE ($1 OR m.customer_id = ANY($2)) AND ($3::uuid IS NULL OR m.customer_id = $3)
         ORDER BY m.module, m.id"
    )))
    .bind(all)
    .bind(&ids)
    .bind(params.customer_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(Json(MibList {
        mibs: rows.into_iter().map(|r| mib_view(&user, r)).collect(),
    }))
}

/// The modules a customer uploaded, as the parser knows them.
async fn known(db: &mut PgConnection, customer: Uuid) -> Result<mib::Known, sqlx::Error> {
    let modules: Vec<(Uuid, String, serde_json::Value)> =
        sqlx::query_as("SELECT id, module, types FROM snmp_mibs WHERE customer_id = $1")
            .bind(customer)
            .fetch_all(&mut *db)
            .await?;
    let objects: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT o.mib_id, o.name, o.oid FROM snmp_mib_objects o JOIN snmp_mibs m ON m.id = o.mib_id
         WHERE m.customer_id = $1",
    )
    .bind(customer)
    .fetch_all(&mut *db)
    .await?;
    let mut known = mib::Known::default();
    for (id, module, types) in modules {
        let entry = known.modules.entry(module).or_default();
        entry.types = serde_json::from_value(types).unwrap_or_default();
        for (_, name, oid) in objects.iter().filter(|o| o.0 == id) {
            if let Some(arcs) = parse_oid(oid) {
                entry.nodes.insert(name.clone(), arcs);
            }
        }
    }
    Ok(known)
}

fn safe_file_name(name: &str) -> String {
    let clean: String = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .chars()
        .filter(|c| !c.is_control())
        .take(255)
        .collect();
    if clean.trim().is_empty() {
        "upload.mib".into()
    } else {
        clean
    }
}

/// `POST /api/snmp-mibs`: reads the files and stores what they name,
/// replacing modules of the same names.
pub async fn upload_mibs(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<MibUpload>, JsonRejection>,
) -> Result<(StatusCode, Json<Uploaded>), ApiError> {
    let request = body(request)?;
    if request.files.is_empty() {
        return Err(ApiError::Invalid("Choose at least one MIB file.".into()));
    }
    let mut tx = state.pool.begin().await?;
    let customer = customer_to_change(&mut tx, &user, request.customer_id).await?;
    let files: Vec<(String, String)> = request
        .files
        .iter()
        .map(|f| (safe_file_name(&f.name), f.text.clone()))
        .collect();
    let known = known(&mut tx, customer).await?;
    // Parsing is bounded but takes processor time: off the runtime.
    let parsed = tokio::task::spawn_blocking(move || mib::parse(&files, &known))
        .await
        .map_err(|e| ApiError::Invalid(format!("the files could not be read: {e}")))?;
    let modules = parsed.map_err(|e| match e {
        mib::MibError::Missing(missing) => ApiError::Invalid(format!(
            "Upload these modules too, in the same upload or before: {}.",
            missing
                .iter()
                .map(|(module, names)| format!(
                    "{module} (for {})",
                    names.iter().take(3).cloned().collect::<Vec<_>>().join(", ")
                ))
                .collect::<Vec<_>>()
                .join("; ")
        )),
        other => ApiError::Invalid(other.to_string()),
    })?;
    let mut ids = Vec::new();
    let mut offered = Vec::new();
    for module in &modules {
        sqlx::query("DELETE FROM snmp_mibs WHERE customer_id = $1 AND module = $2")
            .bind(customer)
            .bind(&module.name)
            .execute(&mut *tx)
            .await?;
        let types =
            serde_json::to_value(&module.types).map_err(|e| ApiError::Invalid(e.to_string()))?;
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO snmp_mibs (customer_id, module, file_name, types, uploaded_by)
             VALUES ($1, $2, $3, $4, $5) RETURNING id",
        )
        .bind(customer)
        .bind(&module.name)
        .bind(&module.file)
        .bind(types)
        .bind(user.id)
        .fetch_one(&mut *tx)
        .await?;
        for chunk in module.objects.chunks(2000) {
            let column =
                |f: fn(&mib::Object) -> Option<String>| chunk.iter().map(f).collect::<Vec<_>>();
            let names: Vec<String> = chunk.iter().map(|o| o.name.clone()).collect();
            let oids: Vec<String> = chunk.iter().map(|o| oid_text(&o.oid)).collect();
            let kinds: Vec<String> = chunk
                .iter()
                .map(|o| {
                    serde_json::to_value(o.kind)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_owned))
                        .unwrap_or_default()
                })
                .collect();
            let enums: Vec<serde_json::Value> =
                chunk.iter().map(|o| json!(o.enumeration)).collect();
            let descriptions: Vec<String> = chunk.iter().map(|o| o.description.clone()).collect();
            sqlx::query(
                "INSERT INTO snmp_mib_objects (mib_id, name, oid, kind, syntax, enumeration, units, access, description)
                 SELECT $1, * FROM unnest($2::text[], $3::text[], $4::text[], $5::text[], $6::jsonb[],
                                          $7::text[], $8::text[], $9::text[])
                 ON CONFLICT (mib_id, name) DO NOTHING",
            )
            .bind(id)
            .bind(&names)
            .bind(&oids)
            .bind(&kinds)
            .bind(column(|o| o.syntax.clone().map(|s| s.chars().take(128).collect())))
            .bind(&enums)
            .bind(column(|o| o.units.clone()))
            .bind(column(|o| o.access.clone().map(|s| s.chars().take(64).collect())))
            .bind(&descriptions)
            .execute(&mut *tx)
            .await?;
        }
        for object in module.objects.iter().filter(|o| !o.enumeration.is_empty()) {
            offered.push(OfferedLookup {
                name: object.name.clone(),
                module: module.name.clone(),
                entries: library::lookup_from(&object.enumeration).entries,
            });
        }
        let details = json!({"customer_id": customer, "module": module.name, "file_name": module.file,
                             "objects": module.objects.len()});
        record(
            &mut tx,
            &user,
            "snmp_mib.upload",
            ("snmp_mib", id.to_string()),
            details,
        )
        .await?;
        ids.push(id);
    }
    // SQL-safe: interpolates only the constant MIB_SELECT.
    let rows: Vec<MibRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{MIB_SELECT} WHERE m.id = ANY($1) ORDER BY m.module"
    )))
    .bind(&ids)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    offered.truncate(MAX_FOUND);
    Ok((
        StatusCode::CREATED,
        Json(Uploaded {
            mibs: rows.into_iter().map(|r| mib_view(&user, r)).collect(),
            offered_lookups: offered,
        }),
    ))
}

/// `DELETE /api/snmp-mibs/{id}`.
pub async fn delete_mib(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let id = Uuid::parse_str(&id).map_err(|_| ApiError::NotFound)?;
    let mut tx = state.pool.begin().await?;
    let row: Option<(Uuid, String)> =
        sqlx::query_as("SELECT customer_id, module FROM snmp_mibs WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let (customer, module) = row.ok_or(ApiError::NotFound)?;
    if !readable_customers(&mut tx, &user).await?.contains(customer) {
        return Err(ApiError::NotFound);
    }
    customer_to_change(&mut tx, &user, Some(customer)).await?;
    sqlx::query("DELETE FROM snmp_mibs WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    record(
        &mut tx,
        &user,
        "snmp_mib.delete",
        ("snmp_mib", id.to_string()),
        json!({"module": module}),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Query string of `GET /api/snmp-objects`.
#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct ObjectParams {
    /// Names of this customer's MIBs, and the library; every readable
    /// customer's when left out.
    pub customer_id: Option<Uuid>,
    /// A part of a name (without case), or the start of an OID.
    pub q: Option<String>,
    /// Up to 50 OIDs separated by commas, each named by the object it is
    /// or is an instance of (`ifInOctets` names `1.3.6.1.2.1.2.2.1.10.3`).
    pub oids: Option<String>,
}

/// What a channel reading an object should be.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[schema(as = SnmpChannelSuggestion)]
pub struct Suggestion {
    /// `gauge`, `counter` or `text`.
    #[schema(value_type = String)]
    pub r#type: ValueKind,
    pub unit: String,
    pub multiply: f64,
    pub divide: f64,
    /// A built-in lookup, when one fits.
    pub lookup: Option<String>,
    /// For a column: its table and the column naming the rows.
    pub table: Option<String>,
    pub name_column: Option<String>,
}

/// One named object.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[schema(as = SnmpObject)]
pub struct ObjectView {
    pub name: String,
    /// The MIB module, or the vendor for the library.
    pub module: String,
    /// `library` or `mib`.
    pub source: &'static str,
    pub oid: String,
    /// `node`, `scalar`, `table`, `entry`, `column`, `notification`; the
    /// library's objects are `scalar` or `column`.
    pub kind: String,
    pub syntax: Option<String>,
    /// `[[1, "normal"], [2, "failed"]]`.
    #[schema(value_type = Vec<Object>)]
    pub enumeration: Vec<(i64, String)>,
    pub units: Option<String>,
    pub description: String,
    pub suggestion: Suggestion,
}

/// Body of `GET /api/snmp-objects`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = SnmpObjectList)]
pub struct ObjectList {
    /// For `q`: at most 200, the library first. For `oids`: one per OID
    /// named, keyed by the OID asked.
    pub objects: Vec<ObjectView>,
    /// For `oids`: the OID asked, then the name with the instance, like
    /// `ifInOctets.3`.
    pub names: BTreeMap<String, String>,
}

fn library_view(o: &library::LibraryObject) -> ObjectView {
    ObjectView {
        name: o.name.to_owned(),
        module: format!("{} ({})", o.vendor, o.module),
        source: "library",
        oid: o.oid.to_owned(),
        kind: if o.table.is_some() {
            "column"
        } else {
            "scalar"
        }
        .to_owned(),
        syntax: None,
        enumeration: Vec::new(),
        units: (!o.unit.is_empty()).then(|| o.unit.to_owned()),
        description: o.about.to_owned(),
        suggestion: Suggestion {
            r#type: o.kind,
            unit: o.unit.to_owned(),
            multiply: o.multiply,
            divide: o.divide,
            lookup: o.lookup.map(str::to_owned),
            table: o.table.map(str::to_owned),
            name_column: o.name_column.map(str::to_owned),
        },
    }
}

type ObjectRow = (
    String,
    String,
    String,
    String,
    Option<String>,
    serde_json::Value,
    Option<String>,
    String,
);

fn mib_object_view(
    row: ObjectRow,
    columns: &BTreeMap<String, (String, Option<String>)>,
) -> ObjectView {
    let (name, module, oid, kind, syntax, enumeration, units, description) = row;
    let enumeration: Vec<(i64, String)> = serde_json::from_value(enumeration).unwrap_or_default();
    let object = mib::Object {
        name: name.clone(),
        oid: parse_oid(&oid).unwrap_or_default(),
        kind: mib::ObjectKind::Node,
        syntax: syntax.clone(),
        enumeration: enumeration.clone(),
        units: units.clone(),
        access: None,
        description: String::new(),
    };
    let (r#type, unit) = mib::suggest(&object);
    let (table, name_column) = match columns.get(&oid) {
        Some((table, name_column)) => (Some(table.clone()), name_column.clone()),
        None => (None, None),
    };
    ObjectView {
        name,
        module,
        source: "mib",
        oid,
        kind,
        syntax,
        enumeration,
        units,
        description,
        suggestion: Suggestion {
            r#type,
            unit,
            multiply: 1.0,
            divide: 1.0,
            lookup: None,
            table,
            name_column,
        },
    }
}

/// For columns among `rows`: their table (the entry's OID) and the first
/// text column of the same entry, a likely name for the rows.
async fn table_of_columns(
    db: &mut PgConnection,
    scope: &Scope,
    rows: &[ObjectRow],
) -> Result<BTreeMap<String, (String, Option<String>)>, sqlx::Error> {
    let (all, ids) = scope.bind();
    let mut out = BTreeMap::new();
    for row in rows.iter().filter(|r| r.3 == "column") {
        let Some(entry) = row.2.rsplit_once('.').map(|(e, _)| e.to_owned()) else {
            continue;
        };
        let name: Option<String> = sqlx::query_scalar(
            "SELECT o.oid FROM snmp_mib_objects o JOIN snmp_mibs m ON m.id = o.mib_id
             WHERE ($2 OR m.customer_id = ANY($3)) AND o.oid LIKE $1 || '.%' AND o.kind = 'column'
               AND (o.syntax ILIKE '%String%' OR o.syntax = 'SnmpAdminString')
             ORDER BY length(o.oid), o.oid LIMIT 1",
        )
        .bind(&entry)
        .bind(all)
        .bind(&ids)
        .fetch_optional(&mut *db)
        .await?;
        out.insert(row.2.clone(), (entry, name));
    }
    Ok(out)
}

const OBJECT_SELECT: &str =
    "SELECT o.name, m.module, o.oid, o.kind, o.syntax, o.enumeration, o.units, o.description
    FROM snmp_mib_objects o JOIN snmp_mibs m ON m.id = o.mib_id";

/// `GET /api/snmp-objects`: the names the library and the customers' MIBs
/// give to OIDs.
pub async fn objects(
    State(state): State<ApiState>,
    user: CurrentUser,
    params: Result<Query<ObjectParams>, QueryRejection>,
) -> Result<Json<ObjectList>, ApiError> {
    let Query(params) = params.map_err(|r| ApiError::Invalid(r.body_text()))?;
    let mut conn = state.pool.acquire().await?;
    let scope = readable_customers(&mut conn, &user).await?;
    if let Some(c) = params.customer_id {
        if !scope.contains(c) {
            return Err(ApiError::NotFound);
        }
    }
    let scope = match params.customer_id {
        Some(c) => Scope::Only(vec![c]),
        None => scope,
    };
    let (all, ids) = scope.bind();
    match (params.q.as_deref().map(str::trim), params.oids.as_deref()) {
        (Some(q), None) if !q.is_empty() && q.len() <= 128 => {
            let lower = q.to_ascii_lowercase();
            let mut objects: Vec<ObjectView> = library::library()
                .iter()
                .filter(|o| {
                    o.name.to_ascii_lowercase().contains(&lower)
                        || o.oid.starts_with(q)
                        || o.about.to_ascii_lowercase().contains(&lower)
                })
                .map(library_view)
                .collect();
            let like = format!("%{}%", lower.replace(['%', '_', '\\'], ""));
            // SQL-safe: interpolates only the constant OBJECT_SELECT.
            let rows: Vec<ObjectRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "{OBJECT_SELECT} WHERE ($1 OR m.customer_id = ANY($2))
                   AND (lower(o.name) LIKE $3 OR o.oid LIKE $4 || '%')
                 ORDER BY length(o.name), o.name LIMIT {MAX_FOUND}"
            )))
            .bind(all)
            .bind(&ids)
            .bind(&like)
            .bind(q.trim_start_matches('.'))
            .fetch_all(&mut *conn)
            .await?;
            let columns = table_of_columns(&mut conn, &scope, &rows).await?;
            objects.extend(rows.into_iter().map(|r| mib_object_view(r, &columns)));
            objects.truncate(MAX_FOUND);
            Ok(Json(ObjectList {
                objects,
                names: BTreeMap::new(),
            }))
        }
        (None, Some(oids)) => {
            let asked: Vec<Vec<u32>> = oids
                .split(',')
                .map(|o| {
                    parse_oid(o).ok_or_else(|| {
                        ApiError::Invalid("`oids` are numeric OIDs separated by commas.".into())
                    })
                })
                .collect::<Result<_, _>>()?;
            if asked.len() > 50 {
                return Err(ApiError::Invalid("Name at most 50 OIDs at once.".into()));
            }
            let names = name_oids(&mut conn, &scope, &asked).await?;
            Ok(Json(ObjectList {
                objects: Vec::new(),
                names,
            }))
        }
        _ => Err(ApiError::Invalid(
            "Search with `q` (1 to 128 characters) or name `oids`, not both.".into(),
        )),
    }
}

/// Names each of `oids` by the longest known OID it starts with, the rest
/// as an instance: `ifInOctets.3`. OIDs nothing names are left out.
pub async fn name_oids(
    db: &mut PgConnection,
    scope: &Scope,
    oids: &[Vec<u32>],
) -> Result<BTreeMap<String, String>, sqlx::Error> {
    let mut prefixes: Vec<String> = oids
        .iter()
        .flat_map(|o| (2..=o.len()).map(|n| oid_text(&o[..n])))
        .collect();
    prefixes.sort();
    prefixes.dedup();
    let (all, ids) = scope.bind();
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT o.oid, min(o.name) FROM snmp_mib_objects o JOIN snmp_mibs m ON m.id = o.mib_id
         WHERE ($1 OR m.customer_id = ANY($2)) AND o.oid = ANY($3) GROUP BY o.oid",
    )
    .bind(all)
    .bind(&ids)
    .bind(&prefixes)
    .fetch_all(&mut *db)
    .await?;
    // The customer's MIBs first, then the library, then the standard roots:
    // a name the customer uploaded wins over one MonitorFlow knows.
    let mibs: BTreeMap<Vec<u32>, String> = rows
        .into_iter()
        .filter_map(|(oid, name)| Some((parse_oid(&oid)?, name)))
        .collect();
    let library: BTreeMap<Vec<u32>, String> = library::library()
        .iter()
        .filter_map(|o| Some((parse_oid(o.oid)?, o.name.trim_end_matches(".0").to_owned())))
        .collect();
    let roots: BTreeMap<Vec<u32>, String> = mib::base()
        .modules
        .values()
        .flat_map(|m| &m.nodes)
        .map(|(n, o)| (o.clone(), n.clone()))
        .collect();
    Ok(oids
        .iter()
        .filter_map(|oid| {
            // Roots shorter than `internet` (1.3.6.1) name nothing useful.
            let (n, name) = [&mibs, &library, &roots].into_iter().find_map(|known| {
                let n = (4..=oid.len())
                    .rev()
                    .find(|n| known.contains_key(&oid[..*n]))?;
                Some((n, known[&oid[..n]].clone()))
            })?;
            let name = &name;
            let rest = &oid[n..];
            let full = if rest.is_empty() {
                name.clone()
            } else {
                format!("{name}.{}", oid_text(rest).trim_start_matches('.'))
            };
            Some((oid_text(oid), full))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_lose_their_folders() {
        assert_eq!(safe_file_name("C:\\mibs\\NAS-MIB.txt"), "NAS-MIB.txt");
        assert_eq!(safe_file_name("../../etc/x.mib"), "x.mib");
        assert_eq!(safe_file_name("a\u{0}b"), "ab");
        assert_eq!(safe_file_name("/"), "upload.mib");
    }

    #[test]
    fn library_objects_suggest_their_channel() {
        let raid = library::library()
            .into_iter()
            .find(|o| o.name == "raidStatus")
            .unwrap();
        let view = library_view(&raid);
        assert_eq!(view.kind, "column");
        assert_eq!(
            view.suggestion.table.as_deref(),
            Some("1.3.6.1.4.1.6574.3.1.1")
        );
        assert!(view.module.starts_with("Synology"));
    }

    #[test]
    fn lookups_are_checked() {
        let request = |name: &str, entries: serde_json::Value| LookupRequest {
            customer_id: None,
            name: name.into(),
            entries: serde_json::from_value(entries).unwrap(),
            otherwise: None,
        };
        let ok = checked(&request(
            " Power ",
            json!([{"value": 1, "name": "On", "state": "ok"}]),
        ))
        .unwrap();
        assert_eq!(ok.0, "Power");
        assert_eq!(ok.1.otherwise, LookupState::Warning);
        assert!(checked(&request(
            "",
            json!([{"value": 1, "name": "On", "state": "ok"}])
        ))
        .is_err());
        assert!(checked(&request("x", json!([]))).is_err());
    }
}
