//! Batch 1 of the public API's description (ADR-0101): customers, and
//! the routes under a server or a site. `openapi.rs` merges [`Doc`] into
//! the document; the functions only carry each route's description.
//!
//! [`ex`] holds the made-up data the examples share, so a server or a site
//! reads the same in every answer.

#![allow(dead_code)] // The functions exist for their `#[utoipa::path]`.

use serde_json::json;
use utoipa::OpenApi;

use super::openapi::ErrorBody;
use super::{check_interval, customers, offline_alerts, sensors, thresholds};

#[derive(OpenApi)]
#[openapi(
    paths(
        rename_customer,
        server_sensors, server_sensor_configs, create_server_sensor_config,
        site_sensors, site_sensor_configs, create_site_sensor_config,
        update_sensor_config, delete_sensor_config,
        site_check_interval, server_offline_alerts, site_offline_alerts,
        server_thresholds_effective, server_thresholds, create_server_threshold,
        site_thresholds_effective, site_thresholds, create_site_threshold,
        update_threshold, delete_threshold,
    ),
    tags(
        (name = "sensors", description = "What each server checks and how often. A server runs the built-in \
checks (cpu, memory, disk and disk activity, every minute or at its site's interval), overlaid with its site's \
rows, then its own: a row replaces the sensor of the same name and instance below it, and a disabled row removes \
it. Agents pick up a change within seconds; the effective view says whether the agent runs the current \
configuration. Offline alerts and the site's check interval are here too. Area `alerts`."),
        (name = "thresholds", description = "When a metric opens an incident. Every server is checked \
against the built-in thresholds (CPU, memory, disk space, disk wait), overlaid with its site's rows, then its \
own: a row replaces the threshold of the same metric and instance below it, and a disabled row turns it off. \
A threshold opens a warning or critical incident once the value has stayed past it for `duration_s`, and \
resolves it once the value is back by more than the recovery margin (`hysteresis`). Area `alerts`."),
    )
)]
pub struct Doc;

/// Example data: IDs, names and times reused across operations.
pub mod ex {
    use serde_json::{json, Value};

    pub const SERVER: &str = "6f1c2a7e-3b4d-4e8f-9a0b-1c2d3e4f5a6b";
    pub const SERVER_2: &str = "0d8e7f6a-5b4c-4d3e-8f2a-1b0c9d8e7f6a";
    pub const SITE: &str = "2b7e1516-28ae-4d2a-a6f7-15880928cf4f";
    pub const SITE_2: &str = "5a1d2c3b-4e5f-4a6b-8c7d-9e0f1a2b3c4d";
    pub const SENSOR_CONFIG: &str = "a7b8c9d0-e1f2-4a3b-8c4d-5e6f7a8b9c0d";
    pub const CUSTOMER: &str = "9c1e5f3a-7b2d-4c8e-8f6a-0d4b3e2a1c5f";
    pub const RELAY: &str = "3e4f5a6b-7c8d-4e9f-a0b1-c2d3e4f5a6b7";
    pub const INCIDENT: &str = "4d3c2b1a-0f9e-4d8c-b7a6-5f4e3d2c1b0a";
    pub const INCIDENT_2: &str = "8b7a6f5e-4d3c-4b2a-9f8e-7d6c5b4a3f2e";
    pub const GROUP: &str = "1a2b3c4d-5e6f-4a7b-8c9d-0e1f2a3b4c5d";
    pub const SILENCE: &str = "7c6b5a4f-3e2d-4c1b-a09f-8e7d6c5b4a3f";
    pub const COMMAND: &str = "c0ffee00-1234-4abc-8def-0123456789ab";
    pub const USER: &str = "e1d2c3b4-a5f6-4e7d-8c9b-0a1f2e3d4c5b";
    pub const PENDING: &str = "b1c2d3e4-f5a6-4b7c-8d9e-0f1a2b3c4d5e";
    pub const REMEDIATION: &str = "f0e1d2c3-b4a5-4f6e-9d8c-7b6a5f4e3d2c";

    /// `base` with the fields of `extra` added or replaced.
    pub fn merge(mut base: Value, extra: Value) -> Value {
        if let (Some(base), Value::Object(extra)) = (base.as_object_mut(), extra) {
            base.extend(extra);
        }
        base
    }

