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
use super::{
    ai_commands, check_interval, commands, customers, logs, metrics, offline_alerts,
    package_sources, packages, proxmox, revoke, rollback, sensors, sites, sources, third_party,
    thresholds,
};

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
        server_metrics, server_logs, revoke_server,
        server_proxmox, set_server_proxmox, clear_server_proxmox, server_proxmox_guests,
        server_packages, server_third_party, server_rollback, server_sources, configure_server_sources,
        revert_server_sources, server_use_mirror, site_sources, site_use_mirror, revert_site_sources,
        site_agent_auto_update, server_ai_commands, site_ai_commands,
    ),
    components(schemas(logs::Level)),
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
            "parameters": {"packages": ["openssl", "libssl3"], "security_only": false, "allow_reboot": false,
                "include_os": true, "third_party": []},
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

#[utoipa::path(get, path = "/api/v1/servers/{id}/metrics", tag = "metrics",
    summary = "List a server's metrics",
    description = "The metrics the server reported in the last day, with the instances seen for each: the \
names to pass as `metric` and `instance` to the query routes. `truncated` is `true` when the server had more \
than 10,000 series, and the list is then partial.\n\n\
**Who**: any role on the server's site. **Area**: `servers`.",
    params(("id" = uuid::Uuid, Path, description = "The server")),
    responses(
        (status = 200, description = "Its metrics", body = metrics::MetricList,
            example = json!({"metrics": [
                {"name": "monitorflow_cpu_usage_ratio", "instances": ["total"]},
                {"name": "monitorflow_disk_used_ratio", "instances": ["/", "/var"]},
                {"name": "monitorflow_memory_available_ratio", "instances": [""]}
            ], "truncated": false})),
        (status = 404, description = "No such server, or outside the key's reach: `not_found`",
            body = ErrorBody),
        (status = 502, description = "The metrics database is unreachable or not configured: \
`metrics_unavailable`", body = ErrorBody),
        (status = 504, description = "The metrics database did not answer in time: `metrics_timeout`",
            body = ErrorBody),
    ))]
fn server_metrics() {}

#[utoipa::path(get, path = "/api/v1/servers/{id}/logs", tag = "servers",
    summary = "Read a server's system logs",
    description = "The warnings and errors of the server's journal that its agent forwarded, newest first, \
from `start` to `end` (the last hour by default, at most 31 days). `level=error` keeps errors and worse. `unit` \
keeps one systemd unit, `q` lines containing a text, whatever the case. `units` lists the units of the range \
whatever the other filters, to choose from. `more` is `true` when more lines matched than `limit`: narrow the \
range to see them. MonitorFlow builds the query itself; it never accepts a raw query.\n\n\
**Who**: any role on the server's site. **Area**: `servers`. Unknown parameters are 400.",
    params(("id" = uuid::Uuid, Path, description = "The server"), logs::LogParams),
    responses(
        (status = 200, description = "The lines", body = logs::LogList,
            example = json!({
                "lines": [
                    {"time": "2026-11-14T09:28:41.120Z", "message": "upstream timed out (110: Connection timed out)",
                     "priority": 3, "unit": "nginx.service", "identifier": "nginx", "pid": 1412},
                    {"time": "2026-11-14T09:12:03.004Z", "message": "Disk quota warning on /var",
                     "priority": 4, "unit": null, "identifier": "kernel", "pid": null}
                ],
                "more": false,
                "units": ["nginx.service", "postgresql.service"]
            })),
        (status = 400, description = "`start` not before `end`, a range over 31 days, `limit` out of \
range, a `unit` or `q` over 256 bytes or with control characters, or an unknown parameter: `invalid`",
            body = ErrorBody),
        (status = 404, description = "No such server, or outside the key's reach: `not_found`",
            body = ErrorBody),
        (status = 502, description = "The logs database is unreachable or not configured: \
`logs_unavailable`", body = ErrorBody),
        (status = 504, description = "The logs database did not answer in time: `logs_timeout`",
            body = ErrorBody),
    ))]
