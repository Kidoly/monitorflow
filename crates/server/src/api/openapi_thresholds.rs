//! The OpenAPI description of thresholds (ADR-0017). Merged into the
//! document by [`super::openapi::build`]; the router in `mod.rs` serves
//! the routes.

#![allow(dead_code)] // The functions exist for their `#[utoipa::path]`.

use utoipa::OpenApi;

use super::openapi::{Common, ErrorBody};
use super::{sensors, thresholds};

#[derive(OpenApi)]
#[openapi(
    paths(
        server_thresholds_effective, site_thresholds_effective, list_server_thresholds,
        create_server_threshold, list_site_thresholds, create_site_threshold, update_threshold,
        delete_threshold,
    ),
    components(schemas(sensors::Origin, thresholds::EffectiveThreshold)),
    modifiers(&Common),
    tags(
        (name = "thresholds", description = "When a measurement opens an incident. Thresholds come \
in three layers: built-in defaults, a site's rows and a server's rows; for one metric and instance \
the server's row wins over the site's, which wins over the default, and a row with `enabled` false \
turns the threshold off for the layers below. The central server evaluates them: nothing is sent to \
agents, and the API never writes incidents. Every role reads the thresholds of the sites it reads; \
operators and above change them. Needs the key area \"alerts\"."),
    )
)]
pub struct Doc;

fn threshold(id: &str, site: Option<&str>, server: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "site_id": site,
        "server_id": server,
        "metric": "monitorflow_disk_used_ratio",
        "instance": "/var",
        "comparison": ">",
        "warning": 0.8,
        "critical": 0.9,
        "duration_s": 300,
        "hysteresis": 0.02,
        "enabled": true,
        "created_at": "2026-11-03T10:00:00Z",
        "updated_at": "2026-11-03T10:00:00Z"
    })
}
fn server_threshold() -> serde_json::Value {
    threshold(
        "7c8d9e0f-1a2b-4c3d-8e4f-5a6b7c8d9e0f",
        None,
        Some("1e2f3a4b-5c6d-4e7f-8a9b-0c1d2e3f4a5b"),
    )
}
fn site_threshold() -> serde_json::Value {
    threshold(
        "8d9e0f1a-2b3c-4d4e-9f5a-6b7c8d9e0f1a",
        Some("3c4d5e6f-7a8b-4c9d-8e0f-1a2b3c4d5e6f"),
        None,
    )
}
fn server_thresholds() -> serde_json::Value {
    serde_json::json!({"entries": [server_threshold()]})
}
fn site_thresholds() -> serde_json::Value {
    serde_json::json!({"entries": [site_threshold()]})
}
fn effective_thresholds() -> serde_json::Value {
    serde_json::json!([{
        "origin": "default",
        "threshold_id": null,
        "metric": "monitorflow_disk_used_ratio",
        "instance": null,
        "enabled": true,
        "comparison": ">",
        "warning": 0.85,
        "critical": 0.95,
        "duration_s": 300,
        "hysteresis": 0.02
    }, {
        "origin": "server",
        "threshold_id": "7c8d9e0f-1a2b-4c3d-8e4f-5a6b7c8d9e0f",
        "metric": "monitorflow_disk_used_ratio",
        "instance": "/var",
        "enabled": true,
        "comparison": ">",
        "warning": 0.8,
        "critical": 0.9,
        "duration_s": 300,
        "hysteresis": 0.02
    }, {
        "origin": "site",
        "threshold_id": "9e0f1a2b-3c4d-4e5f-8a6b-7c8d9e0f1a2b",
        "metric": "monitorflow_diskio_wait_seconds",
        "instance": null,
        "enabled": false,
        "comparison": ">",
        "warning": 0.2,
        "critical": null,
        "duration_s": 600,
        "hysteresis": 0.02
    }])
}
fn server_effective() -> serde_json::Value {
    serde_json::json!({
        "server_id": "1e2f3a4b-5c6d-4e7f-8a9b-0c1d2e3f4a5b",
        "site_id": "3c4d5e6f-7a8b-4c9d-8e0f-1a2b3c4d5e6f",
        "revision": 7,
        "thresholds": effective_thresholds()
    })
}
fn site_effective() -> serde_json::Value {
    let mut thresholds = effective_thresholds();
    thresholds.as_array_mut().unwrap().remove(1);
    serde_json::json!({"site_id": "3c4d5e6f-7a8b-4c9d-8e0f-1a2b3c4d5e6f", "thresholds": thresholds})
}