    /// A row of `GET /servers`.
    pub fn server() -> Value {
        json!({
            "id": SERVER, "peer_id": "web-1", "hostname": "web-1",
            "site_id": SITE, "site_name": "paris",
            "customer_id": CUSTOMER, "customer_name": "acme", "relay_id": null,
            "os_family": "debian", "os_version": "12", "package_updates": true,
            "command_kinds": {"apply_updates": "accepted", "self_update": "accepted", "shell": "off"},
            "agent_version": "0.9.2",
            "created_at": "2026-09-01T08:00:00Z", "revoked_at": null,
            "status": "online",
            "connected_at": "2026-11-14T06:12:40Z",
            "last_heartbeat": "2026-11-14T09:29:55Z",
            "via_relay": null,
            "open_incidents": 1,
            "offline_alerts": true, "site_offline_alerts": true,
            "silenced_until": null,
            "cert_not_after": "2027-02-12T06:12:40Z"
        })
    }

    /// A site's row checking two services every 30 seconds.
    pub fn sensor_config() -> Value {
        json!({
            "id": SENSOR_CONFIG, "site_id": SITE, "server_id": null,
            "kind": "systemd", "name": "services", "instance": "",
            "interval_ms": 30000, "timeout_ms": null,
            "params": {"units": "nginx.service,postgresql.service"},
            "enabled": true,
            "created_at": "2026-11-02T10:15:00Z", "updated_at": "2026-11-02T10:15:00Z"
        })
    }

    /// Effective sensors: the built-in ones and a site row.
    pub fn effective_sensors() -> Value {
        let builtin = |kind: &str| {
            json!({"origin": "default", "entry_id": null, "kind": kind, "name": kind,
                   "instance": "", "interval_ms": 60000, "timeout_ms": null, "params": {}})
        };
        json!([
            builtin("cpu"), builtin("disk"), builtin("diskio"), builtin("memory"),
            {"origin": "site", "entry_id": SENSOR_CONFIG, "kind": "systemd", "name": "services",
             "instance": "", "interval_ms": 30000, "timeout_ms": null,
             "params": {"units": "nginx.service,postgresql.service"}}
        ])
    }

    pub const THRESHOLD: &str = "d4e5f6a7-b8c9-4dae-8fb0-c1d2e3f4a5b6";

    /// A site's row on available memory.
    pub fn threshold() -> Value {
        json!({
            "id": THRESHOLD, "site_id": SITE, "server_id": null,
            "metric": "monitorflow_memory_available_ratio", "instance": null,
            "comparison": "<", "warning": 0.15, "critical": 0.05,
            "duration_s": 600, "hysteresis": 0.02, "enabled": true,
            "created_at": "2026-11-02T10:20:00Z", "updated_at": "2026-11-02T10:20:00Z"
        })
    }

    /// Effective thresholds: the built-in ones, the memory one replaced by
    /// the site's row.
    pub fn effective_thresholds() -> Value {
        let builtin = |metric: &str,
                       instance: Value,
                       warning: f64,
                       critical: Value,
                       duration_s: u32| {
            json!({"origin": "default", "threshold_id": null, "metric": metric, "instance": instance,
                   "enabled": true, "comparison": ">", "warning": warning, "critical": critical,
                   "duration_s": duration_s, "hysteresis": 0.02})
        };
        json!([
            builtin("monitorflow_cpu_usage_ratio", json!("total"), 0.9, Value::Null, 600),
            builtin("monitorflow_disk_used_ratio", Value::Null, 0.85, json!(0.95), 300),
            builtin("monitorflow_diskio_wait_seconds", Value::Null, 0.2, Value::Null, 600),
            {"origin": "site", "threshold_id": THRESHOLD,
             "metric": "monitorflow_memory_available_ratio", "instance": null, "enabled": true,
             "comparison": "<", "warning": 0.15, "critical": 0.05, "duration_s": 600,
             "hysteresis": 0.02}
        ])
    }

    /// A site as `GET /sites` lists it.
    pub fn site() -> Value {
        json!({
            "id": SITE, "name": "paris", "server_count": 12, "relay_count": 1,
            "customer_id": CUSTOMER, "customer_name": "acme",
            "agent_auto_update": null, "offline_alerts": true,
            "agent_auto_update_effective": true
        })
    }

