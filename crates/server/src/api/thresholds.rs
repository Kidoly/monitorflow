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
use crate::alerting::rules::{self, Comparison, Condition, Function, Kind, Rule};
use crate::sensor_config::Layer;

/// Longest accepted duration, in seconds: one day, as the schema allows.
pub const MAX_DURATION_S: u32 = 86_400;

/// Body of a create or an update. An update replaces every field.
#[derive(Debug, Clone, Deserialize, Serialize, utoipa::ToSchema)]
#[schema(as = ThresholdRequest)]
#[serde(deny_unknown_fields)]
pub struct EntryRequest {
    /// Full metric name, `monitorflow_<sensor metric>`.
    pub metric: String,
    /// `null` or absent: every instance of the metric.
    #[serde(default)]
    pub instance: Option<String>,
    /// What the rule judges (ADR-0153): `value` (the default), `window`,
    /// `compare`, `forecast` or `nodata`. One metric and instance can have
    /// one rule of each kind per site or server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// `>`, `>=`, `<` or `<=`: the alarm side of the values. For a
    /// `forecast`, the side of `target` the value is heading to (`>=` 1:
    /// "reaches 100%"). Optional for `nodata`, which alarms on a long
    /// silence; required otherwise.
    #[serde(default)]
    pub comparison: Option<String>,
    /// The value that opens a warning incident, in the metric's unit (a
    /// ratio from 0 to 1 for `_ratio` metrics); for a `forecast`, seconds
    /// left; for `nodata`, seconds of silence; `null` for none.
    #[serde(default)]
    pub warning: Option<f64>,
    /// The value that opens a critical incident, on the alarm side of
    /// `warning`; `null` for none. An enabled row needs one of the two.
    #[serde(default)]
    pub critical: Option<f64>,
    /// Seconds the value must stay past the threshold before an incident
    /// opens: 0 to 86400, 0 by default; always 0 for `nodata`.
    #[serde(default)]
    pub duration_s: u32,
    /// The recovery margin: how far back past the threshold the value
    /// must go before the incident resolves, in the unit of the limits; 0
    /// or more, 0 by default.
    #[serde(default)]
    pub hysteresis: f64,
    /// `false` mutes the threshold of the same metric, instance and kind
    /// from the layers below, built-in defaults included.
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    /// `window`: `avg`, `min`, `max`, `change` or `rate` (per second);
    /// `compare`: `ratio` (now divided by then) or `difference` (now minus
    /// then). Absent for other kinds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
    /// Seconds. `window`: 60 to 86400; `compare`: the average compared,
    /// 300 to 86400; `forecast`: the time the trend is learned over, 3600
    /// to 604800. Absent for other kinds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_s: Option<u32>,
    /// `compare` only: 86400 (yesterday) or 604800 (last week).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset_s: Option<u32>,
    /// `forecast` only: the value it is about, in the metric's unit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<f64>,
    /// Up to two more conditions joined with "and", on the same server;
    /// not for `nodata`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<ThresholdCondition>,
}

impl Default for EntryRequest {
    /// An enabled `value` threshold with no field set.
    fn default() -> Self {
        Self {
            metric: String::new(),
            instance: None,
            kind: None,
            comparison: None,
            warning: None,
            critical: None,
            duration_s: 0,
            hysteresis: 0.0,
            enabled: true,
            function: None,
            window_s: None,
            offset_s: None,
            target: None,
            conditions: Vec::new(),
        }
    }
}

/// A condition joined with "and" to a threshold (ADR-0153).
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ThresholdCondition {
    /// Full metric name of the same server.
    pub metric: String,
    /// `null` or absent: the instance of the series the threshold judges,
    /// else the series without an instance.
    #[serde(default)]
    pub instance: Option<String>,
    /// `value` (the latest sample, the default), or `avg`, `min`, `max`,
    /// `change` or `rate` over `window_s`.
    #[serde(default = "value_function")]
    pub function: String,
    /// Seconds, 60 to 86400, with a function other than `value`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_s: Option<u32>,
    /// `>`, `>=`, `<` or `<=`.
    pub comparison: String,
    /// The limit, in the metric's unit.
    pub value: f64,
}

fn value_function() -> String {
    "value".into()
}