fn server_logs() {}

#[utoipa::path(post, path = "/api/v1/servers/{id}/revoke", tag = "servers",
    summary = "Remove a server",
    description = "Removes a server from the fleet: its certificates are revoked and published in the \
revocation list, its connections are closed, its open incidents resolve, and its unused single-use tokens are \
deleted. It leaves the lists and counts; `GET /api/v1/servers/{id}` still answers, with `revoked_at` set, and \
its metrics and incidents stay for their retention. Installing it again in the same site brings it back with \
its history.\n\n\
`reason` is recorded in the revocation list: `unspecified` (the default), `key_compromise`, \
`affiliation_changed`, `superseded` or `cessation_of_operation`. Send `{}` for none. Audited as \
`server.revoke`. A second request is 409.\n\n\
**Who**: an administrator of the server's customer as a whole: a global administrator or one of that \
customer. A site administrator may not, and neither may a key that reaches one site. **Area**: `servers`.",
    params(("id" = uuid::Uuid, Path, description = "The server")),
    request_body(content = revoke::RevokeRequest, examples(
        ("default" = (summary = "No reason", value = json!({}))),
        ("decommissioned" = (summary = "The machine is gone",
            value = json!({"reason": "cessation_of_operation"}))),
    )),
    responses(
        (status = 200, description = "Removed", body = revoke::Revoked,
            example = json!({
                "revoked_at": "2026-11-14T09:45:00Z", "reason": "cessation_of_operation",
                "serials": ["3f2a9c41d07e"], "closed_streams": 1
            })),
        (status = 400, description = "An unknown `reason`, or no JSON body: `invalid`", body = ErrorBody),
        (status = 403, description = "The key may not administer the server's customer: \
`forbidden`; or outside the key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such server, or outside the key's reach: `not_found`",
            body = ErrorBody),
        (status = 409, description = "The server is removed already: `conflict`", body = ErrorBody),
    ))]
fn revoke_server() {}

/// A server's Proxmox view, mapped automatically.
fn proxmox_view() -> serde_json::Value {
    json!({
        "server_id": ex::SERVER,
        "mapping": {
            "cluster": "lab", "node": "pve1", "vmid": 104, "kind": "qemu",
            "updated_by": null, "updated_at": "2026-11-10T03:00:00Z",
            "checked_at": "2026-11-14T03:00:00Z", "cannot_snapshot": null, "left_out": null,
            "matched_by": "uuid", "name": "web-1"
        },
        "snapshots": {"available": true, "reason": null, "kind": "qemu", "detail": null,
                      "left_out": null, "method": "proxmox"},
        "clusters": ["lab"],
        "candidates": []
    })
}

#[utoipa::path(get, path = "/api/v1/servers/{id}/proxmox", tag = "servers",
    summary = "Get a server's Proxmox VM or container",
    description = concat!("The Proxmox VM or container the server runs in, which is snapshotted before \
an AI change, and whether that works now. MonitorFlow matches most servers by itself (`matched_by` is then \
`uuid`, `mac` or `hostname`); when several guests match equally, `candidates` lists them. `clusters` are the \
configured clusters that serve the server's customer and site.\n\n\
`snapshots.available` is `false` with a `reason`: `disabled` (Proxmox is not configured), `not_mapped`, \
`unknown_cluster`, or `cannot_snapshot` with `detail` saying why.\n\n",
        "**Who**: an administrator of the server's customer as a whole: a global administrator or one of \
that customer. **Area**: `servers`."),
    params(("id" = uuid::Uuid, Path, description = "The server")),
    responses(
        (status = 200, description = "The mapping and what it allows", body = proxmox::ProxmoxView,
            example = json!(proxmox_view())),
        (status = 403, description = "The key may not administer the server's customer: \
`forbidden`; or outside the key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such server, or outside the key's reach: `not_found`",
            body = ErrorBody),
    ))]