    /// An open, unacknowledged incident.
    pub fn incident() -> Value {
        json!({
            "id": INCIDENT, "server_id": SERVER, "relay_id": null, "hostname": "web-1",
            "site_id": SITE, "site_name": "paris", "customer_id": CUSTOMER,
            "metric": "monitorflow_disk_used_ratio", "instance": "/var",
            "severity": "critical", "state": "open",
            "opened_at": "2026-11-14T08:41:00Z", "resolved_at": null,
            "last_value": 0.97, "last_value_at": "2026-11-14T09:29:00Z", "peak_value": 0.97,
            "threshold": {
                "metric": "monitorflow_disk_used_ratio", "instance": null, "comparison": ">",
                "warning": 0.85, "critical": 0.95, "duration_s": 300, "hysteresis": 0.02
            },
            "silenced_until": null, "silenced": false,
            "acknowledged_by": null, "acknowledged_at": null,
            "can_acknowledge": true
        })
    }

    /// A command as listings show it, without its output.
    pub fn command() -> Value {
        json!({
            "id": COMMAND, "server_id": SERVER, "kind": "apply_updates",
            "parameters": {"packages": ["openssl", "libssl3"], "security_only": false, "allow_reboot": false},
            "state": "succeeded",
            "issued_at": "2026-11-14T09:00:00Z", "expires_at": "2026-11-14T10:00:00Z",
            "sent_at": "2026-11-14T09:00:01Z", "started_at": "2026-11-14T09:00:02Z",
            "finished_at": "2026-11-14T09:00:41Z",
            "exit_code": 0, "error": null, "requested_by": USER, "campaign_id": null
        })
    }

    /// A silence of one server.
    pub fn silence() -> Value {
        json!({
            "id": SILENCE, "scope": "server", "target_id": SERVER, "target_name": "web-1",
            "site_id": SITE, "customer_id": CUSTOMER,
            "starts_at": "2026-11-14T22:00:00Z", "ends_at": "2026-11-15T00:00:00Z",
            "active": false, "reason": "Kernel upgrade and reboot",
            "created_by_username": "alice", "created_at": "2026-11-14T09:30:00Z",
            "can_end": true
        })
    }
}

#[utoipa::path(put, path = "/api/v1/customers/{id}", tag = "sites",
    summary = "Rename a customer",
    description = "Changes a customer's name. Only the name changes: its sites, servers, roles and API keys \
refer to its ID and keep working.\n\n\
**Who**: a global administrator; the key must reach everything with \"Administer\" access. \
**Area**: `administration`.\n\n\
Names are 1 to 64 characters, without control characters or spaces at either end, and unique. Audited as \
`customer.rename` with the old and new names, unless the name is unchanged. Safe to repeat.",
    params(("id" = uuid::Uuid, Path, description = "The customer")),
    request_body(content = customers::NameRequest, example = json!({"name": "acme-europe"})),
    responses(
        (status = 200, description = "The customer, renamed", body = customers::CustomerView,
            example = json!({"id": ex::CUSTOMER, "name": "acme-europe", "site_count": 3})),
        (status = 400, description = "The name is empty, too long, or has control characters or \
spaces at either end: `invalid`", body = ErrorBody),
        (status = 403, description = "The key or its maker is not a global administrator: \
`forbidden`; or outside the key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such customer: `not_found`", body = ErrorBody),
        (status = 409, description = "Another customer has this name: `conflict`", body = ErrorBody),
    ))]
fn rename_customer() {}

/// What creating or replacing a sensor row checks and records, for
/// `concat!` in the descriptions.
macro_rules! sensor_config_rules {
    () => {
        "Rows are checked as the agent checks them: `kind` must be a sensor it knows, \
`params` only the settings of that kind, with valid values, and `interval_ms` 10 s to 24 h. A `plugin` row names a \
check plugin turned on for the site in `params.plugin`. The answer to a bad row is 400 `invalid`, saying what is \
wrong. A second row with the same `name` and `instance` for the same site or server is 409.\n\n\
Audited as `sensor_config.create`, `sensor_config.update` or `sensor_config.delete`, with the names of the \
parameters but never their values. Connected agents get the change within seconds."
    };
}

