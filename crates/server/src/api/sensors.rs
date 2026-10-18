//! Sensor configuration endpoints (ADR-0016): the effective configuration
//! of a server, and the site and server rows of `sensor_configs`.
//!
//! Every role reads the sites it covers; operators and above change them.
//! A site or server the user cannot read is reported as not found, so the
//! API does not reveal which IDs exist. Rows are checked with the same
//! `monitorflow_sensors` rules and registry as the agent, so a row the API
//! accepts is one the agent runs. Changes bump the revision through the
//! schema's trigger, and connected agents get them at the next poll.

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use monitorflow_sensors::{registry, validate, SensorConfig};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::{ApiError, ApiState};
use crate::sensor_config::{self, Layer, Row, Spec};

/// Most parameters on one row.
pub const MAX_PARAMS: usize = 32;
/// Longest parameter name, in bytes.
pub const MAX_PARAM_NAME_LEN: usize = 64;
/// Longest parameter value once turned into the agent's string, in bytes.
pub const MAX_PARAM_VALUE_LEN: usize = 1024;

const UNIQUE_VIOLATION: &str = "23505";
const FOREIGN_KEY_VIOLATION: &str = "23503";
const CHECK_VIOLATION: &str = "23514";

/// Body of a create or an update. An update replaces every field.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntryRequest {
    pub kind: String,
    /// Defaults to `kind`.
    pub name: Option<String>,
    #[serde(default)]
    pub instance: String,
    pub interval_ms: u64,
    /// `null` or absent: the agent's default.
    pub timeout_ms: Option<u64>,
    /// A flat object of strings, numbers and booleans.
    #[serde(default)]
    pub params: Map<String, Value>,
    /// `false` removes the sensor of the same name and instance from the
    /// layers below.
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
}

fn enabled_by_default() -> bool {
    true
}

/// An [`EntryRequest`] that passed [`validate_entry`], in database types.
#[derive(Debug, Clone, PartialEq)]
pub struct ValidEntry {
    pub kind: String,
    pub name: String,
    pub instance: String,
    pub interval_ms: i64,
    pub timeout_ms: Option<i64>,
    pub params: Value,
    pub enabled: bool,
}

/// Checks `request` as the agent will: ADR-0012's name, instance,
/// interval and timeout rules, then the kind and its parameters with
/// [`registry::build`]. The error is a message for the client.
pub fn validate_entry(request: EntryRequest) -> Result<ValidEntry, String> {
    let name = request.name.unwrap_or_else(|| request.kind.clone());
    validate::sensor_name(&request.kind).map_err(|e| format!("kind: {e}"))?;
    validate::sensor_name(&name).map_err(|e| format!("name: {e}"))?;
    if request.params.len() > MAX_PARAMS {
        return Err(format!("at most {MAX_PARAMS} params allowed"));
    }
    for (key, value) in &request.params {
        if key.is_empty() || key.len() > MAX_PARAM_NAME_LEN || key.chars().any(char::is_control) {
            return Err(format!(
                "param names must be 1 to {MAX_PARAM_NAME_LEN} bytes without control characters"
            ));
        }
        let scalar = matches!(value, Value::String(_) | Value::Number(_) | Value::Bool(_));
        if !scalar {
            return Err(format!(
                "param `{key}` must be a string, a number or a boolean"
            ));
        }
    }
    let params = Value::Object(request.params);
    let config = SensorConfig {
        name: name.clone(),
        instance: request.instance.clone(),
        interval: std::time::Duration::from_millis(request.interval_ms),
        timeout: request.timeout_ms.map(std::time::Duration::from_millis),
        params: sensor_config::params(params.clone()),
    };
    config.validate().map_err(|e| e.to_string())?;
    if let Some((key, _)) = config
        .params
        .iter()
        .find(|(_, v)| v.len() > MAX_PARAM_VALUE_LEN)
    {
        return Err(format!(
            "param `{key}` must be at most {MAX_PARAM_VALUE_LEN} bytes"
        ));
    }
    registry::build(&request.kind, &config.params).map_err(|e| e.to_string())?;
    // Both fit: validate() bounds the interval to 24 h and the timeout by it.
    let millis = |ms: u64| i64::try_from(ms).unwrap_or(i64::MAX);
    Ok(ValidEntry {
        kind: request.kind,
        name,
        instance: request.instance,
        interval_ms: millis(request.interval_ms),
        timeout_ms: request.timeout_ms.map(millis),
        params,
        enabled: request.enabled,
    })
}

