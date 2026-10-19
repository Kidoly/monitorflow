//! Metric query proxy to VictoriaMetrics (ADR-0018).
//!
//! Callers never send PromQL. They name a server, a metric and optionally
//! an instance and an aggregation; the API builds the query itself with
//! the server's `peer` and `site` matchers (ADR-0013) and also passes them
//! as VictoriaMetrics `extra_label` filters, so a series of another server
//! cannot be selected even if the builder had a bug. The server is looked
//! up with the user's site scope first: one outside it is 404 and nothing
//! is sent to VictoriaMetrics.

use std::collections::{BTreeMap, BTreeSet};

use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::{DateTime, Duration, Utc};
use monitorflow_sensors::validate;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::auth::CurrentUser;
use super::{ApiError, ApiState};
use crate::vm_query::{QueryError, VmQuery};

/// Prefix of every metric the ingest writes (ADR-0013).
pub const METRIC_PREFIX: &str = "monitorflow_";
/// Longest range of a range query.
pub const MAX_RANGE: Duration = Duration::days(31);
/// Default range of a range query without `start`.
pub const DEFAULT_RANGE: Duration = Duration::hours(1);
/// Most points per series, as Prometheus allows.
pub const MAX_POINTS: i64 = 11_000;
/// Smallest step, in seconds.
pub const MIN_STEP: i64 = 1;
/// Points per series when the step is not given.
const DEFAULT_POINTS: i64 = 720;
/// Most series in one answer.
pub const MAX_SERIES: usize = 500;
/// How far back discovery looks.
pub const DISCOVERY_WINDOW: Duration = Duration::days(1);
/// Most series discovery reads for one server.
pub const MAX_DISCOVERY_SERIES: usize = 10_000;

/// A simple aggregation across the selected series.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Aggregation {
    Avg,
    Min,
    Max,
    Sum,
    Count,
}

impl Aggregation {
    fn as_str(self) -> &'static str {
        match self {
            Self::Avg => "avg",
            Self::Min => "min",
            Self::Max => "max",
            Self::Sum => "sum",
            Self::Count => "count",
        }
    }
}

/// Query string of `GET /api/metrics/query`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstantParams {
    pub server_id: Uuid,
    /// Full name, `monitorflow_<sensor metric>`.
    pub metric: String,
    pub instance: Option<String>,
    /// Defaults to now.
    pub time: Option<DateTime<Utc>>,
    pub agg: Option<Aggregation>,
}

/// Query string of `GET /api/metrics/query_range`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RangeParams {
    pub server_id: Uuid,
    pub metric: String,
    pub instance: Option<String>,
    /// Defaults to `end` minus one hour.
    pub start: Option<DateTime<Utc>>,
    /// Defaults to now.
    pub end: Option<DateTime<Utc>>,
    /// Seconds; defaults to the range over 720, at least one second.
    pub step: Option<i64>,
    pub agg: Option<Aggregation>,
}

/// One series: its labels and its `[unix seconds, value]` points. A value
/// that is not finite is `null`.
#[derive(Debug, Serialize)]
pub struct SeriesView {
    pub labels: BTreeMap<String, String>,
    pub points: Vec<(f64, Option<f64>)>,
}

/// Body of both query endpoints.
#[derive(Debug, Serialize)]
pub struct QueryResult {
    /// The PromQL the API ran, filters included.
    pub query: String,
    pub series: Vec<SeriesView>,
}

/// One metric of a server and the instances seen for it.
#[derive(Debug, Serialize)]
pub struct MetricInfo {
    pub name: String,
    pub instances: Vec<String>,
}

/// Body of `GET /api/servers/{id}/metrics`.
#[derive(Debug, Serialize)]
pub struct MetricList {
    pub metrics: Vec<MetricInfo>,
    /// The server had more series than discovery reads.
    pub truncated: bool,
}

/// The labels that tie a series to one server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub peer: String,
    pub site: String,
}