#[utoipa::path(get, path = "/api/v1/servers/{id}/sensors", tag = "sensors",
    summary = "Get a server's effective sensors",
    description = "What the server's agent runs: the built-in checks overlaid with its site's rows, then its \
own, each with where it comes from (`origin`, and the row's `entry_id`). `revision` is the configuration's; \
`in_sync` says whether the agent has applied it.\n\n\
**Who**: any role on the server's site. **Area**: `alerts`.",
    params(("id" = uuid::Uuid, Path, description = "The server")),
    responses(
        (status = 200, description = "The effective sensors", body = sensors::Effective,
            example = json!({
                "server_id": ex::SERVER, "site_id": ex::SITE,
                "check_interval_ms": 60000, "check_interval_origin": "default",
                "revision": 7, "applied_revision": 7, "applied_at": "2026-11-02T10:15:21Z",
                "in_sync": true, "sensors": ex::effective_sensors()
            })),
        (status = 404, description = "No such server, or outside the key's reach: `not_found`",
            body = ErrorBody),
    ))]
fn server_sensors() {}

#[utoipa::path(get, path = "/api/v1/servers/{id}/sensor_configs", tag = "sensors",
    summary = "List a server's own sensor rows",
    description = "The rows set on this server alone, by name and instance, in full. Its site's rows are under \
`GET /api/v1/sites/{id}/sensor_configs`; what it runs in the end is `GET /api/v1/servers/{id}/sensors`.\n\n\
**Who**: any role on the server's site. **Area**: `alerts`.",
    params(("id" = uuid::Uuid, Path, description = "The server")),
    responses(
        (status = 200, description = "The server's rows", body = sensors::EntryList,
            example = json!({"entries": [ex::merge(ex::sensor_config(), json!({
                "site_id": null, "server_id": ex::SERVER, "kind": "port", "name": "https",
                "params": {"host": "127.0.0.1", "port": 443}
            }))]})),
        (status = 404, description = "No such server, or outside the key's reach: `not_found`",
            body = ErrorBody),
    ))]
fn server_sensor_configs() {}

#[utoipa::path(post, path = "/api/v1/servers/{id}/sensor_configs", tag = "sensors",
    summary = "Add a sensor row to a server",
    description = concat!("Adds a sensor to one server, or replaces for it the sensor of the same name and instance \
that its site or the built-in checks give; with `enabled: false`, removes that sensor from it.\n\n\
**Who**: operator or above (\"Read and change\") on the server's site. **Area**: `alerts`.\n\n", sensor_config_rules!()),
    params(("id" = uuid::Uuid, Path, description = "The server")),
    request_body(content = sensors::EntryRequest, examples(
        ("port" = (summary = "Check that a port answers",
            value = json!({"kind": "port", "name": "https", "interval_ms": 30000,
                           "params": {"host": "127.0.0.1", "port": 443}}))),
        ("remove" = (summary = "Stop checking disk activity on this server",
            value = json!({"kind": "diskio", "interval_ms": 60000, "enabled": false}))),
    )),
    responses(
        (status = 201, description = "The new row", body = sensors::Entry,
            example = json!(ex::merge(ex::sensor_config(), json!({
                "site_id": null, "server_id": ex::SERVER, "kind": "port", "name": "https",
                "params": {"host": "127.0.0.1", "port": 443}
            })))),
        (status = 400, description = "The row is not valid, and `error` says why: `invalid`",
            body = ErrorBody),
        (status = 403, description = "The key may only read the server's site: `forbidden`; or \
outside the key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such server, or outside the key's reach: `not_found`",
            body = ErrorBody),
        (status = 409, description = "The server has a row with this name and instance already: \
`conflict`", body = ErrorBody),
    ))]
fn create_server_sensor_config() {}

#[utoipa::path(get, path = "/api/v1/sites/{id}/sensors", tag = "sensors",
    summary = "Get a site's effective sensors",
    description = "What a server of this site runs when it has no rows of its own: the built-in checks at the \
site's interval, overlaid with the site's rows.\n\n\
**Who**: any role on the site. **Area**: `alerts`.",
    params(("id" = uuid::Uuid, Path, description = "The site")),
    responses(
        (status = 200, description = "The site's effective sensors", body = sensors::SiteEffective,
            example = json!({
                "site_id": ex::SITE, "check_interval_ms": 60000, "check_interval_origin": "default",
                "sensors": ex::effective_sensors()
            })),
        (status = 404, description = "No such site, or outside the key's reach: `not_found`",
            body = ErrorBody),
    ))]