fn server_proxmox() {}

#[utoipa::path(put, path = "/api/v1/servers/{id}/proxmox", tag = "servers",
    summary = "Map a server to its Proxmox VM or container",
    description = concat!("Sets or replaces, by hand, the VM or container the server runs in. Automatic \
matching never changes a mapping made by hand, and a guest refused earlier with `DELETE` may be mapped again \
this way. Whether Proxmox can snapshot the new guest is checked again at the next pass. Audited as \
`server.proxmox.set` with the previous mapping. Safe to repeat.\n\n\
On a cluster that serves several customers, only a global administrator may name any guest; others choose \
one of the server's `candidates`.\n\n",
        "**Who**: an administrator of the server's customer as a whole: a global administrator or one of \
that customer. **Area**: `servers`."),
    params(("id" = uuid::Uuid, Path, description = "The server")),
    request_body(content = proxmox::MappingRequest,
        example = json!({"cluster": "lab", "node": "pve1", "vmid": 104, "kind": "qemu"})),
    responses(
        (status = 200, description = "The mapping and what it allows", body = proxmox::ProxmoxView,
            example = json!(ex::merge(proxmox_view(), json!({"mapping": {
                "cluster": "lab", "node": "pve1", "vmid": 104, "kind": "qemu",
                "updated_by": ex::USER, "updated_at": "2026-11-14T09:50:00Z",
                "checked_at": null, "cannot_snapshot": null, "left_out": null,
                "matched_by": null, "name": "web-1"
            }})))),
        (status = 400, description = "A bad cluster, node, `vmid` or `kind`, a cluster that does not \
serve the server, or a guest that is not a candidate on a cluster of several customers: `invalid`",
            body = ErrorBody),
        (status = 403, description = "The key may not administer the server's customer: \
`forbidden`; or outside the key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such server, or outside the key's reach: `not_found`",
            body = ErrorBody),
        (status = 409, description = "The guest is mapped to another server: `conflict`",
            body = ErrorBody),
    ))]
fn set_server_proxmox() {}

#[utoipa::path(delete, path = "/api/v1/servers/{id}/proxmox", tag = "servers",
    summary = "Unmap a server from its Proxmox VM or container",
    description = concat!("Removes the mapping: AI changes on the server are no longer snapshotted on \
Proxmox. Snapshots already taken stay on the guest. Automatic matching will never map the server to this \
guest again, unless it is mapped by hand. Audited as `server.proxmox.clear`. A second request is 404.\n\n",
        "**Who**: an administrator of the server's customer as a whole: a global administrator or one of \
that customer. **Area**: `servers`."),
    params(("id" = uuid::Uuid, Path, description = "The server")),
    responses(
        (status = 204, description = "Unmapped"),
        (status = 403, description = "The key may not administer the server's customer: \
`forbidden`; or outside the key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such server, outside the key's reach, or not mapped: \
`not_found`", body = ErrorBody),
    ))]
fn clear_server_proxmox() {}

#[utoipa::path(get, path = "/api/v1/servers/{id}/proxmox/guests", tag = "servers",
    summary = "List the Proxmox guests a server may be mapped to",
    description = concat!("The VMs and containers of the clusters that serve the server, as MonitorFlow \
last saw them, by cluster and ID, up to 2000. `taken` marks those mapped to another server. A cluster that \
serves every customer is listed to global administrators only.\n\n",
        "**Who**: an administrator of the server's customer as a whole: a global administrator or one of \
that customer. **Area**: `servers`."),
    params(("id" = uuid::Uuid, Path, description = "The server")),
    responses(
        (status = 200, description = "The guests", body = proxmox::GuestList,
            example = json!({"guests": [
                {"cluster": "lab", "node": "pve1", "vmid": 104, "kind": "qemu", "name": "web-1",
                 "matched_by": null, "taken": false},
                {"cluster": "lab", "node": "pve2", "vmid": 211, "kind": "lxc", "name": "db-1",
                 "matched_by": null, "taken": true}
            ]})),
        (status = 403, description = "The key may not administer the server's customer: \
`forbidden`; or outside the key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such server, or outside the key's reach: `not_found`",
            body = ErrorBody),
    ))]
