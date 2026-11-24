//! Custom SNMP checks of a device as one structured body (ADR-0157): its
//! channels with their limits, written as a `sensor_configs` row of kind
//! `snmp_custom` and the device's thresholds in one transaction; read
//! back with each channel's last values. Also "Test" and "Browse": a
//! read of OIDs or a walk from the device's probe, with its credentials.

use std::collections::{BTreeMap, BTreeSet};

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use monitorflow_snmp::custom::{self, Custom, ValueKind};
use monitorflow_snmp::Val;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sqlx::PgConnection;
use uuid::Uuid;

use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::devices::{device_site, probe_for, run_once};
use super::metrics::{self, backend_error, quote, Target, MAX_LOOKBACK, METRIC_PREFIX};
use super::sensors::{body, check_lookups, check_run_by, parse_id, run_by, EntryRequest, Owner};
use super::snmp_library::{name_oids, readable_customers};
use super::{ApiError, ApiState};
use crate::snmp;

/// The longest metric name after `monitorflow_`, as thresholds take it.
const MAX_METRIC: usize = 64;

/// Limits of one channel: an ordinary threshold of the device (ADR-0017).
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, utoipa::ToSchema)]
#[schema(as = SnmpChannelLimits)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    /// `>` (the default), `>=`, `<` or `<=`: the alarm side.
    #[serde(default = "above")]
    pub comparison: String,
    pub warning: Option<f64>,
    pub critical: Option<f64>,
    /// Seconds past the limit before an incident opens, 0 by default.
    #[serde(default)]
    pub duration_s: u32,
}

fn above() -> String {
    ">".into()
}

/// One channel of a request.
#[derive(Debug, Clone, Deserialize, Serialize, utoipa::ToSchema)]
#[schema(as = SnmpChannelRequest)]
#[serde(deny_unknown_fields)]
pub struct ChannelRequest {
    /// A numeric OID: an object (`...8.0`), or a column of `table`.
    pub oid: String,
    /// What people read: "Sessions".
    pub name: String,
    /// The metric suffix; from the name when absent.
    #[serde(default)]
    pub key: Option<String>,
    /// `gauge` (the default), `counter` or `text`.
    #[serde(default)]
    pub r#type: Option<String>,
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub multiply: Option<f64>,
    #[serde(default)]
    pub divide: Option<f64>,
    /// `builtin:<name>` or a lookup's ID.
    #[serde(default)]
    pub lookup: Option<String>,
    /// For a text: `contains`, `not_contains`, `equals`, `not_equals`,
    /// `matches` or `not_matches`, with `expect` and `severity`.
    #[serde(default)]
    pub r#match: Option<String>,
    #[serde(default)]
    pub expect: Option<String>,
    /// `warning` or `critical` (the default).
    #[serde(default)]
    pub severity: Option<String>,
    /// The channel's limits on its value; absent or `null` for none. A
    /// channel with a lookup or an expected text also alerts on its state.
    #[serde(default)]
    pub limits: Option<Limits>,
}

/// Body of `POST /api/devices/{id}/snmp-checks` and of `PUT` and `test`.
#[derive(Debug, Clone, Deserialize, Serialize, utoipa::ToSchema)]
#[schema(as = SnmpCheckRequest)]
#[serde(deny_unknown_fields)]
pub struct CheckRequest {
    /// Lowercase letters, digits and `_`, like `snmp_fw`; a change keeps
    /// the name of the path.
    pub name: String,
    /// `central` or a relay of the site; absent: the site's default.
    #[serde(default)]
    pub probe: Option<String>,
    /// 60000 (a minute) by default.
    #[serde(default)]
    pub interval_ms: Option<u64>,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// For a table: its OID (the table or its entry), and the column whose
    /// values name the rows.
    #[serde(default)]
    pub table: Option<String>,
    #[serde(default)]
    pub name_column: Option<String>,
    /// 1 to 10 channels.
    pub channels: Vec<ChannelRequest>,
}

fn yes() -> bool {
    true
}

/// One value a channel wrote, by row.
#[derive(Debug, Clone, Serialize, PartialEq, utoipa::ToSchema)]
#[schema(as = SnmpChannelValue)]
pub struct ChannelValue {
    /// The table's row; `""` for one object.
    pub instance: String,
    pub value: Option<f64>,
    /// 0 OK, 1 warning, 2 critical, for a text or a lookup.
    pub state: Option<u8>,
    /// The value's name or the text.
    pub text: Option<String>,
    pub at: Option<DateTime<Utc>>,
}

/// One channel as stored, with its limits and last values.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[schema(as = SnmpChannel)]
pub struct ChannelView {
    #[serde(flatten)]
    pub channel: ChannelRequest,
    pub key: String,
    /// The series of its value, and of its state when it has one.
    pub metric: String,
    pub state_metric: Option<String>,
    pub values: Vec<ChannelValue>,
}

/// One custom check of a device.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[schema(as = SnmpCheck)]
pub struct CheckView {
    pub entry_id: Uuid,
    pub name: String,
    pub instance: String,
    /// `central` or `relay`, with `probe_relay_id`.
    pub probe: Option<String>,
    pub probe_relay_id: Option<Uuid>,
    pub interval_ms: i64,
    pub enabled: bool,
    pub table: Option<String>,
    pub name_column: Option<String>,
    pub channels: Vec<ChannelView>,
}

/// Body of `GET /api/devices/{id}/snmp-checks`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = SnmpCheckList)]
pub struct CheckList {
    pub checks: Vec<CheckView>,
}