/// See the effective thresholds of a server
///
/// The thresholds the central server applies to the server: built-in
/// defaults overlaid with its site's rows and its own, each with its
/// `origin` and the row it comes from. Turned-off ones are listed with
/// `enabled` false and the row that turns them off. `revision` changes
/// with every change of the server's or its site's rows.
///
/// Needs any role on the server's site and the key area "alerts".
#[utoipa::path(get, path = "/api/v1/servers/{id}/thresholds/effective", tag = "thresholds",
    params(("id" = uuid::Uuid, Path, description = "The server")),
    responses(
        (status = 200, body = thresholds::Effective, example = server_effective),
        (status = 404, body = ErrorBody, description = "`not_found`: no such server, or one on a \
site the caller does not read"),
    ))]
fn server_thresholds_effective() {}

/// See the effective thresholds of a site
///
/// What a server of the site without rows of its own gets: the built-in
/// defaults overlaid with the site's rows, each with its `origin`.
///
/// Needs any role on the site and the key area "alerts".
#[utoipa::path(get, path = "/api/v1/sites/{id}/thresholds/effective", tag = "thresholds",
    params(("id" = uuid::Uuid, Path, description = "The site")),
    responses(
        (status = 200, body = thresholds::SiteEffective, example = site_effective),
        (status = 404, body = ErrorBody, description = "`not_found`: no such site, or one the \
caller does not read"),
    ))]
fn site_thresholds_effective() {}

/// List a server's thresholds
///
/// The server's own rows, by metric and instance, the row for every
/// instance first. Rows of its site and built-in defaults are not
/// included: see the effective thresholds for what applies.
///
/// Needs any role on the server's site and the key area "alerts".
#[utoipa::path(get, path = "/api/v1/servers/{id}/thresholds", tag = "thresholds",
    params(("id" = uuid::Uuid, Path, description = "The server")),
    responses(
        (status = 200, body = thresholds::EntryList, example = server_thresholds),
        (status = 404, body = ErrorBody, description = "`not_found`: no such server, or one on a \
site the caller does not read"),
    ))]
fn list_server_thresholds() {}

/// Add a threshold to a server
///
/// Adds a row for one server, which wins over its site's row and the
/// built-in default for the same metric and instance. The central server
/// applies it at its next poll; nothing is sent to the agent. An enabled
/// row needs `warning` or `critical`; with `enabled` false it turns the
/// threshold off for this server.
///
/// Needs an operator role or above on the server's site and the key area
/// "alerts". Audited as `threshold.create`. The same metric and instance
/// twice is 409.
#[utoipa::path(post, path = "/api/v1/servers/{id}/thresholds", tag = "thresholds",
    params(("id" = uuid::Uuid, Path, description = "The server")),
    request_body(content = thresholds::EntryRequest, example = json!({
        "metric": "monitorflow_disk_used_ratio",
        "instance": "/var",
        "comparison": ">",
        "warning": 0.8,
        "critical": 0.9,
        "duration_s": 300,
        "hysteresis": 0.02
    })),
    responses(
        (status = 201, body = thresholds::Entry, example = server_threshold),
        (status = 400, body = ErrorBody, description = "`invalid`, with `error` saying why: not the expected JSON, a \
`metric` that is not `monitorflow_` and a sensor metric name, a bad `instance`, a `comparison` other \
than `>`, `>=`, `<`, `<=`, an enabled row without `warning` nor `critical`, `critical` on the safe \
side of `warning`, `duration_s` over 86400, a negative `hysteresis`, or a value that is not finite"),
        (status = 403, body = ErrorBody, description = "`forbidden`: the caller reads the site \
without an operator role. `api_key_scope`: the key lacks the area"),
        (status = 404, body = ErrorBody, description = "`not_found`: no such server, or one on a \
site the caller does not read"),
        (status = 409, body = ErrorBody, description = "`conflict`: a threshold on this metric and \
instance already exists for the server"),
    ))]
fn create_server_threshold() {}

/// List a site's thresholds
///
/// The site's own rows, by metric and instance, the row for every
/// instance first. Rows of its servers are listed per server.
///
/// Needs any role on the site and the key area "alerts".
#[utoipa::path(get, path = "/api/v1/sites/{id}/thresholds", tag = "thresholds",
    params(("id" = uuid::Uuid, Path, description = "The site")),
    responses(
        (status = 200, body = thresholds::EntryList, example = site_thresholds),
        (status = 404, body = ErrorBody, description = "`not_found`: no such site, or one the \
caller does not read"),
    ))]
