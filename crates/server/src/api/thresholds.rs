//! Threshold endpoints (ADR-0017): the effective thresholds of a server,
//! and the site and server rows of `thresholds`.
//!
//! Access follows the sensor configuration endpoints: every role reads the
//! sites it covers, operators and above change them, and a site or server
//! the user cannot read is not found. Changes bump the threshold revision
//! through the schema's trigger, and the engine reloads the servers
//! concerned at its next poll. The API writes `thresholds` only; incidents,
//! their events and alert states belong to the engine.

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use monitorflow_sensors::validate;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgConnection;
use uuid::Uuid;

use super::access::authorize;
use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::metrics::check_metric;
use super::sensors::{
    body, parse_id, Origin, Owner, CHECK_VIOLATION, FOREIGN_KEY_VIOLATION, UNIQUE_VIOLATION,
};
use super::{ApiError, ApiState};
use crate::alerting::rules::{self, Comparison, Rule};
use crate::sensor_config::Layer;

/// Longest accepted duration, in seconds: one day, as the schema allows.
pub const MAX_DURATION_S: u32 = 86_400;

/// Body of a create or an update. An update replaces every field.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntryRequest {
    /// Full metric name, `monitorflow_<sensor metric>`.
    pub metric: String,
    /// `null` or absent: every instance of the metric.
    #[serde(default)]
    pub instance: Option<String>,
    /// `>`, `>=`, `<` or `<=`.
    pub comparison: String,
    #[serde(default)]
    pub warning: Option<f64>,
    #[serde(default)]
    pub critical: Option<f64>,
    #[serde(default)]
    pub duration_s: u32,
    #[serde(default)]
    pub hysteresis: f64,
    /// `false` mutes the threshold of the same metric and instance from
    /// the layers below, built-in defaults included.
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
}

fn enabled_by_default() -> bool {
    true
}

/// An [`EntryRequest`] that passed [`validate_entry`].
#[derive(Debug, Clone, PartialEq)]
pub struct ValidEntry {
    pub metric: String,
    pub instance: Option<String>,
    pub comparison: Comparison,
    pub warning: Option<f64>,
    pub critical: Option<f64>,
    pub duration_s: i32,
    pub hysteresis: f64,
    pub enabled: bool,
}

/// Checks `request` against what the engine can use, the schema's CHECK
/// constraints included. The error is a message for the client.
pub fn validate_entry(request: EntryRequest) -> Result<ValidEntry, String> {
    check_metric(&request.metric).map_err(|e| e.to_string())?;
    if let Some(instance) = &request.instance {
        validate::instance(instance).map_err(|e| e.to_string())?;
    }
    let comparison: Comparison = request
        .comparison
        .parse()
        .map_err(|_| "comparison must be one of `>`, `>=`, `<`, `<=`".to_owned())?;
    for (name, value) in [("warning", request.warning), ("critical", request.critical)] {
        if value.is_some_and(|v| !v.is_finite()) {
            return Err(format!("{name} must be a finite number"));
        }
    }
    if request.enabled && request.warning.is_none() && request.critical.is_none() {
        return Err("a warning or a critical value is required unless enabled is false".into());
    }
    if let (Some(warning), Some(critical)) = (request.warning, request.critical) {
        if comparison.above() && critical < warning {
            return Err(format!(
                "critical must not be below warning with `{comparison}`"
            ));
        }
        if !comparison.above() && critical > warning {
            return Err(format!(
                "critical must not be above warning with `{comparison}`"
            ));
        }
    }
    if request.duration_s > MAX_DURATION_S {
        return Err(format!("duration_s must be between 0 and {MAX_DURATION_S}"));
    }
    if !(request.hysteresis.is_finite() && request.hysteresis >= 0.0) {
        return Err("hysteresis must be a finite number, 0 or more".into());
    }
    Ok(ValidEntry {
        metric: request.metric,
        instance: request.instance,
        comparison,
        warning: request.warning,
        critical: request.critical,
        // Bounded by MAX_DURATION_S above.
        duration_s: i32::try_from(request.duration_s).unwrap_or(i32::MAX),
        hysteresis: request.hysteresis,
        enabled: request.enabled,
    })
}

