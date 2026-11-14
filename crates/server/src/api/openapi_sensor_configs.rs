//! The OpenAPI description of sensor configuration rows (ADR-0016, ADR-0097). Merged into the
//! document by [`super::openapi::build`]; the router in `mod.rs` serves
//! the routes.

#![allow(dead_code)] // The functions exist for their `#[utoipa::path]`.

use utoipa::OpenApi;

use super::openapi::{Common, ErrorBody};
use super::sensors;

#[derive(OpenApi)]
#[openapi(
    paths(
        list_server_sensor_configs, create_server_sensor_config, list_site_sensor_configs,
        create_site_sensor_config, update_sensor_config, delete_sensor_config,
    ),
    modifiers(&Common),
    tags(
        (name = "sensor configs", description = "What the agents measure and how often, beyond the \
built-in checks. Rows belong to a site (all its servers) or one server, which wins for the same \
name and instance; a row with `enabled` false removes that check from the layers below. A change \
reaches the agents of the servers concerned at their next configuration poll, as configuration, \
not as a command: no approval and no risk rules apply, and an agent still refuses what its own \
administrator did not allow (log folders, port targets, scripts). Rows are checked with the \
agent's own rules, so a row the API accepts is one the agent runs. Every role reads the rows of the \
sites it reads; operators and above change them. Needs the key area \"alerts\"."),
    )
)]
pub struct Doc;

fn sensor_config(id: &str, site: Option<&str>, server: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "site_id": site,
        "server_id": server,
        "kind": "systemd",
        "name": "web-services",
        "instance": "",
        "interval_ms": 30000,
        "timeout_ms": null,
        "params": {"units": "nginx.service,php-fpm.service"},
        "enabled": true,
        "created_at": "2026-11-03T10:05:00Z",
        "updated_at": "2026-11-03T10:05:00Z"
    })
}
fn server_sensor_config() -> serde_json::Value {
    sensor_config(
        "a0b1c2d3-e4f5-4a6b-8c7d-9e0f1a2b3c4d",
        None,
        Some("1e2f3a4b-5c6d-4e7f-8a9b-0c1d2e3f4a5b"),
    )
}
fn site_sensor_config() -> serde_json::Value {
    sensor_config(
        "b1c2d3e4-f5a6-4b7c-9d8e-0f1a2b3c4d5e",
        Some("3c4d5e6f-7a8b-4c9d-8e0f-1a2b3c4d5e6f"),
        None,
    )
}
fn server_sensor_configs() -> serde_json::Value {
    serde_json::json!({"entries": [server_sensor_config()]})
}
fn site_sensor_configs() -> serde_json::Value {
    serde_json::json!({"entries": [site_sensor_config()]})
}

/// List a server's sensor configs
///
/// The server's own rows, by name and instance, parameters included.
/// Rows of its site and the built-in checks are not included.
///
/// Needs any role on the server's site and the key area "alerts".
#[utoipa::path(get, path = "/api/v1/servers/{id}/sensor_configs", tag = "sensor configs",
    params(("id" = uuid::Uuid, Path, description = "The server")),
    responses(
        (status = 200, body = sensors::EntryList, example = server_sensor_configs),
        (status = 404, body = ErrorBody, description = "`not_found`: no such server, or one on a \
site the caller does not read"),
    ))]
fn list_server_sensor_configs() {}

/// Add a sensor config to a server
///
/// Adds a check for one server, or with `enabled` false removes a check
/// of the same name and instance that its site or the built-in defaults
/// give it. The server's agent gets it at its next configuration poll,
/// as configuration, not as a command: nothing waits for approval. The
/// agent still refuses a check its own administrator did not allow.
///
/// `kind` is one the agent knows (`cpu`, `memory`, `disk`, `diskio`,
/// `systemd`, `port`, `log`, `script`, `eventlog`, `windows_service`) or
/// `check` for a check plugin turned on for the site, named by the
/// `plugin` parameter.
///
/// Needs an operator role or above on the server's site and the key area
/// "alerts". Audited as `sensor_config.create`, with parameter names only.
/// The same name and instance twice is 409.
#[utoipa::path(post, path = "/api/v1/servers/{id}/sensor_configs", tag = "sensor configs",
    params(("id" = uuid::Uuid, Path, description = "The server")),
    request_body(content = sensors::EntryRequest, example = json!({
        "kind": "systemd",
        "name": "web-services",
        "interval_ms": 30000,
        "params": {"units": "nginx.service,php-fpm.service"}
    })),
    responses(
        (status = 201, body = sensors::Entry, example = server_sensor_config),
        (status = 400, body = ErrorBody, description = "`invalid`, with `error` saying why: not the expected JSON, a \
bad `kind`, `name` or `instance`, `interval_ms` outside 10 s to 24 h, a timeout longer than the \
interval, more than 32 parameters, a parameter that is not a string, number or boolean or is too \
long, a parameter the kind does not take or a missing required one, or a check plugin not turned \
on for the site"),
        (status = 403, body = ErrorBody, description = "`forbidden`: the caller reads the site \
without an operator role. `api_key_scope`: the key lacks the area"),
        (status = 404, body = ErrorBody, description = "`not_found`: no such server, or one on a \
site the caller does not read"),
        (status = 409, body = ErrorBody, description = "`conflict`: this name and instance are \
already configured for the server"),
    ))]