impl From<&Condition> for ThresholdCondition {
    fn from(c: &Condition) -> Self {
        Self {
            metric: c.metric.clone(),
            instance: c.instance.clone(),
            function: c.function.map_or("value", Function::as_str).to_owned(),
            window_s: c.window_s,
            comparison: c.comparison.as_str().to_owned(),
            value: c.value,
        }
    }
}

fn enabled_by_default() -> bool {
    true
}

/// An [`EntryRequest`] that passed [`validate_entry`].
#[derive(Debug, Clone, PartialEq)]
pub struct ValidEntry {
    pub metric: String,
    pub instance: Option<String>,
    pub kind: Kind,
    pub comparison: Comparison,
    pub warning: Option<f64>,
    pub critical: Option<f64>,
    pub duration_s: i32,
    pub hysteresis: f64,
    pub enabled: bool,
    pub function: Option<Function>,
    pub window_s: Option<i32>,
    pub offset_s: Option<i32>,
    pub target: Option<f64>,
    pub conditions: Vec<Condition>,
}

/// Bounds of the windows of each kind, in seconds (ADR-0153).
pub const WINDOW_S: (u32, u32) = (60, 86_400);
pub const COMPARE_WINDOW_S: (u32, u32) = (300, 86_400);
pub const FORECAST_WINDOW_S: (u32, u32) = (3_600, 604_800);
/// The offsets a comparison may use: yesterday and last week.
pub const OFFSETS_S: [u32; 2] = [86_400, 604_800];
/// Time left a forecast may warn about: a minute to 90 days.
pub const FORECAST_LIMIT_S: (f64, f64) = (60.0, 7_776_000.0);
/// Silence a no-data rule may wait for: a minute to 7 days.
pub const NODATA_LIMIT_S: (f64, f64) = (60.0, 604_800.0);
/// Conditions joined with "and" to one rule.
pub const MAX_CONDITIONS: usize = 2;

fn seconds_in(name: &str, value: Option<u32>, (low, high): (u32, u32)) -> Result<i32, String> {
    match value {
        Some(v) if (low..=high).contains(&v) => Ok(i32::try_from(v).unwrap_or(i32::MAX)),
        _ => Err(format!("{name} must be between {low} and {high} seconds")),
    }
}

fn none_of(kind: Kind, fields: &[(&str, bool)]) -> Result<(), String> {
    match fields.iter().find(|(_, set)| *set) {
        Some((name, _)) => Err(format!("{name} is not used by a `{kind}` threshold")),
        None => Ok(()),
    }
}

fn comparison_of(text: Option<&str>) -> Result<Comparison, String> {
    text.ok_or_else(|| "comparison is required".to_owned())?
        .parse()
        .map_err(|_| "comparison must be one of `>`, `>=`, `<`, `<=`".to_owned())
}

fn condition_of(index: usize, c: ThresholdCondition) -> Result<Condition, String> {
    let at = |message: String| format!("conditions[{index}]: {message}");
    check_metric(&c.metric).map_err(|e| at(e.to_string()))?;
    if let Some(instance) = &c.instance {
        validate::instance(instance).map_err(|e| at(e.to_string()))?;
    }
    let function = match c.function.as_str() {
        "value" => None,
        name => match name.parse::<Function>() {
            Ok(f) if f.over_window() => Some(f),
            _ => {
                return Err(at(
                    "function must be one of `value`, `avg`, `min`, `max`, `change`, `rate`".into(),
                ))
            }
        },
    };
    let window_s = match function {
        Some(_) => Some(seconds_in("window_s", c.window_s, WINDOW_S).map_err(at)? as u32),
        None if c.window_s.is_some() => {
            return Err(at("window_s needs a function other than `value`".into()))
        }
        None => None,
    };
    if !c.value.is_finite() {
        return Err(at("value must be a finite number".into()));
    }
    Ok(Condition {
        metric: c.metric,
        instance: c.instance,
        function,
        window_s,
        comparison: comparison_of(Some(&c.comparison)).map_err(at)?,
        value: c.value,
    })
}

