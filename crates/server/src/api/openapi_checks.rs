//! The OpenAPI description of what helps choose a server's checks: the
//! values its sensors report now, the checks recommended from what its
//! agent saw (ADR-0134), and how many servers run each check. Merged into
//! the document by [`super::openapi::build`]; the router in `mod.rs`
//! serves the routes.

#![allow(dead_code)] // The functions exist for their `#[utoipa::path]`.

use serde_json::json;
use utoipa::OpenApi;

use super::openapi::{Common, ErrorBody};
use super::openapi_core::ex;
use super::{checks, recommendations, sensor_values};
use crate::recommend;

#[derive(OpenApi)]
#[openapi(
    paths(sensor_values, recommendations, dismiss_recommendation, restore_recommendation, check_usage),
    components(schemas(recommend::Recommendation, recommend::Proposed, recommend::Params)),
    modifiers(&Common),
)]
pub struct Doc;

fn recommendation() -> serde_json::Value {
    json!({
        "key": "postgresql",
        "title": "PostgreSQL is running",
        "evidence": ["postgresql@16-main.service is running", "port 5432 is listening"],
        "checks": [
            {"kind": "systemd", "params": {"units": "postgresql@16-main.service"}},
            {"kind": "port", "params": {"port": "5432", "host": "127.0.0.1"}}
        ],
        "dismissed_by": null,
        "dismissed_at": null
    })
}

#[utoipa::path(get, path = "/api/v1/servers/{id}/sensor-values", tag = "sensors",
    summary = "Read the recent values of a server's sensors",
    description = "Every sensor the server runs (its effective configuration), each with the metric that \
stands for it and, per instance seen, a sparkline of the last hour (30 points, one every two minutes, each the \
worst value of its step: the highest for usage, the lowest for what is left or up), the last value and its \
time. `reason` says why a sensor has no series at all (\"No data yet\") or why an instance's last sample is \
late (\"No data for 5 minutes\"). For other ranges or metrics, use the metric queries.\n\n\
**Who**: any role on the server's site. **Area**: `servers`.",
    params(("id" = uuid::Uuid, Path, description = "The server")),
    responses(
        (status = 200, description = "The sensors and their values", body = sensor_values::SensorValues,
            example = json!({"start": 1_763_290_800, "end": 1_763_294_400, "step": 120, "sensors": [
                {"name": "cpu", "instance": "", "metric": "monitorflow_cpu_usage_ratio", "reason": null,
                 "series": [{"instance": "total", "points": [[1_763_290_800, 0.12], [1_763_290_920, 0.31]],
                             "last_value": 0.18, "last_at": "2026-11-16T12:39:40Z", "reason": null}]},
                {"name": "port_5432", "instance": "", "metric": "monitorflow_port_5432_up", "reason": null,
                 "series": [{"instance": "127.0.0.1:5432", "points": [[1_763_290_800, 1.0], [1_763_290_920, null]],
                             "last_value": 1.0, "last_at": "2026-11-16T12:34:12Z",
                             "reason": "No data for 5 minutes"}]},
                {"name": "service_nginx", "instance": "", "metric": "monitorflow_service_nginx_active",
                 "reason": "No data yet", "series": []}
            ]})),
        (status = 403, description = "Outside the key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such server, or outside the key's reach: `not_found`",
            body = ErrorBody),
        (status = 502, description = "The metrics database is unreachable or not configured: \
`metrics_unavailable`", body = ErrorBody),
        (status = 504, description = "The metrics database did not answer in time: `metrics_timeout`",
            body = ErrorBody),
    ))]
fn sensor_values() {}

