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
use super::{check_templates, checks, recommendations, sensor_values, website_checks};
use crate::recommend;

#[derive(OpenApi)]
#[openapi(
    paths(sensor_values, website_checks, recommendations, dismiss_recommendation, restore_recommendation,
        check_usage, list_check_templates),
    components(schemas(recommend::Recommendation, recommend::Proposed, recommend::Params,
        check_templates::TemplateView, check_templates::ParamView, check_templates::MetricView,
        check_templates::AlertView)),
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

#[utoipa::path(get, path = "/api/v1/servers/{id}/website-checks", tag = "sensors",
    summary = "Read the last run of a server's website checks",
    description = "Each website check the server has (kind `http`, ADR-0150), whether its agent, the \
central server or a relay (`relay`, its ID) runs it, and what its last run found: `up` and, when not, `reason` (`dns`, `connect`, \
`timeout`, `tls`, `certificate`, `status`, `content`, `redirects`, `refused` or `protocol`), the status code, \
the time from the start to the end of the page, the redirects followed and where they ended, whether the \
text conditions passed, and the certificate of an https site: whether it is trusted, valid now and for the \
name (`reason`: `expired`, `not_yet_valid`, `self_signed`, `untrusted`, `name`, `revoked` or `other`), its \
days left, expiry, issuer and subject. Values are `null` before the first run. Checks are added and changed \
with `POST /api/v1/servers/{id}/sensor_configs` and kind `http`.\n\n\
**Who**: any role on the server's site. **Area**: `servers`.",
    params(("id" = uuid::Uuid, Path, description = "The server")),
    responses(
        (status = 200, description = "The website checks and their last run",
            body = website_checks::WebsiteChecks,
            example = json!({"checks": [{
                "name": "http", "instance": "shop", "entry_id": "0f8e4c1a-6d1b-4c55-9a5e-2b1f8b7a9c10",
                "origin": "server", "url": "https://shop.example.com/", "run_by": "central", "relay": null,
                "interval_ms": 60_000, "last_at": "2026-11-18T09:12:00Z", "up": true, "reason": null,
                "status_code": 200, "response_seconds": 0.212, "redirects": 1,
                "final_url": "https://www.shop.example.com/", "content_ok": true,
                "certificate": {"ok": true, "reason": null, "name_ok": true, "days_left": 41.5,
                    "not_after": "2026-12-30T00:00:00Z", "issuer": "R11", "subject": "www.shop.example.com"}
            }]})),
        (status = 403, description = "Outside the key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such server, or outside the key's reach: `not_found`",
            body = ErrorBody),
        (status = 502, description = "The metrics database is unreachable or not configured: \
`metrics_unavailable`", body = ErrorBody),
        (status = 504, description = "The metrics database did not answer in time: `metrics_timeout`",
            body = ErrorBody),
    ))]
fn website_checks() {}

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

#[utoipa::path(get, path = "/api/v1/check-templates", tag = "sensors",
    summary = "List the ready-made application checks",
    description = "The templates of the ready-made application checks (ADR-0183): PostgreSQL \
(`postgresql`), MySQL and MariaDB (`mysql`), SQL Server (`mssql`), Redis (`redis`) and Docker containers \
(`docker`). Each says who \
runs it (`agent_or_probe`: the server's agent against `127.0.0.1`, or the central server or a relay against \
the server's or device's address; `agent`: the server's agent only), its parameters with their defaults, the \
metrics it writes, and the alerts that come with it. Metrics are named after the check: \
`monitorflow_<check name>_<suffix>`, shown here for a check of the default name, its kind.\n\n\
A check is added with `POST /api/v1/servers/{id}/sensor_configs` (or a site's), with its `kind`, its \
`params` (the `password` is kept sealed and never returned) and `\"default_alerts\": true` to add the \
template's alerts as ordinary thresholds where the server or site has none on that metric yet. The account \
should be read-only: the guides list the least privileges per database.\n\n\
**Who**: any role. **Area**: `alerts`.",
    responses(
        (status = 200, description = "The templates", body = check_templates::CheckTemplates,
            example = json!({"templates": [{
                "kind": "redis", "title": "Redis", "runs": "agent_or_probe", "default_port": 6379,
                "params": [
                    {"name": "host", "default": "127.0.0.1",
                     "description": "Address or name to connect to; from a probe, the device's address by default"},
                    {"name": "password", "default": null,
                     "description": "Kept sealed by the server, never returned; absent from an update, the stored one is kept"}
                ],
                "metrics": [
                    {"metric": "monitorflow_redis_up", "suffix": "up", "unit": "", "per": null,
                     "description": "1 when it answered and the login worked; 0 with labels `reason` and `error`"},
                    {"metric": "monitorflow_redis_memory_usage_ratio", "suffix": "memory_usage_ratio",
                     "unit": "ratio", "per": null, "description": "Used memory against `maxmemory`, when set"}
                ],
                "alerts": [
                    {"metric": "monitorflow_redis_up", "comparison": "<", "warning": null, "critical": 1.0,
                     "duration_s": 120},
                    {"metric": "monitorflow_redis_memory_usage_ratio", "comparison": ">", "warning": 0.8,
                     "critical": 0.95, "duration_s": 300}
                ]
            }]})),
        (status = 403, description = "Outside the key's areas: `api_key_scope`", body = ErrorBody),
    ))]
fn list_check_templates() {}