/// Checks `request` against what the engine can use, the schema's CHECK
/// constraints included. The error is a message for the client.
pub fn validate_entry(request: EntryRequest) -> Result<ValidEntry, String> {
    check_metric(&request.metric).map_err(|e| e.to_string())?;
    if let Some(instance) = &request.instance {
        validate::instance(instance).map_err(|e| e.to_string())?;
    }
    let kind: Kind = request
        .kind
        .as_deref()
        .unwrap_or("value")
        .parse()
        .map_err(|_| {
            "kind must be one of `value`, `window`, `compare`, `forecast`, `nodata`".to_owned()
        })?;
    let comparison = match (kind, request.comparison.as_deref()) {
        (Kind::NoData, None | Some(">")) => Comparison::Gt,
        (Kind::NoData, Some(_)) => return Err("a `nodata` threshold compares with `>`".into()),
        (_, text) => comparison_of(text)?,
    };
    for (name, value) in [("warning", request.warning), ("critical", request.critical)] {
        if value.is_some_and(|v| !v.is_finite()) {
            return Err(format!("{name} must be a finite number"));
        }
    }
    if request.enabled && request.warning.is_none() && request.critical.is_none() {
        return Err("a warning or a critical value is required unless enabled is false".into());
    }
    if let (Some(warning), Some(critical)) = (request.warning, request.critical) {
        let (refused, side) = match kind {
            Kind::Forecast => (critical > warning, "more time left than warning".to_owned()),
            Kind::NoData => (
                critical < warning,
                "a shorter silence than warning".to_owned(),
            ),
            _ if comparison.above() => (
                critical < warning,
                format!("below warning with `{comparison}`"),
            ),
            _ => (
                critical > warning,
                format!("above warning with `{comparison}`"),
            ),
        };
        if refused {
            return Err(format!("critical must not be {side}"));
        }
    }
    let limits = |name: &str, (low, high): (f64, f64)| {
        let bad = [request.warning, request.critical]
            .into_iter()
            .flatten()
            .any(|v| !(low..=high).contains(&v));
        if bad {
            Err(format!("{name} must be between {low} and {high} seconds"))
        } else {
            Ok(())
        }
    };
    if request.duration_s > MAX_DURATION_S {
        return Err(format!("duration_s must be between 0 and {MAX_DURATION_S}"));
    }
    if !(request.hysteresis.is_finite() && request.hysteresis >= 0.0) {
        return Err("hysteresis must be a finite number, 0 or more".into());
    }
    let function = match request.function.as_deref() {
        None => None,
        Some(name) => Some(name.parse::<Function>().map_err(|_| {
            "function must be one of `avg`, `min`, `max`, `change`, `rate`, `ratio`, `difference`"
                .to_owned()
        })?),
    };
    let (mut window_s, mut offset_s) = (None, None);
    match kind {
        Kind::Value => none_of(
            kind,
            &[
                ("function", function.is_some()),
                ("window_s", request.window_s.is_some()),
                ("offset_s", request.offset_s.is_some()),
                ("target", request.target.is_some()),
            ],
        )?,
        Kind::Window => {
            if !function.is_some_and(Function::over_window) {
                return Err(
                    "a `window` threshold needs a function: `avg`, `min`, `max`, `change` or `rate`"
                        .into(),
                );
            }
            window_s = Some(seconds_in("window_s", request.window_s, WINDOW_S)?);
            none_of(
                kind,
                &[
                    ("offset_s", request.offset_s.is_some()),
                    ("target", request.target.is_some()),
                ],
            )?;
        }
        Kind::Compare => {
            if !matches!(function, Some(Function::Ratio | Function::Difference)) {
                return Err(
                    "a `compare` threshold needs a function: `ratio` or `difference`".into(),
                );
            }
            window_s = Some(seconds_in("window_s", request.window_s, COMPARE_WINDOW_S)?);
            match request.offset_s {
                Some(o) if OFFSETS_S.contains(&o) => offset_s = Some(o as i32),
                _ => return Err("offset_s must be 86400 (yesterday) or 604800 (last week)".into()),
            }
            none_of(kind, &[("target", request.target.is_some())])?;
        }
        Kind::Forecast => {
            if !request.target.is_some_and(f64::is_finite) {
                return Err("a `forecast` threshold needs a finite target".into());
            }
            window_s = Some(seconds_in("window_s", request.window_s, FORECAST_WINDOW_S)?);
            limits("warning and critical", FORECAST_LIMIT_S)?;
            none_of(
                kind,
                &[
                    ("function", function.is_some()),
                    ("offset_s", request.offset_s.is_some()),
                ],
            )?;
        }
        Kind::NoData => {
            limits("warning and critical", NODATA_LIMIT_S)?;
            none_of(
                kind,
                &[
                    ("function", function.is_some()),
                    ("window_s", request.window_s.is_some()),
                    ("offset_s", request.offset_s.is_some()),
                    ("target", request.target.is_some()),
                    ("duration_s", request.duration_s > 0),
                    ("conditions", !request.conditions.is_empty()),
                ],
            )?;
        }
    }
    if request.conditions.len() > MAX_CONDITIONS {
        return Err(format!("at most {MAX_CONDITIONS} conditions"));
    }
    let conditions = request
        .conditions
        .into_iter()
        .enumerate()
        .map(|(i, c)| condition_of(i, c))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ValidEntry {
        metric: request.metric,
        instance: request.instance,
        kind,
        comparison,
        warning: request.warning,
        critical: request.critical,
        // Bounded by MAX_DURATION_S above.
        duration_s: i32::try_from(request.duration_s).unwrap_or(i32::MAX),
        hysteresis: request.hysteresis,
        enabled: request.enabled,
        function,
        window_s,
        offset_s,
        target: request.target,
        conditions,
    })
}

