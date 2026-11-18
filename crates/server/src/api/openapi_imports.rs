//! Imports from another monitoring tool in the OpenAPI document
//! (ADR-0146): merged into it by [`super::openapi::build`].

#![allow(dead_code)] // The functions exist for their `#[utoipa::path]`.

use serde_json::json;
use utoipa::OpenApi;

use super::imports;
use super::openapi::{Common, ErrorBody};
use crate::import::store::Failure;
use crate::import::{Create, Item, List, NoteView, OriginView};

#[derive(OpenApi)]
#[openapi(
    paths(list_import_sources, list_imports, preview_import, import, confirm_import, undo_import),
    components(schemas(ErrorBody, Item, List, NoteView, OriginView, Create, Failure)),
    modifiers(&Common),
    tags((name = "imports", description = "Moving from another monitoring tool. An import reads the \
other tool's configuration, from export files or through its API, and shows first what MonitorFlow would \
create: servers waiting for their agent, checks, thresholds, silences and notification channels. Each item \
is in one of three lists: `exact` (MonitorFlow does the same), `approximated` (close, the note says how it \
differs) or `not_movable` (the note says why), and says whether MonitorFlow has it already (`present`). \
Nothing is created until the preview is confirmed, with the items to leave out; items that need one left \
out are left out too. Importing the same configuration again creates nothing twice. An import can be \
undone: what it created goes, except servers an agent has taken over. Servers then only need the agent: \
one that enrolls with a server's ID, or with an install key of its site from a host of that name, takes \
it over with its checks. The other tool is only read, never changed, and can keep running. A connection's \
token is used for the request and never stored. Administrators of the target site only; everything is in \
the audit log. API keys need the `administration` area."))
)]
pub(super) struct Doc;

/// List the tools an import reads
///
/// Each source says which files it reads and the fields of its
/// connection. Administrators only.
#[utoipa::path(get, path = "/api/v1/imports/sources", tag = "imports",
    responses(
        (status = 200, description = "The sources", body = imports::SourceList, example = json!({
            "sources": [{
                "id": "example",
                "name": "Example tool",
                "extensions": ["yaml", "yml", "xml", "json"],
                "many_files": true,
                "connect": [
                    {"key": "url", "secret": false, "optional": false, "placeholder": "https://monitoring.example.com"},
                    {"key": "token", "secret": true, "optional": false, "placeholder": ""},
                ],
            }],
        })),
        (status = 403, description = "Not an administrator: `forbidden`; `api_key_scope`", body = ErrorBody),
    ))]
fn list_import_sources() {}

/// List imports
///
/// The latest 50 imports of the sites the key's maker administers, newest
/// first, without their items.
#[utoipa::path(get, path = "/api/v1/imports", tag = "imports",
    responses(
        (status = 200, description = "The imports", body = imports::RunList, example = json!({
            "imports": [run(false)],
        })),
        (status = 403, description = "Not an administrator: `forbidden`; `api_key_scope`", body = ErrorBody),
    ))]
fn list_imports() {}

/// Read another tool's configuration into a preview
///
/// Reads `files`, or connects with `connection`, and stores a preview of
/// what a confirmation would create; nothing else is written. `site_id`
/// may be left out by someone who administers one site. A connection's
/// token is used for this request only. Audited as `import.preview`. A
/// preview can be confirmed for a day.
#[utoipa::path(post, path = "/api/v1/imports", tag = "imports",
    request_body(content = imports::PreviewRequest, example = json!({
        "source": "example",
        "site_id": "9b2e6a41-3c7d-4e8f-a1b2-c3d4e5f60718",
        "connection": {"url": "https://monitoring.example.com", "token": "an API token"},
    })),
    responses(
        (status = 201, description = "The preview", body = imports::RunView, example = json!(run(true))),
        (status = 400, description = "`invalid`: unknown source, files and a connection together or neither, a file it cannot read, a field missing, the other tool unreachable or refusing the token", body = ErrorBody),
        (status = 403, description = "`forbidden`: not an administrator of the site; `api_key_scope`", body = ErrorBody),
        (status = 404, description = "`not_found`: no such site", body = ErrorBody),
    ))]
fn preview_import() {}

/// Read an import
///
/// One import with its items, in the order a confirmation creates them.
#[utoipa::path(get, path = "/api/v1/imports/{id}", tag = "imports",
    params(("id" = uuid::Uuid, Path, description = "The import")),
    responses(
        (status = 200, description = "The import", body = imports::RunView, example = json!(run(true))),
        (status = 404, description = "`not_found`: no such import, or not of a site the key's maker administers", body = ErrorBody),
    ))]
