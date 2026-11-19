//! What a rule would have done over the last days (ADR-0153): the
//! history of a server's series read from VictoriaMetrics and replayed
//! through the engine's code ([`crate::alerting::preview`]).
//!
//! Read-only: any role on the site may preview, nothing is written and
//! nothing is audited. Queries carry the server's `peer` and `site`
//! matchers and `extra_label` filters, as ADR-0018's do.

use std::collections::HashMap;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::Json;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::access::authorize;
use super::auth::CurrentUser;
use super::metrics::{backend_error, quote, Target};
use super::sensors::{body, parse_id};
use super::thresholds::{validate_entry, EntryRequest};
use super::{ApiError, ApiState};
use crate::alerting::history::{matrix, Points};
use crate::alerting::preview::{replay, Inputs, PreviewSeries};
use crate::alerting::rules::{Kind, Rule};
use crate::vm_query::VmQuery;

/// Days replayed by default, and at most.
pub const DEFAULT_DAYS: u32 = 3;
pub const MAX_DAYS: u32 = 7;
/// Series of one server replayed, and servers of a site.
pub const MAX_SERIES: usize = 20;
pub const MAX_SERVERS: usize = 20;
/// Points a range query may return per series (Prometheus' limit).
const MAX_POINTS: i64 = 11_000;
const PAST_STEP_S: i64 = 300;

/// Body of both preview routes.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PreviewRequest {
    /// The rule as it would be saved.
    pub rule: EntryRequest,
    /// Days back, 1 to 7, 3 by default.
    #[serde(default)]
    pub days: Option<u32>,
}

/// Body of `POST /api/servers/{id}/thresholds/preview`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ServerPreview {
    pub server_id: Uuid,
    /// Unix milliseconds: the range replayed.
    pub from: i64,
    pub to: i64,
    pub days: u32,
    /// The series the rule judges, by instance, at most 20.
    pub series: Vec<PreviewSeries>,
    /// More series matched than were replayed.
    pub truncated: bool,
}

/// One server of a site preview.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct SitePreviewServer {
    pub server_id: Uuid,
    pub hostname: String,
    /// Incidents the rule would have opened on it, every series together.
    pub incidents: usize,
    /// Its series, without their points.
    pub series: Vec<PreviewSeries>,
}

/// Body of `POST /api/sites/{id}/thresholds/preview`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct SitePreview {
    pub site_id: Uuid,
    pub from: i64,
    pub to: i64,
    pub days: u32,
    /// Up to 20 servers of the site, by name.
    pub servers: Vec<SitePreviewServer>,
    /// The site has more servers than were replayed.
    pub more_servers: bool,
}

fn checked(request: PreviewRequest) -> Result<(Rule, u32), ApiError> {
    let days = request.days.unwrap_or(DEFAULT_DAYS);
    if !(1..=MAX_DAYS).contains(&days) {
        return Err(ApiError::Invalid(format!(
            "days must be between 1 and {MAX_DAYS}"
        )));
    }
    let valid = validate_entry(request.rule).map_err(ApiError::Invalid)?;
    Ok((valid.rule(), days))
}

/// The selector of `metric` of `target`, of one instance when given.
fn selector(metric: &str, target: &Target, instance: Option<&str>) -> String {
    let instance = instance.map_or_else(String::new, |i| format!(",instance={}", quote(i)));
    format!(
        "{metric}{{peer={},site={}{instance}}}",
        quote(&target.peer),
        quote(&target.site)
    )
}

async fn range(
    vm: &VmQuery,
    target: &Target,
    promql: String,
    (from, to, step): (i64, i64, i64),
) -> Result<Vec<(String, Points)>, ApiError> {
    let params = vec![
        ("query", promql),
        ("start", (from / 1000).to_string()),
        ("end", (to / 1000).to_string()),
        ("step", format!("{step}s")),
        ("extra_label", format!("peer={}", target.peer)),
        ("extra_label", format!("site={}", target.site)),
    ];
    let data = vm
        .get("/api/v1/query_range", &params)
        .await
        .map_err(backend_error)?;
    let mut series = matrix(&data, target)
        .ok_or_else(|| ApiError::MetricsUnavailable("unexpected query result".into()))?;
    series.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(series)
}

/// Replays `rule` on one server's history.
async fn preview_server(
    vm: &VmQuery,
    target: &Target,
    rule: &Rule,
    days: u32,
    now: i64,
) -> Result<(i64, Vec<PreviewSeries>, bool), ApiError> {
    let from = now - i64::from(days) * 86_400_000;
    // Windows are filled before the range starts, as the engine's are.
    let warm = from - rule.window_ms();
    let step = ((now - warm) / 1000 / MAX_POINTS + 1).max(60);
    let span = (warm, now, step);
    let raw = |metric: &str, instance: Option<&str>| {
        format!(
            "last_over_time({}[{step}s])",
            selector(metric, target, instance)
        )
    };
    let mut series = range(
        vm,
        target,
        raw(&rule.metric, rule.instance.as_deref()),
        span,
    )
    .await?;
    let truncated = series.len() > MAX_SERIES;
    series.truncate(MAX_SERIES);
    let mut conditions = Vec::new();
    for condition in &rule.conditions {
        let found = range(
            vm,
            target,
            raw(&condition.metric, condition.instance.as_deref()),
            span,
        )
        .await?;
        conditions.push(found.into_iter().collect::<HashMap<_, _>>());
    }
    let mut past = HashMap::new();
    if rule.kind == Kind::Compare {
        let offset = i64::from(rule.offset_s);
        let promql = format!(
            "avg_over_time({}[{}s] offset {offset}s)",
            selector(&rule.metric, target, rule.instance.as_deref()),
            rule.window_s
        );
        let step = PAST_STEP_S.max(step);
        past = range(vm, target, promql, (warm, now, step))
            .await?
            .into_iter()
            .collect();
    }
    let inputs = Inputs {
        series,
        conditions,
        past,
        past_step_ms: PAST_STEP_S.max(step) * 1000,
        from_ms: from,
        to_ms: now,
        // The engine's gap, or two steps when the history is coarser.
        max_gap_ms: (600_000).max(2 * step * 1000),
    };
    Ok((from, replay(rule, &inputs), truncated))
}

