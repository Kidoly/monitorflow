//! Network scans in the OpenAPI document (ADR-0154): merged into it by
//! [`super::openapi::build`].

#![allow(dead_code)] // The functions exist for their `#[utoipa::path]`.

use serde_json::json;
use utoipa::OpenApi;

use super::network_scans as scans;
use super::openapi::{Common, ErrorBody};
use super::openapi_core::ex;

#[derive(OpenApi)]
#[openapi(
    paths(list_scans, create_scan, get_scan, update_scan, delete_scan, run_scan, scan_runs,
          scan_results, add_from_scan, dismiss_address, restore_address),
    components(schemas(ErrorBody, scans::SnmpView, scans::RunView, scans::HostView,
                       scans::AddDevice, scans::Added, scans::Skipped,
                       crate::netscan::proposals::Proposal, crate::netscan::proposals::MonitoredBy,
                       crate::netscan::proposals::ProposedCheck)),
    modifiers(&Common),
    tags((name = "network-scans", description = "Network scans: a range of a site's IP addresses \
tried from the central server or a relay of the site (ping, a few TCP ports, SNMP with the credentials \
the site's devices have and those given to the scan, the MAC address on the probe's own link, the name \
from reverse DNS), at most 50 probes a second and never outside the range. Each run keeps what answered; \
the results propose for each address: install the agent (SSH or RDP open), an SNMP device with its \
template, or a device with its ping and ports, and recognise what the site monitors already. Nothing is \
added until `POST /api/v1/network-scans/{id}/add`. A scan can run every hour, day or week and say what is \
new or gone, with an optional notification. Viewers of the site read scans; operators of the site change \
them. Area `servers`."))
)]
pub(super) struct Doc;

const SCAN: &str = "6a7c1e4e-3c0e-4f9a-9b3e-2f6d8c1a5b70";
const RUN: &str = "0b9e2c55-7d41-4a8e-8f61-3d2a9c7e1f04";

fn run() -> serde_json::Value {
    json!({
        "id": RUN, "state": "done", "trigger": "person", "queued_at": "2026-11-18T10:00:00Z",
        "started_at": "2026-11-18T10:00:02Z", "finished_at": "2026-11-18T10:02:10Z",
        "addresses": 254, "scanned": 254, "answered": 3, "compared": true, "new": 1,
        "gone": ["192.0.2.40"], "dropped": 0, "ping_error": null, "error": null
    })
}

fn scan() -> serde_json::Value {
    json!({
        "id": SCAN, "site_id": ex::SITE, "site_name": "paris", "name": "Office network",
        "targets": ["192.0.2.0/24"], "addresses": 254,
        "ports": [22, 23, 80, 135, 139, 443, 445, 3389, 5985, 8080, 8443],
        "probe": "relay", "probe_relay_id": ex::RELAY, "probe_key": "relay-paris",
        "every": "day", "notify": false, "snmp": null,
        "created_at": "2026-11-18T09:58:00Z", "updated_at": "2026-11-18T09:58:00Z",
        "last_run": run()
    })
}

const FORBIDDEN: &str = "The key may only read the site: `forbidden`; or outside the key's areas: \
`api_key_scope`";

#[utoipa::path(get, path = "/api/v1/network-scans", tag = "network-scans",
    summary = "List network scans",
    description = "The scans of the sites the caller reads, by site and name, each with its latest run.\n\n\
**Who**: any role on the site. **Area**: `servers`.",
    params(scans::ListQuery),
    responses(
        (status = 200, description = "The scans", body = scans::ScanList, example = json!({"scans": [scan()]})),
    ))]
fn list_scans() {}