/// One row of `thresholds`. Exactly one of `site_id` and `server_id` is
/// set.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Entry {
    pub id: Uuid,
    pub site_id: Option<Uuid>,
    pub server_id: Option<Uuid>,
    pub metric: String,
    /// `null`: every instance.
    pub instance: Option<String>,
    pub comparison: String,
    pub warning: Option<f64>,
    pub critical: Option<f64>,
    pub duration_s: i32,
    pub hysteresis: f64,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Body of the list endpoints, sorted by metric and instance, the row for
/// every instance first.
#[derive(Debug, Serialize)]
pub struct EntryList {
    pub entries: Vec<Entry>,
}

/// One effective threshold, or a muted one, with the layer it comes from.
#[derive(Debug, Serialize)]
pub struct EffectiveThreshold {
    pub origin: Origin,
    /// The row it comes from; `null` for a built-in default.
    pub threshold_id: Option<Uuid>,
    pub metric: String,
    pub instance: Option<String>,
    /// `false`: muted by a disabled row, which the engine skips.
    pub enabled: bool,
    pub comparison: String,
    pub warning: Option<f64>,
    pub critical: Option<f64>,
    pub duration_s: u32,
    pub hysteresis: f64,
}

/// Body of `GET /api/servers/{id}/thresholds/effective`.
#[derive(Debug, Serialize)]
pub struct Effective {
    pub server_id: Uuid,
    pub site_id: Uuid,
    /// Revision the engine compares to reload the server's thresholds.
    pub revision: i64,
    pub thresholds: Vec<EffectiveThreshold>,
}

const ENTRY_COLUMNS: &str = "id, site_id, server_id, metric, instance, comparison, warning, \
                             critical, duration_s, hysteresis, enabled, created_at, updated_at";

async fn list(state: &ApiState, user: &CurrentUser, owner: Owner) -> Result<EntryList, ApiError> {
    authorize(user, owner.site(&state.pool).await?, false)?;
    let (site_id, server_id) = owner.columns();
    let entries = sqlx::query_as(&format!(
        "SELECT {ENTRY_COLUMNS} FROM thresholds
         WHERE site_id = $1 OR server_id = $2 ORDER BY metric, instance NULLS FIRST"
    ))
    .bind(site_id)
    .bind(server_id)
    .fetch_all(&state.pool)
    .await?;
    Ok(EntryList { entries })
}

