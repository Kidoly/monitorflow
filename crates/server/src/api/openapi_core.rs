//! Batch 1 of the public API's description (ADR-0101): customers, and
//! the routes under a server or a site. `openapi.rs` merges [`Doc`] into
//! the document; the functions only carry each route's description.
//!
//! [`ex`] holds the made-up data the examples share, so a server or a site
//! reads the same in every answer.

#![allow(dead_code)] // The functions exist for their `#[utoipa::path]`.

use utoipa::OpenApi;

use super::customers;
use super::openapi::ErrorBody;

#[derive(OpenApi)]
#[openapi(paths(rename_customer))]
pub struct Doc;

/// Example data: IDs, names and times reused across operations.
pub mod ex {
    use serde_json::{json, Value};

    pub const SERVER: &str = "6f1c2a7e-3b4d-4e8f-9a0b-1c2d3e4f5a6b";
    pub const SERVER_2: &str = "0d8e7f6a-5b4c-4d3e-8f2a-1b0c9d8e7f6a";
    pub const SITE: &str = "2b7e1516-28ae-4d2a-a6f7-15880928cf4f";
    pub const SITE_2: &str = "5a1d2c3b-4e5f-4a6b-8c7d-9e0f1a2b3c4d";
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