#[utoipa::path(post, path = "/api/v1/network-scans", tag = "network-scans",
    summary = "Create a network scan",
    description = "Creates a scan of a site's addresses and queues its first run, unless `run` is false. \
It runs from the relay of its site, or the central server when the site has none, unless `probe` says \
otherwise. A /24 takes about two minutes. Read what it found with \
`GET /api/v1/network-scans/{id}/results`.\n\n\
**Who**: operator or above on the site. **Area**: `servers`. Audited as `network_scan.create`, and \
`network_scan.run` when it is queued.",
    request_body(content = scans::ScanRequest, examples(
        ("office" = (summary = "An office network, scanned every day",
            value = json!({"site_id": ex::SITE, "name": "Office network", "targets": ["192.0.2.0/24"],
                           "every": "day"}))),
        ("snmp" = (summary = "A few ranges from the central server, with SNMP v2c",
            value = json!({"site_id": ex::SITE, "name": "Server room", "probe": "central",
                           "targets": ["198.51.100.10-40", "198.51.100.200"], "ports": [22, 443, 3389],
                           "snmp": {"version": "v2c", "community": "a community"}}))),
    )),
    responses(
        (status = 201, description = "The scan", body = scans::ScanView,
            example = json!(ex::merge(scan(), json!({"last_run": ex::merge(run(), json!({
                "state": "queued", "started_at": null, "finished_at": null, "scanned": 0, "answered": 0,
                "compared": false, "new": 0, "gone": []}))})))),
        (status = 400, description = "Targets that are not IP addresses, blocks or ranges, more than \
1,024 addresses, a refused address (loopback, multicast, link-local), ports, `every`, credentials or a \
`probe` that are not valid: `invalid`", body = ErrorBody),
        (status = 403, description = FORBIDDEN, body = ErrorBody),
        (status = 404, description = "No such site, or outside the key's reach: `not_found`", body = ErrorBody),
        (status = 409, description = "The site has 50 scans already: `conflict`", body = ErrorBody),
    ))]
fn create_scan() {}

#[utoipa::path(get, path = "/api/v1/network-scans/{id}", tag = "network-scans",
    summary = "Read a network scan",
    description = "The scan, its own SNMP credentials without secrets, and its latest run.\n\n\
**Who**: any role on the site. **Area**: `servers`.",
    params(("id" = uuid::Uuid, Path, description = "The scan")),
    responses(
        (status = 200, description = "The scan", body = scans::ScanView, example = json!(scan())),
        (status = 404, description = "No such scan, or outside the key's reach: `not_found`", body = ErrorBody),
    ))]
fn get_scan() {}

#[utoipa::path(put, path = "/api/v1/network-scans/{id}", tag = "network-scans",
    summary = "Change a network scan",
    description = "Replaces everything but its site and runs: send the whole scan. `every` null scans \
only when asked. The scan's SNMP credentials stay unless `snmp` or `remove_snmp` is given, and a secret \
left out keeps the stored one while the version stays.\n\n\
**Who**: operator or above on the site. **Area**: `servers`. Audited as `network_scan.update`.",
    params(("id" = uuid::Uuid, Path, description = "The scan")),
    request_body(content = scans::ScanUpdate, example = json!({"name": "Office network",
        "targets": ["192.0.2.0/24", "198.51.100.0/25"], "every": "week", "notify": true})),
    responses(
        (status = 200, description = "The scan", body = scans::ScanView,
            example = json!(ex::merge(scan(), json!({"every": "week", "notify": true})))),
        (status = 400, description = "As for creating one: `invalid`", body = ErrorBody),
        (status = 403, description = FORBIDDEN, body = ErrorBody),
        (status = 404, description = "No such scan, or outside the key's reach: `not_found`", body = ErrorBody),
    ))]
fn update_scan() {}

#[utoipa::path(delete, path = "/api/v1/network-scans/{id}", tag = "network-scans",
    summary = "Remove a network scan",
    description = "Removes it with its runs and dismissed addresses; a run in progress stops. Devices it \
added stay.\n\n\
**Who**: operator or above on the site. **Area**: `servers`. Audited as `network_scan.delete`.",
    params(("id" = uuid::Uuid, Path, description = "The scan")),
    responses(
        (status = 204, description = "Removed"),
        (status = 403, description = FORBIDDEN, body = ErrorBody),
        (status = 404, description = "No such scan, or outside the key's reach: `not_found`", body = ErrorBody),
    ))]
fn delete_scan() {}