fn site_sensors() {}

#[utoipa::path(get, path = "/api/v1/sites/{id}/sensor_configs", tag = "sensors",
    summary = "List a site's sensor rows",
    description = "The rows set on the site, which every server of the site inherits, by name and instance, \
in full.\n\n\
**Who**: any role on the site. **Area**: `alerts`.",
    params(("id" = uuid::Uuid, Path, description = "The site")),
    responses(
        (status = 200, description = "The site's rows", body = sensors::EntryList,
            example = json!({"entries": [ex::sensor_config()]})),
        (status = 404, description = "No such site, or outside the key's reach: `not_found`",
            body = ErrorBody),
    ))]
fn site_sensor_configs() {}

#[utoipa::path(post, path = "/api/v1/sites/{id}/sensor_configs", tag = "sensors",
    summary = "Add a sensor row to a site",
    description = concat!("Adds a sensor to every server of the site, or replaces the built-in sensor of the same name \
and instance; with `enabled: false`, removes it. A server's own row still wins over the site's.\n\n\
**Who**: operator or above (\"Read and change\") on the site. **Area**: `alerts`.\n\n", sensor_config_rules!()),
    params(("id" = uuid::Uuid, Path, description = "The site")),
    request_body(content = sensors::EntryRequest, example = json!({
        "kind": "systemd", "name": "services", "interval_ms": 30000,
        "params": {"units": "nginx.service,postgresql.service"}
    })),
    responses(
        (status = 201, description = "The new row", body = sensors::Entry,
            example = json!(ex::sensor_config())),
        (status = 400, description = "The row is not valid, and `error` says why: `invalid`",
            body = ErrorBody),
        (status = 403, description = "The key may only read the site: `forbidden`; or outside the \
key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such site, or outside the key's reach: `not_found`",
            body = ErrorBody),
        (status = 409, description = "The site has a row with this name and instance already: \
`conflict`", body = ErrorBody),
    ))]
fn create_site_sensor_config() {}

#[utoipa::path(put, path = "/api/v1/sensor_configs/{id}", tag = "sensors",
    summary = "Replace a sensor row",
    description = concat!("Replaces every field of a site's or a server's row; fields left out take their defaults. \
The row stays on its site or server. Safe to repeat.\n\n\
**Who**: operator or above (\"Read and change\") on the row's site. **Area**: `alerts`.\n\n", sensor_config_rules!()),
    params(("id" = uuid::Uuid, Path, description = "The row")),
    request_body(content = sensors::EntryRequest, example = json!({
        "kind": "systemd", "name": "services", "interval_ms": 60000,
        "params": {"units": "nginx.service,postgresql.service,redis-server.service"}
    })),
    responses(
        (status = 200, description = "The row as it is now", body = sensors::Entry,
            example = json!(ex::merge(ex::sensor_config(), json!({
                "interval_ms": 60000,
                "params": {"units": "nginx.service,postgresql.service,redis-server.service"},
                "updated_at": "2026-11-14T09:40:00Z"
            })))),
        (status = 400, description = "The row is not valid, and `error` says why: `invalid`",
            body = ErrorBody),
        (status = 403, description = "The key may only read the row's site: `forbidden`; or \
outside the key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such row, or outside the key's reach: `not_found`",
            body = ErrorBody),
        (status = 409, description = "Another row of the same site or server has this name and \
instance: `conflict`", body = ErrorBody),
    ))]
fn update_sensor_config() {}

#[utoipa::path(delete, path = "/api/v1/sensor_configs/{id}", tag = "sensors",
    summary = "Delete a sensor row",
    description = "Deletes a site's or a server's row: what it replaced or removed below comes back. A second \
request is 404.\n\n\
**Who**: operator or above (\"Read and change\") on the row's site. **Area**: `alerts`.",
    params(("id" = uuid::Uuid, Path, description = "The row")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 403, description = "The key may only read the row's site: `forbidden`; or \
outside the key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such row, or outside the key's reach: `not_found`",
            body = ErrorBody),
    ))]