fn metrics(state: &ApiState) -> Result<VmQuery, ApiError> {
    state
        .metrics
        .clone()
        .ok_or_else(|| ApiError::MetricsUnavailable("no VictoriaMetrics configured".into()))
}

/// `POST /api/servers/{id}/thresholds/preview`: what the rule would have
/// done to the server over the last days.
pub async fn server(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<PreviewRequest>, JsonRejection>,
) -> Result<Json<ServerPreview>, ApiError> {
    let server_id = parse_id(&id)?;
    let row: Option<(String, Uuid, String)> = sqlx::query_as(
        "SELECT s.peer_id, s.site_id, si.name FROM servers s
         JOIN sites si ON si.id = s.site_id WHERE s.id = $1",
    )
    .bind(server_id)
    .fetch_optional(&state.pool)
    .await?;
    authorize(&user, row.as_ref().map(|r| r.1), false)?;
    let (peer, _, site) = row.ok_or(ApiError::NotFound)?;
    let (rule, days) = checked(body(request)?)?;
    let vm = metrics(&state)?;
    let now = Utc::now().timestamp_millis();
    let target = Target { peer, site };
    let (from, series, truncated) = preview_server(&vm, &target, &rule, days, now).await?;
    Ok(Json(ServerPreview {
        server_id,
        from,
        to: now,
        days,
        series,
        truncated,
    }))
}

/// `POST /api/sites/{id}/thresholds/preview`: the same over up to 20
/// servers of the site, by name.
pub async fn site(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<PreviewRequest>, JsonRejection>,
) -> Result<Json<SitePreview>, ApiError> {
    let site_id = parse_id(&id)?;
    let name: Option<String> = sqlx::query_scalar("SELECT name FROM sites WHERE id = $1")
        .bind(site_id)
        .fetch_optional(&state.pool)
        .await?;
    authorize(&user, name.as_ref().map(|_| site_id), false)?;
    let site = name.ok_or(ApiError::NotFound)?;
    let (rule, days) = checked(body(request)?)?;
    let vm = metrics(&state)?;
    let servers: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT id, hostname, peer_id FROM servers
         WHERE site_id = $1 AND revoked_at IS NULL AND NOT awaiting_agent
         ORDER BY hostname, id LIMIT $2",
    )
    .bind(site_id)
    .bind(MAX_SERVERS as i64 + 1)
    .fetch_all(&state.pool)
    .await?;
    let more_servers = servers.len() > MAX_SERVERS;
    let now = Utc::now().timestamp_millis();
    let mut from = now;
    let mut out = Vec::new();
    for (server_id, hostname, peer) in servers.into_iter().take(MAX_SERVERS) {
        let target = Target {
            peer,
            site: site.clone(),
        };
        let (start, mut series, _) = preview_server(&vm, &target, &rule, days, now).await?;
        from = start;
        for s in &mut series {
            s.points.clear();
        }
        out.push(SitePreviewServer {
            server_id,
            hostname,
            incidents: series.iter().map(|s| s.incidents.len()).sum(),
            series,
        });
    }
    Ok(Json(SitePreview {
        site_id,
        from: if out.is_empty() {
            now - i64::from(days) * 86_400_000
        } else {
            from
        },
        to: now,
        days,
        servers: out,
        more_servers,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn requests_are_checked_before_any_query() {
        let request = |value: serde_json::Value| -> Result<(Rule, u32), ApiError> {
            checked(serde_json::from_value(value).unwrap())
        };
        let rule = json!({"metric": "monitorflow_cpu_load1", "comparison": ">", "warning": 4});
        assert_eq!(request(json!({"rule": rule})).unwrap().1, 3);
        assert_eq!(request(json!({"rule": rule, "days": 7})).unwrap().1, 7);
        for days in [0, 8] {
            assert!(matches!(
                request(json!({"rule": rule, "days": days})),
                Err(ApiError::Invalid(m)) if m.contains("days must be")
            ));
        }
        assert!(matches!(
            request(json!({"rule": {"metric": "cpu", "comparison": ">", "warning": 1}})),
            Err(ApiError::Invalid(_))
        ));
        assert!(serde_json::from_value::<PreviewRequest>(json!({"rule": rule, "x": 1})).is_err());
    }

    #[test]
    fn selectors_quote_every_value() {
        let target = Target {
            peer: "web-1".into(),
            site: "par\"is".into(),
        };
        assert_eq!(
            selector("monitorflow_disk_used_ratio", &target, Some("/v\"ar")),
            r#"monitorflow_disk_used_ratio{peer="web-1",site="par\"is",instance="/v\"ar"}"#
        );
    }
}