fn import() {}

/// Confirm an import
///
/// Creates the preview's items but those in `skip` and those that need
/// one of them, in one transaction. What exists meanwhile is not created
/// again; an item that fails is listed in `result.failed` and the others
/// are created. Audited as `import.confirm`.
#[utoipa::path(post, path = "/api/v1/imports/{id}/confirm", tag = "imports",
    params(("id" = uuid::Uuid, Path, description = "The import")),
    request_body(content = imports::ConfirmRequest, example = json!({
        "skip": ["check:web-01:log_syslog"],
    })),
    responses(
        (status = 200, description = "Confirmed", body = imports::RunView, example = json!(run(true))),
        (status = 404, description = "`not_found`", body = ErrorBody),
        (status = 409, description = "`conflict`: confirmed already, or a preview more than a day old", body = ErrorBody),
    ))]
fn confirm_import() {}

/// Undo an import
///
/// Removes what the import created and still exists. A server an agent
/// has taken over stays, with what was created on it, and is listed in
/// `result.kept_servers`. Audited as `import.undo`.
#[utoipa::path(post, path = "/api/v1/imports/{id}/undo", tag = "imports",
    params(("id" = uuid::Uuid, Path, description = "The import")),
    responses(
        (status = 200, description = "Undone", body = imports::RunView, example = json!(run(true))),
        (status = 404, description = "`not_found`", body = ErrorBody),
        (status = 409, description = "`conflict`: not confirmed, or undone already", body = ErrorBody),
    ))]
fn undo_import() {}

fn run(items: bool) -> serde_json::Value {
    let mut run = json!({
        "id": "3f6c2a1e-8b4d-4e7f-9a1c-5d2e3f4a6b7c",
        "source": "example",
        "source_name": "Example tool",
        "site_id": "9b2e6a41-3c7d-4e8f-a1b2-c3d4e5f60718",
        "site_name": "paris",
        "input": "https://monitoring.example.com",
        "state": "confirmed",
        "created_at": "2026-11-17T09:00:00Z",
        "created_by": "alice",
        "expires_at": null,
        "confirmed_at": "2026-11-17T09:05:00Z",
        "undone_at": null,
        "counts": {"exact": 2, "approximated": 1, "not_movable": 1, "present": 0, "to_create": 3},
        "result": {
            "created": {"server": 1, "check": 1, "threshold": 1},
            "skipped": 0, "present": 0, "failed": [], "removed": {}, "kept_servers": [],
        },
    });
    if items {
        run["items"] = json!([
            {
                "id": "server:web-01", "kind": "server", "list": "exact",
                "origin": {"kind": "host", "name": "web-01"},
                "hosts": ["web-01"], "present": false, "needs": [],
                "create": {"object": "server", "peer_id": "web-01", "hostname": "web-01",
                           "os_family": "debian", "addresses": ["10.0.0.5"]},
            },
            {
                "id": "check:web-01:http_shop_example_com", "kind": "check", "list": "approximated",
                "note": {"code": "web_password", "args": {"username": "monitor"},
                         "text": "Passwords are not imported: set the password of monitor in the check after the import."},
                "origin": {"kind": "web_scenario", "name": "Shop", "detail": "https://shop.example.com/"},
                "hosts": ["web-01"], "present": false, "needs": ["server:web-01"],
                "create": {"object": "check", "server": "web-01", "kind": "http", "name": "http_shop_example_com",
                           "interval_ms": 60000, "timeout_ms": 15000,
                           "params": {"url": "https://shop.example.com/", "status": "200", "contains": "Welcome"}},
            },
            {
                "id": "threshold:web-01:monitorflow_cpu_usage_ratio:total", "kind": "threshold", "list": "exact",
                "origin": {"kind": "trigger", "name": "High CPU utilization",
                           "detail": "min(/web-01/system.cpu.util,5m)>90"},
                "hosts": ["web-01"], "present": false, "needs": ["server:web-01"],
                "create": {"object": "threshold", "server": "web-01", "metric": "monitorflow_cpu_usage_ratio",
                           "instance": "total", "comparison": ">", "warning": 0.9, "critical": null,
                           "duration_s": 300, "hysteresis": 0.0},
            },
            {
                "id": "other:item:Interface eth0: Bits received", "kind": "other", "list": "not_movable",
                "note": {"code": "network_traffic", "args": {},
                         "text": "MonitorFlow does not measure network interface traffic yet."},
                "origin": {"kind": "item", "name": "Interface eth0: Bits received", "detail": "net.if.in[\"eth0\"]"},
                "hosts": ["web-01"], "present": false, "needs": [],
            },
        ]);
    }
    run
}