impl ValidEntry {
    /// The rule the engine would apply.
    pub fn rule(&self) -> Rule {
        let seconds = |v: Option<i32>| v.and_then(|v| u32::try_from(v).ok()).unwrap_or(0);
        Rule {
            metric: self.metric.clone(),
            instance: self.instance.clone(),
            kind: self.kind,
            comparison: self.comparison,
            warning: self.warning,
            critical: self.critical,
            duration_s: seconds(Some(self.duration_s)),
            hysteresis: self.hysteresis,
            function: self.function,
            window_s: seconds(self.window_s),
            offset_s: seconds(self.offset_s),
            target: self.target,
            conditions: self.conditions.clone(),
        }
    }

    fn conditions_json(&self) -> Value {
        json!(self.conditions)
    }
}

/// One row of `thresholds`. Exactly one of `site_id` and `server_id` is
/// set.
#[derive(Debug, Clone, Serialize, sqlx::FromRow, utoipa::ToSchema)]
#[schema(as = Threshold)]
pub struct Entry {
    pub id: Uuid,
    /// Set for a site's row, which its servers inherit.
    pub site_id: Option<Uuid>,
    /// Set for one server's row.
    pub server_id: Option<Uuid>,
    pub metric: String,
    /// `null`: every instance.
    pub instance: Option<String>,
    /// `>`, `>=`, `<` or `<=`.
    pub comparison: String,
    /// `null`: no warning level.
    pub warning: Option<f64>,
    /// `null`: no critical level.
    pub critical: Option<f64>,
    /// Seconds.
    pub duration_s: i32,
    /// The recovery margin, in the metric's unit.
    pub hysteresis: f64,
    /// `false`: mutes the same metric, instance and kind of the layers
    /// below.
    pub enabled: bool,
    /// `value`, `window`, `compare`, `forecast` or `nodata` (ADR-0153).
    pub kind: String,
    /// `window` and `compare` only.
    pub function: Option<String>,
    /// Seconds; `window`, `compare` and `forecast` only.
    pub window_s: Option<i32>,
    /// Seconds; `compare` only.
    pub offset_s: Option<i32>,
    /// `forecast` only.
    pub target: Option<f64>,
    /// Conditions joined with "and".
    #[schema(value_type = Vec<ThresholdCondition>)]
    pub conditions: sqlx::types::Json<Vec<ThresholdCondition>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Body of the list endpoints, sorted by metric and instance, the row for
/// every instance first.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = ThresholdList)]
pub struct EntryList {
    pub entries: Vec<Entry>,
}

/// One effective threshold, or a muted one, with the layer it comes from.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct EffectiveThreshold {
    pub origin: Origin,
    /// The row it comes from; `null` for a built-in default.
    pub threshold_id: Option<Uuid>,
    pub metric: String,
    /// `null`: every instance.
    pub instance: Option<String>,
    /// `false`: muted by a disabled row, which the engine skips.
    pub enabled: bool,
    /// `value`, `window`, `compare`, `forecast` or `nodata` (ADR-0153).
    pub kind: String,
    /// `>`, `>=`, `<` or `<=`.
    pub comparison: String,
    pub warning: Option<f64>,
    pub critical: Option<f64>,
    /// Seconds.
    pub duration_s: u32,
    /// The recovery margin, in the unit of the limits.
    pub hysteresis: f64,
    /// `window` and `compare` only.
    pub function: Option<String>,
    /// Seconds; `window`, `compare` and `forecast` only.
    pub window_s: Option<u32>,
    /// Seconds; `compare` only.
    pub offset_s: Option<u32>,
    /// `forecast` only.
    pub target: Option<f64>,
    pub conditions: Vec<ThresholdCondition>,
}