fn delete_sensor_config() {}

#[utoipa::path(put, path = "/api/v1/sites/{id}/check-interval", tag = "sensors",
    summary = "Set how often a site's built-in checks run",
    description = "Sets how often the built-in checks (cpu, memory, disk, disk activity) of the site's servers \
run, from 10 seconds to 24 hours, or with `null` back to every minute. Rows that set their own interval keep \
it. Agents pick it up within seconds. Audited as `site.check_interval` with the old and new values. Safe to \
repeat.\n\n\
**Who**: operator or above (\"Read and change\") on the site. **Area**: `alerts`.",
    params(("id" = uuid::Uuid, Path, description = "The site")),
    request_body(content = check_interval::CheckInterval, examples(
        ("five_minutes" = (summary = "Every 5 minutes", value = json!({"interval_ms": 300000}))),
        ("default" = (summary = "Back to every minute", value = json!({"interval_ms": null}))),
    )),
    responses(
        (status = 200, description = "The interval as set", body = check_interval::CheckInterval,
            example = json!({"interval_ms": 300000})),
        (status = 400, description = "Under 10 seconds or over 24 hours: `invalid`", body = ErrorBody),
        (status = 403, description = "The key may only read the site: `forbidden`; or outside the \
key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such site, or outside the key's reach: `not_found`",
            body = ErrorBody),
    ))]
fn site_check_interval() {}

#[utoipa::path(put, path = "/api/v1/servers/{id}/offline-alerts", tag = "sensors",
    summary = "Turn a server's offline alerts on or off",
    description = "With `false`, the server opens no incident when it stops reporting, and its open offline \
incident resolves within a minute: for a machine that is often off on purpose. With `true`, they are back, \
unless its site has them off (`site_offline_alerts` of the server). For a few hours of maintenance, a silence \
is the better tool. Audited as `server.offline_alerts`. Safe to repeat.\n\n\
**Who**: operator or above (\"Read and change\") on the server's site. **Area**: `alerts`.",
    params(("id" = uuid::Uuid, Path, description = "The server")),
    request_body(content = offline_alerts::OfflineAlerts, example = json!({"enabled": false})),
    responses(
        (status = 200, description = "The setting as set", body = offline_alerts::OfflineAlerts,
            example = json!({"enabled": false})),
        (status = 403, description = "The key may only read the server's site: `forbidden`; or \
outside the key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such server, or outside the key's reach: `not_found`",
            body = ErrorBody),
    ))]
fn server_offline_alerts() {}

#[utoipa::path(put, path = "/api/v1/sites/{id}/offline-alerts", tag = "sensors",
    summary = "Turn a site's offline alerts on or off",
    description = "With `false`, no server or relay of the site opens an incident when it stops reporting, \
whatever its own setting, and their open offline incidents resolve within a minute. With `true`, each one's own \
setting applies again. Audited as `site.offline_alerts`. Safe to repeat.\n\n\
**Who**: operator or above (\"Read and change\") on the site. **Area**: `alerts`.",
    params(("id" = uuid::Uuid, Path, description = "The site")),
    request_body(content = offline_alerts::OfflineAlerts, example = json!({"enabled": true})),
    responses(
        (status = 200, description = "The setting as set", body = offline_alerts::OfflineAlerts,
            example = json!({"enabled": true})),
        (status = 403, description = "The key may only read the site: `forbidden`; or outside the \
key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such site, or outside the key's reach: `not_found`",
            body = ErrorBody),
    ))]
fn site_offline_alerts() {}

/// What creating or replacing a threshold row checks and records, for
/// `concat!` in the descriptions.
macro_rules! threshold_rules {
    () => {
        "`metric` is `monitorflow_` followed by a sensor's metric name. An enabled row needs `warning` or \
`critical`, and `critical` must be on the alarm side of `warning` (above it for `>` and `>=`, below it for `<` \
and `<=`); `duration_s` is 0 to 86400 and `hysteresis` 0 or more. A bad row is 400 `invalid`, saying what is \
wrong; a second row with the same `metric` and `instance` for the same site or server is 409.\n\n\
Audited as `threshold.create`, `threshold.update` or `threshold.delete`. MonitorFlow applies the change \
within seconds; an open incident's `threshold` still shows the values it opened with."
    };
}

