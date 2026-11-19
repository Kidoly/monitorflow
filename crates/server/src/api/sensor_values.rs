//! `GET /api/servers/{id}/sensor-values`: for the server page's sensor
//! table, the recent values of every sensor the server runs, in three
//! VictoriaMetrics queries whatever the number of sensors: a sparkline of
//! the last hour, the last value, and its time. A sensor whose last
//! sample is late carries why, in the words of [`reason::no_data`].
//!
//! Each sensor stands for one metric, picked by kind as the web interface
//! picks it (`web/src/lib/sensors.ts`), and each point of a sparkline is
//! the worst value of its step: the highest for usage, the lowest for
//! what is left or up.

use std::collections::{BTreeMap, BTreeSet};

use axum::extract::{Path, State};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use super::access::authorize;
use super::auth::CurrentUser;
use super::metrics::{self, backend_error, quote, Rollup, Target, MAX_LOOKBACK, METRIC_PREFIX};
use super::sensors::{parse_id, server_effective, EffectiveSensor};
use super::{ApiError, ApiState};
use crate::alerting::reason;

/// What a sparkline covers, in seconds.
pub const SPARKLINE_S: i64 = 3_600;
/// Points per sparkline: one every two minutes.
pub const SPARKLINE_POINTS: i64 = 30;
/// A sample is late after two intervals, and never before five minutes,
/// VictoriaMetrics' own staleness for the last value of a series.
const MIN_LATE_S: i64 = 300;

/// Body of `GET /api/servers/{id}/sensor-values`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct SensorValues {
    /// The sparklines' range and step, in Unix seconds.
    pub start: i64,
    pub end: i64,
    pub step: i64,
    pub sensors: Vec<SensorValue>,
}

/// One sensor, by its name and configured instance.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct SensorValue {
    pub name: String,
    /// `""` when the sensor is not limited to one instance.
    pub instance: String,
    /// The metric that stands for it.
    pub metric: String,
    /// One per instance seen, like one per mount of a disk sensor.
    pub series: Vec<SeriesValue>,
    /// Why there is no series at all: "No data yet".
    pub reason: Option<String>,
}

/// One instance of a sensor's metric.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct SeriesValue {
    /// `""` for a metric without instances.
    pub instance: String,
    /// `[unix seconds, value]` from `start` to `end`; the value is null
    /// for a step without a sample.
    #[schema(value_type = Vec<Vec<Option<f64>>>)]
    pub points: Vec<(f64, Option<f64>)>,
    pub last_value: Option<f64>,
    pub last_at: Option<DateTime<Utc>>,
    /// Set when the last sample is late: "No data for 5 minutes".
    pub reason: Option<String>,
}

/// The metric standing for `sensor`, the one instance to keep when it
/// writes several, and how a sparkline point sums up its step.
fn primary(sensor: &EffectiveSensor) -> (String, Option<String>, Rollup) {
    let (suffix, instance, rollup) = match sensor.kind.as_str() {
        "cpu" => (Some("usage_ratio"), Some("total"), Rollup::Max),
        "memory" => (Some("available_ratio"), None, Rollup::Min),
        "disk" => (Some("used_ratio"), None, Rollup::Max),
        "diskio" => (Some("busy_ratio"), None, Rollup::Max),
        "net" => (Some("receive_bytes_per_second"), None, Rollup::Max),
        "systemd" | "windows_service" => (Some("active"), None, Rollup::Min),
        "port" | "ping" | "http" => (Some("up"), None, Rollup::Min),
        // An SNMP check stands for what its group watches most (ADR-0149).
        "snmp" => match sensor.params.get("what").map(String::as_str) {
            Some("interfaces") => (Some("oper_up"), None, Rollup::Min),
            Some("host") => (Some("cpu_usage_ratio"), None, Rollup::Max),
            Some("ups") => (Some("battery_charge_ratio"), None, Rollup::Min),
            Some("temperature") => (Some("celsius"), None, Rollup::Max),
            Some("printer") => (Some("supply_level_ratio"), None, Rollup::Min),
            _ => (Some("up"), None, Rollup::Min),
        },
        "log" | "eventlog" => (Some("matches"), None, Rollup::Max),
        // A custom check stands for its first channel (ADR-0157).
        "snmp_custom" => match crate::snmp_custom::primary_suffix(&sensor.params) {
            Some((suffix, true)) => return primary_named(sensor, &suffix, Rollup::Max),
            Some((suffix, false)) => return primary_named(sensor, &suffix, Rollup::Last),
            None => (None, None, Rollup::Last),
        },
        "script" => (Some("status"), None, Rollup::Max),
        _ => (None, None, Rollup::Last),
    };
    let metric = match suffix {
        Some(suffix) => format!("{METRIC_PREFIX}{}_{suffix}", sensor.name),
        None => format!("{METRIC_PREFIX}{}", sensor.name),
    };
    // Interfaces, sensors and supplies name their own instances.
    let own_instances = sensor.kind == "snmp"
        && matches!(
            sensor.params.get("what").map(String::as_str),
            Some("interfaces" | "temperature" | "printer")
        );
    let instance = Some(sensor.instance.as_str())
        .filter(|i| !i.is_empty() && !own_instances)
        .or(instance)
        .map(str::to_owned);
    (metric, instance, rollup)
}

