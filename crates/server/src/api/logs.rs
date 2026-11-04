//! A server's system logs from VictoriaLogs (ADR-0091).
//!
//! Callers never send LogsQL. They name a server, a time range and
//! optional filters; the API builds the query itself, starting with the
//! server's `{peer,site}` stream (ADR-0013) and `source:="journal"`, and
//! also passes the stream as VictoriaLogs' `extra_stream_filters`, so a
//! line of another server cannot be selected even if the builder had a
//! bug. The server is looked up with the user's site scope first: one
//! outside it is 404 and nothing is sent to VictoriaLogs.

use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::access::authorize;
use super::auth::CurrentUser;
use super::metrics::{quote, Target};
use super::{ApiError, ApiState};
use crate::ingest::journal::SOURCE;
use crate::vm_query::QueryError;

/// Longest range, as for metrics.
pub const MAX_RANGE: Duration = Duration::days(31);
/// Range without `start`.
pub const DEFAULT_RANGE: Duration = Duration::hours(1);
/// Lines without `limit`.
pub const DEFAULT_LIMIT: usize = 200;
/// Most lines in one answer.
pub const MAX_LIMIT: usize = 1000;
/// Most units listed for the filter.
pub const MAX_UNITS: usize = 200;
/// Bytes of `unit` and `q`.
pub const MAX_FILTER: usize = 256;

/// Which priorities to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// Error and worse: priority 3 or less.
    Error,
    /// Warning and worse, all that is forwarded: priority 4 or less.
    Warning,
}

/// Query string of `GET /api/servers/{id}/logs`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogParams {
    /// Defaults to `end` minus one hour.
    pub start: Option<DateTime<Utc>>,
    /// Defaults to now.
    pub end: Option<DateTime<Utc>>,
    /// Defaults to warning.
    pub level: Option<Level>,
    /// Exact systemd unit, like `nginx.service`.
    pub unit: Option<String>,
    /// Text anywhere in the message, without regard to case.
    pub q: Option<String>,
    pub limit: Option<usize>,
}

/// One line.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LogLine {
    pub time: DateTime<Utc>,
    pub message: String,
    pub priority: Option<u8>,
    pub unit: Option<String>,
    pub identifier: Option<String>,
    pub pid: Option<u32>,
}

/// Body of `GET /api/servers/{id}/logs`.
#[derive(Debug, Serialize)]
pub struct LogList {
    /// Newest first.
    pub lines: Vec<LogLine>,
    /// More lines matched than `limit`.
    pub more: bool,
    /// Units of the range whatever the other filters, sorted.
    pub units: Vec<String>,
}

/// A checked request.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub level: Level,
    pub unit: Option<String>,
    pub q: Option<String>,
    pub limit: usize,
}

impl Request {
    pub fn check(params: LogParams, now: DateTime<Utc>) -> Result<Self, ApiError> {
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
        let limit = params.limit.unwrap_or(DEFAULT_LIMIT);
        if !(1..=MAX_LIMIT).contains(&limit) {
            return Err(ApiError::Invalid(format!("limit must be 1 to {MAX_LIMIT}")));
        }
        let filter = |name: &str, value: Option<String>| -> Result<Option<String>, ApiError> {
            match value {
                Some(v) if v.len() > MAX_FILTER || v.chars().any(char::is_control) => {
                    Err(ApiError::Invalid(format!(
                        "{name} must be at most {MAX_FILTER} bytes without control characters"
                    )))
                }
                Some(v) if v.trim().is_empty() => Ok(None),
                v => Ok(v),
            }
        };
        Ok(Self {
            start,
            end,
            level: params.level.unwrap_or(Level::Warning),
            unit: filter("unit", params.unit)?,
            q: filter("q", params.q)?,
            limit,
        })
    }
}

/// The LogsQL stream filter of `target`.
pub fn stream(target: &Target) -> String {
    format!(
        "{{peer={},site={}}}",
        quote(&target.peer),
        quote(&target.site)
    )
}