/// A rule MonitorFlow suggests and nobody added or declined yet
/// (ADR-0153): `POST` its `rule` to the site's thresholds to add it, or
/// the same with `enabled: false` to decline it.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct RecommendedThreshold {
    /// Stable name of the suggestion, like `disk_forecast`.
    pub key: String,
    pub rule: EntryRequest,
}

/// Body of `GET /api/servers/{id}/thresholds/effective`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = EffectiveThresholds)]
pub struct Effective {
    pub server_id: Uuid,
    pub site_id: Uuid,
    /// Revision the engine compares to reload the server's thresholds.
    pub revision: i64,
    /// By metric and instance, muted ones included.
    pub thresholds: Vec<EffectiveThreshold>,
    /// Suggestions for the site, which its servers would get.
    pub recommended: Vec<RecommendedThreshold>,
}

/// Body of `GET /api/sites/{id}/thresholds/effective`: what the site's
/// servers get unless they override it.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = SiteThresholds)]
pub struct SiteEffective {
    pub site_id: Uuid,
    /// By metric and instance, muted ones included.
    pub thresholds: Vec<EffectiveThreshold>,
    /// Suggestions for the site.
    pub recommended: Vec<RecommendedThreshold>,
}

const ENTRY_COLUMNS: &str = "id, site_id, server_id, metric, instance, comparison, warning, \
                             critical, duration_s, hysteresis, enabled, kind, function, window_s, \
                             offset_s, target, conditions, created_at, updated_at";

async fn list(state: &ApiState, user: &CurrentUser, owner: Owner) -> Result<EntryList, ApiError> {
    authorize(user, owner.site(&state.pool).await?, false)?;
    let (site_id, server_id) = owner.columns();
    // SQL-safe: interpolates only the constant ENTRY_COLUMNS.
    let entries = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {ENTRY_COLUMNS} FROM thresholds
         WHERE site_id = $1 OR server_id = $2 ORDER BY metric, instance NULLS FIRST, kind"
    )))
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
        "kind": entry.kind,
        "function": entry.function,
        "window_s": entry.window_s,
        "offset_s": entry.offset_s,
        "target": entry.target,
        "conditions": entry.conditions.0,
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
            let kind = match entry.kind {
                Kind::Value => String::new(),
                kind => format!(" of kind `{kind}`"),
            };
            ApiError::Conflict(format!(
                "a threshold{kind} on `{}` for {instance} already exists here",
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
    // SQL-safe: interpolates only the constant ENTRY_COLUMNS.
    let entry: Entry = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "INSERT INTO thresholds
             (site_id, server_id, metric, instance, comparison, warning, critical, duration_s,
              hysteresis, enabled, kind, function, window_s, offset_s, target, conditions)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)
         RETURNING {ENTRY_COLUMNS}"
    )))
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
    .bind(valid.kind.as_str())
    .bind(valid.function.map(Function::as_str))
    .bind(valid.window_s)
    .bind(valid.offset_s)
    .bind(valid.target)
    .bind(valid.conditions_json())
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
    // SQL-safe: interpolates only the constant ENTRY_COLUMNS.
    let entry: Option<Entry> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {ENTRY_COLUMNS} FROM thresholds WHERE id = $1 FOR UPDATE"
    )))
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
    // SQL-safe: interpolates only the constant ENTRY_COLUMNS.
    let entry: Entry = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "UPDATE thresholds
         SET metric = $2, instance = $3, comparison = $4, warning = $5, critical = $6,
             duration_s = $7, hysteresis = $8, enabled = $9, kind = $10, function = $11,
             window_s = $12, offset_s = $13, target = $14, conditions = $15,
             updated_at = now()
         WHERE id = $1
         RETURNING {ENTRY_COLUMNS}"
    )))
    .bind(id)
    .bind(&valid.metric)
    .bind(&valid.instance)
    .bind(valid.comparison.as_str())
    .bind(valid.warning)
    .bind(valid.critical)
    .bind(valid.duration_s)
    .bind(valid.hysteresis)
    .bind(valid.enabled)
    .bind(valid.kind.as_str())
    .bind(valid.function.map(Function::as_str))
    .bind(valid.window_s)
    .bind(valid.offset_s)
    .bind(valid.target)
    .bind(valid.conditions_json())
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
    // SQL-safe: interpolates only the constant ENTRY_COLUMNS.
    let entries: Vec<Entry> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {ENTRY_COLUMNS} FROM thresholds WHERE site_id = $1 OR server_id = $2"
    )))
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
        recommended: recommended(&entries),
    }))
}