/// `GET /api/sites/{id}/thresholds`.
pub async fn list_site(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<EntryList>, ApiError> {
    Ok(Json(
        list(&state, &user, Owner::Site(parse_id(&id)?)).await?,
    ))
}

/// `GET /api/servers/{id}/thresholds`.
pub async fn list_server(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<EntryList>, ApiError> {
    Ok(Json(
        list(&state, &user, Owner::Server(parse_id(&id)?)).await?,
    ))
}

/// What the audit log keeps of a row: all of it but the timestamps.
fn audit_summary(entry: &Entry) -> Value {
    json!({
        "site_id": entry.site_id,
        "server_id": entry.server_id,
        "metric": entry.metric,
        "instance": entry.instance,
        "comparison": entry.comparison,
        "warning": entry.warning,
        "critical": entry.critical,
        "duration_s": entry.duration_s,
        "hysteresis": entry.hysteresis,
        "enabled": entry.enabled,
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
            target: Some(("threshold", &target)),
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
        Some(UNIQUE_VIOLATION) => {
            let instance = entry.instance.as_ref().map_or_else(
                || "every instance".to_owned(),
                |i| format!("instance `{i}`"),
            );
            ApiError::Conflict(format!(
                "a threshold on `{}` for {instance} already exists here",
                entry.metric
            ))
        }
        // The owner was deleted meanwhile.
        Some(FOREIGN_KEY_VIOLATION) => ApiError::NotFound,
        Some(CHECK_VIOLATION) => ApiError::Invalid("threshold refused".into()),
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
        "INSERT INTO thresholds
             (site_id, server_id, metric, instance, comparison, warning, critical, duration_s,
              hysteresis, enabled)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
         RETURNING {ENTRY_COLUMNS}"
    ))
    .bind(site_id)
    .bind(server_id)
    .bind(&valid.metric)
    .bind(&valid.instance)
    .bind(valid.comparison.as_str())
    .bind(valid.warning)
    .bind(valid.critical)
    .bind(valid.duration_s)
    .bind(valid.hysteresis)
    .bind(valid.enabled)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| write_error(e, &valid))?;
    record(
        &mut tx,
        user,
        "threshold.create",
        &entry,
        audit_summary(&entry),
    )
    .await?;
    tx.commit().await?;
    tracing::info!(user = %user.id, threshold = %entry.id, metric = %entry.metric, "threshold created");
    Ok((StatusCode::CREATED, Json(entry)))
}

/// `POST /api/sites/{id}/thresholds`: 201 with the new [`Entry`].
pub async fn create_site(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<EntryRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<Entry>), ApiError> {
    create(&state, &user, Owner::Site(parse_id(&id)?), request).await
}

/// `POST /api/servers/{id}/thresholds`: 201 with the new [`Entry`].
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
        "SELECT {ENTRY_COLUMNS} FROM thresholds WHERE id = $1 FOR UPDATE"
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

/// `PUT /api/thresholds/{id}`: replaces the row; 200 with the updated
/// [`Entry`].
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
        "UPDATE thresholds
         SET metric = $2, instance = $3, comparison = $4, warning = $5, critical = $6,
             duration_s = $7, hysteresis = $8, enabled = $9, updated_at = now()
         WHERE id = $1
         RETURNING {ENTRY_COLUMNS}"
    ))
    .bind(id)
    .bind(&valid.metric)
    .bind(&valid.instance)
    .bind(valid.comparison.as_str())
    .bind(valid.warning)
    .bind(valid.critical)
    .bind(valid.duration_s)
    .bind(valid.hysteresis)
    .bind(valid.enabled)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| write_error(e, &valid))?;
    let details = json!({ "before": audit_summary(&before), "after": audit_summary(&entry) });
    record(&mut tx, &user, "threshold.update", &entry, details).await?;
    tx.commit().await?;
    tracing::info!(user = %user.id, threshold = %entry.id, "threshold updated");
    Ok(Json(entry))
}