fn server_proxmox_guests() {}

/// A command just queued, of `kind` with `parameters`.
fn queued(kind: &str, parameters: serde_json::Value) -> serde_json::Value {
    json!({
        "id": ex::COMMAND, "server_id": ex::SERVER, "kind": kind, "parameters": parameters,
        "state": "queued", "issued_at": "2026-11-14T10:00:00Z", "expires_at": "2026-11-15T10:00:00Z",
        "sent_at": null, "started_at": null, "finished_at": null, "exit_code": null, "error": null,
        "requested_by": ex::USER, "campaign_id": null, "stdout": null, "stderr": null
    })
}

/// A server's package sources on a mirror.
fn sources_status() -> serde_json::Value {
    json!({
        "using": "mirror",
        "mirrors": [{"name": "debian-12", "snapshot": "current"},
                    {"name": "debian-12-security", "snapshot": "current"}],
        "changing_to": null, "failed": null, "campaign": false,
        "distribution": "debian-12", "problem": null, "add_mirror": false,
        "windows_update_source": null
    })
}

#[utoipa::path(get, path = "/api/v1/servers/{id}/packages", tag = "updates",
    summary = "List a server's packages",
    description = "The packages of the server's latest report, by name and architecture, with the version \
an upgrade would install. `upgradable=true` keeps those with an upgrade. On Windows servers, the entries are \
updates: `name` is the KB number or update ID, with `title`, `classification`, `severity` and `size_bytes`. \
`reported_at` and `reboot_required` are `null` until the agent sent a report.\n\n\
**Pages**: `limit` packages per page, 1 to 5000, 500 by default; pass `next_cursor` as `cursor` for the next \
page, `null` on the last one. Unknown parameters are 400.\n\n\
**Who**: any role on the server's site. **Area**: `updates`.",
    params(("id" = uuid::Uuid, Path, description = "The server"), packages::ListParams),
    responses(
        (status = 200, description = "One page of packages", body = packages::PackageList,
            example = json!({
                "reboot_required": false, "reported_at": "2026-11-14T06:00:00Z",
                "packages": [
                    {"name": "libssl3", "arch": "amd64", "version": "3.0.14-1~deb12u2",
                     "upgradable_version": "3.0.15-1~deb12u1", "title": null, "classification": null,
                     "severity": null, "size_bytes": null, "reboot_required": false},
                    {"name": "openssl", "arch": "amd64", "version": "3.0.14-1~deb12u2",
                     "upgradable_version": "3.0.15-1~deb12u1", "title": null, "classification": null,
                     "severity": null, "size_bytes": null, "reboot_required": false}
                ],
                "next_cursor": null
            })),
        (status = 400, description = "`limit` out of range, a bad `cursor` or an unknown parameter: \
`invalid`", body = ErrorBody),
        (status = 404, description = "No such server, or outside the key's reach: `not_found`",
            body = ErrorBody),
    ))]
fn server_packages() {}

