//! Dependencies (ADR-0152) in the OpenAPI document: merged into it by
//! [`super::openapi::build`]; the router in `mod.rs` serves the routes.

#![allow(dead_code)] // The functions exist for their `#[utoipa::path]`.

use serde_json::{json, Value};
use utoipa::OpenApi;

use super::dependencies;
use super::openapi::{Common, ErrorBody};
use super::openapi_core::ex;

#[derive(OpenApi)]
#[openapi(
    paths(list_dependencies, create_dependencies, remove_dependency, server_dependencies,
          relay_dependencies),
    components(schemas(ErrorBody)),
    modifiers(&Common),
)]
pub struct Doc;

const DEPENDENCY: &str = "5e6f7a8b-9c0d-4e1f-a2b3-c4d5e6f7a8b9";
const SWITCH: &str = "1f2e3d4c-5b6a-4978-8a9b-0c1d2e3f4a5b";

fn switch() -> Value {
    json!({"kind": "device", "server_id": SWITCH, "relay_id": null, "name": "sw-core",
           "site_id": ex::SITE, "check": null})
}

fn web() -> Value {
    json!({"kind": "server", "server_id": ex::SERVER, "relay_id": null, "name": "web-1",
           "site_id": ex::SITE, "check": null})
}

fn dependency() -> Value {
    json!({
        "id": DEPENDENCY, "automatic": false, "child": web(), "parent": switch(),
        "created_at": "2026-11-18T09:30:00Z", "created_by": "alice", "can_change": true
    })
}

fn request() -> Value {
    json!({
        "parent": {"server_id": SWITCH},
        "children": [{"server_id": ex::SERVER}, {"server_id": ex::SERVER_2, "check": {"name": "https"}}]
    })
}

fn page() -> Value {
    json!({
        "parents": [
            dependency(),
            {"id": null, "automatic": true, "child": web(),
             "parent": {"kind": "relay", "server_id": null, "relay_id": ex::RELAY, "name": "gw-1",
                        "site_id": ex::SITE, "check": null},
             "created_at": null, "created_by": null, "can_change": false}
        ],
        "children": [],
        "behind": [],
        "suggestions": [{"parent": {"kind": "device", "server_id": "0a1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d",
                                    "relay_id": null, "name": "fw-1", "site_id": ex::SITE, "check": null},
                         "reason": "network_device"}],
        "unreachable": {"because": switch(), "incident_id": ex::INCIDENT,
                        "since": "2026-11-18T10:02:00Z"},
        "unreachable_checks": [],
        "can_change": true
    })
}

#[utoipa::path(get, path = "/api/v1/dependencies", tag = "alerts",
    summary = "List dependencies",
    description = "What is behind what, among the servers, devices and relays the key reads. A \
dependency says that a child (a server, a device, or one of its checks) is behind a parent (a server, a \
device, a relay, or one check such as an uplink ping). While a parent is down, what is behind it opens no \
incident of its own and notifies nobody: it is unreachable, and the parent's incident speaks for it. \
Children are judged again 2 minutes after the parent's incident resolves.\n\n\
A parent is down while its offline incident is open (a server's agent, a relay), while a ping check of it \
has a critical incident open (a server or device), or, for a check, while that check has a critical \
incident open.\n\n\
This lists the dependencies people set; the automatic ones (a server behind its relay, a check behind \
the relay that runs it) are in `GET /api/v1/servers/{id}/dependencies`.\n\n\
**Who**: any role. **Area**: `alerts`.\n\n\
**Filter**: `server_id` for those whose child or parent is that server or device or one of its checks, \
`relay_id` for those behind that relay. Unknown parameters are 400.",
    params(dependencies::ListParams),
    responses(
        (status = 200, description = "The dependencies", body = dependencies::DependencyList,
            example = json!({"dependencies": [dependency()]})),
        (status = 400, description = "An unknown parameter: `invalid`", body = ErrorBody),
        (status = 404, description = "The server or relay of the filter does not exist or is outside \
the key's reach: `not_found`", body = ErrorBody),
    ))]
fn list_dependencies() {}