#[utoipa::path(post, path = "/api/v1/network-scans/{id}/run", tag = "network-scans",
    summary = "Scan now",
    description = "Queues a run. The central server instance that reaches the scan's probe takes it \
within seconds; a relay that is not connected keeps it queued until it is. Follow it with \
`GET /api/v1/network-scans/{id}/results` (`current`).\n\n\
**Who**: operator or above on the site. **Area**: `servers`. Audited as `network_scan.run`.",
    params(("id" = uuid::Uuid, Path, description = "The scan")),
    responses(
        (status = 202, description = "The queued run", body = scans::RunView,
            example = json!(ex::merge(run(), json!({"state": "queued", "started_at": null,
                "finished_at": null, "scanned": 0, "answered": 0, "compared": false, "new": 0, "gone": []})))),
        (status = 403, description = FORBIDDEN, body = ErrorBody),
        (status = 404, description = "No such scan, or outside the key's reach: `not_found`", body = ErrorBody),
        (status = 409, description = "A run waits or runs already: `conflict`", body = ErrorBody),
    ))]
fn run_scan() {}

#[utoipa::path(get, path = "/api/v1/network-scans/{id}/runs", tag = "network-scans",
    summary = "List the runs of a network scan",
    description = "The latest 10, the most recent first, with what each found new or gone and why one \
failed: `interrupted`, `relay_disconnected`, `relay_outdated` (update the relay), `relay_removed`, or the \
probe's own words.\n\n**Who**: any role on the site. **Area**: `servers`.",
    params(("id" = uuid::Uuid, Path, description = "The scan")),
    responses(
        (status = 200, description = "The runs", body = scans::RunList, example = json!({"runs": [run()]})),
        (status = 404, description = "No such scan, or outside the key's reach: `not_found`", body = ErrorBody),
    ))]
fn scan_runs() {}

#[utoipa::path(get, path = "/api/v1/network-scans/{id}/results", tag = "network-scans",
    summary = "Read what a network scan found and proposes",
    description = "The addresses that answered in the latest finished run, each with what it answered and \
the proposal, computed now from the site's inventory: `agent` (install the agent: SSH or RDP is open, \
unless SNMP says it is a switch, router, firewall, printer or UPS), `snmp_device` (with the template \
detection suggests), `device` (its ping and a check per open port), `monitored` (a device at that \
address, or a server whose agent reports the MAC address the probe saw or has the host name it answered \
with) or `dismissed`. Text from the network (names, descriptions) is shown as text. `current` is a run \
queued or running now.\n\n**Who**: any role on the site. **Area**: `servers`.",
    params(("id" = uuid::Uuid, Path, description = "The scan")),
    responses(
        (status = 200, description = "What it found", body = scans::Results, example = json!({
            "run": run(), "current": null, "hosts": [
                {"address": "192.0.2.10", "ping": true, "rtt_seconds": 0.0004, "ports": [22, 80],
                 "name": "web-1.lan", "mac": "02:00:5e:00:53:10", "snmp": false, "snmp_description": null,
                 "snmp_name": null, "snmp_object_id": null, "snmp_groups": [], "new": true,
                 "proposal": {"action": "agent", "os": "linux", "monitored_by": null, "name": "web-1.lan",
                              "template": null, "checks": [
                                  {"kind": "ping", "name": "ping", "port": null},
                                  {"kind": "port", "name": "port_22", "port": 22},
                                  {"kind": "port", "name": "port_80", "port": 80}]}},
                {"address": "192.0.2.20", "ping": true, "rtt_seconds": 0.0011, "ports": [80],
                 "name": null, "mac": null, "snmp": true, "snmp_description": "UPS network card",
                 "snmp_name": "ups-1", "snmp_object_id": "1.3.6.1.4.1.8072.3.2.10", "snmp_groups": ["ups"],
                 "new": false,
                 "proposal": {"action": "snmp_device", "os": null, "monitored_by": null, "name": "ups-1",
                              "template": "ups", "checks": [
                                  {"kind": "ping", "name": "ping", "port": null},
                                  {"kind": "snmp", "name": "snmp", "port": null},
                                  {"kind": "snmp", "name": "snmp_ups", "port": null}]}},
                {"address": "192.0.2.30", "ping": true, "rtt_seconds": 0.0007, "ports": [445],
                 "name": "files.lan", "mac": null, "snmp": false, "snmp_description": null,
                 "snmp_name": null, "snmp_object_id": null, "snmp_groups": [], "new": false,
                 "proposal": {"action": "monitored", "os": null, "monitored_by": {"server_id": ex::SERVER_2,
                              "name": "files", "matched_by": "name"}, "name": "files.lan", "template": null,
                              "checks": []}}
            ]})),
        (status = 404, description = "No such scan, or outside the key's reach: `not_found`", body = ErrorBody),
    ))]