#[utoipa::path(get, path = "/api/v1/servers/{id}/third-party", tag = "updates",
    summary = "List a server's third-party apps",
    description = "The third-party package managers of the server's latest report (ADR-0138): winget on \
Windows, Flatpak and Snap on Linux, each `available`, `absent` or `unusable` with its counts, and the apps \
they list as installed, by manager, ID and scope, with the version an update would install. `pending=true` \
keeps those with an update, `manager` one manager's. `reported` is false, with nothing listed, until the agent \
reports third-party managers. OS packages stay in `/servers/{id}/packages`.\n\n\
**Pages**: `limit` apps per page, 1 to 5000, 500 by default; pass `next_cursor` as `cursor` for the next \
page, `null` on the last one. `managers` is complete on every page. Unknown parameters are 400.\n\n\
**Who**: any role on the server's site. **Area**: `updates`.",
    params(("id" = uuid::Uuid, Path, description = "The server"), third_party::ListParams),
    responses(
        (status = 200, description = "The managers and one page of apps", body = third_party::List,
            example = json!({
                "reported": true,
                "managers": [
                    {"manager": "flatpak", "state": "available", "version": "1.14.10", "error": null,
                     "installed": 2, "pending": 1},
                    {"manager": "snap", "state": "absent", "version": null, "error": null,
                     "installed": 0, "pending": 0}
                ],
                "packages": [
                    {"manager": "flatpak", "id": "app/org.mozilla.firefox/x86_64/stable",
                     "name": "Firefox", "version": "131.0", "available_version": "132.0",
                     "scope": "machine", "origin": "flathub"},
                    {"manager": "flatpak", "id": "runtime/org.freedesktop.Platform/x86_64/23.08",
                     "name": "Freedesktop Platform", "version": "23.08.24", "available_version": null,
                     "scope": "machine", "origin": "flathub"}
                ],
                "next_cursor": null
            })),
        (status = 400, description = "`limit` out of range, an unknown `manager`, a bad `cursor` or an \
unknown parameter: `invalid`", body = ErrorBody),
        (status = 404, description = "No such server, or outside the key's reach: `not_found`",
            body = ErrorBody),
    ))]
fn server_third_party() {}

#[utoipa::path(get, path = "/api/v1/servers/{id}/rollback", tag = "updates",
    summary = "Get what a change on a server can roll back to",
    description = "What an AI change on the server gets before it runs: a snapshot of its Proxmox VM or \
container, else a local LVM or ZFS snapshot of its system volumes, else nothing, with why (`snapshots`). It \
also lists the system volumes of the last report, the mount points a local snapshot leaves out, and the local \
snapshots MonitorFlow keeps on the machine with when they are deleted.\n\n\
**Who**: any role on the server's site. **Area**: `updates`.",
    params(("id" = uuid::Uuid, Path, description = "The server")),
    responses(
        (status = 200, description = "Rollback for the server", body = rollback::RollbackView,
            example = json!({
                "server_id": ex::SERVER,
                "snapshots": {"available": true, "reason": null, "kind": null,
                              "detail": "LVM snapshot of / and /var", "left_out": null, "method": "lvm"},
                "not_covered": ["/boot"],
                "volumes": [
                    {"mount_point": "/", "fs_type": "ext4", "kind": "lvm", "source": "vg0/root",
                     "size_bytes": 21474836480u64, "free_bytes": 10737418240u64,
                     "pool_bytes": 107374182400u64, "metadata_percent": 0.0},
                    {"mount_point": "/var", "fs_type": "ext4", "kind": "lvm", "source": "vg0/var",
                     "size_bytes": 53687091200u64, "free_bytes": 10737418240u64,
                     "pool_bytes": 107374182400u64, "metadata_percent": 0.0}
                ],
                "local_snapshots": [],
                "reported_at": "2026-11-14T06:00:00Z"
            })),
        (status = 404, description = "No such server, or outside the key's reach: `not_found`",
            body = ErrorBody),
    ))]
fn server_rollback() {}

#[utoipa::path(get, path = "/api/v1/servers/{id}/sources", tag = "updates",
    summary = "Get where a server's packages come from",
    description = "Whether the server takes its packages from MonitorFlow's mirrors (and which) or from \
its own sources, a change on its way (`changing_to`), why the last change failed, and why it cannot use a \
mirror (`problem`; `add_mirror` when adding its `distribution` on the Mirrors page would fix it). For a \
Windows server, `windows_update_source` says where Windows Update takes updates from.\n\n\
**Who**: any role on the server's site. **Area**: `updates`.",
    params(("id" = uuid::Uuid, Path, description = "The server")),
    responses(
        (status = 200, description = "Its package sources", body = package_sources::SourcesStatus,
            example = json!(sources_status())),
        (status = 404, description = "No such server, removed, or outside the key's reach: \
`not_found`", body = ErrorBody),
    ))]