fn list_site_thresholds() {}

/// Add a threshold to a site
///
/// Adds a row for every server of the site, which wins over the built-in
/// default for the same metric and instance; a server's own row still
/// wins over it. The central server applies it at its next poll; nothing
/// is sent to agents. An enabled row needs `warning` or `critical`; with
/// `enabled` false it turns the threshold off for the site.
///
/// Needs an operator role or above on the site and the key area "alerts".
/// Audited as `threshold.create`. The same metric and instance twice is
/// 409.
#[utoipa::path(post, path = "/api/v1/sites/{id}/thresholds", tag = "thresholds",
    params(("id" = uuid::Uuid, Path, description = "The site")),
    request_body(content = thresholds::EntryRequest, example = json!({
        "metric": "monitorflow_disk_used_ratio",
        "instance": "/var",
        "comparison": ">",
        "warning": 0.8,
        "critical": 0.9,
        "duration_s": 300,
        "hysteresis": 0.02
    })),
    responses(
        (status = 201, body = thresholds::Entry, example = site_threshold),
        (status = 400, body = ErrorBody, description = "`invalid`, with `error` saying why: not the expected JSON, a \
`metric` that is not `monitorflow_` and a sensor metric name, a bad `instance`, a `comparison` other \
than `>`, `>=`, `<`, `<=`, an enabled row without `warning` nor `critical`, `critical` on the safe \
side of `warning`, `duration_s` over 86400, a negative `hysteresis`, or a value that is not finite"),
        (status = 403, body = ErrorBody, description = "`forbidden`: the caller reads the site \
without an operator role. `api_key_scope`: the key lacks the area"),
        (status = 404, body = ErrorBody, description = "`not_found`: no such site, or one the \
caller does not read"),
        (status = 409, body = ErrorBody, description = "`conflict`: a threshold on this metric and \
instance already exists for the site"),
    ))]
fn create_site_threshold() {}

/// Replace a threshold
///
/// Replaces every field of a site's or a server's row; it stays with its
/// site or server. Absent optional fields take their defaults, as when
/// creating. The central server applies it at its next poll.
///
/// Needs an operator role or above on the row's site and the key area
/// "alerts". Audited as `threshold.update` with the row before and after.
/// Safe to repeat.
#[utoipa::path(put, path = "/api/v1/thresholds/{id}", tag = "thresholds",
    params(("id" = uuid::Uuid, Path, description = "The row")),
    request_body(content = thresholds::EntryRequest, example = json!({
        "metric": "monitorflow_disk_used_ratio",
        "instance": "/var",
        "comparison": ">",
        "warning": 0.8,
        "critical": 0.9,
        "duration_s": 300,
        "hysteresis": 0.02
    })),
    responses(
        (status = 200, body = thresholds::Entry, example = server_threshold),
        (status = 400, body = ErrorBody, description = "`invalid`, with `error` saying why: not the expected JSON, a \
`metric` that is not `monitorflow_` and a sensor metric name, a bad `instance`, a `comparison` other \
than `>`, `>=`, `<`, `<=`, an enabled row without `warning` nor `critical`, `critical` on the safe \
side of `warning`, `duration_s` over 86400, a negative `hysteresis`, or a value that is not finite"),
        (status = 403, body = ErrorBody, description = "`forbidden`: the caller reads the site \
without an operator role. `api_key_scope`: the key lacks the area"),
        (status = 404, body = ErrorBody, description = "`not_found`: no such row, or one of a site \
the caller does not read"),
        (status = 409, body = ErrorBody, description = "`conflict`: another row of the same site or \
server has this metric and instance"),
    ))]
fn update_threshold() {}

/// Delete a threshold
///
/// Removes a site's or a server's row: the layer below applies again
/// (the site's row, then the built-in default). Incidents it opened
/// resolve or stay as the engine decides at its next poll.
///
/// Needs an operator role or above on the row's site and the key area
/// "alerts". Audited as `threshold.delete`. Deleting again is 404.
#[utoipa::path(delete, path = "/api/v1/thresholds/{id}", tag = "thresholds",
    params(("id" = uuid::Uuid, Path, description = "The row")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 403, body = ErrorBody, description = "`forbidden`: the caller reads the site \
without an operator role. `api_key_scope`: the key lacks the area"),
        (status = 404, body = ErrorBody, description = "`not_found`: no such row, or one of a site \
the caller does not read"),
    ))]
fn delete_threshold() {}
