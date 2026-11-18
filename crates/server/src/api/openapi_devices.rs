//! Devices and probes in the OpenAPI document (ADR-0148): merged into it
//! by [`super::openapi::build`].

#![allow(dead_code)] // The functions exist for their `#[utoipa::path]`.

use serde_json::json;
use utoipa::OpenApi;

use super::devices;
use super::openapi::{Common, ErrorBody};
use super::openapi_core::ex;
use super::servers;

#[derive(OpenApi)]
#[openapi(
    paths(create_device, update_device, site_probes),
    components(schemas(ErrorBody, devices::Probe)),
    modifiers(&Common),
    tags((name = "devices", description = "Devices: switches, routers, printers, UPSs or any address \
watched without the agent, by network checks that run from the central server or from a relay of the \
device's site (a probe). A device is a server of kind `device`: it is listed, read, silenced, charted \
and removed with the server routes, its checks are `sensor_configs` rows with a `probe`, and its \
thresholds are a server's. It is never offline nor waiting for an agent: its checks say what is wrong. \
The same probe checks can watch a server that has the agent, from outside. Operators of the site add and \
change devices. Area `servers`."))
)]
pub(super) struct Doc;

fn device() -> serde_json::Value {
    ex::merge(
        ex::server(),
        json!({
            "id": ex::SERVER_2, "peer_id": "core-switch", "hostname": "Core switch",
            "os_family": "device", "os_version": null, "kind": "device", "address": "192.0.2.10",
            "package_updates": false, "command_kinds": null, "agent_version": null,
            "status": "device", "connected_at": null, "last_heartbeat": null,
            "open_incidents": 0, "cert_not_after": null, "awaiting_agent": false
        }),
    )
}

#[utoipa::path(post, path = "/api/v1/devices", tag = "devices",
    summary = "Add a device",
    description = "Adds an address without an agent to a site, with an ID made from its name (made \
unique as install keys do), and by default a ping every minute from the relay of its site, or from the \
central server when the site has no relay, with its thresholds: critical when it gets no answer for 2 \
minutes, a warning above 20% packet loss for 5 minutes. Add more checks with \
`POST /api/v1/servers/{id}/sensor_configs` and a `probe`.\n\n\
**Who**: operator or above on the site. **Area**: `servers`. Audited as `device.create`.",
    request_body(content = devices::DeviceRequest, examples(
        ("ping" = (summary = "A switch pinged from the site's relay",
            value = json!({"site_id": ex::SITE, "name": "Core switch", "address": "192.0.2.10"}))),
        ("central" = (summary = "A website's server pinged from the central server",
            value = json!({"site_id": ex::SITE, "name": "Shop front", "address": "shop.example.com",
                           "probe": "central"}))),
    )),
    responses(
        (status = 201, description = "The device, as a server", body = servers::ServerView,
            example = json!(device())),
        (status = 400, description = "A name or address that is not valid, or a `probe` that is \
neither `central` nor a relay of the site: `invalid`", body = ErrorBody),
        (status = 403, description = "The key may only read the site: `forbidden`; or outside the \
key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such site, or outside the key's reach: `not_found`",
            body = ErrorBody),
    ))]
fn create_device() {}

#[utoipa::path(put, path = "/api/v1/devices/{id}", tag = "devices",
    summary = "Rename a device or change its address",
    description = "Changes what is given and keeps the rest. Checks without a `host` of their own \
follow the new address within seconds.\n\n\
**Who**: operator or above on the site. **Area**: `servers`. Audited as `device.update`.",
    params(("id" = uuid::Uuid, Path, description = "The device")),
    request_body(content = devices::DeviceUpdate, example = json!({"address": "192.0.2.11"})),
    responses(
        (status = 200, description = "The device", body = servers::ServerView,
            example = json!(ex::merge(device(), json!({"address": "192.0.2.11"})))),
        (status = 400, description = "A name or address that is not valid: `invalid`",
            body = ErrorBody),
        (status = 403, description = "The key may only read the site: `forbidden`; or outside the \
key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such device (a server with an agent is not one), or \
outside the key's reach: `not_found`", body = ErrorBody),
    ))]
fn update_device() {}

#[utoipa::path(get, path = "/api/v1/sites/{id}/probes", tag = "devices",
    summary = "List where a site's network checks can run from",
    description = "The central server, then the relays of the site, each with whether it is \
connected and which one a new check of a device uses when it names none (`default`). A check's \
`probe` is `central` or the relay's `relay_id`.\n\n\
**Who**: any role on the site. **Area**: `servers`.",
    params(("id" = uuid::Uuid, Path, description = "The site")),
    responses(
        (status = 200, description = "The probes", body = devices::ProbeList, example = json!({
            "probes": [
                {"probe": "central", "relay_id": null, "key": "central", "name": null,
                 "online": true, "default": false},
                {"probe": "relay", "relay_id": ex::RELAY, "key": "relay-paris", "name": "Paris relay",
                 "online": true, "default": true}
            ]})),
        (status = 404, description = "No such site, or outside the key's reach: `not_found`",
            body = ErrorBody),
    ))]
fn site_probes() {}
