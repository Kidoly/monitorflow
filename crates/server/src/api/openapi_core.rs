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
    pub const SITE: &str = "2b7e1516-28ae-4d2a-a6f7-15880928cf4f";
    pub const SITE_2: &str = "5a1d2c3b-4e5f-4a6b-8c7d-9e0f1a2b3c4d";
    pub const CUSTOMER: &str = "9c1e5f3a-7b2d-4c8e-8f6a-0d4b3e2a1c5f";

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
