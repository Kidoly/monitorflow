//! VMware and Hyper-V in the OpenAPI document (ADR-0156): merged into it
//! by [`super::openapi::build`].

#![allow(dead_code)] // The functions exist for their `#[utoipa::path]`.

use serde_json::json;
use utoipa::OpenApi;

use super::devices;
use super::openapi::{Common, ErrorBody};
use super::openapi_core::ex;
use super::virtualization;
use crate::vmware;

#[derive(OpenApi)]
#[openapi(
    paths(server_virtualization, link_virtual_machine, install_agent, watch_hyperv, test_vmware, device_vmware, set_device_vmware,
          delete_device_vmware),
    components(schemas(ErrorBody, crate::vmware::Threshold, devices::TestedCertificate)),
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

#[utoipa::path(post, path = "/api/v1/servers/{id}/hyperv", tag = "virtualization",
    summary = "Watch Hyper-V on a Windows server",
    description = "Adds the `hyperv` check, run by the server's agent every 5 minutes, with its alerts: \
a warning when Hyper-V cannot be read for 15 minutes, for a checkpoint older than 7 days \
(`monitorflow_hyperv_vm_snapshot_age_seconds`), and on replication health (`monitorflow_hyperv_vm_replication_health`, \
warning at 1, critical at 2). Thresholds the server has already stay. The agent's account must be in the \
host's Hyper-V Administrators group, which only a person on the host can do: `Add-LocalGroupMember -Group \
\"Hyper-V Administrators\" -Member \"NT SERVICE\\MonitorFlowAgent\"`, then restart the agent. Until then the \
source reads `denied` with that sentence.\n\n**Who**: operator or above on the server's site. **Area**: \
`servers`. Audited as `server.hyperv_watch`.",
    params(("id" = uuid::Uuid, Path, description = "A Windows server with the agent")),
    responses(
        (status = 200, description = "Watched", body = virtualization::HypervWatch,
            example = json!({"added": true, "check": "hyperv"})),
        (status = 400, description = "Not a Windows server with the agent: `invalid`", body = ErrorBody),
        (status = 403, description = "The key may only read the site: `forbidden`", body = ErrorBody),
        (status = 404, description = "No such server, or outside the key's reach: `not_found`", body = ErrorBody),
    ))]
fn watch_hyperv() {}

fn tested() -> serde_json::Value {
    json!({"answers": true, "error": null, "product": "VMware vCenter Server 8.0.2 build-22385739",
           "version": "8.0.2", "api_type": "VirtualCenter",
           "certificate": {"fingerprint": "3F:9A:1C:44:0B:7E:D2:58:91:AA:6C:03:E5:2D:7B:19:C8:40:FE:61:2A:9D:B3:07:5E:8C:11:D4:6A:F0:27:B9",
                           "trusted": true, "by_roots": false, "subject": "vcenter.example.test",
                           "not_after": "2028-03-01T12:00:00Z"},
           "login": true, "objects": {"vm": 412, "host": 12, "cluster": 2, "datastore": 9}, "probe": "central"})
}

#[utoipa::path(post, path = "/api/v1/devices/vmware/test", tag = "virtualization",
    summary = "Test a vCenter or ESXi host",
    description = "Connects once from the probe that would check the address (or the one named) and says \
what answers, and the certificate it has: its SHA-256 fingerprint, subject and expiry, and whether the \
probe's roots trust it. It logs in, and counts what the account sees, only when the certificate is \
trusted, by the roots or by the `fingerprint` given: the password is never sent to a certificate that is \
not. For a self-signed vCenter, compare the fingerprint with the one the vCenter shows, then test again \
with it, and send it with the credentials (`POST /api/devices` or `PUT /api/devices/{id}/vmware`). Nothing \
is stored.\n\n**Who**: operator or above on the site. **Area**: `servers`.",
    request_body(content = devices::VmwareTestRequest, example = json!({
        "site_id": ex::SITE, "address": "vcenter.example.test",
        "vmware": {"user": "monitor@vsphere.local", "password": "a password",
                   "fingerprint": "3F:9A:1C:44:0B:7E:D2:58:91:AA:6C:03:E5:2D:7B:19:C8:40:FE:61:2A:9D:B3:07:5E:8C:11:D4:6A:F0:27:B9"}})),
    responses(
        (status = 200, description = "What answered; also when nothing did: `answers` and `error` say so",
            body = devices::VmwareTested, example = json!(tested())),
        (status = 400, description = "An address, port or fingerprint that is not valid: `invalid`", body = ErrorBody),
        (status = 409, description = "The relay is held by another central server now: `conflict`", body = ErrorBody),
    ))]
fn test_vmware() {}

fn credentials_view() -> serde_json::Value {
    json!({"port": 443, "user": "monitor@vsphere.local",
           "fingerprint": "3F:9A:1C:44:0B:7E:D2:58:91:AA:6C:03:E5:2D:7B:19:C8:40:FE:61:2A:9D:B3:07:5E:8C:11:D4:6A:F0:27:B9",
           "password_set": true, "updated_at": "2026-11-18T10:20:00Z"})
}