/// A request as row parameters, numbered from 1.
pub fn params_of(request: &CheckRequest) -> Result<Map<String, Value>, String> {
    if request.channels.is_empty() || request.channels.len() > custom::MAX_CHANNELS {
        return Err(format!(
            "A check has 1 to {} channels.",
            custom::MAX_CHANNELS
        ));
    }
    let mut params = Map::new();
    let mut kept: Vec<(String, String)> = Vec::new();
    // Empty values are left out, but an empty expected text of `equals`.
    let mut put = |key: String, value: Option<String>| {
        if let Some(value) = value.filter(|v| !v.trim().is_empty() || key.starts_with("expect")) {
            kept.push((key, value));
        }
    };
    put("table".into(), request.table.clone());
    put("name_column".into(), request.name_column.clone());
    for (i, c) in request.channels.iter().enumerate() {
        let n = i + 1;
        let factor = |f: Option<f64>| f.filter(|f| *f != 1.0).map(|f| f.to_string());
        put(format!("oid{n}"), Some(c.oid.clone()));
        put(format!("name{n}"), Some(c.name.clone()));
        put(format!("key{n}"), c.key.clone());
        put(
            format!("type{n}"),
            c.r#type.clone().filter(|t| t != "gauge"),
        );
        put(format!("unit{n}"), c.unit.clone());
        put(format!("multiply{n}"), factor(c.multiply));
        put(format!("divide{n}"), factor(c.divide));
        put(format!("lookup{n}"), c.lookup.clone());
        put(format!("match{n}"), c.r#match.clone());
        put(format!("severity{n}"), c.severity.clone());
        if c.r#match.is_some() {
            put(
                format!("expect{n}"),
                Some(c.expect.clone().unwrap_or_default()),
            );
        }
    }
    for (key, value) in kept {
        params.insert(key, Value::String(value));
    }
    Ok(params)
}

/// The channels of stored parameters, as a request names them.
fn channels_of(params: &BTreeMap<String, String>) -> Vec<ChannelRequest> {
    let get = |k: String| params.get(&k).cloned().filter(|v| !v.is_empty());
    let factor = |k: String| params.get(&k).and_then(|v| v.parse().ok());
    (1..=custom::MAX_CHANNELS)
        .filter_map(|n| {
            Some(ChannelRequest {
                oid: get(format!("oid{n}"))?,
                name: get(format!("name{n}")).unwrap_or_else(|| format!("Channel {n}")),
                key: get(format!("key{n}")),
                r#type: get(format!("type{n}")),
                unit: get(format!("unit{n}")),
                multiply: factor(format!("multiply{n}")),
                divide: factor(format!("divide{n}")),
                lookup: get(format!("lookup{n}")),
                r#match: get(format!("match{n}")),
                expect: params.get(&format!("expect{n}")).cloned(),
                severity: get(format!("severity{n}")),
                limits: None,
            })
        })
        .collect()
}

/// The metric names of a check's channels: the value's (none for a text)
/// and the state's (with a lookup, an expected text, or for a text).
fn metrics_of(name: &str, custom: &Custom) -> Vec<(Option<String>, Option<String>)> {
    custom
        .channels
        .iter()
        .map(|c| {
            let value =
                (c.kind != ValueKind::Text).then(|| format!("{METRIC_PREFIX}{name}_{}", c.key));
            let state = (c.kind == ValueKind::Text || c.lookup.is_some() || c.expect.is_some())
                .then(|| format!("{METRIC_PREFIX}{name}_{}_state", c.key));
            (value, state)
        })
        .collect()
}

/// A request checked: the row's parameters, the channels as the probe
/// reads them, and each one's limits.
#[derive(Debug)]
struct Valid {
    params: Map<String, Value>,
    custom: Custom,
    limits: Vec<Option<Limits>>,
}

fn validate(request: &CheckRequest, address: Option<&str>) -> Result<Valid, String> {
    monitorflow_sensors::validate::sensor_name(&request.name).map_err(|e| format!("name: {e}"))?;
    let params = params_of(request)?;
    let flat = crate::sensor_config::params(Value::Object(params.clone()));
    let custom = crate::snmp_custom::check_row(&flat, address)?;
    for (value, state) in metrics_of(&request.name, &custom) {
        for metric in value.iter().chain(&state) {
            if metric.len() - METRIC_PREFIX.len() > MAX_METRIC {
                return Err(format!(
                    "`{metric}` is too long: shorten the check's name or the channel's key."
                ));
            }
        }
    }
    let limits: Vec<Option<Limits>> = request.channels.iter().map(|c| c.limits.clone()).collect();
    for (channel, limits) in custom.channels.iter().zip(&limits) {
        let Some(limits) = limits else { continue };
        if channel.kind == ValueKind::Text {
            return Err(format!(
                "`{}` is a text: it alerts on what it expects, not on limits.",
                channel.name
            ));
        }
        super::thresholds::validate_entry(threshold_request("monitorflow_x", limits))
            .map_err(|e| format!("Limits of `{}`: {e}", channel.name))?;
    }
    Ok(Valid {
        params,
        custom,
        limits,
    })
}

fn threshold_request(metric: &str, limits: &Limits) -> super::thresholds::EntryRequest {
    serde_json::from_value(json!({
        "metric": metric, "comparison": limits.comparison, "warning": limits.warning,
        "critical": limits.critical, "duration_s": limits.duration_s
    }))
    .unwrap_or_else(|_| unreachable!("a fixed shape"))
}