fn server_sources() {}

#[utoipa::path(post, path = "/api/v1/servers/{id}/sources", tag = "updates",
    summary = "Point a server at chosen mirrors",
    description = concat!("Queues a `configure_sources` command pointing the server's package manager at \
the mirrors named, of its distribution, each at its newest snapshot (`current`) or a published one. `replace` \
sets the server's other sources aside; `add` keeps them. `POST .../sources/use-mirror` does the same with \
the mirrors MonitorFlow picks. Audited as `command.create`. **Not safe to repeat**: send \
`Idempotency-Key` to retry.\n\n\
**Who**: ", "an administrator of the server's customer as a whole: a global administrator or one of that \
customer. A site administrator may not, and neither may a key that reaches one site", ". **Area**: `updates`."),
    params(("id" = uuid::Uuid, Path, description = "The server")),
    request_body(content = sources::ConfigureRequest, example = json!({
        "mirrors": [{"name": "debian-12"}, {"name": "debian-12-security", "snapshot": "current"}],
        "mode": "replace"
    })),
    responses(
        (status = 201, description = "The queued command", body = commands::CommandDetail,
            example = json!(queued("configure_sources", json!({
                "mirror_base_url": "http://monitorflow.example.com:8081", "family": "FAMILY_APT",
                "mode": "MODE_REPLACE",
                "repositories": [{"mirror": "debian-12", "snapshot": "current"},
                                 {"mirror": "debian-12-security", "snapshot": "current"}],
                "key_fingerprints": ["0123456789ABCDEF0123456789ABCDEF01234567"]
            })))),
        (status = 400, description = "No mirror or too many, an unknown mirror or snapshot, a mirror \
of another distribution, a Windows server, or `ttl_s` out of range: `invalid`", body = ErrorBody),
        (status = 403, description = "The key may not administer the server's customer: \
`forbidden`; or outside the key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such server, or outside the key's reach: `not_found`",
            body = ErrorBody),
        (status = 409, description = "The server is removed or has no package manager MonitorFlow \
can drive, mirrors are not enabled, their address is not set, or their signing key is missing: \
`conflict`", body = ErrorBody),
    ))]
fn configure_server_sources() {}

#[utoipa::path(delete, path = "/api/v1/servers/{id}/sources", tag = "updates",
    summary = "Put a server's own package sources back",
    description = concat!("Queues a `revert_sources` command: the server goes back to the package sources \
it had before MonitorFlow's mirrors. Answers 202 with the command. Audited as `command.create`. Each request \
queues a command; the agent does nothing when the sources are its own already.\n\n\
**Who**: ", "an administrator of the server's customer as a whole: a global administrator or one of that \
customer. A site administrator may not, and neither may a key that reaches one site", ". **Area**: `updates`."),
    params(("id" = uuid::Uuid, Path, description = "The server")),
    responses(
        (status = 202, description = "The queued command", body = commands::CommandDetail,
            example = json!(queued("revert_sources", json!({})))),
        (status = 400, description = "A Windows server: `invalid`", body = ErrorBody),
        (status = 403, description = "The key may not administer the server's customer: \
`forbidden`; or outside the key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such server, or outside the key's reach: `not_found`",
            body = ErrorBody),
        (status = 409, description = "The server is removed or has no package manager MonitorFlow \
can drive: `conflict`", body = ErrorBody),
    ))]
fn revert_server_sources() {}