/// One row of `sensor_configs`. Exactly one of `site_id` and `server_id`
/// is set.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Entry {
    pub id: Uuid,
    pub site_id: Option<Uuid>,
    pub server_id: Option<Uuid>,
    pub kind: String,
    pub name: String,
    pub instance: String,
    pub interval_ms: i64,
    pub timeout_ms: Option<i64>,
    pub params: Value,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Body of the list endpoints, sorted by name and instance.
#[derive(Debug, Serialize)]
pub struct EntryList {
    pub entries: Vec<Entry>,
}

/// Which layer an effective sensor comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    /// The built-in defaults in server code.
    Default,
    Site,
    Server,
}

/// One sensor as the agent receives it.
#[derive(Debug, Serialize)]
pub struct EffectiveSensor {
    pub origin: Origin,
    /// The row it comes from; `null` for a built-in default.
    pub entry_id: Option<Uuid>,
    pub kind: String,
    pub name: String,
    pub instance: String,
    pub interval_ms: u64,
    pub timeout_ms: Option<u64>,
    /// As sent to the agent: every value is a string.
    pub params: std::collections::BTreeMap<String, String>,
}

/// Body of `GET /api/servers/{id}/sensors`.
#[derive(Debug, Serialize)]
pub struct Effective {
    pub server_id: Uuid,
    pub site_id: Uuid,
    /// Revision of the current configuration.
    pub revision: i64,
    /// Revision the agent last acknowledged, `null` if it never did.
    pub applied_revision: Option<i64>,
    pub applied_at: Option<DateTime<Utc>>,
    /// Whether the agent acknowledged the current revision.
    pub in_sync: bool,
    pub sensors: Vec<EffectiveSensor>,
}

const ENTRY_COLUMNS: &str = "id, site_id, server_id, kind, name, instance, interval_ms, \
                             timeout_ms, params, enabled, created_at, updated_at";

/// A path ID that is not a UUID names nothing.
fn parse_id(id: &str) -> Result<Uuid, ApiError> {
    id.parse().map_err(|_| ApiError::NotFound)
}

fn body<T>(body: Result<Json<T>, JsonRejection>) -> Result<T, ApiError> {
    body.map(|Json(b)| b)
        .map_err(|rejection| ApiError::Invalid(rejection.body_text()))
}

/// The site `site` if the user may read it (and change it, with
/// `modify`). Unreadable or missing gives 404, readable but not
/// changeable 403.
fn authorize(user: &CurrentUser, site: Option<Uuid>, modify: bool) -> Result<Uuid, ApiError> {
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    let site = site
        .filter(|s| user.can_read_site(*s))
        .ok_or(ApiError::NotFound)?;
    if modify && !user.can_modify_site(site) {
        return Err(ApiError::Forbidden);
    }
    Ok(site)
}

async fn site_exists(pool: &PgPool, site: Uuid) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar("SELECT id FROM sites WHERE id = $1")
        .bind(site)
        .fetch_optional(pool)
        .await
}

async fn site_of_server(pool: &PgPool, server: Uuid) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar("SELECT site_id FROM servers WHERE id = $1")
        .bind(server)
        .fetch_optional(pool)
        .await
}

/// Who owns a row: the site it applies to, or one server of a site.
#[derive(Debug, Clone, Copy)]
enum Owner {
    Site(Uuid),
    Server(Uuid),
}

impl Owner {
    fn columns(self) -> (Option<Uuid>, Option<Uuid>) {
        match self {
            Self::Site(id) => (Some(id), None),
            Self::Server(id) => (None, Some(id)),
        }
    }