/// Writes the thresholds of `valid`'s channels on `device`: each value's
/// limits replace its rule for every instance, and each state gets its
/// warning at 1 and critical at 2 unless it has a rule already.
async fn write_thresholds(
    db: &mut PgConnection,
    device: Uuid,
    name: &str,
    valid: &Valid,
    before: &[String],
) -> Result<(), sqlx::Error> {
    let wanted = metrics_of(name, &valid.custom);
    let now: BTreeSet<&String> = wanted.iter().flat_map(|(v, s)| v.iter().chain(s)).collect();
    // The channels that went: their rules for every instance go too.
    let gone: Vec<&String> = before.iter().filter(|m| !now.contains(m)).collect();
    sqlx::query(
        "DELETE FROM thresholds WHERE server_id = $1 AND metric = ANY($2) AND instance IS NULL AND kind = 'value'",
    )
    .bind(device)
    .bind(&gone)
    .execute(&mut *db)
    .await?;
    for ((value, state), limits) in wanted.iter().zip(&valid.limits) {
        if let Some(metric) = value {
            sqlx::query(
                "DELETE FROM thresholds WHERE server_id = $1 AND metric = $2 AND instance IS NULL AND kind = 'value'",
            )
            .bind(device)
            .bind(metric)
            .execute(&mut *db)
            .await?;
            if let Some(l) = limits {
                sqlx::query(
                    "INSERT INTO thresholds (server_id, metric, comparison, warning, critical, duration_s)
                     VALUES ($1, $2, $3, $4, $5, $6)",
                )
                .bind(device)
                .bind(metric)
                .bind(&l.comparison)
                .bind(l.warning)
                .bind(l.critical)
                .bind(i32::try_from(l.duration_s).unwrap_or(0))
                .execute(&mut *db)
                .await?;
            }
        }
        if let Some(metric) = state {
            sqlx::query(
                "INSERT INTO thresholds (server_id, metric, comparison, warning, critical, duration_s)
                 SELECT $1, $2, '>=', 1, 2, 0
                 WHERE NOT EXISTS (SELECT 1 FROM thresholds WHERE server_id = $1 AND metric = $2
                                   AND instance IS NULL AND kind = 'value')",
            )
            .bind(device)
            .bind(metric)
            .execute(&mut *db)
            .await?;
        }
    }
    Ok(())
}

type Row = (
    Uuid,
    String,
    String,
    Option<String>,
    Option<Uuid>,
    i64,
    bool,
    Value,
);

const SELECT: &str =
    "SELECT id, name, instance, probe, probe_relay_id, interval_ms, enabled, params
    FROM sensor_configs WHERE server_id = $1 AND kind = 'snmp_custom'";

/// The device's address, peer and site name, for checks and metrics.
async fn device(
    db: &mut PgConnection,
    id: Uuid,
) -> Result<(Option<String>, String, String, Uuid), sqlx::Error> {
    sqlx::query_as(
        "SELECT s.address, s.peer_id, si.name, si.customer_id FROM servers s JOIN sites si ON si.id = s.site_id
         WHERE s.id = $1",
    )
    .bind(id)
    .fetch_one(&mut *db)
    .await
}

async fn record(
    db: &mut PgConnection,
    user: &CurrentUser,
    action: &str,
    device: Uuid,
    details: Value,
) -> Result<(), sqlx::Error> {
    let actor = user.id.to_string();
    audit::record(
        &mut *db,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action,
            target: Some(("server", &device.to_string())),
            details,
        },
    )
    .await
}

fn audited(name: &str, valid: &Valid) -> Value {
    json!({
        "name": name,
        "table": valid.params.get("table"),
        "channels": valid.custom.channels.iter().map(|c| json!({
            "key": c.key, "oid": custom::oid_text(&c.oid), "type": c.kind.as_str()
        })).collect::<Vec<_>>(),
    })
}

/// Stores `request` for `id` in `tx`: a new row, or the row `existing`.
async fn save(
    tx: &mut PgConnection,
    user: &CurrentUser,
    id: Uuid,
    request: CheckRequest,
    existing: Option<(Uuid, BTreeMap<String, String>)>,
) -> Result<Uuid, ApiError> {
    let site = device_site(tx, user, id, true).await?;
    let (address, _, _, _) = device(tx, id).await?;
    let valid = validate(&request, address.as_deref()).map_err(ApiError::Invalid)?;
    let by = run_by(tx, Owner::Server(id), request.probe.as_deref()).await?;
    let entry = EntryRequest {
        kind: monitorflow_snmp::CUSTOM_KIND.into(),
        name: Some(request.name.clone()),
        instance: String::new(),
        interval_ms: request.interval_ms.unwrap_or(60_000),
        timeout_ms: None,
        params: valid.params.clone(),
        enabled: request.enabled,
        probe: request.probe.clone(),
        default_alerts: false,
    };
    let mut row = super::sensors::validate_with_host(entry, by.address.as_deref())
        .map_err(ApiError::Invalid)?;
    check_run_by(&mut row, &by)?;
    check_lookups(tx, &row, Some(site)).await?;
    let before: Vec<String> = match &existing {
        Some((_, params)) => crate::snmp_custom::check_row(params, address.as_deref())
            .map(|c| {
                metrics_of(&request.name, &c)
                    .into_iter()
                    .flat_map(|(v, s)| v.into_iter().chain(s))
                    .collect()
            })
            .unwrap_or_default(),
        None => Vec::new(),
    };
    let entry_id: Uuid = match &existing {
        None => sqlx::query_scalar(
            "INSERT INTO sensor_configs (server_id, kind, name, instance, interval_ms, params, enabled, probe, probe_relay_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
        )
        .bind(id)
        .bind(&row.kind)
        .bind(&row.name)
        .bind(&row.instance)
        .bind(row.interval_ms)
        .bind(&row.params)
        .bind(row.enabled)
        .bind(by.probe)
        .bind(by.relay)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(d) if d.constraint() == Some("sensor_configs_key") => {
                ApiError::Conflict(format!("The device has a check named {} already.", row.name))
            }
            _ => e.into(),
        })?,
        Some((entry, _)) => sqlx::query_scalar(
            "UPDATE sensor_configs SET interval_ms = $2, params = $3, enabled = $4, probe = $5,
                    probe_relay_id = $6, instance = $7, updated_at = now()
             WHERE id = $1 RETURNING id",
        )
        .bind(entry)
        .bind(row.interval_ms)
        .bind(&row.params)
        .bind(row.enabled)
        .bind(by.probe)
        .bind(by.relay)
        .bind(&row.instance)
        .fetch_one(&mut *tx)
        .await?,
    };
    write_thresholds(tx, id, &request.name, &valid, &before).await?;
    let action = if existing.is_some() {
        "snmp_check.update"
    } else {
        "snmp_check.create"
    };
    record(tx, user, action, id, audited(&request.name, &valid)).await?;
    Ok(entry_id)
}