fn create_server_sensor_config() {}

/// List a site's sensor configs
///
/// The site's own rows, by name and instance, parameters included.
///
/// Needs any role on the site and the key area "alerts".
#[utoipa::path(get, path = "/api/v1/sites/{id}/sensor_configs", tag = "sensor configs",
    params(("id" = uuid::Uuid, Path, description = "The site")),
    responses(
        (status = 200, body = sensors::EntryList, example = site_sensor_configs),
        (status = 404, body = ErrorBody, description = "`not_found`: no such site, or one the \
caller does not read"),
    ))]
fn list_site_sensor_configs() {}

/// Add a sensor config to a site
///
/// Adds a check for every server of the site, or with `enabled` false
/// removes a built-in check of the same name and instance. A server's own
/// row wins over it. The agents of the site get it at their next
/// configuration poll, as configuration: nothing waits for approval.
/// `kind` is as for a server's row.
///
/// Needs an operator role or above on the site and the key area "alerts".
/// Audited as `sensor_config.create`, with parameter names only. The same
/// name and instance twice is 409.
#[utoipa::path(post, path = "/api/v1/sites/{id}/sensor_configs", tag = "sensor configs",
    params(("id" = uuid::Uuid, Path, description = "The site")),
    request_body(content = sensors::EntryRequest, example = json!({
        "kind": "systemd",
        "name": "web-services",
        "interval_ms": 30000,
        "params": {"units": "nginx.service,php-fpm.service"}
    })),
    responses(
        (status = 201, body = sensors::Entry, example = site_sensor_config),
        (status = 400, body = ErrorBody, description = "`invalid`, with `error` saying why: not the expected JSON, a \
bad `kind`, `name` or `instance`, `interval_ms` outside 10 s to 24 h, a timeout longer than the \
interval, more than 32 parameters, a parameter that is not a string, number or boolean or is too \
long, a parameter the kind does not take or a missing required one, or a check plugin not turned \
on for the site"),
        (status = 403, body = ErrorBody, description = "`forbidden`: the caller reads the site \
without an operator role. `api_key_scope`: the key lacks the area"),
        (status = 404, body = ErrorBody, description = "`not_found`: no such site, or one the \
caller does not read"),
        (status = 409, body = ErrorBody, description = "`conflict`: this name and instance are \
already configured for the site"),
    ))]
fn create_site_sensor_config() {}

/// Replace a sensor config
///
/// Replaces every field of a site's or a server's row; it stays with its
/// site or server. Absent optional fields take their defaults, as when
/// creating. The agents concerned get it at their next configuration
/// poll.
///
/// Needs an operator role or above on the row's site and the key area
/// "alerts". Audited as `sensor_config.update` with the row before and
/// after, parameter names only. Safe to repeat.
#[utoipa::path(put, path = "/api/v1/sensor_configs/{id}", tag = "sensor configs",
    params(("id" = uuid::Uuid, Path, description = "The row")),
    request_body(content = sensors::EntryRequest, example = json!({
        "kind": "systemd",
        "name": "web-services",
        "interval_ms": 30000,
        "params": {"units": "nginx.service,php-fpm.service"}
    })),
    responses(
        (status = 200, body = sensors::Entry, example = server_sensor_config),
        (status = 400, body = ErrorBody, description = "`invalid`, with `error` saying why: not the expected JSON, a \
bad `kind`, `name` or `instance`, `interval_ms` outside 10 s to 24 h, a timeout longer than the \
interval, more than 32 parameters, a parameter that is not a string, number or boolean or is too \
long, a parameter the kind does not take or a missing required one, or a check plugin not turned \
on for the site"),
        (status = 403, body = ErrorBody, description = "`forbidden`: the caller reads the site \
without an operator role. `api_key_scope`: the key lacks the area"),
        (status = 404, body = ErrorBody, description = "`not_found`: no such row, or one of a site \
the caller does not read"),
        (status = 409, body = ErrorBody, description = "`conflict`: another row of the same site or \
server has this name and instance"),
    ))]
fn update_sensor_config() {}

/// Delete a sensor config
///
/// Removes a site's or a server's row: the layer below applies again (the
/// site's row, then the built-in check, if any). The agents concerned stop
/// or change the check at their next configuration poll.
///
/// Needs an operator role or above on the row's site and the key area
/// "alerts". Audited as `sensor_config.delete`. Deleting again is 404.
#[utoipa::path(delete, path = "/api/v1/sensor_configs/{id}", tag = "sensor configs",
    params(("id" = uuid::Uuid, Path, description = "The row")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 403, body = ErrorBody, description = "`forbidden`: the caller reads the site \
without an operator role. `api_key_scope`: the key lacks the area"),
        (status = 404, body = ErrorBody, description = "`not_found`: no such row, or one of a site \
the caller does not read"),
    ))]
fn delete_sensor_config() {}
