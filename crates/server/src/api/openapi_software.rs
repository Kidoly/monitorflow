//! The OpenAPI description of the software catalogue (ADR-0188). Merged
//! into the document by [`super::openapi::build`]; the router in `mod.rs`
//! serves the routes.

#![allow(dead_code)] // The functions exist for their `#[utoipa::path]`.

use serde_json::json;
use utoipa::OpenApi;

use super::commands::CommandDetail;
use super::openapi::{Common, ErrorBody};
use super::openapi_core::{ex, queued};
use super::{software, software_switch};

#[derive(OpenApi)]
#[openapi(
    paths(list_software, create_software, update_software, delete_software, software_off),
    components(schemas(software::SoftwareRequest, software_switch::SwitchRequest)),
    modifiers(&Common),
    tags(
        (name = "software", description = "The software catalogue: what campaigns and commands \
install on servers or remove from them (`software` of `POST /campaigns` and of `POST \
/servers/{id}/commands`). An entry names a winget package and the source it installs from (Windows), or \
a package of the distribution for apt (Debian and Ubuntu) or dnf (RHEL and its derivatives), \
optionally from a mirror of the central server that a campaign adds to the servers' sources first. \
Each server takes the entries of its own manager. An entry is shared by every customer \
(`customer_id` null), which only global administrators change, or belongs to one customer, whose \
administrators change it. A server takes software only where its administrator allowed it at \
install (`SOFTWARE_FROM_WEB=1` on Windows, `MF_SOFTWARE_FROM_WEB=1` on Linux, or `monitorflow-agent \
software on`): the central server cannot grant itself that right. It can only take it away, with \
`POST /servers/{id}/software`. Needs the key area \"updates\"."),
    )
)]
pub struct Doc;

fn entry() -> serde_json::Value {
    json!({
        "id": "5b6c7d8e-9f0a-4b1c-8d2e-3f4a5b6c7d8e",
        "customer_id": null,
        "name": "7-Zip",
        "manager": "winget",
        "package_id": "7zip.7zip",
        "source": "winget",
        "mirror": null,
        "description": "File archiver",
        "created_by": "9a1b2c3d-4e5f-4a6b-8c7d-6e5f4a3b2c1d",
        "created_at": "2026-11-20T09:00:00Z",
        "updated_at": "2026-11-20T09:00:00Z"
    })
}

/// List the software catalogue
///
/// The entries shared by every customer and those of the customers the
/// caller sees, sorted by name. `customer_id` keeps the shared entries
/// and that customer's.
///
/// Needs any role and the key area "updates".
#[utoipa::path(get, path = "/api/v1/software", tag = "software",
    params(software::ListParams),
    responses(
        (status = 200, description = "The catalogue", body = software::Catalog,
            example = json!({"software": [entry()]})),
        (status = 400, body = ErrorBody, description = "`invalid`: a bad `customer_id`"),
        (status = 403, body = ErrorBody, description = "`forbidden`: the caller has no role. \
`api_key_scope`: the key lacks the area"),
    ))]
fn list_software() {}

/// Add software to the catalogue
///
/// A winget package by its ID, and the source it installs from (`winget`,
/// the community source, by default; never the store's, `msstore`). Or,
/// with `manager` `apt` or `dnf`, a package of the distribution by name,
/// without a source, and optionally the `mirror` of the central server
/// of the same family it installs from. A package is listed once per
/// customer and manager, and once among shared entries.
///
/// Needs an administrator role on `customer_id` as a whole, or a global
/// one for a shared entry, and the key area "updates". Audited as
/// `software.create`.
#[utoipa::path(post, path = "/api/v1/software", tag = "software",
    request_body(content = software::EntryRequest, example = json!({
        "name": "7-Zip", "package_id": "7zip.7zip", "source": "winget",
        "description": "File archiver"
    })),
    responses(
        (status = 201, description = "The entry", body = software::Entry, example = entry),
        (status = 400, body = ErrorBody, description = "`invalid`: a bad name, manager, \
package ID, source, mirror or description"),
        (status = 403, body = ErrorBody, description = "`forbidden`: the caller does not \
administer the customer, or is not a global administrator for a shared entry. `api_key_scope`: the \
key lacks the area"),
        (status = 404, body = ErrorBody, description = "`not_found`: a customer the caller does \
not see"),
        (status = 409, body = ErrorBody, description = "`conflict`: the package is in the \
catalogue already for that customer"),
    ))]
