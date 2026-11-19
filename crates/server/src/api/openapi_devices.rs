//! Devices and probes in the OpenAPI document (ADR-0148): merged into it
//! by [`super::openapi::build`].

#![allow(dead_code)] // The functions exist for their `#[utoipa::path]`.

use serde_json::json;
use utoipa::OpenApi;

use super::devices;
use super::openapi::{Common, ErrorBody};
use super::openapi_core::ex;
use super::servers;
use crate::snmp;

#[derive(OpenApi)]
#[openapi(
    paths(create_device, update_device, site_probes, device_templates, detect_device, device_snmp,
          set_device_snmp, delete_device_snmp),
    components(schemas(ErrorBody, devices::Probe, crate::snmp::Template, crate::snmp::Threshold)),
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
With `snmp` credentials and a `template` (ADR-0149), it also gets the SNMP checks and thresholds of that kind \
of device, run from the same probe; `POST /api/v1/devices/detect` suggests the template. The credentials are \
stored sealed and never returned.\n\n\
With `vmware` credentials (ADR-0156), a vCenter or ESXi host also gets its `vmware` check every 5 minutes \
from the same probe, and its alerts; test the connection and confirm its certificate first with \
`POST /api/v1/devices/vmware/test`.\n\n\
**Who**: operator or above on the site. **Area**: `servers`. Audited as `device.create`.",
    request_body(content = devices::DeviceRequest, examples(
        ("ping" = (summary = "A switch pinged from the site's relay",
            value = json!({"site_id": ex::SITE, "name": "Core switch", "address": "192.0.2.10"}))),
        ("central" = (summary = "A website's server pinged from the central server",
            value = json!({"site_id": ex::SITE, "name": "Shop front", "address": "shop.example.com",
                           "probe": "central"}))),
        ("vmware" = (summary = "A vCenter, read every 5 minutes with a Read-only account (ADR-0156)",
            value = json!({"site_id": ex::SITE, "name": "vcenter-1", "address": "vcenter.example.test",
                           "vmware": {"user": "monitor@vsphere.local", "password": "a password"}}))),
        ("snmp" = (summary = "A switch over SNMP v3, with the checks and alerts of its kind",
            value = json!({"site_id": ex::SITE, "name": "Core switch", "address": "192.0.2.10",
                           "template": "switch", "snmp": {"version": "v3", "user": "monitor",
                           "auth_protocol": "sha256", "auth_password": "an auth password",
                           "priv_protocol": "aes128", "priv_password": "a privacy password"}}))),
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

#[utoipa::path(get, path = "/api/v1/devices/templates", tag = "devices",
    summary = "List the kinds of SNMP devices",
    description = "Each template names the groups of SNMP objects its checks poll (`system`, `interfaces`, \
`host`, `ups`, `temperature`, `printer`) and the thresholds a device of that kind gets. Every template alerts \
when SNMP stops answering. A check of group `g` is named after it (`snmp` for `system`, `snmp_if`, \
`snmp_host`, `snmp_ups`, `snmp_temp`, `snmp_printer`), and so are its metrics, like \
`monitorflow_snmp_if_in_bytes_per_second`.\n\n\
**Who**: any role. **Area**: `servers`.",
    responses(
        (status = 200, description = "The templates", body = devices::TemplateList, example = json!({
            "templates": [{"key": "ups", "groups": ["system", "ups"], "thresholds": [
                {"metric": "monitorflow_snmp_up", "comparison": "<", "warning": null, "critical": 1.0, "duration_s": 120},
                {"metric": "monitorflow_snmp_ups_on_battery", "comparison": ">=", "warning": null, "critical": 1.0, "duration_s": 0}
            ]}]})),
    ))]
fn device_templates() {}