#[utoipa::path(post, path = "/api/v1/servers/{id}/sources/use-mirror", tag = "updates",
    summary = "Point a server at the mirrors of its distribution",
    description = concat!("Queues a `configure_sources` command pointing the server at the mirrors \
MonitorFlow has for its distribution, replacing its own sources. 409 says why not when it cannot: it uses a \
mirror already, a change is on its way, a campaign holds its sources, or no mirror fits its distribution. \
Audited as `command.create`.\n\n\
**Who**: ", "an administrator of the server's customer as a whole: a global administrator or one of that \
customer. A site administrator may not, and neither may a key that reaches one site", ". **Area**: `updates`."),
    params(("id" = uuid::Uuid, Path, description = "The server")),
    responses(
        (status = 201, description = "The queued command", body = commands::CommandDetail,
            example = json!(queued("configure_sources", json!({
                "mirror_base_url": "http://monitorflow.example.com:8081", "family": "FAMILY_APT",
                "mode": "MODE_REPLACE",
                "repositories": [{"mirror": "debian-12", "snapshot": "current"}],
                "key_fingerprints": ["0123456789ABCDEF0123456789ABCDEF01234567"]
            })))),
        (status = 403, description = "The key may not administer the server's customer: \
`forbidden`; or outside the key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such server, or outside the key's reach: `not_found`",
            body = ErrorBody),
        (status = 409, description = "The server cannot use a mirror now, and `error` says why: \
`conflict`", body = ErrorBody),
    ))]
fn server_use_mirror() {}

#[utoipa::path(get, path = "/api/v1/sites/{id}/sources", tag = "updates",
    summary = "Get where a site's servers take packages from",
    description = "The package sources of every server of the site, as for one server, with its id, \
hostname and `os_family`, by hostname.\n\n\
**Who**: any role on the site. **Area**: `updates`.",
    params(("id" = uuid::Uuid, Path, description = "The site")),
    responses(
        (status = 200, description = "Its servers' sources", body = package_sources::SiteSources,
            example = json!({"servers": [ex::merge(sources_status(), json!({
                "id": ex::SERVER, "hostname": "web-1", "os_family": "debian"
            }))]})),
        (status = 404, description = "No such site, or outside the key's reach: `not_found`",
            body = ErrorBody),
    ))]
fn site_sources() {}

#[utoipa::path(post, path = "/api/v1/sites/{id}/sources/use-mirror", tag = "updates",
    summary = "Point a site's servers at the mirrors",
    description = "Does `POST /api/v1/servers/{id}/sources/use-mirror` for every server of the \
site that does not use a mirror yet. Each server is queued or skipped on its own: the answer lists both, with \
why each was skipped. Each command is audited as `command.create`. Safe to repeat: servers already on a \
mirror are left out.\n\n\
**Who**: an administrator of the site's customer as a whole. **Area**: `updates`.",
    params(("id" = uuid::Uuid, Path, description = "The site")),
    responses(
        (status = 200, description = "What was queued and skipped", body = package_sources::SiteChange,
            example = json!({"queued": [ex::SERVER], "skipped": [
                {"id": ex::SERVER_2, "hostname": "db-1",
                 "reason": "An update campaign has this server's package sources for now."}
            ]})),
        (status = 403, description = "The key may not administer the site's customer: `forbidden`; \
or outside the key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such site, or outside the key's reach: `not_found`",
            body = ErrorBody),
    ))]
fn site_use_mirror() {}

#[utoipa::path(delete, path = "/api/v1/sites/{id}/sources", tag = "updates",
    summary = "Put a site's own package sources back",
    description = "Queues `revert_sources` for every server of the site that uses a mirror. Each server is \
queued or skipped on its own, with why. Each command is audited as `command.create`. Safe to repeat: \
servers on their own sources are left out.\n\n\
**Who**: an administrator of the site's customer as a whole. **Area**: `updates`.",
    params(("id" = uuid::Uuid, Path, description = "The site")),
    responses(
        (status = 200, description = "What was queued and skipped", body = package_sources::SiteChange,
            example = json!({"queued": [ex::SERVER], "skipped": []})),
        (status = 403, description = "The key may not administer the site's customer: `forbidden`; \
or outside the key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such site, or outside the key's reach: `not_found`",
            body = ErrorBody),
    ))]
fn revert_site_sources() {}