#[utoipa::path(get, path = "/api/v1/devices/{id}/vmware", tag = "virtualization",
    summary = "Read a device's vCenter credentials, without the password",
    description = "**Who**: any role on the device's site. **Area**: `servers`.",
    params(("id" = uuid::Uuid, Path, description = "The device")),
    responses(
        (status = 200, description = "The credentials", body = vmware::View, example = json!(credentials_view())),
        (status = 404, description = "No such device, none set, or outside the key's reach: `not_found`",
            body = ErrorBody),
    ))]
fn device_vmware() {}

#[utoipa::path(put, path = "/api/v1/devices/{id}/vmware", tag = "virtualization",
    summary = "Set a device's vCenter credentials",
    description = "Replaces them; what is left out is kept, so a new certificate's `fingerprint` is \
confirmed without typing the password again, and `\"fingerprint\": \"\"` goes back to the roots. A device \
without a `vmware` check gets one, from its site's default probe, with the alerts that come with it: the \
API stops answering, a host disconnected, a datastore under 10% free, a snapshot older than 7 days, a red \
alarm on a host or datastore, a hardware sensor not green. Its probe uses them within seconds. The \
account needs only vCenter's Read-only role.\n\n**Who**: operator or above on the device's site. **Area**: \
`servers`. Audited as `device.vmware_update`, without the password.",
    params(("id" = uuid::Uuid, Path, description = "The device")),
    request_body(content = vmware::CredentialsRequest, example = json!({"user": "monitor@vsphere.local", "password": "a password"})),
    responses(
        (status = 200, description = "The credentials, without the password", body = vmware::View,
            example = json!(credentials_view())),
        (status = 400, description = "Credentials that are not valid: `invalid`", body = ErrorBody),
        (status = 403, description = "The key may only read the site: `forbidden`", body = ErrorBody),
        (status = 404, description = "No such device, or outside the key's reach: `not_found`", body = ErrorBody),
    ))]
fn set_device_vmware() {}

#[utoipa::path(delete, path = "/api/v1/devices/{id}/vmware", tag = "virtualization",
    summary = "Remove a device's vCenter credentials",
    description = "Its `vmware` check stops being sent to its probe until credentials are set again.\n\n\
**Who**: operator or above on the device's site. **Area**: `servers`. Audited as `device.vmware_delete`.",
    params(("id" = uuid::Uuid, Path, description = "The device")),
    responses(
        (status = 204, description = "Removed"),
        (status = 404, description = "No such device, none set, or outside the key's reach: `not_found`",
            body = ErrorBody),
    ))]
fn delete_device_vmware() {}

#[utoipa::path(put, path = "/api/v1/virtual-objects/{id}/server", tag = "virtualization",
    summary = "Tie a virtual machine to a server by hand",
    description = "Says which server or device of the source's customer a virtual machine is, or that it is \
none (`\"server_id\": null`); matching then never changes it. `\"automatic\": true` hands it back to \
matching, which runs at once.\n\n**Who**: operator or above on the source's site, for a server they can read. \
**Area**: `servers`. Audited as `virt.link`, with the server before and after.",
    params(("id" = uuid::Uuid, Path, description = "The virtual machine, from `objects` or `guest_of`")),
    request_body(content = virtualization::LinkRequest, example = json!({"server_id": ex::SERVER})),
    responses(
        (status = 200, description = "The virtual machine", body = virtualization::ObjectView,
            example = json!(ex::merge(vm(), json!({"matched_by": "person", "link_fixed": true})))),
        (status = 400, description = "Not a virtual machine, or a server of another customer: `invalid`", body = ErrorBody),
        (status = 403, description = "The key may only read the site: `forbidden`", body = ErrorBody),
        (status = 404, description = "No such virtual machine, or outside the key's reach: `not_found`", body = ErrorBody),
    ))]
fn link_virtual_machine() {}

#[utoipa::path(post, path = "/api/v1/virtual-objects/{id}/install-agent", tag = "virtualization",
    summary = "Install the agent on a virtual machine",
    description = "Adds a server waiting for its agent in the source's site, named after the virtual \
machine's guest host name (else its name), a Windows server when its guest system says so, and ties the \
virtual machine to it. Its page then shows the install command; the agent takes it over when it enrolls \
with that name (ADR-0146).\n\n**Who**: administrator of the source's customer, as for install keys. **Area**: `servers`. Audited as \
`virt.install_agent`.",
    params(("id" = uuid::Uuid, Path, description = "The virtual machine")),
    responses(
        (status = 201, description = "The server waiting for its agent", body = super::servers::ServerView,
            example = json!(ex::merge(ex::server(), json!({"awaiting_agent": true, "status": "waiting"})))),
        (status = 400, description = "Not a virtual machine: `invalid`", body = ErrorBody),
        (status = 403, description = "Not an administrator of the site: `forbidden`", body = ErrorBody),
        (status = 404, description = "No such virtual machine, or outside the key's reach: `not_found`", body = ErrorBody),
        (status = 409, description = "It is already a server: `conflict`", body = ErrorBody),
    ))]
fn install_agent() {}