#[utoipa::path(post, path = "/api/v1/devices/detect", tag = "devices",
    summary = "See what an address answers over SNMP",
    description = "Runs a detection once, from the probe that would check the address (the site's \
default, or `probe`): the system's description, name and object ID, which groups answer, and the template \
that fits. Nothing is stored. A device that does not answer, or refuses the credentials, gives \
`answers: false` and the reason in `error`. A relay that this central server does not hold the stream of \
gives 409; try again.\n\n\
**Who**: operator or above on the site, since it sends credentials to the probe. **Area**: `servers`.",
    request_body(content = devices::DetectRequest, example = json!({
        "site_id": ex::SITE, "address": "192.0.2.10", "snmp": {"version": "v2c", "community": "a community"}})),
    responses(
        (status = 200, description = "What it answered", body = devices::Detected, example = json!({
            "answers": true, "error": null, "description": "24-port gigabit switch", "name": "core-switch",
            "object_id": "1.3.6.1.4.1.8072.3.2.10", "groups": ["interfaces", "temperature"],
            "template": "switch", "probe": "relay-paris"})),
        (status = 400, description = "An address, credentials or `probe` that are not valid: `invalid`",
            body = ErrorBody),
        (status = 403, description = "The key may only read the site: `forbidden`; or outside the \
key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such site, or outside the key's reach: `not_found`", body = ErrorBody),
        (status = 409, description = "The relay is not connected to this central server now: `conflict`",
            body = ErrorBody),
    ))]
fn detect_device() {}

fn credentials_view() -> serde_json::Value {
    json!({"version": "v3", "port": 161, "user": "monitor", "auth_protocol": "sha256", "priv_protocol": "aes128",
           "community_set": false, "auth_password_set": true, "priv_password_set": true,
           "updated_at": "2026-11-18T10:20:00Z"})
}

#[utoipa::path(get, path = "/api/v1/devices/{id}/snmp", tag = "devices",
    summary = "Read a device's SNMP credentials, without secrets",
    description = "The version, port, user and protocols, and which secrets are set; never the community \
or a password.\n\n**Who**: any role on the device's site. **Area**: `servers`.",
    params(("id" = uuid::Uuid, Path, description = "The device")),
    responses(
        (status = 200, description = "The credentials", body = snmp::View, example = json!(credentials_view())),
        (status = 404, description = "No such device, none set, or outside the key's reach: `not_found`",
            body = ErrorBody),
    ))]
fn device_snmp() {}

#[utoipa::path(put, path = "/api/v1/devices/{id}/snmp", tag = "devices",
    summary = "Set a device's SNMP credentials",
    description = "Replaces them. A secret left out keeps the stored one while the version stays, so the \
port or the user change without typing the passwords again. Its probes use them within seconds. v3 \
passwords have at least 8 characters; privacy needs authentication.\n\n\
**Who**: operator or above on the device's site. **Area**: `servers`. Audited as `device.snmp_update`, \
without secrets.",
    params(("id" = uuid::Uuid, Path, description = "The device")),
    request_body(content = snmp::CredentialsRequest, example = json!({"version": "v2c", "community": "a community"})),
    responses(
        (status = 200, description = "The credentials, without secrets", body = snmp::View,
            example = json!(credentials_view())),
        (status = 400, description = "Credentials that are not valid: `invalid`", body = ErrorBody),
        (status = 403, description = "The key may only read the site: `forbidden`; or outside the \
key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such device, or outside the key's reach: `not_found`", body = ErrorBody),
    ))]
fn set_device_snmp() {}

#[utoipa::path(delete, path = "/api/v1/devices/{id}/snmp", tag = "devices",
    summary = "Remove a device's SNMP credentials",
    description = "Its SNMP checks stop being sent to its probes until credentials are set again.\n\n\
**Who**: operator or above on the device's site. **Area**: `servers`. Audited as `device.snmp_delete`.",
    params(("id" = uuid::Uuid, Path, description = "The device")),
    responses(
        (status = 204, description = "Removed"),
        (status = 404, description = "No such device, none set, or outside the key's reach: `not_found`",
            body = ErrorBody),
    ))]
fn delete_device_snmp() {}
