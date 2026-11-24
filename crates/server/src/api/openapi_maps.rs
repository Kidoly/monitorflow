//! Network maps (ADR-0185) in the OpenAPI document: merged into it by
//! [`super::openapi::build`]; the router in `mod.rs` serves the routes.

#![allow(dead_code)] // The functions exist for their `#[utoipa::path]`.

use serde_json::{json, Value};
use utoipa::OpenApi;

use super::maps;
use super::openapi::{Common, ErrorBody};
use super::openapi_core::ex;

#[derive(OpenApi)]
#[openapi(
    paths(list_maps, create_map, get_map, update_map, delete_map, live_map, get_background,
          set_background, remove_background, read_neighbors),
    components(schemas(ErrorBody)),
    modifiers(&Common),
)]
pub struct Doc;

const MAP: &str = "3c4d5e6f-7a8b-4c9d-8e0f-1a2b3c4d5e6f";
const SWITCH: &str = "1f2e3d4c-5b6a-4978-8a9b-0c1d2e3f4a5b";

fn document() -> Value {
    json!({
        "version": 1,
        "sources": {"relays": true, "dependencies": true, "neighbors": true, "discovered": false},
        "group_by_site": true,
        "nodes": [{"key": format!("server:{SWITCH}"), "x": 400.0, "y": 120.0},
                  {"key": format!("server:{}", ex::SERVER), "x": 400.0, "y": 300.0}],
        "zones": [{"id": "rack-a", "label": "Rack A", "x": 300.0, "y": 60.0, "width": 240.0,
                   "height": 320.0, "color": 2}],
        "links": [{"id": "wan", "from": format!("server:{SWITCH}"), "to": format!("relay:{}", ex::RELAY),
                   "label": "Fibre", "interface": {"server_id": SWITCH, "instance": "Gi1/0/48"}}],
        "labels": [{"id": "title", "text": "Paris office", "x": 300.0, "y": 20.0, "size": "large"}]
    })
}

fn summary() -> Value {
    json!({
        "id": MAP, "name": "Paris network",
        "place": {"kind": "site", "id": ex::SITE, "name": "paris"},
        "updated_at": "2026-12-27T09:30:00Z", "updated_by_username": "alice",
        "background_updated_at": null, "can_edit": true
    })
}

fn view() -> Value {
    let mut view = summary();
    view["document"] = document();
    view["revision"] = json!(3);
    view
}

fn live() -> Value {
    json!({
        "id": MAP, "name": "Paris network", "group_by_site": true,
        "sites": [{"id": ex::SITE, "name": "paris"}],
        "nodes": [
            {"key": "central", "kind": "central", "name": "MonitorFlow", "site_id": null, "state": "ok",
             "server_id": null, "relay_id": null, "address": null, "template": null, "os_family": null,
             "x": null, "y": null, "hidden": false},
            {"key": format!("relay:{}", ex::RELAY), "kind": "relay", "name": "gw-1", "site_id": ex::SITE,
             "state": "ok", "server_id": null, "relay_id": ex::RELAY, "address": null, "template": null,
             "os_family": null, "x": null, "y": null, "hidden": false},
            {"key": format!("server:{SWITCH}"), "kind": "device", "name": "sw-core", "site_id": ex::SITE,
             "state": "warning", "server_id": SWITCH, "relay_id": null, "address": "192.0.2.2",
             "template": "switch", "os_family": "device", "x": 400.0, "y": 120.0, "hidden": false},
            {"key": format!("server:{}", ex::SERVER), "kind": "server", "name": "web-1",
             "site_id": ex::SITE, "state": "ok", "server_id": ex::SERVER, "relay_id": null,
             "address": null, "template": null, "os_family": "debian", "x": 400.0, "y": 300.0,
             "hidden": false}
        ],
        "links": [
            {"id": format!("central|relay:{}", ex::RELAY), "from": "central",
             "to": format!("relay:{}", ex::RELAY), "source": "relay", "state": "ok", "label": null,
             "from_port": null, "to_port": null, "interface": null, "traffic": null},
            {"id": format!("server:{SWITCH}|server:{}", ex::SERVER), "from": format!("server:{SWITCH}"),
             "to": format!("server:{}", ex::SERVER), "source": "neighbor", "state": "warning",
             "label": null, "from_port": "Gi1/0/3", "to_port": "eth0",
             "interface": {"server_id": SWITCH, "instance": "Gi1/0/3"},
             "traffic": {"in_bytes_per_second": 1250000.0, "out_bytes_per_second": 310000.0,
                         "utilization": 0.01, "up": true}}
        ],
        "zones": [], "labels": [], "background": null,
        "neighbors_read_at": "2026-12-27T09:00:00Z", "truncated": false, "now": 1798362000
    })
}