/// A custom SNMP check's metric: a table names its rows itself.
fn primary_named(
    sensor: &EffectiveSensor,
    suffix: &str,
    rollup: Rollup,
) -> (String, Option<String>, Rollup) {
    let metric = format!("{METRIC_PREFIX}{}_{suffix}", sensor.name);
    let table = sensor.params.contains_key("table");
    let instance = Some(sensor.instance.as_str())
        .filter(|i| !i.is_empty() && !table)
        .map(str::to_owned);
    (metric, instance, rollup)
}

/// The sensors' metrics, grouped by rollup and instance filter, so one
/// selector with a name regex covers each group.
type Groups = BTreeMap<(&'static str, Option<String>), BTreeSet<String>>;

fn groups(sensors: &[EffectiveSensor]) -> Groups {
    let mut groups = Groups::new();
    for sensor in sensors {
        let (metric, instance, rollup) = primary(sensor);
        groups
            .entry((rollup.function(), instance))
            .or_default()
            .insert(metric);
    }
    groups
}

fn selector(metrics: &BTreeSet<String>, instance: Option<&str>, target: &Target) -> String {
    // Metric names are `[a-z0-9_]` (checked when sensors are saved), so
    // they need no escaping inside the regex.
    let names = metrics.iter().cloned().collect::<Vec<_>>().join("|");
    let mut selector = format!(
        "{{__name__=~{},peer={},site={}",
        quote(&names),
        quote(&target.peer),
        quote(&target.site)
    );
    if let Some(instance) = instance {
        selector.push_str(&format!(",instance={}", quote(instance)));
    }
    selector.push('}');
    selector
}

/// One query over every group: `function` of each group's selector over
/// `window` seconds, the group's own rollup when `function` is `None`.
fn query(groups: &Groups, target: &Target, function: Option<&str>, window: i64) -> String {
    groups
        .iter()
        .map(|((rollup, instance), metrics)| {
            let selector = selector(metrics, instance.as_deref(), target);
            let function = function.unwrap_or(rollup);
            format!("{function}({selector}[{window}s]) keep_metric_names")
        })
        .collect::<Vec<_>>()
        .join(" or ")
}

/// `(metric, instance)` of a series.
fn key(labels: &BTreeMap<String, String>) -> (String, String) {
    (
        labels.get("__name__").cloned().unwrap_or_default(),
        labels.get("instance").cloned().unwrap_or_default(),
    )
}

/// Points of one query's series, by `(metric, instance)`.
type ByKey = BTreeMap<(String, String), Vec<(f64, Option<f64>)>>;

/// The series of one query, by `(metric, instance)`.
fn by_key(data: Value) -> Result<ByKey, ApiError> {
    Ok(metrics::series(data)?
        .into_iter()
        .map(|s| (key(&s.labels), s.points))
        .collect())
}

/// `GET /api/servers/{id}/sensor-values`.
pub async fn sensor_values(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<SensorValues>, ApiError> {
    let server_id = parse_id(&id)?;
    type Server = (String, Uuid, String, Option<i32>, DateTime<Utc>);
    let row: Option<Server> = sqlx::query_as(
        "SELECT s.peer_id, s.site_id, si.name, si.check_interval_ms, s.created_at
         FROM servers s JOIN sites si ON si.id = s.site_id WHERE s.id = $1",
    )
    .bind(server_id)
    .fetch_optional(&state.pool)
    .await?;
    authorize(&user, row.as_ref().map(|r| r.1), false)?;
    let (peer, site_id, site, site_interval, created_at) = row.ok_or(ApiError::NotFound)?;
    let sensors = server_effective(&state.pool, server_id, site_id, site_interval).await?;
    let vm = state
        .metrics
        .clone()
        .ok_or_else(|| ApiError::MetricsUnavailable("no VictoriaMetrics configured".into()))?;
    let target = Target { peer, site };

    let now = Utc::now();
    let step = SPARKLINE_S / SPARKLINE_POINTS;
    let end = now.timestamp();
    let start = end - SPARKLINE_S;
    // The last value of the slowest check is within two of its intervals.
    let slowest = sensors
        .iter()
        .map(|s| s.interval_ms / 1000)
        .max()
        .unwrap_or(0);
    let lookback =
        (2 * i64::try_from(slowest).unwrap_or(MAX_LOOKBACK)).clamp(SPARKLINE_S, MAX_LOOKBACK);

    let (mut sparklines, mut values, mut times): (ByKey, ByKey, ByKey) = Default::default();
    let groups = groups(&sensors);
    if !groups.is_empty() {
        let extra = target.extra_labels();
        let range = [
            ("query", query(&groups, &target, None, step)),
            ("start", start.to_string()),
            ("end", end.to_string()),
            ("step", step.to_string()),
        ];
        let range: Vec<_> = range.into_iter().chain(extra.clone()).collect();
        sparklines = by_key(
            vm.get("/api/v1/query_range", &range)
                .await
                .map_err(backend_error)?,
        )?;
        // Sequential: three small queries, and a backend under load gets
        // one at a time from each page.
        for (function, out) in [
            ("last_over_time", &mut values),
            ("tlast_over_time", &mut times),
        ] {
            let instant = [
                ("query", query(&groups, &target, Some(function), lookback)),
                ("time", end.to_string()),
            ];
            let instant: Vec<_> = instant.into_iter().chain(extra.clone()).collect();
            *out = by_key(
                vm.get("/api/v1/query", &instant)
                    .await
                    .map_err(backend_error)?,
            )?;
        }
    }

    let last =
        |points: Option<&Vec<(f64, Option<f64>)>>| points.and_then(|p| p.last()).and_then(|p| p.1);
    let sensors = sensors
        .iter()
        .map(|sensor| {
            let (metric, only, _) = primary(sensor);
            let instances: BTreeSet<String> = sparklines
                .keys()
                .chain(values.keys())
                .filter(|(m, i)| *m == metric && only.as_ref().is_none_or(|o| o == i))
                .map(|(_, i)| i.clone())
                .collect();
            let late_after = (2 * i64::try_from(sensor.interval_ms / 1000).unwrap_or(i64::MAX / 2))
                .max(MIN_LATE_S);
            let series: Vec<SeriesValue> = instances
                .into_iter()
                .map(|instance| {
                    let key = (metric.clone(), instance);
                    let last_at =
                        last(times.get(&key)).and_then(|t| DateTime::from_timestamp(t as i64, 0));
                    let age = last_at.map(|t| (now - t).num_seconds());
                    SeriesValue {
                        points: sparklines.remove(&key).unwrap_or_default(),
                        last_value: last(values.get(&key)),
                        last_at,
                        reason: age
                            .filter(|&a| a > late_after)
                            .map(|a| reason::no_data(Some(a))),
                        instance: key.1,
                    }
                })
                .collect();
            let reason = series.is_empty().then(|| {
                if (now - created_at).num_seconds() > lookback {
                    reason::no_data_within(lookback)
                } else {
                    reason::no_data(None)
                }
            });
            SensorValue {
                name: sensor.name.clone(),
                instance: sensor.instance.clone(),
                metric,
                series,
                reason,
            }
        })
        .collect();
    Ok(Json(SensorValues {
        start,
        end,
        step,
        sensors,
    }))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap as Map;

    use super::*;
    use crate::api::sensors::Origin;

    fn sensor(kind: &str, name: &str, instance: &str) -> EffectiveSensor {
        EffectiveSensor {
            origin: Origin::Default,
            entry_id: None,
            kind: kind.into(),
            name: name.into(),
            instance: instance.into(),
            interval_ms: 30_000,
            timeout_ms: None,
            params: Map::new(),
            probe: None,
            probe_relay_id: None,
        }
    }

    #[test]
    fn custom_snmp_checks_stand_for_their_first_channel() {
        let with = |pairs: &[(&str, &str)], instance: &str| EffectiveSensor {
            params: pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ..sensor("snmp_custom", "snmp_fw", instance)
        };
        let gauge = with(
            &[("oid1", "1.3.6.1.4.1.1.0"), ("name1", "Sessions")],
            "central",
        );
        let (metric, instance, rollup) = primary(&gauge);
        assert_eq!(
            (metric.as_str(), instance.as_deref()),
            ("monitorflow_snmp_fw_sessions", Some("central"))
        );
        assert_eq!(rollup.function(), Rollup::Last.function());
        let table = with(
            &[
                ("table", "1.3.6.1.4.1.6574.3.1.1"),
                ("oid1", "1.3.6.1.4.1.6574.3.1.1.3"),
                ("name1", "RAID"),
                ("lookup1", "builtin:ok_warning_failed"),
            ],
            "central",
        );
        let (metric, instance, rollup) = primary(&table);
        assert_eq!(
            (metric.as_str(), instance),
            ("monitorflow_snmp_fw_raid_state", None)
        );
        assert_eq!(rollup.function(), Rollup::Max.function());
    }

    #[test]
    fn snmp_checks_stand_for_their_group() {
        let with = |what: &str, instance: &str| EffectiveSensor {
            params: [("what".to_string(), what.to_string())].into(),
            ..sensor("snmp", "snmp_x", instance)
        };
        let (metric, instance, _) = primary(&with("interfaces", "central"));
        assert_eq!(
            (metric.as_str(), instance),
            ("monitorflow_snmp_x_oper_up", None)
        );
        let (metric, instance, _) = primary(&with("ups", "central"));
        assert_eq!(
            (metric.as_str(), instance.as_deref()),
            ("monitorflow_snmp_x_battery_charge_ratio", Some("central"))
        );
        let (metric, _, rollup) = primary(&with("temperature", "central"));
        assert_eq!(
            (metric.as_str(), rollup),
            ("monitorflow_snmp_x_celsius", Rollup::Max)
        );
        assert_eq!(primary(&with("system", "")).0, "monitorflow_snmp_x_up");
    }

    #[test]
    fn each_kind_stands_for_one_metric() {
        let cases = [
            (
                sensor("cpu", "cpu", ""),
                "monitorflow_cpu_usage_ratio",
                Some("total"),
                Rollup::Max,
            ),
            (
                sensor("memory", "memory", ""),
                "monitorflow_memory_available_ratio",
                None,
                Rollup::Min,
            ),
            (
                sensor("disk", "disk", "/var"),
                "monitorflow_disk_used_ratio",
                Some("/var"),
                Rollup::Max,
            ),
            (
                sensor("port", "port_https", ""),
                "monitorflow_port_https_up",
                None,
                Rollup::Min,
            ),
            (
                sensor("script", "backup", ""),
                "monitorflow_backup_status",
                None,
                Rollup::Max,
            ),
            (
                sensor("plugin", "queue_depth", ""),
                "monitorflow_queue_depth",
                None,
                Rollup::Last,
            ),
        ];
        for (sensor, metric, instance, rollup) in cases {
            assert_eq!(
                primary(&sensor),
                (metric.into(), instance.map(Into::into), rollup),
                "{}",
                sensor.kind
            );
        }
    }

    #[test]
    fn one_query_covers_every_sensor() {
        let target = Target {
            peer: "agent-1".into(),
            site: "o\"lyon".into(),
        };
        let sensors = [
            sensor("cpu", "cpu", ""),
            sensor("disk", "disk", ""),
            sensor("diskio", "diskio", ""),
            sensor("memory", "memory", ""),
        ];
        assert_eq!(
            query(&groups(&sensors), &target, None, 120),
            [
                r#"max_over_time({__name__=~"monitorflow_disk_used_ratio|monitorflow_diskio_busy_ratio",peer="agent-1",site="o\"lyon"}[120s]) keep_metric_names"#,
                r#"max_over_time({__name__=~"monitorflow_cpu_usage_ratio",peer="agent-1",site="o\"lyon",instance="total"}[120s]) keep_metric_names"#,
                r#"min_over_time({__name__=~"monitorflow_memory_available_ratio",peer="agent-1",site="o\"lyon"}[120s]) keep_metric_names"#,
            ]
            .join(" or ")
        );
        assert!(
            query(&groups(&sensors), &target, Some("tlast_over_time"), 3600)
                .starts_with(r#"tlast_over_time({__name__=~"monitorflow_disk_used_ratio|"#)
        );
    }
}