/// `GET /api/sites/{id}/thresholds/effective`: the built-in defaults
/// overlaid with the site's rows, as a server of the site without rows of
/// its own gets them.
pub async fn effective_site(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<SiteEffective>, ApiError> {
    let owner = Owner::Site(parse_id(&id)?);
    let site_id = authorize(&user, owner.site(&state.pool).await?, false)?;
    // SQL-safe: interpolates only the constant ENTRY_COLUMNS.
    let entries: Vec<Entry> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {ENTRY_COLUMNS} FROM thresholds WHERE site_id = $1"
    )))
    .bind(site_id)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(SiteEffective {
        site_id,
        thresholds: effective_thresholds(&entries),
        recommended: recommended(&entries),
    }))
}

/// The rule a stored row holds, or `None` for one this version cannot
/// read.
pub fn rule_of(e: &Entry) -> Option<Rule> {
    let seconds = |v: Option<i32>| v.and_then(|v| u32::try_from(v).ok()).unwrap_or(0);
    let conditions = e
        .conditions
        .0
        .iter()
        .cloned()
        .enumerate()
        .map(|(i, c)| condition_of(i, c).ok())
        .collect::<Option<Vec<_>>>()?;
    Some(Rule {
        metric: e.metric.clone(),
        instance: e.instance.clone(),
        kind: e.kind.parse().ok()?,
        comparison: e.comparison.parse().ok()?,
        warning: e.warning,
        critical: e.critical,
        duration_s: seconds(Some(e.duration_s)),
        hysteresis: e.hysteresis,
        function: match &e.function {
            Some(f) => Some(f.parse().ok()?),
            None => None,
        },
        window_s: seconds(e.window_s),
        offset_s: seconds(e.offset_s),
        target: e.target,
        conditions,
    })
}

fn view(
    origin: Origin,
    threshold_id: Option<Uuid>,
    rule: &Rule,
    enabled: bool,
) -> EffectiveThreshold {
    EffectiveThreshold {
        origin,
        threshold_id,
        metric: rule.metric.clone(),
        instance: rule.instance.clone(),
        enabled,
        kind: rule.kind.as_str().to_owned(),
        comparison: rule.comparison.as_str().to_owned(),
        warning: rule.warning,
        critical: rule.critical,
        duration_s: rule.duration_s,
        hysteresis: rule.hysteresis,
        function: rule.function.map(|f| f.as_str().to_owned()),
        window_s: (rule.window_s > 0).then_some(rule.window_s),
        offset_s: (rule.offset_s > 0).then_some(rule.offset_s),
        target: rule.target,
        conditions: rule.conditions.iter().map(Into::into).collect(),
    }
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
                rule: rule_of(e)?,
                enabled: e.enabled,
            })
        })
        .collect();
    rules::resolve(rows)
        .entries()
        .filter_map(|(metric, instance, kind, rule)| {
            let of = |server: bool| {
                entries.iter().find(|e| {
                    e.server_id.is_some() == server
                        && e.metric == metric
                        && e.instance.as_deref() == instance
                        && e.kind == kind.as_str()
                })
            };
            let (origin, row) = match (of(true), of(false)) {
                (Some(e), _) => (Origin::Server, Some(e)),
                (None, Some(e)) => (Origin::Site, Some(e)),
                (None, None) => (Origin::Default, None),
            };
            // An enabled key shows the rule in force, a muted one the row
            // that mutes it; only rows mute, so a muted key has one.
            match (rule, row) {
                (Some(rule), _) => Some(view(origin, row.map(|e| e.id), rule, true)),
                (None, Some(e)) => Some(view(origin, Some(e.id), &rule_of(e)?, false)),
                (None, None) => None,
            }
        })
        .collect()
}