#[utoipa::path(get, path = "/api/v1/servers/{id}/recommendations", tag = "sensors",
    summary = "List the checks recommended for a server",
    description = "What the server's agent saw running (services, listening ports, mounted shares) that \
none of its enabled checks covers yet, one recommendation per thing seen, each with the checks that would \
cover it: a kind and its `params`, as `POST /api/v1/servers/{id}/sensor_configs` takes them. `reported_at` is \
when the agent last reported; it is null before its first report and for agents that do not report (Windows, \
older ones), and the list is then empty. A dismissed recommendation stays listed with `dismissed_by` and \
`dismissed_at` until it is restored or no longer applies.\n\n\
Adding one is adding its checks, and the thresholds that go with them, through the routes of checks and \
thresholds: MonitorFlow adds nothing by itself.\n\n\
**Who**: any role on the server's site. **Area**: `servers`.",
    params(("id" = uuid::Uuid, Path, description = "The server")),
    responses(
        (status = 200, description = "The recommendations", body = recommendations::RecommendationList,
            example = json!({"reported_at": "2026-11-16T08:02:11Z", "recommendations": [
                recommendation(),
                ex::merge(json!({"key": "share:/mnt/backup", "title": "/mnt/backup is a share on nas.lan",
                    "evidence": ["/mnt/backup is mounted from nas.lan (nfs4)"],
                    "checks": [{"kind": "port", "params": {"port": "2049", "host": "nas.lan"}}]}),
                    json!({"dismissed_by": "alice", "dismissed_at": "2026-11-12T10:20:00Z"}))
            ]})),
        (status = 403, description = "Outside the key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such server, or outside the key's reach: `not_found`",
            body = ErrorBody),
    ))]
fn recommendations() {}

#[utoipa::path(post, path = "/api/v1/servers/{id}/recommendations/dismiss", tag = "sensors",
    summary = "Dismiss a recommendation of a server",
    description = "Hides a current recommendation of the server for good, for everyone: it is listed as \
dismissed, by whom and when. Only a recommendation the server has now can be dismissed. Audited as \
`check_recommendation.dismiss`. Safe to repeat.\n\n\
**Who**: operator or above (\"Read and change\") on the server's site. **Area**: `servers`.",
    params(("id" = uuid::Uuid, Path, description = "The server")),
    request_body(content = recommendations::KeyRequest, example = json!({"key": "postgresql"})),
    responses(
        (status = 204, description = "Dismissed"),
        (status = 400, description = "Not the expected JSON: `invalid`", body = ErrorBody),
        (status = 403, description = "The key may only read the server's site: `forbidden`; or outside \
the key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such server, outside the key's reach, or no current \
recommendation with this `key`: `not_found`", body = ErrorBody),
    ))]
fn dismiss_recommendation() {}

#[utoipa::path(post, path = "/api/v1/servers/{id}/recommendations/restore", tag = "sensors",
    summary = "Restore a dismissed recommendation of a server",
    description = "Shows a dismissed recommendation of the server again. Audited as \
`check_recommendation.restore`. Repeating it is 404: it is no longer dismissed.\n\n\
**Who**: operator or above (\"Read and change\") on the server's site. **Area**: `servers`.",
    params(("id" = uuid::Uuid, Path, description = "The server")),
    request_body(content = recommendations::KeyRequest, example = json!({"key": "share:/mnt/backup"})),
    responses(
        (status = 204, description = "Shown again"),
        (status = 400, description = "Not the expected JSON: `invalid`", body = ErrorBody),
        (status = 403, description = "The key may only read the server's site: `forbidden`; or outside \
the key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such server, outside the key's reach, or no dismissed \
recommendation with this `key`: `not_found`", body = ErrorBody),
    ))]
fn restore_recommendation() {}

#[utoipa::path(get, path = "/api/v1/checks/usage", tag = "sensors",
    summary = "Count the servers running each check",
    description = "Each enabled check with the number of servers that run it, over the sites within the \
key's reach, most servers first, at most 200. A site's check counts every server of the site that is not \
removed; a server's own check counts that server. A server with its own copy of a site's check counts twice. \
The web interface shows the first few as the \"Most used\" checks.\n\n\
**Who**: any role. **Area**: `alerts`.",
    responses(
        (status = 200, description = "Checks by servers", body = checks::UsageList,
            example = json!({"checks": [
                {"kind": "systemd", "name": "service_nginx", "plugin": null, "servers": 42},
                {"kind": "port", "name": "port_443", "plugin": null, "servers": 40},
                {"kind": "plugin", "name": "http_status_shop", "plugin": "http-status", "servers": 3}
            ]})),
        (status = 403, description = "The key's maker has no role: `forbidden`; or outside the key's \
areas: `api_key_scope`", body = ErrorBody),
    ))]
fn check_usage() {}