    /// The site whose roles decide access, if the owner exists.
    async fn site(self, pool: &PgPool) -> Result<Option<Uuid>, sqlx::Error> {
        match self {
            Self::Site(id) => site_exists(pool, id).await,
            Self::Server(id) => site_of_server(pool, id).await,
        }
    }
}

async fn list(state: &ApiState, user: &CurrentUser, owner: Owner) -> Result<EntryList, ApiError> {
    authorize(user, owner.site(&state.pool).await?, false)?;
    let (site_id, server_id) = owner.columns();
    let entries = sqlx::query_as(&format!(
        "SELECT {ENTRY_COLUMNS} FROM sensor_configs
         WHERE site_id = $1 OR server_id = $2 ORDER BY name, instance"
    ))
    .bind(site_id)
    .bind(server_id)
    .fetch_all(&state.pool)
    .await?;
    Ok(EntryList { entries })
}

/// `GET /api/sites/{id}/sensor_configs`.
pub async fn list_site(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<EntryList>, ApiError> {
    Ok(Json(
        list(&state, &user, Owner::Site(parse_id(&id)?)).await?,
    ))
}

/// `GET /api/servers/{id}/sensor_configs`.
pub async fn list_server(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<EntryList>, ApiError> {
    Ok(Json(
        list(&state, &user, Owner::Server(parse_id(&id)?)).await?,
    ))
}

/// What the audit log keeps of a row. Parameter values are left out:
/// future sensors may take credentials as parameters.
fn audit_summary(entry: &Entry) -> Value {
    let param_names: Vec<&String> = entry
        .params
        .as_object()
        .map(|m| m.keys().collect())
        .unwrap_or_default();
    json!({
        "site_id": entry.site_id,
        "server_id": entry.server_id,
        "kind": entry.kind,
        "name": entry.name,
        "instance": entry.instance,
        "interval_ms": entry.interval_ms,
        "timeout_ms": entry.timeout_ms,
        "enabled": entry.enabled,
        "param_names": param_names,
    })
}

async fn record(
    db: &mut PgConnection,
    user: &CurrentUser,
    action: &str,
    entry: &Entry,
    details: Value,
) -> Result<(), sqlx::Error> {
    let actor_id = user.id.to_string();
    let target = entry.id.to_string();
    audit::record(
        db,
        Event {
            actor_kind: "user",
            actor_id: &actor_id,
            action,
            target: Some(("sensor_config", &target)),
            details,
        },
    )
    .await
}

/// Maps constraint violations to client errors.
fn write_error(error: sqlx::Error, entry: &ValidEntry) -> ApiError {
    let code = error
        .as_database_error()
        .and_then(|e| e.code())
        .map(|c| c.into_owned());
    match code.as_deref() {
        Some(UNIQUE_VIOLATION) => ApiError::Conflict(format!(
            "sensor `{}` instance `{}` is already configured here",
            entry.name, entry.instance
        )),
        // The owner was deleted meanwhile.
        Some(FOREIGN_KEY_VIOLATION) => ApiError::NotFound,
        Some(CHECK_VIOLATION) => ApiError::Invalid("sensor configuration refused".into()),
        _ => ApiError::Database(error),
    }
}

async fn create(
    state: &ApiState,
    user: &CurrentUser,
    owner: Owner,
    request: Result<Json<EntryRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<Entry>), ApiError> {
    authorize(user, owner.site(&state.pool).await?, true)?;
    let valid = validate_entry(body(request)?).map_err(ApiError::Invalid)?;
    let (site_id, server_id) = owner.columns();
    let mut tx = state.pool.begin().await?;
    let entry: Entry = sqlx::query_as(&format!(
        "INSERT INTO sensor_configs
             (site_id, server_id, kind, name, instance, interval_ms, timeout_ms, params, enabled)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         RETURNING {ENTRY_COLUMNS}"
    ))
    .bind(site_id)
    .bind(server_id)
    .bind(&valid.kind)
    .bind(&valid.name)
    .bind(&valid.instance)
    .bind(valid.interval_ms)
    .bind(valid.timeout_ms)
    .bind(&valid.params)
    .bind(valid.enabled)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| write_error(e, &valid))?;
    record(
        &mut tx,
        user,
        "sensor_config.create",
        &entry,
        audit_summary(&entry),
    )
    .await?;
    tx.commit().await?;
    tracing::info!(user = %user.id, entry = %entry.id, name = %entry.name, "sensor config created");
    Ok((StatusCode::CREATED, Json(entry)))
}

/// `POST /api/sites/{id}/sensor_configs`: 201 with the new [`Entry`].
pub async fn create_site(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<EntryRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<Entry>), ApiError> {
    create(&state, &user, Owner::Site(parse_id(&id)?), request).await
}

/// `POST /api/servers/{id}/sensor_configs`: 201 with the new [`Entry`].
pub async fn create_server(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<EntryRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<Entry>), ApiError> {
    create(&state, &user, Owner::Server(parse_id(&id)?), request).await
}

/// Locks the row `id` in `tx` after checking the user may change it.
async fn lock_entry(
    tx: &mut PgConnection,
    user: &CurrentUser,
    id: Uuid,
) -> Result<Entry, ApiError> {
    let entry: Option<Entry> = sqlx::query_as(&format!(
        "SELECT {ENTRY_COLUMNS} FROM sensor_configs WHERE id = $1 FOR UPDATE"
    ))
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let site = match &entry {
        Some(Entry {
            server_id: Some(server),
            ..
        }) => {
            sqlx::query_scalar("SELECT site_id FROM servers WHERE id = $1")
                .bind(server)
                .fetch_optional(&mut *tx)
                .await?
        }
        Some(entry) => entry.site_id,
        None => None,
    };
    authorize(user, site, true)?;
    entry.ok_or(ApiError::NotFound)
}

/// `PUT /api/sensor_configs/{id}`: replaces the row; 200 with the
/// updated [`Entry`].
pub async fn update(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<EntryRequest>, JsonRejection>,
) -> Result<Json<Entry>, ApiError> {
    let id = parse_id(&id)?;
    let mut tx = state.pool.begin().await?;
    let before = lock_entry(&mut tx, &user, id).await?;
    let valid = validate_entry(body(request)?).map_err(ApiError::Invalid)?;
    let entry: Entry = sqlx::query_as(&format!(
        "UPDATE sensor_configs
         SET kind = $2, name = $3, instance = $4, interval_ms = $5, timeout_ms = $6,
             params = $7, enabled = $8, updated_at = now()
         WHERE id = $1
         RETURNING {ENTRY_COLUMNS}"
    ))
    .bind(id)
    .bind(&valid.kind)
    .bind(&valid.name)
    .bind(&valid.instance)
    .bind(valid.interval_ms)
    .bind(valid.timeout_ms)
    .bind(&valid.params)
    .bind(valid.enabled)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| write_error(e, &valid))?;
    let details = json!({ "before": audit_summary(&before), "after": audit_summary(&entry) });
    record(&mut tx, &user, "sensor_config.update", &entry, details).await?;
    tx.commit().await?;
    tracing::info!(user = %user.id, entry = %entry.id, "sensor config updated");
    Ok(Json(entry))
}

