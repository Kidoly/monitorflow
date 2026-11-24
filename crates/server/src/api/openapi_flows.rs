//! Traffic flows in the OpenAPI document (ADR-0184): merged into it by
//! [`super::openapi::build`].

#![allow(dead_code)] // The functions exist for their `#[utoipa::path]`.

use serde_json::json;
use utoipa::OpenApi;

use super::flows;
use super::openapi::{Common, ErrorBody};
use crate::flows::query;

#[derive(OpenApi)]
#[openapi(
    paths(device_flow_exporter, set_device_flow_exporter, delete_device_flow_exporter, device_flows),
    components(schemas(ErrorBody, flows::ExporterView, flows::ExporterRequest, query::FlowView,
        query::DimensionView, query::KeyView, query::ChartView, query::ChartKey, query::ChartPoint)),
    modifiers(&Common),
)]
pub(super) struct Doc;

fn exporter() -> serde_json::Value {
    json!({"probe": "0b3f2c4e-1111-2222-3333-444455556666", "relay_name": "relay-paris",
           "source_address": null, "address": "192.0.2.1", "created_at": "2026-12-26T09:00:00Z",
           "updated_at": "2026-12-26T09:00:00Z", "last_flow_at": "2026-12-26T10:41:00Z"})
}

#[utoipa::path(get, path = "/api/v1/devices/{id}/flow-exporter", tag = "devices",
    summary = "Read who receives a device's traffic flows",
    description = "The probe (the central server or a relay of the device's site) that receives the \
NetFlow, sFlow or IPFIX the device sends, the address they must come from, and when the newest minute \
arrived.\n\n**Who**: any role on the device's site. **Area**: `servers`.",
    params(("id" = uuid::Uuid, Path, description = "The device")),
    responses(
        (status = 200, description = "Who receives them", body = flows::ExporterView, example = json!(exporter())),
        (status = 404, description = "No such device, its flows are not received, or outside the key's \
reach: `not_found`", body = ErrorBody),
    ))]
fn device_flow_exporter() {}

#[utoipa::path(put, path = "/api/v1/devices/{id}/flow-exporter", tag = "devices",
    summary = "Receive a device's traffic flows",
    description = "Its probe opens its flow ports (UDP 2055, 6343 and 4739 by default, each taking \
NetFlow v5 and v9, IPFIX and sFlow v5) within seconds and takes datagrams from `source_address`, else \
the device's address. Point the device's exporter at the probe's address. Sums of each minute are kept \
48 hours, then hourly sums 31 days.\n\n**Who**: operator or above on the device's site. **Area**: \
`servers`. Audited as `device.flow_exporter_update`.",
    params(("id" = uuid::Uuid, Path, description = "The device")),
    request_body(content = flows::ExporterRequest, example = json!({"probe": "central", "source_address": "192.0.2.254"})),
    responses(
        (status = 200, description = "Who receives them", body = flows::ExporterView, example = json!(exporter())),
        (status = 400, description = "A `probe` or `source_address` that is not valid, or a device \
without an address and no `source_address`: `invalid`", body = ErrorBody),
        (status = 403, description = "The key may only read the site: `forbidden`; or outside the \
key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such device, or outside the key's reach: `not_found`", body = ErrorBody),
    ))]
fn set_device_flow_exporter() {}

#[utoipa::path(delete, path = "/api/v1/devices/{id}/flow-exporter", tag = "devices",
    summary = "Stop receiving a device's traffic flows",
    description = "Its probe drops its datagrams from the next configuration, and closes its ports \
when no device is left. Flows already stored stay until they expire.\n\n**Who**: operator or above on \
the device's site. **Area**: `servers`. Audited as `device.flow_exporter_delete`.",
    params(("id" = uuid::Uuid, Path, description = "The device")),
    responses(
        (status = 204, description = "Stopped"),
        (status = 404, description = "No such device, its flows are not received, or outside the key's \
reach: `not_found`", body = ErrorBody),
    ))]
fn delete_device_flow_exporter() {}

#[utoipa::path(get, path = "/api/v1/devices/{id}/flows", tag = "devices",
    summary = "Read a device's traffic flows over a period",
    description = "The largest source and destination addresses, applications (protocol and service \
port, named when known), protocols, and input and output interfaces of the exporter (ifIndex, named \
from its SNMP interfaces check), with the rest of each; and a chart of the largest keys of one \
dimension over time, in bits per second. With `address`, every view counts only the traffic that \
address sent or received, from the largest conversations kept.\n\nEach address of the largest also \
writes the series `monitorflow_flow_address_bits_per_second` (instance: the address), and the device \
`monitorflow_flow_bits_per_second`: thresholds on them alert on an address above a rate for some \
minutes.\n\n**Who**: any role on the device's site. **Area**: `servers`.",
    params(("id" = uuid::Uuid, Path, description = "The device"), flows::FlowParams),
    responses(
        (status = 200, description = "The views", body = query::FlowView, example = json!({
            "from": "2026-12-26T09:00:00Z", "to": "2026-12-26T10:00:00Z", "step_s": 60, "address": null,
            "bytes": 4500000000_i64, "packets": 3200000, "bits_per_second": 10000000.0,
            "dimensions": [{"dimension": "app", "other_bytes": 500000000, "keys": [
                {"key": "tcp/443", "label": "HTTPS", "bytes": 4000000000_i64, "packets": 2900000,
                 "bits_per_second": 8888888.9}]}],
            "chart": {"by": "src", "keys": [{"key": "192.0.2.15", "label": null}],
                      "points": [{"at": "2026-12-26T09:00:00Z", "values": [6000000.0], "other": 2500000.0}]}})),
        (status = 400, description = "A period, `address`, `by` or `limit` that is not valid: `invalid`",
            body = ErrorBody),
        (status = 404, description = "No such device, or outside the key's reach: `not_found`", body = ErrorBody),
    ))]
fn device_flows() {}