#[utoipa::path(post, path = "/api/v1/dependencies", tag = "alerts",
    summary = "Add dependencies",
    description = "Puts `children` behind `parent`: \"these servers are behind this switch\". Each end is \
a `server_id` (a server or device), with `check` for one of its checks instead of all of it; a parent may \
be a `relay_id` instead. All are added, or none.\n\n\
Refused: a child behind itself, a server behind one of its own checks (one check of a server may depend on \
another of its checks), a dependency that already exists (409), one that would put something behind \
itself through others (409, with the path in words: \"web-1 cannot depend on sw-core: sw-core already \
depends on web-1 (sw-core, then fw-1, then web-1)\"), and ends of two customers (400).\n\n\
**Who**: operator or above on the site of every end. **Area**: `alerts`. Audited as \
`dependency.create`, one entry per child. **Not safe to repeat**: a second time answers 409.",
    request_body(content = dependencies::DependencyRequest, example = json!(request())),
    responses(
        (status = 201, description = "The dependencies added", body = dependencies::DependencyList,
            example = json!({"dependencies": [dependency()]})),
        (status = 400, description = "No child or over 500, an end without `server_id` (or `relay_id` \
for the parent), a relay as a child, an unknown check, something behind itself, or two customers: \
`invalid`", body = ErrorBody),
        (status = 403, description = "The key may only read one of the sites: `forbidden`; or outside the \
key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "An end does not exist, was removed, or is outside the key's reach: \
`not_found`", body = ErrorBody),
        (status = 409, description = "It exists already, or would close a loop: `conflict`",
            body = ErrorBody),
    ))]
fn create_dependencies() {}

#[utoipa::path(delete, path = "/api/v1/dependencies/{id}", tag = "alerts",
    summary = "Remove a dependency",
    description = "The child is no longer behind the parent: it opens its own incidents again at its \
next check that fails. Automatic dependencies have no ID and cannot be removed.\n\n\
**Who**: operator or above on the sites of both ends. **Area**: `alerts`. Audited as \
`dependency.remove`.",
    params(("id" = uuid::Uuid, Path, description = "The dependency")),
    responses(
        (status = 204, description = "Removed"),
        (status = 403, description = "The key may only read one of the sites: `forbidden`; or outside \
the key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such dependency, or one the key does not see: `not_found`",
            body = ErrorBody),
    ))]
fn remove_dependency() {}

#[utoipa::path(get, path = "/api/v1/servers/{id}/dependencies", tag = "alerts",
    summary = "A server's dependencies",
    description = "What a server's or device's page shows: what it and its checks depend on, automatic \
ones included (`automatic: true`, such as its relay); what depends directly on it (`children`); \
everything behind it, directly or through others (`behind`); parents that look obvious and are not set \
(`suggestions`: the Proxmox host its virtual machine or container runs on, when monitored, and the \
switches, routers and firewalls of its site), never added without a request; and, while a parent is \
down, which one and its incident (`unreachable`), for it or for some of its checks \
(`unreachable_checks`).\n\n\
**Who**: any role on its site. **Area**: `alerts`.",
    params(("id" = uuid::Uuid, Path, description = "The server or device")),
    responses(
        (status = 200, description = "Its dependencies", body = dependencies::PageView,
            example = json!(page())),
        (status = 404, description = "No such server, or one the key does not see: `not_found`",
            body = ErrorBody),
    ))]
fn server_dependencies() {}

#[utoipa::path(get, path = "/api/v1/relays/{id}/dependencies", tag = "alerts",
    summary = "What is behind a relay",
    description = "The servers behind a relay and the checks it runs, which are behind it without being \
set (`automatic: true`), and what people put behind it. A relay depends on nothing, so `parents` and \
`suggestions` are empty.\n\n\
**Who**: any role on its site. **Area**: `alerts`.",
    params(("id" = uuid::Uuid, Path, description = "The relay")),
    responses(
        (status = 200, description = "What is behind it", body = dependencies::PageView,
            example = json!({"parents": [], "children": [{"id": null, "automatic": true, "child": web(),
                "parent": {"kind": "relay", "server_id": null, "relay_id": ex::RELAY, "name": "gw-1",
                           "site_id": ex::SITE, "check": null},
                "created_at": null, "created_by": null, "can_change": false}],
                "behind": [web()], "suggestions": [], "unreachable": null, "unreachable_checks": [],
                "can_change": true})),
        (status = 404, description = "No such relay, or one the key does not see: `not_found`",
            body = ErrorBody),
    ))]
fn relay_dependencies() {}