#[utoipa::path(get, path = "/api/v1/maps", tag = "dashboards",
    summary = "List network maps",
    description = "The maps the caller sees, by name: those of the sites it reads, and of the customers \
it reads as a whole. `site_id` keeps those of one site, `customer_id` those of a customer and of its \
sites.\n\n**Who**: any role. **Area**: `dashboards`.",
    params(maps::ListParams),
    responses(
        (status = 200, description = "The maps", body = maps::MapList,
            example = json!({"maps": [summary()]})),
        (status = 400, description = "An unknown parameter: `invalid`", body = ErrorBody),
        (status = 404, description = "The site of the filter is outside the key's reach: `not_found`",
            body = ErrorBody),
    ))]
fn list_maps() {}

#[utoipa::path(post, path = "/api/v1/maps", tag = "dashboards",
    summary = "Make a network map",
    description = "A map of a site (`{\"kind\": \"site\", \"id\"}`) or of a customer as a whole \
(`{\"kind\": \"customer\", \"id\"}`). Without `document`, it draws everything from its sources and \
lays it out by itself; `document` holds what people placed by hand: positions (`nodes`, by key: \
`central`, `server:<id>`, `relay:<id>`, `host:<address>`, `neighbor:<name>`, with `hidden` to leave \
one off), `zones`, `links` (with the `interface` whose traffic a link shows), `labels`, and where the \
background lies. `sources` says what draws it: relays, dependencies and neighbours by default, and the \
unmonitored addresses of the latest network scans when `discovered` is set.\n\n\
**Who**: operator or above on the site, or on the customer as a whole. **Area**: `dashboards`. \
Audited as `network_map.create`.",
    request_body(content = maps::MapRequest, example = json!({
        "name": "Paris network", "place": {"kind": "site", "id": ex::SITE}, "document": document()})),
    responses(
        (status = 201, description = "The map", body = maps::MapView, example = json!(view())),
        (status = 400, description = "A name too long, a document that does not hold, or a link's \
interface outside the place: `invalid`", body = ErrorBody),
        (status = 403, description = "The key may only read the place: `forbidden`; or outside the key's \
areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "The site or customer is outside the key's reach: `not_found`",
            body = ErrorBody),
    ))]
fn create_map() {}

#[utoipa::path(get, path = "/api/v1/maps/{id}", tag = "dashboards",
    summary = "A network map",
    description = "Its name, place and what people placed by hand, with the `revision` to pass back \
when saving.\n\n**Who**: any role on its place. **Area**: `dashboards`.",
    params(("id" = uuid::Uuid, Path, description = "The map")),
    responses(
        (status = 200, description = "The map", body = maps::MapView, example = json!(view())),
        (status = 404, description = "No such map, or one the key does not see: `not_found`",
            body = ErrorBody),
    ))]
fn get_map() {}

#[utoipa::path(put, path = "/api/v1/maps/{id}", tag = "dashboards",
    summary = "Change a network map",
    description = "Replaces its name, place and document. `revision` is the one read: when someone \
saved since, the answer is 409 and nothing changes.\n\n**Who**: operator or above on its place, and \
on the new one. **Area**: `dashboards`. Audited as `network_map.update`.",
    params(("id" = uuid::Uuid, Path, description = "The map")),
    request_body(content = maps::MapRequest, example = json!({
        "name": "Paris network", "place": {"kind": "site", "id": ex::SITE}, "document": document(),
        "revision": 3})),
    responses(
        (status = 200, description = "The map", body = maps::MapView, example = json!(view())),
        (status = 400, description = "No `revision`, or as for making one: `invalid`", body = ErrorBody),
        (status = 403, description = "The key may only read it: `forbidden`", body = ErrorBody),
        (status = 404, description = "No such map, or one the key does not see: `not_found`",
            body = ErrorBody),
        (status = 409, description = "Someone saved it since `revision`: `conflict`", body = ErrorBody),
    ))]
fn update_map() {}

#[utoipa::path(delete, path = "/api/v1/maps/{id}", tag = "dashboards",
    summary = "Delete a network map",
    description = "With its background. Dashboard panels that showed it say it is gone.\n\n\
**Who**: operator or above on its place. **Area**: `dashboards`. Audited as `network_map.delete`.",
    params(("id" = uuid::Uuid, Path, description = "The map")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 403, description = "The key may only read it: `forbidden`", body = ErrorBody),
        (status = 404, description = "No such map, or one the key does not see: `not_found`",
            body = ErrorBody),
    ))]
fn delete_map() {}