/// The disk forecast ADR-0153 suggests: warn 3 days before a disk is
/// full, critical 1 day before, learned over a day.
pub fn disk_forecast() -> EntryRequest {
    EntryRequest {
        metric: "monitorflow_disk_used_ratio".into(),
        instance: None,
        kind: Some("forecast".into()),
        comparison: Some(">=".into()),
        warning: Some(3.0 * 86_400.0),
        critical: Some(86_400.0),
        hysteresis: 3_600.0,
        window_s: Some(86_400),
        target: Some(1.0),
        ..EntryRequest::default()
    }
}

/// What to suggest given the rows of a site (and its server): the disk
/// forecast, until a row of any instance has a forecast on disks, added
/// or declined.
fn recommended(entries: &[Entry]) -> Vec<RecommendedThreshold> {
    let forecast = disk_forecast();
    let has = entries
        .iter()
        .any(|e| e.metric == forecast.metric && e.kind == "forecast");
    if has {
        return Vec::new();
    }
    vec![RecommendedThreshold {
        key: "disk_forecast".into(),
        rule: forecast,
    }]
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
            kind: "value".into(),
            function: None,
            window_s: None,
            offset_s: None,
            target: None,
            conditions: sqlx::types::Json(Vec::new()),
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
                (
                    "monitorflow_diskio_wait_seconds".into(),
                    None,
                    Origin::Default,
                    None,
                    true,
                    Some(0.2)
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

    #[test]
    fn richer_kinds_take_exactly_their_fields() {
        let ok = [
            json!({"metric": DISK, "kind": "window", "function": "avg", "window_s": 900,
                   "comparison": ">", "warning": 0.8}),
            json!({"metric": DISK, "kind": "compare", "function": "ratio", "window_s": 3600,
                   "offset_s": 604800, "comparison": ">", "warning": 2}),
            json!({"metric": DISK, "kind": "forecast", "window_s": 86400, "target": 1,
                   "comparison": ">=", "warning": 259200, "critical": 86400}),
            json!({"metric": DISK, "kind": "nodata", "warning": 300, "critical": 900}),
            json!({"metric": "monitorflow_cpu_usage_ratio", "instance": "total", "comparison": ">",
                   "warning": 0.9, "conditions": [
                       {"metric": "monitorflow_cpu_load1", "comparison": ">", "value": 8},
                       {"metric": "monitorflow_memory_available_ratio", "function": "min",
                        "window_s": 300, "comparison": "<", "value": 0.1}]}),
        ];
        for body in ok {
            let valid = request(body.clone()).unwrap_or_else(|e| panic!("{body}: {e}"));
            // What the API keeps is what the engine reads back.
            let rule = valid.rule();
            assert_eq!(rule.kind.as_str(), body["kind"].as_str().unwrap_or("value"));
        }
        let nodata = request(json!({"metric": DISK, "kind": "nodata", "warning": 300})).unwrap();
        assert_eq!(nodata.comparison, Comparison::Gt);
        let cases = [
            (
                json!({"metric": DISK, "kind": "median", "comparison": ">", "warning": 1}),
                "kind must be",
            ),
            (
                json!({"metric": DISK, "comparison": ">", "warning": 1, "window_s": 600}),
                "window_s is not used by a `value` threshold",
            ),
            (
                json!({"metric": DISK, "kind": "window", "window_s": 600, "comparison": ">", "warning": 1}),
                "needs a function",
            ),
            (
                json!({"metric": DISK, "kind": "window", "function": "ratio", "window_s": 600,
                    "comparison": ">", "warning": 1}),
                "needs a function",
            ),
            (
                json!({"metric": DISK, "kind": "window", "function": "avg", "window_s": 30,
                    "comparison": ">", "warning": 1}),
                "window_s must be between 60 and 86400",
            ),
            (
                json!({"metric": DISK, "kind": "compare", "function": "ratio", "window_s": 600,
                    "offset_s": 3600, "comparison": ">", "warning": 1}),
                "offset_s must be",
            ),
            (
                json!({"metric": DISK, "kind": "forecast", "window_s": 86400, "comparison": ">=",
                    "warning": 259200}),
                "needs a finite target",
            ),
            (
                json!({"metric": DISK, "kind": "forecast", "window_s": 86400, "target": 1,
                    "comparison": ">=", "warning": 3600, "critical": 7200}),
                "critical must not be more time left than warning",
            ),
            (
                json!({"metric": DISK, "kind": "forecast", "window_s": 86400, "target": 1,
                    "comparison": ">=", "warning": 10}),
                "between 60 and 7776000 seconds",
            ),
            (
                json!({"metric": DISK, "kind": "nodata", "warning": 900, "critical": 300}),
                "critical must not be a shorter silence",
            ),
            (
                json!({"metric": DISK, "kind": "nodata", "comparison": "<", "warning": 300}),
                "compares with `>`",
            ),
            (
                json!({"metric": DISK, "kind": "nodata", "warning": 300, "duration_s": 60}),
                "duration_s is not used",
            ),
            (
                json!({"metric": DISK, "kind": "nodata", "warning": 300, "conditions": [
                {"metric": DISK, "comparison": ">", "value": 1}]}),
                "conditions is not used",
            ),
            (
                json!({"metric": DISK, "comparison": ">", "warning": 1, "conditions": [
                {"metric": DISK, "comparison": ">", "value": 1},
                {"metric": DISK, "comparison": ">", "value": 1},
                {"metric": DISK, "comparison": ">", "value": 1}]}),
                "at most 2 conditions",
            ),
            (
                json!({"metric": DISK, "comparison": ">", "warning": 1, "conditions": [
                {"metric": "cpu", "comparison": ">", "value": 1}]}),
                "conditions[0]: metric must be",
            ),
            (
                json!({"metric": DISK, "comparison": ">", "warning": 1, "conditions": [
                {"metric": DISK, "function": "avg", "comparison": ">", "value": 1}]}),
                "conditions[0]: window_s must be",
            ),
            (
                json!({"metric": DISK, "comparison": ">", "warning": 1, "conditions": [
                {"metric": DISK, "window_s": 60, "comparison": ">", "value": 1}]}),
                "conditions[0]: window_s needs a function",
            ),
            (
                json!({"metric": DISK, "warning": 1}),
                "comparison is required",
            ),
        ];
        for (body, expected) in cases {
            let error = request(body.clone()).unwrap_err();
            assert!(error.contains(expected), "{body}: {error}");
        }
    }

    #[test]
    fn the_disk_forecast_is_offered_until_a_row_takes_it() {
        assert_eq!(recommended(&[])[0].key, "disk_forecast");
        assert!(validate_entry(disk_forecast()).is_ok());
        let mut declined = entry(false, Some("/"), None, false);
        declined.kind = "forecast".into();
        assert!(recommended(&[declined]).is_empty());
        // A plain disk threshold does not count.
        assert_eq!(recommended(&[entry(false, None, Some(0.8), true)]).len(), 1);
        // The effective view lists kinds side by side.
        let mut forecast = entry(false, None, Some(259_200.0), true);
        forecast.kind = "forecast".into();
        forecast.comparison = ">=".into();
        forecast.window_s = Some(86_400);
        forecast.target = Some(1.0);
        let view = effective_thresholds(&[forecast.clone()]);
        let disk: Vec<_> = view.iter().filter(|t| t.metric == DISK).collect();
        assert_eq!(disk.len(), 2);
        assert_eq!(disk[1].kind, "forecast");
        assert_eq!(disk[1].threshold_id, Some(forecast.id));
        assert_eq!(disk[1].target, Some(1.0));
    }
}