#[utoipa::path(get, path = "/api/v1/servers/{id}/thresholds/effective", tag = "thresholds",
    summary = "Get a server's effective thresholds",
    description = "The thresholds MonitorFlow checks the server's metrics against: the built-in ones \
overlaid with its site's rows, then its own, each with where it comes from. Muted ones are listed with \
`enabled: false`.\n\n\
**Who**: any role on the server's site. **Area**: `alerts`.",
    params(("id" = uuid::Uuid, Path, description = "The server")),
    responses(
        (status = 200, description = "The effective thresholds", body = thresholds::Effective,
            example = json!({
                "server_id": ex::SERVER, "site_id": ex::SITE, "revision": 4,
                "thresholds": ex::effective_thresholds()
            })),
        (status = 404, description = "No such server, or outside the key's reach: `not_found`",
            body = ErrorBody),
    ))]
fn server_thresholds_effective() {}

#[utoipa::path(get, path = "/api/v1/servers/{id}/thresholds", tag = "thresholds",
    summary = "List a server's own threshold rows",
    description = "The rows set on this server alone, by metric and instance (the row for every instance \
first), in full.\n\n\
**Who**: any role on the server's site. **Area**: `alerts`.",
    params(("id" = uuid::Uuid, Path, description = "The server")),
    responses(
        (status = 200, description = "The server's rows", body = thresholds::EntryList,
            example = json!({"entries": [ex::merge(ex::threshold(), json!({
                "site_id": null, "server_id": ex::SERVER, "instance": "/var",
                "warning": 0.9, "critical": 0.98
            }))]})),
        (status = 404, description = "No such server, or outside the key's reach: `not_found`",
            body = ErrorBody),
    ))]
fn server_thresholds() {}

#[utoipa::path(post, path = "/api/v1/servers/{id}/thresholds", tag = "thresholds",
    summary = "Add a threshold row to a server",
    description = concat!("Sets a threshold for one server, replacing for it the one of the same metric and \
instance that its site or the built-in thresholds give; with `enabled: false`, turns that threshold off for \
it.\n\n\
**Who**: operator or above (\"Read and change\") on the server's site. **Area**: `alerts`.\n\n",
        threshold_rules!()),
    params(("id" = uuid::Uuid, Path, description = "The server")),
    request_body(content = thresholds::EntryRequest, examples(
        ("disk" = (summary = "Warn later on a big volume",
            value = json!({"metric": "monitorflow_disk_used_ratio", "instance": "/var", "comparison": ">",
                           "warning": 0.9, "critical": 0.98, "duration_s": 300, "hysteresis": 0.02}))),
        ("off" = (summary = "Turn the CPU threshold off for this server",
            value = json!({"metric": "monitorflow_cpu_usage_ratio", "instance": "total",
                           "comparison": ">", "enabled": false}))),
    )),
    responses(
        (status = 201, description = "The new row", body = thresholds::Entry,
            example = json!(ex::merge(ex::threshold(), json!({
                "site_id": null, "server_id": ex::SERVER, "instance": "/var",
                "warning": 0.9, "critical": 0.98
            })))),
        (status = 400, description = "The row is not valid, and `error` says why: `invalid`",
            body = ErrorBody),
        (status = 403, description = "The key may only read the server's site: `forbidden`; or \
outside the key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such server, or outside the key's reach: `not_found`",
            body = ErrorBody),
        (status = 409, description = "The server has a row for this metric and instance already: \
`conflict`", body = ErrorBody),
    ))]
fn create_server_threshold() {}

#[utoipa::path(get, path = "/api/v1/sites/{id}/thresholds/effective", tag = "thresholds",
    summary = "Get a site's effective thresholds",
    description = "What a server of this site is checked against when it has no rows of its own: the \
built-in thresholds overlaid with the site's rows, muted ones included.\n\n\
**Who**: any role on the site. **Area**: `alerts`.",
    params(("id" = uuid::Uuid, Path, description = "The site")),
    responses(
        (status = 200, description = "The site's effective thresholds", body = thresholds::SiteEffective,
            example = json!({"site_id": ex::SITE, "thresholds": ex::effective_thresholds()})),
        (status = 404, description = "No such site, or outside the key's reach: `not_found`",
            body = ErrorBody),
    ))]