/// The filters of the lines query, without the pipes.
pub fn filters(target: &Target, request: &Request) -> String {
    let mut query = format!("{} source:={}", stream(target), quote(SOURCE));
    let max = match request.level {
        Level::Error => 3,
        Level::Warning => 4,
    };
    query.push_str(&format!(" priority:<={max}"));
    if let Some(unit) = &request.unit {
        query.push_str(&format!(" unit:={}", quote(unit)));
    }
    if let Some(q) = &request.q {
        let pattern = format!("(?i){}", regex::escape(q));
        query.push_str(&format!(" _msg:~{}", quote(&pattern)));
    }
    query
}

/// The query of the newest `limit + 1` lines, to see if more are left.
pub fn lines_query(target: &Target, request: &Request) -> String {
    format!(
        "{} | sort by (_time desc) | limit {}",
        filters(target, request),
        request.limit + 1
    )
}

/// The query of the range's units, whatever the other filters.
pub fn units_query(target: &Target) -> String {
    format!(
        "{} source:={} unit:* | stats by (unit) count() lines | sort by (lines desc) | limit {MAX_UNITS}",
        stream(target),
        quote(SOURCE)
    )
}

/// Parses VictoriaLogs' JSON lines.
fn objects(body: &[u8]) -> Result<Vec<serde_json::Map<String, Value>>, ApiError> {
    String::from_utf8_lossy(body)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line)
                .map_err(|_| ApiError::LogsUnavailable("unexpected query result".into()))
        })
        .collect()
}