impl Target {
    /// `extra_label` parameters, which VictoriaMetrics adds to every
    /// selector of the query.
    fn extra_labels(&self) -> [(&'static str, String); 2] {
        [
            ("extra_label", format!("peer={}", self.peer)),
            ("extra_label", format!("site={}", self.site)),
        ]
    }
}

/// A PromQL string literal for `value`.
pub fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Refuses anything but `monitorflow_` and a valid sensor metric name.
pub fn check_metric(metric: &str) -> Result<(), ApiError> {
    let ok = metric
        .strip_prefix(METRIC_PREFIX)
        .is_some_and(|rest| validate::sensor_name(rest).is_ok());
    ok.then_some(()).ok_or_else(|| {
        ApiError::Invalid(format!(
            "metric must be `{METRIC_PREFIX}` followed by a sensor metric name, like `{METRIC_PREFIX}cpu_usage_ratio`"
        ))
    })
}

/// The PromQL for `metric` of `target`, with an optional instance and
/// aggregation. `metric` and `instance` must be checked first.
pub fn build_query(
    metric: &str,
    target: &Target,
    instance: Option<&str>,
    agg: Option<Aggregation>,
) -> String {
    let mut selector = format!(
        "{metric}{{peer={},site={}",
        quote(&target.peer),
        quote(&target.site)
    );
    if let Some(instance) = instance {
        selector.push_str(&format!(",instance={}", quote(instance)));
    }
    selector.push('}');
    match agg {
        Some(agg) => format!("{}({selector})", agg.as_str()),
        None => selector,
    }
}

/// `(start, end, step)` of a range query, bounded.
pub fn range(params: &RangeParams, now: DateTime<Utc>) -> Result<(i64, i64, i64), ApiError> {
    let end = params.end.unwrap_or(now);
    let start = params.start.unwrap_or(end - DEFAULT_RANGE);
    if start >= end {
        return Err(ApiError::Invalid("start must be before end".into()));
    }
    if end - start > MAX_RANGE {
        return Err(ApiError::Invalid(format!(
            "range must be at most {} days",
            MAX_RANGE.num_days()
        )));
    }
    let seconds = (end - start).num_seconds().max(1);
    let step = params
        .step
        .unwrap_or_else(|| (seconds / DEFAULT_POINTS).max(MIN_STEP));
    if step < MIN_STEP {
        return Err(ApiError::Invalid(format!(
            "step must be at least {MIN_STEP} second"
        )));
    }
    if seconds / step >= MAX_POINTS {
        return Err(ApiError::Invalid(format!(
            "range over step must be under {MAX_POINTS} points"
        )));
    }
    Ok((start.timestamp(), end.timestamp(), step))
}

fn check_instance(instance: Option<&str>) -> Result<(), ApiError> {
    match instance {
        Some(i) => validate::instance(i).map_err(|e| ApiError::Invalid(e.to_string())),
        None => Ok(()),
    }
}

fn query_params<T>(params: Result<Query<T>, QueryRejection>) -> Result<T, ApiError> {
    params
        .map(|Query(p)| p)
        .map_err(|rejection| ApiError::Invalid(rejection.body_text()))
}

/// The backend and the `peer`/`site` of `server` if the user may read it.
async fn target(
    state: &ApiState,
    user: &CurrentUser,
    server: Uuid,
) -> Result<(VmQuery, Target), ApiError> {
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    let row: Option<(String, Uuid, String)> = sqlx::query_as(
        "SELECT s.peer_id, s.site_id, si.name FROM servers s
         JOIN sites si ON si.id = s.site_id WHERE s.id = $1",
    )
    .bind(server)
    .fetch_optional(&state.pool)
    .await?;
    let (peer, _, site) = row
        .filter(|(_, site, _)| user.can_read_site(*site))
        .ok_or(ApiError::NotFound)?;
    let vm = state
        .metrics
        .clone()
        .ok_or_else(|| ApiError::MetricsUnavailable("no VictoriaMetrics configured".into()))?;
    Ok((vm, Target { peer, site }))
}

fn backend_error(error: QueryError) -> ApiError {
    match error {
        QueryError::Timeout => ApiError::MetricsTimeout,
        QueryError::Rejected(message) => {
            tracing::warn!(%message, "VictoriaMetrics rejected a query");
            ApiError::Invalid(
                "metrics backend rejected the query; narrow the range or filter by instance".into(),
            )
        }
        other => ApiError::MetricsUnavailable(other.to_string()),
    }
}

/// Converts a `vector` or `matrix` result, refusing more than
/// [`MAX_SERIES`] series.
fn series(data: Value) -> Result<Vec<SeriesView>, ApiError> {
    let malformed = || ApiError::MetricsUnavailable("unexpected query result".into());
    let Value::Array(results) = &data["result"] else {
        return Err(malformed());
    };
    if results.len() > MAX_SERIES {
        return Err(ApiError::Invalid(format!(
            "query returns more than {MAX_SERIES} series; filter by instance or aggregate"
        )));
    }
    let point = |p: &Value| -> Option<(f64, Option<f64>)> {
        let time = p.get(0)?.as_f64()?;
        let value = p.get(1)?.as_str()?.parse::<f64>().ok();
        Some((time, value.filter(|v| v.is_finite())))
    };
    results
        .iter()
        .map(|r| {
            let labels = serde_json::from_value(r["metric"].clone()).map_err(|_| malformed())?;
            let points = match (&r["values"], &r["value"]) {
                (Value::Array(values), _) => values.iter().map(point).collect(),
                (_, value @ Value::Array(_)) => point(value).map(|p| vec![p]),
                _ => None,
            };
            Ok(SeriesView {
                labels,
                points: points.ok_or_else(malformed)?,
            })
        })
        .collect()
}

/// `GET /api/metrics/query`: the value of one metric of one server at
/// `time`.
pub async fn query(
    State(state): State<ApiState>,
    user: CurrentUser,
    params: Result<Query<InstantParams>, QueryRejection>,
) -> Result<Json<QueryResult>, ApiError> {
    let params = query_params(params)?;
    check_metric(&params.metric)?;
    check_instance(params.instance.as_deref())?;
    let (vm, target) = target(&state, &user, params.server_id).await?;
    let promql = build_query(
        &params.metric,
        &target,
        params.instance.as_deref(),
        params.agg,
    );
    let time = params.time.unwrap_or_else(Utc::now).timestamp();
    let mut query = vec![("query", promql.clone()), ("time", time.to_string())];
    query.extend(target.extra_labels());
    let data = vm
        .get("/api/v1/query", &query)
        .await
        .map_err(backend_error)?;
    Ok(Json(QueryResult {
        query: promql,
        series: series(data)?,
    }))
}

/// `GET /api/metrics/query_range`: one metric of one server over a range.
pub async fn query_range(
    State(state): State<ApiState>,
    user: CurrentUser,
    params: Result<Query<RangeParams>, QueryRejection>,
) -> Result<Json<QueryResult>, ApiError> {
    let params = query_params(params)?;
    check_metric(&params.metric)?;
    check_instance(params.instance.as_deref())?;
    let (start, end, step) = range(&params, Utc::now())?;
    let (vm, target) = target(&state, &user, params.server_id).await?;
    let promql = build_query(
        &params.metric,
        &target,
        params.instance.as_deref(),
        params.agg,
    );
    let mut query = vec![
        ("query", promql.clone()),
        ("start", start.to_string()),
        ("end", end.to_string()),
        ("step", step.to_string()),
    ];
    query.extend(target.extra_labels());
    let data = vm
        .get("/api/v1/query_range", &query)
        .await
        .map_err(backend_error)?;
    Ok(Json(QueryResult {
        query: promql,
        series: series(data)?,
    }))
}

/// `GET /api/servers/{id}/metrics`: the `monitorflow_*` metrics the server
/// wrote in the last day and their instances, sorted.
pub async fn discover(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<MetricList>, ApiError> {
    let server = id.parse().map_err(|_| ApiError::NotFound)?;
    let (vm, target) = target(&state, &user, server).await?;
    let now = Utc::now();
    let matcher = format!(
        "{{__name__=~{},peer={},site={}}}",
        quote(&format!("{METRIC_PREFIX}.+")),
        quote(&target.peer),
        quote(&target.site)
    );
    let mut query = vec![
        ("match[]", matcher),
        ("start", (now - DISCOVERY_WINDOW).timestamp().to_string()),
        ("end", now.timestamp().to_string()),
        ("limit", MAX_DISCOVERY_SERIES.to_string()),
    ];
    query.extend(target.extra_labels());
    let data = vm
        .get("/api/v1/series", &query)
        .await
        .map_err(backend_error)?;
    let Value::Array(list) = data else {
        return Err(ApiError::MetricsUnavailable(
            "unexpected series result".into(),
        ));
    };
    let mut metrics: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for labels in list.iter().take(MAX_DISCOVERY_SERIES) {
        let Some(name) = labels["__name__"].as_str() else {
            continue;
        };
        // Defense in depth: the backend applied the same filters.
        if labels["peer"] != target.peer.as_str() || !name.starts_with(METRIC_PREFIX) {
            continue;
        }
        let instances = metrics.entry(name.to_owned()).or_default();
        if let Some(instance) = labels["instance"].as_str() {
            instances.insert(instance.to_owned());
        }
    }
    Ok(Json(MetricList {
        truncated: list.len() >= MAX_DISCOVERY_SERIES,
        metrics: metrics
            .into_iter()
            .map(|(name, instances)| MetricInfo {
                name,
                instances: instances.into_iter().collect(),
            })
            .collect(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    fn target() -> Target {
        Target {
            peer: "agent-1".into(),
            site: "o\"brien\\paris".into(),
        }
    }

    #[test]
    fn queries_carry_escaped_peer_and_site_matchers() {
        assert_eq!(quote("a\"b\\c\nd\u{1}"), r#""a\"b\\c\nd\u0001""#);
        assert_eq!(
            build_query("monitorflow_cpu_usage_ratio", &target(), None, None),
            r#"monitorflow_cpu_usage_ratio{peer="agent-1",site="o\"brien\\paris"}"#
        );
        assert_eq!(
            build_query(
                "monitorflow_disk_used_ratio",
                &target(),
                Some("/\"}or{"),
                Some(Aggregation::Max)
            ),
            r#"max(monitorflow_disk_used_ratio{peer="agent-1",site="o\"brien\\paris",instance="/\"}or{"})"#
        );
    }

    #[test]
    fn only_monitorflow_metric_names_pass() {
        for ok in ["monitorflow_cpu_usage_ratio", "monitorflow_port"] {
            assert!(check_metric(ok).is_ok(), "{ok}");
        }
        for bad in [
            "up",
            "monitorflow_",
            "monitorflow_Cpu",
            "monitorflow_cpu{peer=\"x\"}",
            "monitorflow_cpu or up",
            "sum(monitorflow_cpu)",
        ] {
            assert!(check_metric(bad).is_err(), "{bad}");
        }
    }

    fn params(start: Option<i64>, end: Option<i64>, step: Option<i64>) -> RangeParams {
        let at = |s| Utc.timestamp_opt(s, 0).unwrap();
        RangeParams {
            server_id: Uuid::nil(),
            metric: String::new(),
            instance: None,
            start: start.map(at),
            end: end.map(at),
            step,
            agg: None,
        }
    }

    #[test]
    fn ranges_and_steps_are_bounded() {
        let now = Utc.timestamp_opt(1_000_000, 0).unwrap();
        assert_eq!(
            range(&params(None, None, None), now).unwrap(),
            (1_000_000 - 3_600, 1_000_000, 5)
        );
        assert_eq!(
            range(&params(Some(0), Some(60), None), now).unwrap(),
            (0, 60, 1)
        );
        assert_eq!(
            range(&params(Some(0), Some(600), Some(60)), now).unwrap(),
            (0, 600, 60)
        );
        let day = 86_400;
        for bad in [
            params(Some(10), Some(10), None),
            params(Some(20), Some(10), None),
            params(Some(0), Some(32 * day), None),
            params(Some(0), Some(600), Some(0)),
            params(Some(0), Some(day), Some(1)),
        ] {
            assert!(range(&bad, now).is_err(), "{bad:?}");
        }
        // The default step always fits the point limit.
        assert!(range(&params(Some(0), Some(31 * day), None), now).is_ok());
    }

    #[test]
    fn results_become_series_with_numeric_points() {
        let matrix = json!({"resultType": "matrix", "result": [
            {"metric": {"instance": "total"}, "values": [[1.5, "0.25"], [2, "NaN"]]}
        ]});
        let series = series(matrix).unwrap();
        assert_eq!(series[0].labels["instance"], "total");
        assert_eq!(series[0].points, [(1.5, Some(0.25)), (2.0, None)]);
        let vector =
            json!({"resultType": "vector", "result": [{"metric": {}, "value": [3, "+Inf"]}]});
        assert_eq!(super::series(vector).unwrap()[0].points, [(3.0, None)]);
        assert!(super::series(json!({"result": "x"})).is_err());
        let many: Vec<_> = (0..=MAX_SERIES)
            .map(|i| json!({"metric": {"instance": i.to_string()}, "value": [1, "1"]}))
            .collect();
        let error = super::series(json!({ "result": many })).unwrap_err();
        assert!(matches!(error, ApiError::Invalid(_)), "{error}");
    }
}
