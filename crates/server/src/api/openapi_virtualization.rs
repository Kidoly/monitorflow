//! VMware and Hyper-V in the OpenAPI document (ADR-0156): merged into it
//! by [`super::openapi::build`].

#![allow(dead_code)] // The functions exist for their `#[utoipa::path]`.

use serde_json::json;
use utoipa::OpenApi;

use super::openapi::{Common, ErrorBody};
use super::openapi_core::ex;
use super::virtualization;

#[derive(OpenApi)]
#[openapi(
    paths(server_virtualization),
    components(schemas(ErrorBody)),
    modifiers(&Common),
    tags((name = "virtualization", description = "VMware and Hyper-V: the hosts, clusters, virtual \
machines and datastores of a vCenter or ESXi host (a device with a `vmware` check, run from a probe) or of \
a Hyper-V host (a Windows server with a `hyperv` check), kept from what their checks last reported. Each \
virtual machine is matched to the server or device of the same customer it is, by its hardware ID, a \
network card, an address or its name. Area `servers`."))
)]
pub(super) struct Doc;

/// A virtual machine as the API shows it.
pub(super) fn vm() -> serde_json::Value {
    json!({
        "id": "8a1f3c52-6b7d-4e9f-a0b1-c2d3e4f5a6b7", "kind": "vm", "name": "web-1", "key": "vm-42",
        "details": {"uuid": "4237aa10-1c2b-4d3e-8f4a-5b6c7d8e9f01", "host": "esx-02", "power_state": "poweredOn",
                    "guest_hostname": "web-1.example.test", "guest_os": "Debian GNU/Linux 12 (64-bit)",
                    "addresses": "192.0.2.21", "macs": "00:50:56:aa:bb:01"},
        "metrics": {"powered_on": 1.0, "cpu_count": 2.0, "cpu_usage_ratio": 0.12, "memory_bytes": 4294967296.0,
                    "memory_usage_ratio": 0.4, "storage_used_bytes": 21474836480.0, "snapshots": 1.0,
                    "snapshot_age_seconds": 864000.0, "status": 0.0, "alarms_critical": 0.0, "alarms_warning": 0.0},
        "collected_at": "2026-11-18T10:20:00Z", "seen_at": "2026-11-18T10:20:03Z", "stale": false,
        "server": {"id": ex::SERVER, "hostname": "web-1", "kind": "agent", "awaiting_agent": false},
        "matched_by": "uuid", "link_fixed": false
    })
}

#[utoipa::path(get, path = "/api/v1/servers/{id}/virtualization", tag = "virtualization",
    summary = "Read a server's virtualization",
    description = "For a vCenter, ESXi or Hyper-V source, whether its last collection worked and its hosts, \
clusters, virtual machines and datastores, with their numbers of that collection. For any server, the \
virtual machines it is (`guest_of`), from sources the caller can read. An object the source stopped \
listing is `stale` after 15 minutes and removed after a day. A matched server the caller cannot read is \
shown as `null`.\n\n**Who**: any role on the server's site. **Area**: `servers`.",
    params(("id" = uuid::Uuid, Path, description = "The server or device")),
    responses(
        (status = 200, description = "Its virtualization", body = virtualization::Virtualization,
            example = json!({
                "source": {"up": true, "reason": null, "error": null, "seen_at": "2026-11-18T10:20:03Z",
                           "details": {"product": "VMware vCenter Server 8.0.2", "version": "8.0.2", "api_type": "VirtualCenter"}},
                "objects": [vm()], "guest_of": []})),
        (status = 404, description = "No such server, or outside the key's reach: `not_found`", body = ErrorBody),
    ))]
fn server_virtualization() {}