/// `DELETE /api/thresholds/{id}`: 204.
pub async fn delete(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let id = parse_id(&id)?;
    let mut tx = state.pool.begin().await?;
    let entry = lock_entry(&mut tx, &user, id).await?;
    sqlx::query("DELETE FROM thresholds WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    record(
        &mut tx,
        &user,
        "threshold.delete",
        &entry,
        audit_summary(&entry),
    )
    .await?;
    tx.commit().await?;
    tracing::info!(user = %user.id, threshold = %id, "threshold deleted");
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/servers/{id}/thresholds/effective`: the thresholds the engine
/// applies to the server, muted ones included, with their origin.
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
    let server: Option<(Uuid, i64)> = sqlx::query_as(
        "SELECT s.site_id, si.threshold_revision + s.threshold_revision
         FROM servers s JOIN sites si ON si.id = s.site_id
         WHERE s.id = $1",
    )
    .bind(server_id)
    .fetch_optional(&mut *tx)
    .await?;
    let site_id = authorize(&user, server.map(|s| s.0), false)?;
    let (_, revision) = server.ok_or(ApiError::NotFound)?;
    let entries: Vec<Entry> = sqlx::query_as(&format!(
        "SELECT {ENTRY_COLUMNS} FROM thresholds WHERE site_id = $1 OR server_id = $2"
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
        thresholds: effective_thresholds(&entries),
    }))
}

/// Resolves `entries` with [`rules::resolve`], the function the engine
/// uses, then names the row each key comes from: the server's row if any,
/// else the site's, else a built-in default.
fn effective_thresholds(entries: &[Entry]) -> Vec<EffectiveThreshold> {
    let rows = entries
        .iter()
        .filter_map(|e| {
            Some(rules::Row {
                layer: if e.server_id.is_some() {
                    Layer::Server
                } else {
                    Layer::Site
                },
                rule: Rule {
                    metric: e.metric.clone(),
                    instance: e.instance.clone(),
                    comparison: e.comparison.parse().ok()?,
                    warning: e.warning,
                    critical: e.critical,
                    duration_s: u32::try_from(e.duration_s).unwrap_or(0),
                    hysteresis: e.hysteresis,
                },
                enabled: e.enabled,
            })
        })
        .collect();
    rules::resolve(rows)
        .entries()
        .filter_map(|(metric, instance, rule)| {
            let of = |server: bool| {
                entries.iter().find(|e| {
                    e.server_id.is_some() == server
                        && e.metric == metric
                        && e.instance.as_deref() == instance
                })
            };
            let (origin, row) = match (of(true), of(false)) {
                (Some(e), _) => (Origin::Server, Some(e)),
                (None, Some(e)) => (Origin::Site, Some(e)),
                (None, None) => (Origin::Default, None),
            };
            // An enabled key shows the rule in force, a muted one the row
            // that mutes it; only rows mute, so a muted key has one.
            Some(match (rule, row) {
                (Some(rule), _) => EffectiveThreshold {
                    origin,
                    threshold_id: row.map(|e| e.id),
                    metric: rule.metric.clone(),
                    instance: rule.instance.clone(),
                    enabled: true,
                    comparison: rule.comparison.as_str().to_owned(),
                    warning: rule.warning,
                    critical: rule.critical,
                    duration_s: rule.duration_s,
                    hysteresis: rule.hysteresis,
                },
                (None, Some(e)) => EffectiveThreshold {
                    origin,
                    threshold_id: Some(e.id),
                    metric: e.metric.clone(),
                    instance: e.instance.clone(),
                    enabled: false,
                    comparison: e.comparison.clone(),
                    warning: e.warning,
                    critical: e.critical,
                    duration_s: u32::try_from(e.duration_s).unwrap_or(0),
                    hysteresis: e.hysteresis,
                },
                (None, None) => return None,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DISK: &str = "monitorflow_disk_used_ratio";

    fn request(value: Value) -> Result<ValidEntry, String> {
        validate_entry(serde_json::from_value(value).unwrap())
    }

    #[test]
    fn valid_entries_get_defaults() {
        let entry = request(json!({"metric": DISK, "comparison": ">", "warning": 0.8})).unwrap();
        assert_eq!(entry.instance, None);
        assert_eq!(entry.comparison, Comparison::Gt);
        assert_eq!((entry.duration_s, entry.hysteresis), (0, 0.0));
        assert!(entry.enabled);
        // A disabled row only mutes and needs no value.
        let entry = request(json!({
            "metric": DISK, "instance": "/boot", "comparison": ">", "enabled": false,
        }))
        .unwrap();
        assert_eq!(entry.instance.as_deref(), Some("/boot"));
        assert!(!entry.enabled);
        let entry = request(json!({
            "metric": "monitorflow_memory_available_ratio", "instance": "", "comparison": "<=",
            "warning": 0.2, "critical": 0.1, "duration_s": 86_400, "hysteresis": 0.05,
        }))
        .unwrap();
        assert_eq!(entry.comparison, Comparison::Le);
        assert_eq!(entry.duration_s, 86_400);
    }

    #[test]
    fn invalid_entries_say_why() {
        let cases = [
            (
                json!({"metric": "cpu", "comparison": ">", "warning": 1}),
                "metric must be",
            ),
            (
                json!({"metric": "monitorflow_cpu{x=\"y\"}", "comparison": ">", "warning": 1}),
                "metric must be",
            ),
            (
                json!({"metric": DISK, "instance": "a\nb", "comparison": ">", "warning": 1}),
                "instance",
            ),
            (
                json!({"metric": DISK, "comparison": "=", "warning": 1}),
                "comparison must be",
            ),
            (
                json!({"metric": DISK, "comparison": ">"}),
                "a warning or a critical value",
            ),
            (
                json!({"metric": DISK, "comparison": ">", "warning": 0.9, "critical": 0.8}),
                "critical must not be below warning with `>`",
            ),
            (
                json!({"metric": DISK, "comparison": "<", "warning": 0.1, "critical": 0.2}),
                "critical must not be above warning with `<`",
            ),
            (
                json!({"metric": DISK, "comparison": ">", "warning": 1, "duration_s": 86_401}),
                "duration_s must be between 0 and 86400",
            ),
            (
                json!({"metric": DISK, "comparison": ">", "warning": 1, "hysteresis": -0.1}),
                "hysteresis must be",
            ),
        ];
        for (body, expected) in cases {
            let error = request(body.clone()).unwrap_err();
            assert!(error.contains(expected), "{body}: {error}");
        }
        // Equal values are consistent with every comparison.
        assert!(
            request(json!({"metric": DISK, "comparison": "<", "warning": 1, "critical": 1}))
                .is_ok()
        );
    }

    #[test]
    fn requests_refuse_unknown_fields() {
        let parsed = serde_json::from_value::<EntryRequest>(
            json!({"metric": DISK, "comparison": ">", "warning": 1, "server_id": "x"}),
        );
        assert!(parsed.is_err());
    }

    fn entry(server: bool, instance: Option<&str>, warning: Option<f64>, enabled: bool) -> Entry {
        let owner = Some(Uuid::from_u128(1));
        Entry {
            id: Uuid::new_v4(),
            site_id: owner.filter(|_| !server),
            server_id: owner.filter(|_| server),
            metric: DISK.into(),
            instance: instance.map(Into::into),
            comparison: ">".into(),
            warning,
            critical: None,
            duration_s: 60,
            hysteresis: 0.0,
            enabled,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn effective_thresholds_name_their_origin() {
        let entries = [
            entry(false, None, Some(0.8), true),
            entry(true, None, Some(0.7), true),
            entry(false, Some("/boot"), None, false),
            entry(true, Some("/data"), Some(0.9), true),
        ];
        let view: Vec<_> = effective_thresholds(&entries)
            .into_iter()
            .map(|t| {
                (
                    t.metric,
                    t.instance,
                    t.origin,
                    t.threshold_id,
                    t.enabled,
                    t.warning,
                )
            })
            .collect();
        let cpu = "monitorflow_cpu_usage_ratio".to_owned();
        let memory = "monitorflow_memory_available_ratio".to_owned();
        let some = |s: &str| Some(s.to_owned());
        assert_eq!(
            view,
            [
                (cpu, some("total"), Origin::Default, None, true, Some(0.9)),
                (
                    DISK.into(),
                    None,
                    Origin::Server,
                    Some(entries[1].id),
                    true,
                    Some(0.7)
                ),
                (
                    DISK.into(),
                    some("/boot"),
                    Origin::Site,
                    Some(entries[2].id),
                    false,
                    None
                ),
                (
                    DISK.into(),
                    some("/data"),
                    Origin::Server,
                    Some(entries[3].id),
                    true,
                    Some(0.9)
                ),
                (memory, None, Origin::Default, None, true, Some(0.10)),
            ]
        );
        let defaults = effective_thresholds(&[]);
        assert_eq!(defaults.len(), rules::builtin_defaults().len());
        assert!(defaults
            .iter()
            .all(|t| t.origin == Origin::Default && t.enabled));
    }
}