fn text(object: &serde_json::Map<String, Value>, name: &str) -> Option<String> {
    match object.get(name)? {
        Value::String(s) if s.is_empty() => None,
        Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

/// A line of the lines query; `None` without a time.
pub fn line(object: &serde_json::Map<String, Value>) -> Option<LogLine> {
    let time = DateTime::parse_from_rfc3339(&text(object, "_time")?)
        .ok()?
        .to_utc();
    Some(LogLine {
        time,
        message: text(object, "_msg").unwrap_or_default(),
        priority: text(object, "priority").and_then(|p| p.parse().ok()),
        unit: text(object, "unit"),
        identifier: text(object, "identifier"),
        pid: text(object, "pid").and_then(|p| p.parse().ok()),
    })
}

fn backend_error(error: QueryError) -> ApiError {
    match error {
        QueryError::Timeout => ApiError::LogsTimeout,
        QueryError::Rejected(message) => {
            tracing::warn!(%message, "VictoriaLogs rejected a query");
            ApiError::Invalid(
                "logs backend rejected the query; narrow the range or the filters".into(),
            )
        }
        other => ApiError::LogsUnavailable(other.to_string()),
    }
}

/// `GET /api/servers/{id}/logs`: the server's lines, newest first.
pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    params: Result<Query<LogParams>, QueryRejection>,
) -> Result<Json<LogList>, ApiError> {
    let server: Uuid = id.parse().map_err(|_| ApiError::NotFound)?;
    let params = params
        .map(|Query(p)| p)
        .map_err(|rejection| ApiError::Invalid(rejection.body_text()))?;
    let request = Request::check(params, Utc::now())?;
    let row: Option<(String, Uuid, String)> = sqlx::query_as(
        "SELECT s.peer_id, s.site_id, si.name FROM servers s
         JOIN sites si ON si.id = s.site_id WHERE s.id = $1",
    )
    .bind(server)
    .fetch_optional(&state.pool)
    .await?;
    authorize(&user, row.as_ref().map(|r| r.1), false)?;
    let (peer, _, site) = row.ok_or(ApiError::NotFound)?;
    let target = Target { peer, site };
    let vl = state
        .logs
        .clone()
        .ok_or_else(|| ApiError::LogsUnavailable("no VictoriaLogs configured".into()))?;
    let time = |t: DateTime<Utc>| t.to_rfc3339_opts(SecondsFormat::Millis, true);
    let params = |query: String| {
        vec![
            ("query", query),
            ("start", time(request.start)),
            ("end", time(request.end)),
            ("extra_stream_filters", stream(&target)),
        ]
    };
    let lines_params = params(lines_query(&target, &request));
    let units_params = params(units_query(&target));
    let (lines, units) = tokio::join!(
        vl.get_raw("/select/logsql/query", &lines_params),
        vl.get_raw("/select/logsql/query", &units_params),
    );
    let mut lines: Vec<LogLine> = objects(&lines.map_err(backend_error)?)?
        .iter()
        .filter_map(line)
        .collect();
    lines.sort_by_key(|line| std::cmp::Reverse(line.time));
    let more = lines.len() > request.limit;
    lines.truncate(request.limit);
    let mut units: Vec<String> = objects(&units.map_err(backend_error)?)?
        .iter()
        .filter_map(|o| text(o, "unit"))
        .collect();
    units.sort();
    Ok(Json(LogList { lines, more, units }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> Target {
        Target {
            peer: "agent-1".into(),
            site: "paris".into(),
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-22T14:00:00Z")
            .unwrap()
            .to_utc()
    }

    #[test]
    fn defaults_are_the_last_hour_of_warnings() {
        let request = Request::check(LogParams::default(), now()).unwrap();
        assert_eq!(request.end - request.start, Duration::hours(1));
        assert_eq!(
            (request.level, request.limit),
            (Level::Warning, DEFAULT_LIMIT)
        );
        assert_eq!(
            lines_query(&target(), &request),
            r#"{peer="agent-1",site="paris"} source:="journal" priority:<=4 | sort by (_time desc) | limit 201"#
        );
    }

    #[test]
    fn filters_are_quoted_never_pasted() {
        let params = LogParams {
            level: Some(Level::Error),
            unit: Some(r#"a" or peer:="b"#.into()),
            q: Some(r#"disk (full) "x""#.into()),
            ..LogParams::default()
        };
        let request = Request::check(params, now()).unwrap();
        assert_eq!(
            filters(&target(), &request),
            r#"{peer="agent-1",site="paris"} source:="journal" priority:<=3 unit:="a\" or peer:=\"b" _msg:~"(?i)disk \\(full\\) \"x\"""#
        );
    }

    #[test]
    fn bad_requests_are_refused() {
        let at = |h: u32| {
            DateTime::parse_from_rfc3339(&format!("2026-10-22T{h:02}:00:00Z"))
                .unwrap()
                .to_utc()
        };
        let refused = |params: LogParams| Request::check(params, now()).is_err();
        assert!(refused(LogParams {
            start: Some(at(12)),
            end: Some(at(11)),
            ..LogParams::default()
        }));
        assert!(refused(LogParams {
            start: Some(now() - Duration::days(32)),
            ..LogParams::default()
        }));
        assert!(refused(LogParams {
            limit: Some(MAX_LIMIT + 1),
            ..LogParams::default()
        }));
        assert!(refused(LogParams {
            limit: Some(0),
            ..LogParams::default()
        }));
        assert!(refused(LogParams {
            q: Some("a\nb".into()),
            ..LogParams::default()
        }));
        assert!(refused(LogParams {
            unit: Some("u".repeat(MAX_FILTER + 1)),
            ..LogParams::default()
        }));
        // Blank filters are no filter.
        let blank = Request::check(
            LogParams {
                q: Some("  ".into()),
                ..LogParams::default()
            },
            now(),
        )
        .unwrap();
        assert_eq!(blank.q, None);
    }

    #[test]
    fn lines_are_read_from_victorialogs_json() {
        let body = br#"{"_msg":"disk full","_time":"2026-10-22T13:59:00.5Z","priority":"3","unit":"nginx.service","pid":"42","peer":"agent-1"}
{"_msg":"x","_time":"not a time"}

{"_msg":"no unit","_time":"2026-10-22T13:58:00Z","identifier":"kernel"}
"#;
        let lines: Vec<_> = objects(body).unwrap().iter().filter_map(line).collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0],
            LogLine {
                time: DateTime::parse_from_rfc3339("2026-10-22T13:59:00.5Z")
                    .unwrap()
                    .to_utc(),
                message: "disk full".into(),
                priority: Some(3),
                unit: Some("nginx.service".into()),
                identifier: None,
                pid: Some(42),
            }
        );
        assert_eq!(
            (lines[1].unit.as_deref(), lines[1].identifier.as_deref()),
            (None, Some("kernel"))
        );
        assert!(objects(b"not json").is_err());
    }
}