/// `DELETE /api/sensor_configs/{id}`: 204.
pub async fn delete(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let id = parse_id(&id)?;
    let mut tx = state.pool.begin().await?;
    let entry = lock_entry(&mut tx, &user, id).await?;
    sqlx::query("DELETE FROM sensor_configs WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    record(
        &mut tx,
        &user,
        "sensor_config.delete",
        &entry,
        audit_summary(&entry),
    )
    .await?;
    tx.commit().await?;
    tracing::info!(user = %user.id, entry = %id, "sensor config deleted");
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/servers/{id}/sensors`: the configuration the agent runs, with
/// the origin of each sensor and the drift between applied and current.
pub async fn effective(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<Effective>, ApiError> {
    let server_id = parse_id(&id)?;
    // One snapshot, so the revision matches the rows.
    let mut tx = state.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await?;
    type Revisions = (Uuid, i64, Option<i64>, Option<DateTime<Utc>>);
    let server: Option<Revisions> = sqlx::query_as(
        "SELECT s.site_id, si.sensor_revision + s.sensor_revision,
                s.sensor_revision_applied, s.sensor_applied_at
         FROM servers s JOIN sites si ON si.id = s.site_id
         WHERE s.id = $1",
    )
    .bind(server_id)
    .fetch_optional(&mut *tx)
    .await?;
    let site_id = authorize(&user, server.map(|s| s.0), false)?;
    let (_, revision, applied_revision, applied_at) = server.ok_or(ApiError::NotFound)?;
    let entries: Vec<Entry> = sqlx::query_as(&format!(
        "SELECT {ENTRY_COLUMNS} FROM sensor_configs WHERE site_id = $1 OR server_id = $2"
    ))
    .bind(site_id)
    .bind(server_id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(Effective {
        server_id,
        site_id,
        revision,
        applied_revision,
        applied_at,
        in_sync: applied_revision == Some(revision),
        sensors: effective_sensors(&entries),
    }))
}

/// Resolves `entries` with [`sensor_config::resolve`], the function the
/// push uses, then names the row each sensor comes from: the server's
/// row for its key if any, else the site's, else a built-in default.
fn effective_sensors(entries: &[Entry]) -> Vec<EffectiveSensor> {
    let rows = entries
        .iter()
        .map(|e| Row {
            layer: if e.server_id.is_some() {
                Layer::Server
            } else {
                Layer::Site
            },
            spec: Spec {
                kind: e.kind.clone(),
                name: e.name.clone(),
                instance: e.instance.clone(),
                interval_ms: u64::try_from(e.interval_ms).unwrap_or(0),
                timeout_ms: e.timeout_ms.map(|t| u64::try_from(t).unwrap_or(0)),
                params: sensor_config::params(e.params.clone()),
            },
            enabled: e.enabled,
        })
        .collect();
    sensor_config::resolve(rows)
        .into_iter()
        .map(|spec| {
            let of = |server: bool| {
                entries.iter().find(|e| {
                    e.server_id.is_some() == server
                        && e.name == spec.name
                        && e.instance == spec.instance
                })
            };
            let (origin, entry_id) = match (of(true), of(false)) {
                (Some(e), _) => (Origin::Server, Some(e.id)),
                (None, Some(e)) => (Origin::Site, Some(e.id)),
                (None, None) => (Origin::Default, None),
            };
            EffectiveSensor {
                origin,
                entry_id,
                kind: spec.kind,
                name: spec.name,
                instance: spec.instance,
                interval_ms: spec.interval_ms,
                timeout_ms: spec.timeout_ms,
                params: spec.params,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(value: Value) -> Result<ValidEntry, String> {
        validate_entry(serde_json::from_value(value).unwrap())
    }

    #[test]
    fn valid_entries_get_defaults() {
        let entry = request(json!({"kind": "cpu", "interval_ms": 30_000})).unwrap();
        assert_eq!(entry.name, "cpu");
        assert_eq!(entry.instance, "");
        assert_eq!(entry.timeout_ms, None);
        assert_eq!(entry.params, json!({}));
        assert!(entry.enabled);
        let entry = request(json!({
            "kind": "disk", "name": "disk_data", "instance": "/data", "interval_ms": 10_000,
            "timeout_ms": 5_000, "params": {"mounts_exclude": "/boot"}, "enabled": false,
        }))
        .unwrap();
        assert_eq!(entry.name, "disk_data");
        assert_eq!(entry.timeout_ms, Some(5_000));
        assert!(!entry.enabled);
    }

    #[test]
    fn invalid_entries_say_why() {
        let cases = [
            (
                json!({"kind": "gpu", "interval_ms": 1_000}),
                "unknown sensor kind `gpu`",
            ),
            (
                json!({"kind": "Cpu", "interval_ms": 1_000}),
                "kind: sensor name `Cpu`",
            ),
            (
                json!({"kind": "cpu", "name": "cpu-1", "interval_ms": 1_000}),
                "name: sensor name `cpu-1`",
            ),
            (json!({"kind": "cpu", "interval_ms": 999}), "interval"),
            (
                json!({"kind": "cpu", "interval_ms": 86_400_001u64}),
                "interval",
            ),
            (
                json!({"kind": "cpu", "interval_ms": 1_000, "timeout_ms": 2_000}),
                "timeout",
            ),
            (
                json!({"kind": "cpu", "interval_ms": 1_000, "timeout_ms": 0}),
                "timeout",
            ),
            (
                json!({"kind": "cpu", "interval_ms": 1_000, "instance": "a\nb"}),
                "instance must be",
            ),
            (
                json!({"kind": "cpu", "interval_ms": 1_000, "params": {"per_cores": true}}),
                "unknown parameter `per_cores`",
            ),
            (
                json!({"kind": "cpu", "interval_ms": 1_000, "params": {"per_core": "maybe"}}),
                "`per_core` must be `true` or `false`",
            ),
            (
                json!({"kind": "cpu", "interval_ms": 1_000, "params": {"per_core": [true]}}),
                "must be a string, a number or a boolean",
            ),
            (
                json!({"kind": "cpu", "interval_ms": 1_000, "params": {"per_core": null}}),
                "must be a string, a number or a boolean",
            ),
            (
                json!({"kind": "disk", "interval_ms": 1_000, "params": {"mounts_exclude": "x".repeat(1025)}}),
                "at most 1024 bytes",
            ),
        ];
        for (body, expected) in cases {
            let error = request(body.clone()).unwrap_err();
            assert!(error.contains(expected), "{body}: {error}");
        }
        let many: Map<String, Value> = (0..33).map(|i| (format!("p{i}"), json!(1))).collect();
        let error = request(json!({"kind": "cpu", "interval_ms": 1_000, "params": many}));
        assert!(error.unwrap_err().contains("at most 32 params"));
    }

    #[test]
    fn requests_refuse_unknown_fields() {
        let parsed = serde_json::from_value::<EntryRequest>(
            json!({"kind": "cpu", "interval_ms": 1_000, "site_id": "x"}),
        );
        assert!(parsed.is_err());
    }

    fn entry(server: bool, name: &str, instance: &str, interval_ms: i64, enabled: bool) -> Entry {
        let owner = Some(Uuid::from_u128(1));
        Entry {
            id: Uuid::new_v4(),
            site_id: owner.filter(|_| !server),
            server_id: owner.filter(|_| server),
            kind: name.into(),
            name: name.into(),
            instance: instance.into(),
            interval_ms,
            timeout_ms: None,
            params: json!({"per_core": false}),
            enabled,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn effective_sensors_name_their_origin() {
        let entries = [
            entry(false, "cpu", "", 30_000, true),
            entry(true, "cpu", "", 5_000, true),
            entry(false, "memory", "", 10_000, true),
            entry(false, "disk", "", 60_000, false),
            entry(true, "disk", "/data", 60_000, true),
        ];
        let sensors = effective_sensors(&entries);
        let view: Vec<_> = sensors
            .iter()
            .map(|s| {
                (
                    s.name.as_str(),
                    s.instance.as_str(),
                    s.origin,
                    s.entry_id,
                    s.interval_ms,
                )
            })
            .collect();
        assert_eq!(
            view,
            [
                ("cpu", "", Origin::Server, Some(entries[1].id), 5_000),
                ("disk", "/data", Origin::Server, Some(entries[4].id), 60_000),
                ("memory", "", Origin::Site, Some(entries[2].id), 10_000),
            ]
        );
        assert_eq!(sensors[0].params["per_core"], "false");
        let defaults = effective_sensors(&[]);
        assert!(defaults
            .iter()
            .all(|s| s.origin == Origin::Default && s.entry_id.is_none()));
        assert_eq!(defaults.len(), 3);
    }
}