/// `POST /api/devices/{id}/snmp-checks`: 201 with the check.
pub async fn create(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<CheckRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<CheckView>), ApiError> {
    let id = parse_id(&id)?;
    let request = body(request)?;
    let name = request.name.clone();
    let mut tx = state.pool.begin().await?;
    save(&mut tx, &user, id, request, None).await?;
    tx.commit().await?;
    let view = one(&state, id, &name).await?;
    Ok((StatusCode::CREATED, Json(view)))
}

async fn lock(
    tx: &mut PgConnection,
    id: Uuid,
    name: &str,
) -> Result<(Uuid, BTreeMap<String, String>), ApiError> {
    let row: Option<(Uuid, Value)> = sqlx::query_as(
        "SELECT id, params FROM sensor_configs WHERE server_id = $1 AND kind = 'snmp_custom' AND name = $2
         ORDER BY instance LIMIT 1 FOR UPDATE",
    )
    .bind(id)
    .bind(name)
    .fetch_optional(&mut *tx)
    .await?;
    let (entry, params) = row.ok_or(ApiError::NotFound)?;
    Ok((entry, crate::sensor_config::params(params)))
}

/// `PUT /api/devices/{id}/snmp-checks/{name}`: replaces the check and its
/// channels' limits.
pub async fn update(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path((id, name)): Path<(String, String)>,
    request: Result<Json<CheckRequest>, JsonRejection>,
) -> Result<Json<CheckView>, ApiError> {
    let id = parse_id(&id)?;
    let mut request = body(request)?;
    request.name = name.clone();
    let mut tx = state.pool.begin().await?;
    device_site(&mut tx, &user, id, true).await?;
    let existing = lock(&mut tx, id, &name).await?;
    save(&mut tx, &user, id, request, Some(existing)).await?;
    tx.commit().await?;
    Ok(Json(one(&state, id, &name).await?))
}