fn site_thresholds_effective() {}

#[utoipa::path(get, path = "/api/v1/sites/{id}/thresholds", tag = "thresholds",
    summary = "List a site's threshold rows",
    description = "The rows set on the site, which every server of the site inherits, by metric and \
instance (the row for every instance first), in full.\n\n\
**Who**: any role on the site. **Area**: `alerts`.",
    params(("id" = uuid::Uuid, Path, description = "The site")),
    responses(
        (status = 200, description = "The site's rows", body = thresholds::EntryList,
            example = json!({"entries": [ex::threshold()]})),
        (status = 404, description = "No such site, or outside the key's reach: `not_found`",
            body = ErrorBody),
    ))]
fn site_thresholds() {}

#[utoipa::path(post, path = "/api/v1/sites/{id}/thresholds", tag = "thresholds",
    summary = "Add a threshold row to a site",
    description = concat!("Sets a threshold for every server of the site, replacing the built-in one of the \
same metric and instance; with `enabled: false`, turns it off. A server's own row still wins over the \
site's.\n\n\
**Who**: operator or above (\"Read and change\") on the site. **Area**: `alerts`.\n\n",
        threshold_rules!()),
    params(("id" = uuid::Uuid, Path, description = "The site")),
    request_body(content = thresholds::EntryRequest, example = json!({
        "metric": "monitorflow_memory_available_ratio", "comparison": "<",
        "warning": 0.15, "critical": 0.05, "duration_s": 600, "hysteresis": 0.02
    })),
    responses(
        (status = 201, description = "The new row", body = thresholds::Entry,
            example = json!(ex::threshold())),
        (status = 400, description = "The row is not valid, and `error` says why: `invalid`",
            body = ErrorBody),
        (status = 403, description = "The key may only read the site: `forbidden`; or outside the \
key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such site, or outside the key's reach: `not_found`",
            body = ErrorBody),
        (status = 409, description = "The site has a row for this metric and instance already: \
`conflict`", body = ErrorBody),
    ))]
fn create_site_threshold() {}

#[utoipa::path(put, path = "/api/v1/thresholds/{id}", tag = "thresholds",
    summary = "Replace a threshold row",
    description = concat!("Replaces every field of a site's or a server's row; fields left out take their \
defaults. The row stays on its site or server. Safe to repeat.\n\n\
**Who**: operator or above (\"Read and change\") on the row's site. **Area**: `alerts`.\n\n",
        threshold_rules!()),
    params(("id" = uuid::Uuid, Path, description = "The row")),
    request_body(content = thresholds::EntryRequest, example = json!({
        "metric": "monitorflow_memory_available_ratio", "comparison": "<",
        "warning": 0.2, "critical": 0.05, "duration_s": 600, "hysteresis": 0.02
    })),
    responses(
        (status = 200, description = "The row as it is now", body = thresholds::Entry,
            example = json!(ex::merge(ex::threshold(), json!({
                "warning": 0.2, "updated_at": "2026-11-14T09:40:00Z"
            })))),
        (status = 400, description = "The row is not valid, and `error` says why: `invalid`",
            body = ErrorBody),
        (status = 403, description = "The key may only read the row's site: `forbidden`; or \
outside the key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such row, or outside the key's reach: `not_found`",
            body = ErrorBody),
        (status = 409, description = "Another row of the same site or server has this metric and \
instance: `conflict`", body = ErrorBody),
    ))]
fn update_threshold() {}

#[utoipa::path(delete, path = "/api/v1/thresholds/{id}", tag = "thresholds",
    summary = "Delete a threshold row",
    description = "Deletes a site's or a server's row: what it replaced or turned off below comes back. \
Audited as `threshold.delete`. A second request is 404.\n\n\
**Who**: operator or above (\"Read and change\") on the row's site. **Area**: `alerts`.",
    params(("id" = uuid::Uuid, Path, description = "The row")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 403, description = "The key may only read the row's site: `forbidden`; or \
outside the key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such row, or outside the key's reach: `not_found`",
            body = ErrorBody),
    ))]
fn delete_threshold() {}