fn scan_results() {}

#[utoipa::path(post, path = "/api/v1/network-scans/{id}/add", tag = "network-scans",
    summary = "Add devices a network scan proposes",
    description = "Adds devices from the latest finished run, as `POST /api/v1/devices` would, checked \
from the scan's probe: `all` adds every address proposed as a device or an SNMP device; `devices` adds \
the ones listed, any that answered and is not monitored, with another name or template if given. A \
device gets a ping when the address answered ping; an SNMP device gets the credentials that answered and \
the checks and thresholds of its template; any other a port check per open port, critical when it stays \
closed for 2 minutes. Addresses proposed for the agent are added only when listed. What could not be \
added is in `skipped`, with why.\n\n\
**Who**: operator or above on the site. **Area**: `servers`. Audited as `device.create` per device and \
`network_scan.add`.",
    params(("id" = uuid::Uuid, Path, description = "The scan")),
    request_body(content = scans::AddRequest, examples(
        ("all" = (summary = "Everything proposed as a device", value = json!({"all": true}))),
        ("some" = (summary = "Two addresses, one as a switch",
            value = json!({"devices": [{"address": "192.0.2.20"},
                                       {"address": "192.0.2.21", "name": "Core switch", "template": "switch"}]}))),
    )),
    responses(
        (status = 200, description = "What was added and skipped", body = scans::AddResult, example = json!({
            "added": [{"address": "192.0.2.20", "server_id": ex::SERVER_2, "name": "ups-1", "template": "ups"}],
            "skipped": [{"address": "192.0.2.30", "reason": "monitored"}]})),
        (status = 400, description = "Neither `all` nor `devices`, an address, name or template that is \
not valid: `invalid`", body = ErrorBody),
        (status = 403, description = FORBIDDEN, body = ErrorBody),
        (status = 404, description = "No such scan, or outside the key's reach: `not_found`", body = ErrorBody),
    ))]
fn add_from_scan() {}

#[utoipa::path(post, path = "/api/v1/network-scans/{id}/dismiss", tag = "network-scans",
    summary = "Dismiss an address of a network scan",
    description = "The scan stops proposing it, for good; it is listed as `dismissed` until restored. Only \
an address the scan found can be dismissed.\n\n\
**Who**: operator or above on the site. **Area**: `servers`. Audited as `network_scan.dismiss`.",
    params(("id" = uuid::Uuid, Path, description = "The scan")),
    request_body(content = scans::AddressRequest, example = json!({"address": "192.0.2.30"})),
    responses(
        (status = 204, description = "Dismissed"),
        (status = 400, description = "Not an IP address: `invalid`", body = ErrorBody),
        (status = 403, description = FORBIDDEN, body = ErrorBody),
        (status = 404, description = "No such scan, an address it never found, or outside the key's reach: \
`not_found`", body = ErrorBody),
    ))]
fn dismiss_address() {}

#[utoipa::path(post, path = "/api/v1/network-scans/{id}/restore", tag = "network-scans",
    summary = "Restore a dismissed address",
    description = "The scan proposes it again. Restoring an address that is not dismissed does nothing.\n\n\
**Who**: operator or above on the site. **Area**: `servers`. Audited as `network_scan.restore`.",
    params(("id" = uuid::Uuid, Path, description = "The scan")),
    request_body(content = scans::AddressRequest, example = json!({"address": "192.0.2.30"})),
    responses(
        (status = 204, description = "Restored"),
        (status = 400, description = "Not an IP address: `invalid`", body = ErrorBody),
        (status = 403, description = FORBIDDEN, body = ErrorBody),
        (status = 404, description = "No such scan, or outside the key's reach: `not_found`", body = ErrorBody),
    ))]
fn restore_address() {}