fn create_software() {}

/// Edit software of the catalogue
///
/// The whole entry again. Its customer cannot change. Campaigns and
/// commands made from it keep what they resolved when they were saved.
///
/// Needs what adding it needs. Audited as `software.update`.
#[utoipa::path(put, path = "/api/v1/software/{id}", tag = "software",
    params(("id" = uuid::Uuid, Path, description = "The entry")),
    request_body(content = software::EntryRequest, example = json!({
        "name": "7-Zip", "package_id": "7zip.7zip", "source": "winget",
        "description": "File archiver, machine-wide"
    })),
    responses(
        (status = 200, description = "The entry", body = software::Entry, example = entry),
        (status = 400, body = ErrorBody, description = "`invalid`: as when adding it, or a \
different `customer_id`"),
        (status = 403, body = ErrorBody, description = "`forbidden`: as when adding it. \
`api_key_scope`: the key lacks the area"),
        (status = 404, body = ErrorBody, description = "`not_found`: no such entry, or one the \
caller does not see"),
        (status = 409, body = ErrorBody, description = "`conflict`: another entry has that \
package for that customer"),
    ))]
fn update_software() {}

/// Remove software from the catalogue
///
/// Campaigns and commands made from it keep what they resolved.
///
/// Needs what adding it needs. Audited as `software.delete`.
#[utoipa::path(delete, path = "/api/v1/software/{id}", tag = "software",
    params(("id" = uuid::Uuid, Path, description = "The entry")),
    responses(
        (status = 204, description = "Removed"),
        (status = 403, body = ErrorBody, description = "`forbidden`: as when adding it. \
`api_key_scope`: the key lacks the area"),
        (status = 404, body = ErrorBody, description = "`not_found`: no such entry, or one the \
caller does not see"),
    ))]
fn delete_software() {}

/// Turn software installs off on a server
///
/// Queues the signed `software_off` command: the agent stops installing
/// and removing software from the catalogue, and its root helper and
/// update service refuse too, until the server's administrator turns it
/// on again there (`monitorflow-agent software on`). `{"on": true}` is
/// always refused: the central server can take the right away, never
/// give it. The command waits up to seven days for an offline server; a
/// newer request cancels one still queued. `GET /servers/{id}` shows the
/// last one as `software_off_change`.
///
/// Needs an administrator role on the server's site, its customer or
/// everything, and the key area "updates". Audited as
/// `server.software_off`.
#[utoipa::path(post, path = "/api/v1/servers/{id}/software", tag = "software",
    params(("id" = uuid::Uuid, Path, description = "The server")),
    request_body(content = software_switch::SwitchRequest, example = json!({"on": false})),
    responses(
        (status = 201, description = "The queued command", body = CommandDetail,
            example = json!(ex::merge(queued("software_off", json!({})), json!({
                "expires_at": "2026-11-21T10:00:00Z"
            })))),
        (status = 400, body = ErrorBody, description = "`invalid`: a malformed body"),
        (status = 403, body = ErrorBody, description = "`forbidden`: the caller does not \
administer the server's site. `api_key_scope`: the key lacks the area"),
        (status = 404, body = ErrorBody, description = "`not_found`: no such server, or one the \
caller does not see"),
        (status = 409, body = ErrorBody, description = "`conflict`: `on` is true (the message \
gives the line to run on the server), software installs are already off there, the agent is too \
old, or the server is removed"),
    ))]
fn software_off() {}