#[utoipa::path(get, path = "/api/v1/maps/{id}/live", tag = "dashboards",
    summary = "A network map now",
    description = "Everything needed to draw the map now, as the caller may read it: its nodes with \
their states (as the fleet's pages say them, and `unknown` for what nothing watches) and where they \
were placed (`null` lets the client lay them out), its links with their states and the traffic of their \
interface, its zones, labels and background. A customer's map shows a viewer of one of its sites that \
site only.\n\n\
A link is as bad as the worse of its two ends and of its interface: critical when an end is critical, \
offline or unreachable, or its interface is down; warning when an end has a warning or the interface is \
used at 90 % or more of its speed. Links between the same two nodes are one, whatever said them; links \
drawn by hand always show. At most 2,000 nodes, and 500 found addresses and unmonitored neighbours; \
`truncated` says when some were left out.\n\n\
**Who**: any role on its place. **Area**: `dashboards`.",
    params(("id" = uuid::Uuid, Path, description = "The map")),
    responses(
        (status = 200, description = "The map now", body = maps::MapLive, example = json!(live())),
        (status = 404, description = "No such map, or one the key does not see: `not_found`",
            body = ErrorBody),
    ))]
fn live_map() {}

#[utoipa::path(get, path = "/api/v1/maps/{id}/background", tag = "dashboards",
    summary = "A network map's background",
    description = "The image under the map, as it was given.\n\n**Who**: any role on its place. \
**Area**: `dashboards`.",
    params(("id" = uuid::Uuid, Path, description = "The map")),
    responses(
        (status = 200, description = "The image: PNG, JPEG or WebP, as its `Content-Type` says",
            content(
                (Vec<u8> = "image/png"),
                (Vec<u8> = "image/jpeg"),
                (Vec<u8> = "image/webp"),
            )),
        (status = 404, description = "No such map, one the key does not see, or no background: \
`not_found`", body = ErrorBody),
    ))]
fn get_background() {}

#[utoipa::path(put, path = "/api/v1/maps/{id}/background", tag = "dashboards",
    summary = "Set a network map's background",
    description = "A floor plan, a rack, a city: a PNG, JPEG or WebP image of at most 1 MiB, in \
base64. Other formats, SVG included, are refused. Where it lies is the document's `background`.\n\n\
**Who**: operator or above on its place. **Area**: `dashboards`. Audited as \
`network_map.background`.",
    params(("id" = uuid::Uuid, Path, description = "The map")),
    request_body(content = maps::BackgroundRequest, example = json!({"image": "iVBORw0KGgo..."})),
    responses(
        (status = 204, description = "Set"),
        (status = 400, description = "Not base64, over 1 MiB, or not a PNG, JPEG or WebP image: \
`invalid`", body = ErrorBody),
        (status = 403, description = "The key may only read it: `forbidden`", body = ErrorBody),
        (status = 404, description = "No such map, or one the key does not see: `not_found`",
            body = ErrorBody),
    ))]
fn set_background() {}

#[utoipa::path(delete, path = "/api/v1/maps/{id}/background", tag = "dashboards",
    summary = "Remove a network map's background",
    description = "**Who**: operator or above on its place. **Area**: `dashboards`. Audited as \
`network_map.background` when there was one.",
    params(("id" = uuid::Uuid, Path, description = "The map")),
    responses(
        (status = 204, description = "Removed, or there was none"),
        (status = 403, description = "The key may only read it: `forbidden`", body = ErrorBody),
        (status = 404, description = "No such map, or one the key does not see: `not_found`",
            body = ErrorBody),
    ))]
fn remove_background() {}

#[utoipa::path(post, path = "/api/v1/maps/{id}/neighbors", tag = "dashboards",
    summary = "Read devices' neighbours",
    description = "Asks the SNMP devices of the map's site or customer that the caller may change, at \
most 50, for their LLDP and CDP neighbours, each from its probe, and keeps them for every map. A \
neighbour that is a server or device of the map (by its address, else its host name without the \
domain) becomes a link between them, showing the traffic of the device's port; others are shown as \
unmonitored neighbours. Each device takes six walks of its probe, so this can take a minute.\n\n\
**Who**: operator or above on its place, as it sends the devices' credentials to their probes. \
**Area**: `dashboards`. Not audited: it changes no setting.",
    params(("id" = uuid::Uuid, Path, description = "The map")),
    responses(
        (status = 200, description = "What was read", body = maps::NeighborRead,
            example = json!({"devices": 3, "neighbors": 17, "failed": [{"server_id": SWITCH,
                "name": "sw-core", "error": "the device did not answer in time"}]})),
        (status = 403, description = "The key may only read it: `forbidden`", body = ErrorBody),
        (status = 404, description = "No such map, or one the key does not see: `not_found`",
            body = ErrorBody),
    ))]
fn read_neighbors() {}