#[utoipa::path(put, path = "/api/v1/sites/{id}/agent-auto-update", tag = "updates",
    summary = "Turn automatic agent updates on or off for a site",
    description = "`true` or `false` overrides, for the site's agents, the server-wide setting of \
automatic agent updates; `null` follows it again. The answer is the site, with \
`agent_auto_update_effective` saying whether they apply. Audited as `site.agent_auto_update`. Safe to \
repeat.\n\n\
**Who**: an administrator of the site's customer as a whole. **Area**: `updates`.",
    params(("id" = uuid::Uuid, Path, description = "The site")),
    request_body(content = sites::AutoUpdateRequest, examples(
        ("off" = (summary = "Not on this site", value = json!({"enabled": false}))),
        ("inherit" = (summary = "Follow the server-wide setting", value = json!({"enabled": null}))),
    )),
    responses(
        (status = 200, description = "The site", body = sites::SiteView,
            example = json!(ex::merge(ex::site(), json!({
                "agent_auto_update": false, "agent_auto_update_effective": false
            })))),
        (status = 403, description = "The key may not administer the site's customer: `forbidden`; \
or outside the key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such site, or outside the key's reach: `not_found`",
            body = ErrorBody),
    ))]
fn site_agent_auto_update() {}

#[utoipa::path(post, path = "/api/v1/servers/{id}/ai-commands", tag = "remediation",
    summary = "Turn a server's AI commands on or off",
    description = "Queues a signed `ai_commands` command for the server's agent, replacing one still queued. \
The agent always obeys \"off\". It obeys \"on\" only when its machine allowed AI commands to be turned on from \
the web interface; otherwise, or when its agent is too old to be switched, the answer is 409 saying what to do \
on the machine. \"On\" may start within an hour, \"off\" within a week, for a server that is offline.\n\n\
Audited as `server.ai_commands` and sent to the notification channels covering the server.\n\n\
**Who**: an administrator of the server's site (or of its customer, or a global one). **Area**: \
`remediation`.",
    params(("id" = uuid::Uuid, Path, description = "The server")),
    request_body(content = ai_commands::SwitchRequest, example = json!({"on": false})),
    responses(
        (status = 201, description = "The queued command", body = commands::CommandDetail,
            example = json!(ex::merge(queued("ai_commands", json!({"on": false})), json!({
                "expires_at": "2026-11-21T10:00:00Z"
            })))),
        (status = 403, description = "The key does not administer the server's site: `forbidden`; or \
outside the key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such server, or outside the key's reach: `not_found`",
            body = ErrorBody),
        (status = 409, description = "The agent would refuse it, and `error` says why and how to allow \
it: `conflict`", body = ErrorBody),
    ))]
fn server_ai_commands() {}

#[utoipa::path(post, path = "/api/v1/sites/{id}/ai-commands", tag = "remediation",
    summary = "Turn a site's AI commands on or off",
    description = "Does `POST /api/v1/servers/{id}/ai-commands` for every server of the site whose AI \
commands are not as asked. Each server is queued or skipped on its own, with why; servers already as asked \
are in neither list. Each switch is audited and notified as for one server.\n\n\
**Who**: an administrator of the site (or of its customer, or a global one). **Area**: `remediation`.",
    params(("id" = uuid::Uuid, Path, description = "The site")),
    request_body(content = ai_commands::SwitchRequest, example = json!({"on": true})),
    responses(
        (status = 200, description = "What was queued and skipped", body = package_sources::SiteChange,
            example = json!({"queued": [ex::SERVER], "skipped": [
                {"id": ex::SERVER_2, "hostname": "db-1", "reason": "this server only lets AI commands be \
turned on on the server itself; to allow it here, run the installer there with MF_AI_COMMANDS_FROM_WEB=1"}
            ]})),
        (status = 403, description = "The key does not administer the site: `forbidden`; or outside the \
key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such site, or outside the key's reach: `not_found`",
            body = ErrorBody),
    ))]
fn site_ai_commands() {}