/// `DELETE /api/devices/{id}/snmp-checks/{name}`: the check and the rules
/// of its channels for every instance.
pub async fn remove(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path((id, name)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let id = parse_id(&id)?;
    let mut tx = state.pool.begin().await?;
    device_site(&mut tx, &user, id, true).await?;
    let (entry, params) = lock(&mut tx, id, &name).await?;
    let metrics: Vec<String> = crate::snmp_custom::check_row(&params, Some("192.0.2.1"))
        .map(|c| {
            metrics_of(&name, &c)
                .into_iter()
                .flat_map(|(v, s)| v.into_iter().chain(s))
                .collect()
        })
        .unwrap_or_default();
    sqlx::query("DELETE FROM sensor_configs WHERE id = $1")
        .bind(entry)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM thresholds WHERE server_id = $1 AND metric = ANY($2) AND instance IS NULL AND kind = 'value'")
        .bind(id)
        .bind(&metrics)
        .execute(&mut *tx)
        .await?;
    record(
        &mut tx,
        &user,
        "snmp_check.delete",
        id,
        json!({"name": name}),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The newest value, time and `text` of each `(metric, instance)`.
type Newest = BTreeMap<(String, String), (f64, f64, Option<String>)>;

async fn last_values(state: &ApiState, peer: String, site: String, metrics: &[String]) -> Newest {
    let Some(vm) = state.metrics.clone() else {
        return Newest::new();
    };
    if metrics.is_empty() {
        return Newest::new();
    }
    let target = Target { peer, site };
    // Names are `[a-z0-9_]`, checked when saved: no escaping needed.
    let selector = format!(
        "{{__name__=~{},{}}}",
        quote(&metrics.join("|")),
        target.matchers()
    );
    let now = Utc::now().timestamp().to_string();
    let mut results = Vec::new();
    for function in ["last_over_time", "tlast_over_time"] {
        let params: Vec<(&str, String)> = [
            (
                "query",
                format!("{function}({selector}[{MAX_LOOKBACK}s]) keep_metric_names"),
            ),
            ("time", now.clone()),
        ]
        .into_iter()
        .chain(target.extra_labels())
        .collect();
        match vm
            .get("/api/v1/query", &params)
            .await
            .map_err(backend_error)
            .and_then(metrics::series)
        {
            Ok(series) => results.push(series),
            Err(error) => {
                tracing::debug!(?error, "no last values for custom SNMP checks");
                return Newest::new();
            }
        }
    }
    let times = results.pop().unwrap_or_default();
    let values = results.pop().unwrap_or_default();
    let last = |s: &metrics::SeriesView| s.points.last().and_then(|p| p.1);
    let times: BTreeMap<BTreeMap<String, String>, f64> = times
        .iter()
        .filter_map(|s| Some((s.labels.clone(), last(s)?)))
        .collect();
    let mut newest = Newest::new();
    for series in values {
        let (Some(value), Some(&at)) = (last(&series), times.get(&series.labels)) else {
            continue;
        };
        let key = (
            series.labels.get("__name__").cloned().unwrap_or_default(),
            series.labels.get("instance").cloned().unwrap_or_default(),
        );
        if newest.get(&key).is_none_or(|(_, before, _)| at > *before) {
            newest.insert(key, (value, at, series.labels.get("text").cloned()));
        }
    }
    newest
}

fn view(row: Row, limits: &BTreeMap<String, Limits>, newest: &Newest) -> CheckView {
    let (entry_id, name, instance, probe, probe_relay_id, interval_ms, enabled, raw) = row;
    let params = crate::sensor_config::params(raw);
    let custom = crate::snmp_custom::check_row(&params, Some("192.0.2.1")).ok();
    let table = params.get("table").cloned();
    let mut channels = Vec::new();
    for (i, mut request) in channels_of(&params).into_iter().enumerate() {
        let parsed = custom.as_ref().and_then(|c| c.channels.get(i));
        let key = parsed.map_or_else(
            || request.key.clone().unwrap_or_default(),
            |c| c.key.clone(),
        );
        let metric = format!("{METRIC_PREFIX}{name}_{key}");
        let state_metric = custom
            .as_ref()
            .and_then(|c| metrics_of(&name, c).get(i).cloned())
            .and_then(|(_, s)| s);
        request.limits = limits.get(&metric).cloned();
        // A table's rows, or the check's own instance.
        let mut rows: BTreeSet<String> = newest
            .keys()
            .filter(|(m, _)| m == &metric || Some(m) == state_metric.as_ref())
            .map(|(_, i)| i.clone())
            .collect();
        if table.is_none() {
            rows.retain(|r| r == &instance);
        }
        let values = rows
            .into_iter()
            .map(|row| {
                let value = newest.get(&(metric.clone(), row.clone()));
                let state = state_metric
                    .as_ref()
                    .and_then(|s| newest.get(&(s.clone(), row.clone())));
                ChannelValue {
                    value: value.map(|v| v.0),
                    state: state.map(|s| s.0.clamp(0.0, 2.0) as u8),
                    text: state.and_then(|s| s.2.clone()),
                    at: value
                        .or(state)
                        .and_then(|v| DateTime::from_timestamp(v.1 as i64, 0)),
                    instance: if table.is_some() { row } else { String::new() },
                }
            })
            .collect();
        channels.push(ChannelView {
            channel: request,
            key,
            metric,
            state_metric,
            values,
        });
    }
    CheckView {
        entry_id,
        instance,
        probe,
        probe_relay_id,
        interval_ms,
        enabled,
        name_column: params.get("name_column").cloned(),
        table,
        name,
        channels,
    }
}

async fn views(state: &ApiState, id: Uuid, only: Option<&str>) -> Result<Vec<CheckView>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    // SQL-safe: interpolates only the constant SELECT.
    let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{SELECT} AND ($2::text IS NULL OR name = $2) ORDER BY name, instance"
    )))
    .bind(id)
    .bind(only)
    .fetch_all(&mut *conn)
    .await?;
    type Rule = (String, String, Option<f64>, Option<f64>, i32);
    let limits: Vec<Rule> = sqlx::query_as(
        "SELECT metric, comparison, warning, critical, duration_s FROM thresholds
         WHERE server_id = $1 AND instance IS NULL AND kind = 'value' AND enabled",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    let limits: BTreeMap<String, Limits> = limits
        .into_iter()
        .map(|(metric, comparison, warning, critical, duration)| {
            (
                metric,
                Limits {
                    comparison,
                    warning,
                    critical,
                    duration_s: u32::try_from(duration).unwrap_or(0),
                },
            )
        })
        .collect();
    let (_, peer, site, _) = device(&mut conn, id).await?;
    let metrics: Vec<String> = rows
        .iter()
        .flat_map(|r| {
            let params = crate::sensor_config::params(r.7.clone());
            crate::snmp_custom::check_row(&params, Some("192.0.2.1"))
                .map(|c| {
                    metrics_of(&r.1, &c)
                        .into_iter()
                        .flat_map(|(v, s)| v.into_iter().chain(s))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
        .collect();
    drop(conn);
    let newest = last_values(state, peer, site, &metrics).await;
    Ok(rows
        .into_iter()
        .map(|r| view(r, &limits, &newest))
        .collect())
}

async fn one(state: &ApiState, id: Uuid, name: &str) -> Result<CheckView, ApiError> {
    views(state, id, Some(name))
        .await?
        .pop()
        .ok_or(ApiError::NotFound)
}

/// `GET /api/devices/{id}/snmp-checks`.
pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<CheckList>, ApiError> {
    let id = parse_id(&id)?;
    let mut conn = state.pool.acquire().await?;
    device_site(&mut conn, &user, id, false).await?;
    drop(conn);
    Ok(Json(CheckList {
        checks: views(&state, id, None).await?,
    }))
}

/// Body of `POST /api/devices/{id}/snmp/read`.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[schema(as = SnmpReadRequest)]
#[serde(deny_unknown_fields)]
pub struct ReadRequest {
    /// Up to 20 OIDs to read ("Test").
    #[serde(default)]
    pub oids: Vec<String>,
    /// Or an OID whose subtree to walk ("Browse").
    #[serde(default)]
    pub walk: Option<String>,
    /// For a walk: 500 objects by default, 2000 at most.
    #[serde(default)]
    pub max: Option<u32>,
    /// `central` or a relay of the site; absent: the site's default.
    #[serde(default)]
    pub probe: Option<String>,
}

/// One object a device answered.
#[derive(Debug, Clone, Serialize, PartialEq, utoipa::ToSchema)]
#[schema(as = SnmpReadObject)]
pub struct ReadObject {
    pub oid: String,
    /// What a MIB or the library calls it, like `ifInOctets.3`.
    pub name: Option<String>,
    /// `Integer`, `OctetString`, `Counter64`, `TimeTicks`..., or
    /// `noSuchObject` when the device has none.
    pub r#type: String,
    /// The value as text: printable text, or hexadecimal bytes.
    pub value: String,
}

/// Body of the answer of a read.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = SnmpRead)]
pub struct ReadResult {
    /// `central` or the relay that read.
    pub probe: String,
    pub objects: Vec<ReadObject>,
    /// Whether a walk stopped at `max`.
    pub cut: bool,
}

/// What a read gave as `(oid, type, value)`, and whether it was cut.
fn read_rows(
    samples: &[monitorflow_common::proto::v1::Sample],
) -> (Vec<(String, String, String)>, bool) {
    let cut = samples.iter().any(|s| s.sensor.ends_with("_cut"));
    let rows = samples
        .iter()
        .filter(|s| s.sensor.ends_with("_object"))
        .map(|s| {
            (
                s.instance.clone(),
                s.labels.get("type").cloned().unwrap_or_default(),
                s.labels.get("value").cloned().unwrap_or_default(),
            )
        })
        .collect();
    (rows, cut)
}

/// Runs a `snmp_read` of `params` on `id`'s probe, with its credentials.
async fn read_on(
    state: &ApiState,
    user: &CurrentUser,
    id: Uuid,
    probe: Option<&str>,
    mut params: BTreeMap<String, String>,
) -> Result<(String, Vec<(String, String, String)>, bool, Uuid), ApiError> {
    let mut conn = state.pool.acquire().await?;
    let site = device_site(&mut conn, user, id, true).await?;
    let (address, _, _, customer) = device(&mut conn, id).await?;
    let credentials = snmp::load(&mut conn, id)
        .await?
        .ok_or_else(|| ApiError::Invalid("Set the device's SNMP credentials first.".into()))?
        .map_err(|e| {
            ApiError::Invalid(format!("The device's SNMP credentials do not open: {e}"))
        })?;
    drop(conn);
    // Where the device is checked from, unless told: its SNMP checks'
    // probe, else the site's default.
    let probe = match probe {
        Some(p) => Some(p.to_owned()),
        None => {
            let mut conn = state.pool.acquire().await?;
            let row: Option<(String, Option<Uuid>)> = sqlx::query_as(
                "SELECT probe, probe_relay_id FROM sensor_configs
                 WHERE server_id = $1 AND probe IS NOT NULL AND kind IN ('snmp', 'snmp_custom')
                 ORDER BY kind = 'snmp_custom' DESC, created_at LIMIT 1",
            )
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?;
            row.map(|(probe, relay)| match relay {
                Some(r) if probe == "relay" => r.to_string(),
                _ => probe,
            })
        }
    };
    let by = probe_for(state, site, probe.as_deref()).await?;
    params.extend(credentials.params());
    params.insert(
        "host".into(),
        address.ok_or_else(|| ApiError::Invalid("The device has no address.".into()))?,
    );
    monitorflow_snmp::parse_read(&params).map_err(ApiError::Invalid)?;
    let mut config =
        monitorflow_sensors::SensorConfig::new("snmp", std::time::Duration::from_secs(60));
    config.timeout = Some(std::time::Duration::from_secs(25));
    config.params = params;
    let check = monitorflow_probe::Check {
        device: "read".into(),
        kind: monitorflow_snmp::READ_KIND.into(),
        config,
    };
    let samples = run_once(state, &by, check).await?;
    let (rows, cut) = read_rows(&samples);
    Ok((by.key, rows, cut, customer))
}

/// `POST /api/devices/{id}/snmp/read`: Test and Browse. Operators of the
/// site, as it sends the device's credentials to its probe; not audited.
pub async fn read(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<ReadRequest>, JsonRejection>,
) -> Result<Json<ReadResult>, ApiError> {
    let id = parse_id(&id)?;
    let request = body(request)?;
    let mut params = BTreeMap::new();
    match (&request.walk, request.oids.is_empty()) {
        (Some(walk), true) => {
            params.insert("walk".to_owned(), walk.clone());
            if let Some(max) = request.max {
                params.insert("max".to_owned(), max.to_string());
            }
        }
        (None, false) => {
            params.insert("oids".to_owned(), request.oids.join(","));
        }
        _ => return Err(ApiError::Invalid("Give either `oids` or `walk`.".into())),
    }
    let (probe, rows, cut, customer) =
        read_on(&state, &user, id, request.probe.as_deref(), params).await?;
    let mut conn = state.pool.acquire().await?;
    let scope = match readable_customers(&mut conn, &user).await? {
        s if s.contains(customer) => super::access::Scope::Only(vec![customer]),
        _ => super::access::Scope::Only(Vec::new()),
    };
    let oids: Vec<Vec<u32>> = rows
        .iter()
        .filter_map(|r| custom::parse_oid(&r.0))
        .collect();
    let names = name_oids(&mut conn, &scope, &oids).await?;
    Ok(Json(ReadResult {
        probe,
        objects: rows
            .into_iter()
            .map(|(oid, r#type, value)| ReadObject {
                name: names.get(&oid).cloned(),
                oid,
                r#type,
                value,
            })
            .collect(),
        cut,
    }))
}

/// One channel of one row in a test.
#[derive(Debug, Clone, Serialize, PartialEq, utoipa::ToSchema)]
#[schema(as = SnmpTestedChannel)]
pub struct TestedChannel {
    pub key: String,
    pub name: String,
    pub oid: String,
    /// The SNMP type and raw value read, or why none.
    pub r#type: Option<String>,
    pub raw: Option<String>,
    /// What the channel would write: its value, state and text.
    pub value: Option<f64>,
    pub state: Option<u8>,
    pub text: Option<String>,
    /// In words, when there is nothing to write: "A rate comes from the
    /// next reading", "No such object".
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, utoipa::ToSchema)]
#[schema(as = SnmpTestedRow)]
pub struct TestedRow {
    /// The table's row; `""` for one object.
    pub instance: String,
    pub channels: Vec<TestedChannel>,
}

/// Body of `POST /api/devices/{id}/snmp/test`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = SnmpTested)]
pub struct Tested {
    pub probe: String,
    /// At most 50 rows for a table.
    pub rows: Vec<TestedRow>,
    pub cut: bool,
}

/// Most rows a test shows.
const TEST_ROWS: usize = 50;

/// A value read back from a read's type and text.
pub fn val_of(r#type: &str, value: &str) -> Option<Val> {
    Some(match r#type {
        "Integer" => Val::Int(value.parse().ok()?),
        "Counter32" | "Gauge32" | "UInteger32" | "TimeTicks" | "Counter64" => {
            Val::Uint(value.parse().ok()?)
        }
        "OctetString" | "IpAddress" => Val::Text(value.to_owned()),
        "ObjectIdentifier" => Val::Oid(custom::parse_oid(value)?),
        _ => return None,
    })
}

fn tested(channel: &custom::Channel, read: Option<&(String, String)>) -> TestedChannel {
    let mut out = TestedChannel {
        key: channel.key.clone(),
        name: channel.name.clone(),
        oid: custom::oid_text(&channel.oid),
        r#type: read.map(|r| r.0.clone()),
        raw: read.map(|r| r.1.clone()),
        value: None,
        state: None,
        text: None,
        note: None,
    };
    let Some(value) = read.and_then(|(t, v)| val_of(t, v)) else {
        out.note = Some("No such object on the device.".into());
        return out;
    };
    let (samples, _) = custom::evaluate(channel, &value, None, 0);
    for sample in samples {
        if sample.sensor.ends_with("_state") {
            out.state = Some(sample.value as u8);
            out.text = sample.labels.get("text").cloned();
        } else {
            out.value = Some(sample.value);
        }
    }
    if channel.kind == ValueKind::Counter {
        out.note = Some("A rate per second comes from the second reading.".into());
    } else if out.value.is_none() && out.state.is_none() {
        out.note = Some("This value is not a number: make the channel a text.".into());
    }
    out
}

/// `POST /api/devices/{id}/snmp/test`: reads what the check would,
/// once, from its probe, and says what each channel would write. Changes
/// nothing; not audited.
pub async fn test(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<CheckRequest>, JsonRejection>,
) -> Result<Json<Tested>, ApiError> {
    let id = parse_id(&id)?;
    let request = body(request)?;
    let mut conn = state.pool.acquire().await?;
    let site = device_site(&mut conn, &user, id, true).await?;
    let (address, _, _, _) = device(&mut conn, id).await?;
    let valid = validate(&request, address.as_deref()).map_err(ApiError::Invalid)?;
    let mut flat = crate::sensor_config::params(Value::Object(valid.params.clone()));
    let refs = crate::snmp_custom::lookup_refs(&flat).map_err(ApiError::Invalid)?;
    let customer: Uuid = sqlx::query_scalar("SELECT customer_id FROM sites WHERE id = $1")
        .bind(site)
        .fetch_one(&mut *conn)
        .await?;
    crate::snmp_custom::check_owned(&mut conn, &refs, customer)
        .await?
        .map_err(ApiError::Invalid)?;
    let stored =
        crate::snmp_custom::load_lookups(&mut conn, &crate::snmp_custom::own_ids(&flat)).await?;
    drop(conn);
    crate::snmp_custom::expand(&mut flat, &stored).map_err(ApiError::Invalid)?;
    flat.insert("host".into(), "192.0.2.1".into());
    flat.insert("community".into(), "c".into());
    let custom = custom::parse(&flat).map_err(ApiError::Invalid)?;
    let mut params = BTreeMap::new();
    match &custom.table {
        None => {
            let oids: Vec<String> = custom
                .channels
                .iter()
                .map(|c| custom::oid_text(&c.oid))
                .collect();
            params.insert("oids".to_owned(), oids.join(","));
        }
        Some(table) => {
            params.insert("walk".to_owned(), custom::oid_text(&table.oid));
            params.insert(
                "max".to_owned(),
                monitorflow_snmp::read::MAX_WALK.to_string(),
            );
        }
    }
    let (probe, rows, cut, _) =
        read_on(&state, &user, id, request.probe.as_deref(), params).await?;
    let by_oid: BTreeMap<String, (String, String)> =
        rows.into_iter().map(|(o, t, v)| (o, (t, v))).collect();
    let tested_rows = match &custom.table {
        None => vec![TestedRow {
            instance: String::new(),
            channels: custom
                .channels
                .iter()
                .map(|c| tested(c, by_oid.get(&custom::oid_text(&c.oid))))
                .collect(),
        }],
        Some(table) => {
            let mut indexes: BTreeSet<Vec<u32>> = BTreeSet::new();
            for channel in &custom.channels {
                let prefix = format!("{}.", custom::oid_text(&channel.oid));
                for oid in by_oid.keys().filter(|o| o.starts_with(&prefix)) {
                    if let Some(index) = custom::parse_oid(&format!("0.{}", &oid[prefix.len()..])) {
                        indexes.insert(index[1..].to_vec());
                    }
                }
            }
            let names: BTreeMap<Vec<u32>, String> = match &table.name_column {
                None => BTreeMap::new(),
                Some(column) => indexes
                    .iter()
                    .filter_map(|i| {
                        let oid = format!("{}.{}", custom::oid_text(column), custom::oid_text(i));
                        Some((i.clone(), by_oid.get(&oid)?.1.clone()))
                    })
                    .collect(),
            };
            custom::row_names(&indexes, &names)
                .into_iter()
                .take(TEST_ROWS)
                .map(|(index, instance)| TestedRow {
                    instance,
                    channels: custom
                        .channels
                        .iter()
                        .map(|c| {
                            let oid = format!(
                                "{}.{}",
                                custom::oid_text(&c.oid),
                                custom::oid_text(&index)
                            );
                            tested(c, by_oid.get(&oid))
                        })
                        .collect(),
                })
                .collect()
        }
    };
    Ok(Json(Tested {
        probe,
        rows: tested_rows,
        cut,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> CheckRequest {
        serde_json::from_value(json!({
            "name": "snmp_fw",
            "channels": [
                {"oid": "1.3.6.1.4.1.12356.101.4.1.8.0", "name": "Sessions", "unit": "sessions",
                 "limits": {"warning": 5000, "critical": 8000, "duration_s": 300}},
                {"oid": "1.3.6.1.4.1.12356.101.4.1.1.0", "name": "Firmware", "type": "text",
                 "match": "equals", "expect": ""},
                {"oid": "1.3.6.1.2.1.2.2.1.10.1", "name": "In", "type": "counter", "multiply": 8},
                {"oid": "1.3.6.1.4.1.12356.101.13.2.1.1.15.1", "name": "HA", "lookup": "builtin:ok_warning_failed"}
            ]
        }))
        .unwrap()
    }

    #[test]
    fn requests_become_flat_parameters_and_back() {
        let params = params_of(&request()).unwrap();
        assert_eq!(params["oid1"], json!("1.3.6.1.4.1.12356.101.4.1.8.0"));
        assert_eq!(
            params["expect2"],
            json!(""),
            "an empty expected text is kept"
        );
        assert_eq!(params["multiply3"], json!("8"));
        assert!(!params.contains_key("type1"), "gauge is the default");
        let flat = crate::sensor_config::params(Value::Object(params));
        let back = channels_of(&flat);
        assert_eq!(back.len(), 4);
        assert_eq!(back[2].r#type.as_deref(), Some("counter"));
        assert_eq!(back[2].multiply, Some(8.0));
        assert_eq!(back[1].expect.as_deref(), Some(""));
    }

    #[test]
    fn channels_name_their_series() {
        let valid = validate(&request(), Some("192.0.2.1")).unwrap();
        let metrics = metrics_of("snmp_fw", &valid.custom);
        assert_eq!(
            metrics[0],
            (Some("monitorflow_snmp_fw_sessions".into()), None)
        );
        assert_eq!(
            metrics[1],
            (None, Some("monitorflow_snmp_fw_firmware_state".into()))
        );
        assert_eq!(
            metrics[3].1.as_deref(),
            Some("monitorflow_snmp_fw_ha_state")
        );
        assert_eq!(valid.limits[0].as_ref().unwrap().critical, Some(8000.0));
    }

    #[test]
    fn bad_checks_say_why() {
        let mut long = request();
        long.name = format!("snmp_{}", "x".repeat(55));
        assert!(validate(&long, None).unwrap_err().contains("too long"));
        let mut text_limits = request();
        text_limits.channels[1].limits = Some(Limits {
            comparison: ">".into(),
            warning: Some(1.0),
            critical: None,
            duration_s: 0,
        });
        assert!(validate(&text_limits, None)
            .unwrap_err()
            .contains("is a text"));
        let mut wrong = request();
        wrong.channels[0].limits = Some(Limits {
            comparison: "~".into(),
            warning: Some(1.0),
            critical: None,
            duration_s: 0,
        });
        assert!(validate(&wrong, None)
            .unwrap_err()
            .contains("Limits of `Sessions`"));
        let mut none = request();
        none.channels.clear();
        assert!(validate(&none, None).unwrap_err().contains("1 to 10"));
        let mut bad_name = request();
        bad_name.name = "Firewall".into();
        assert!(validate(&bad_name, None).is_err());
    }

    #[test]
    fn tests_say_what_channels_would_write() {
        let valid = validate(&request(), None).unwrap();
        let c = &valid.custom.channels;
        let sessions = tested(&c[0], Some(&("Gauge32".into(), "1500".into())));
        assert_eq!(
            (sessions.value, sessions.note.as_deref()),
            (Some(1500.0), None)
        );
        let firmware = tested(&c[1], Some(&("OctetString".into(), "v7.2".into())));
        assert_eq!(
            (firmware.state, firmware.text.as_deref()),
            (Some(2), Some("v7.2"))
        );
        let rate = tested(&c[2], Some(&("Counter32".into(), "100".into())));
        assert!(rate.note.unwrap().contains("second reading"));
        let ha = tested(&c[3], Some(&("Integer".into(), "3".into())));
        assert_eq!((ha.state, ha.text.as_deref()), (Some(2), Some("Failed")));
        let missing = tested(&c[0], Some(&("noSuchObject".into(), String::new())));
        assert!(missing.note.unwrap().contains("No such object"));
        assert_eq!(
            val_of("Counter64", "18446744073709551615"),
            Some(Val::Uint(u64::MAX))
        );
        assert_eq!(
            val_of("ObjectIdentifier", "1.3.6"),
            Some(Val::Oid(vec![1, 3, 6]))
        );
    }
}
