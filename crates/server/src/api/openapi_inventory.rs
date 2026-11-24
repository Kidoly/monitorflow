//! The hardware and software inventory in the OpenAPI document
//! (ADR-0187): merged into it by [`super::openapi::build`].

#![allow(dead_code)] // The functions exist for their `#[utoipa::path]`.

use serde_json::json;
use utoipa::OpenApi;

use super::inventory;
use super::openapi::{Common, ErrorBody};
use super::openapi_core::ex;
use crate::server_inventory::search::Hit;

#[derive(OpenApi)]
#[openapi(
    paths(server_inventory, inventory_changes, set_warranty, search_inventory),
    components(schemas(ErrorBody, Hit, inventory::InventoryDates, inventory::ItemView,
        inventory::WarrantyView, inventory::AssetFacts)),
    modifiers(&Common),
    tags((name = "inventory", description = "What each server is made of and what it runs (ADR-0187): \
model, serial numbers, processors, memory, disks, network cards, firmware and BIOS; installed programs, \
services, Windows features and licences. Agents send it when they start, when it changes and at least once a \
day; the server keeps what changed for a year. The warranty is typed by people. Area `servers`.\n\n\
**Item kinds and their fields** (all text; a field the machine does not give is absent):\n\
- `system` (key `system`): `manufacturer`, `model`, `family`, `sku`, `version`, `serial`, `board_manufacturer`, \
`board_model`, `board_serial`, `chassis`, `chassis_manufacturer`, `chassis_serial`, `bios_vendor`, \
`bios_version`, `bios_date`, `bios_release`, `os`, `os_version`, `kernel`, `architecture`, `memory_bytes`.\n\
- `processor` (key: socket): `model`, `cores`, `threads`, `max_mhz`, `socket`.\n\
- `memory` (key: slot, Windows only): `bank`, `size_bytes`, `speed_mhz`, `manufacturer`, `part_number`, `serial`.\n\
- `disk` (key: device): `model`, `vendor`, `serial`, `size_bytes`, `firmware`, `transport`, `media` \
(`ssd` or `hdd`).\n\
- `nic` (key: interface): `model`, `manufacturer`, `mac`, `driver`, `speed_mbps`, `pci_id`, `addresses`.\n\
- `software` (key: program name, Windows only; Linux packages are the server's packages): `version`, \
`publisher`, `installed_on`.\n\
- `service` (key: unit or service name): `start` (Linux `enabled`, `disabled`, `masked`; Windows `auto`, \
`manual`, `disabled`, `boot`, `system`), `display_name`.\n\
- `feature` (key: feature name, Windows only): `display_name`.\n\
- `licence` (key: product, Windows only): `description`, `status`, `partial_key` (the last 5 characters \
Windows shows)."))
)]
pub(super) struct Doc;

fn system() -> serde_json::Value {
    json!({"kind": "system", "key": "system", "fields": {
        "manufacturer": "Dell Inc.", "model": "PowerEdge R650", "serial": "7XK2LM3",
        "bios_vendor": "Dell Inc.", "bios_version": "1.10.2", "bios_date": "2023-05-04",
        "os": "Debian GNU/Linux 12 (bookworm)", "kernel": "6.1.0-27-amd64", "architecture": "x86_64",
        "memory_bytes": "68719476736"}})
}

#[utoipa::path(get, path = "/api/v1/servers/{id}/inventory", tag = "inventory",
    summary = "Read a server's inventory",
    description = "The items of the server's latest inventory, by kind then key, with the dates of its \
reports, the number of entries in its package list, the warranty and the facts a PSA asset receives. \
`reported` is `null` until the agent sent an inventory.\n\n**Who**: any role on the server's site. \
**Area**: `servers`.",
    params(("id" = uuid::Uuid, Path, description = "The server")),
    responses(
        (status = 200, description = "Its inventory", body = inventory::ServerInventory,
            example = json!({
                "reported": {"collected_at": "2026-11-23T06:00:00Z", "first_at": "2026-11-01T08:12:00Z",
                             "changed_at": "2026-11-20T06:00:00Z", "checked_at": "2026-11-23T06:00:02Z"},
                "items": [system(),
                    {"kind": "disk", "key": "sda", "fields": {"model": "SAMSUNG MZ7LH480", "serial": "S45NNA0M123456",
                     "size_bytes": "480103981056", "firmware": "HXT7404Q", "transport": "sata", "media": "ssd"}},
                    {"kind": "service", "key": "nginx.service", "fields": {"start": "enabled"}}],
                "package_count": 612,
                "warranty": {"ends_on": "2028-03-31", "note": "ProSupport 5 years", "updated_at": "2026-11-02T09:00:00Z"},
                "asset": {"hostname": "web-1", "manufacturer": "Dell Inc.", "model": "PowerEdge R650", "serial": "7XK2LM3"}
            })),
        (status = 404, description = "No such server, or outside the key's reach: `not_found`", body = ErrorBody),
    ))]
fn server_inventory() {}

#[utoipa::path(get, path = "/api/v1/servers/{id}/inventory/changes", tag = "inventory",
    summary = "List what changed in a server's inventory",
    description = "Items added, removed or changed, newest first, with their fields before and after, and \
package changes (kind `package`: installed, removed, or another version). Kept a year. A server's first \
inventory and first package list are not changes.\n\n\
**Pages**: `limit` changes per page, 1 to 500, 100 by default; pass `next_cursor` as `cursor` for the next \
page, `null` on the last one.\n\n**Who**: any role on the server's site. **Area**: `servers`.",
    params(("id" = uuid::Uuid, Path, description = "The server"), inventory::ChangesParams),
    responses(
        (status = 200, description = "One page of changes", body = inventory::ChangeList,
            example = json!({"changes": [
                {"id": 812, "at": "2026-11-20T06:00:01Z", "kind": "package", "key": "openssl", "change": "changed",
                 "before": {"version": "3.0.14-1~deb12u2", "arch": "amd64"},
                 "after": {"version": "3.0.15-1~deb12u1", "arch": "amd64"}},
                {"id": 811, "at": "2026-11-19T10:00:00Z", "kind": "disk", "key": "sdb", "change": "added",
                 "before": null, "after": {"model": "WDC WD40EFRX", "size_bytes": "4000787030016", "media": "hdd"}}],
                "next_cursor": null})),
        (status = 400, description = "`limit` out of range, a bad `cursor` or an unknown parameter: `invalid`",
            body = ErrorBody),
        (status = 404, description = "No such server, or outside the key's reach: `not_found`", body = ErrorBody),
    ))]
fn inventory_changes() {}

#[utoipa::path(put, path = "/api/v1/servers/{id}/warranty", tag = "inventory",
    summary = "Set a server's warranty",
    description = "Sets the warranty's last day and a note, as people know them: makers give warranties only \
through services that need a paid or registered key, so MonitorFlow does not look them up. Both `null` \
removes it. Audited as `server.warranty`. Safe to repeat.\n\n\
**Who**: operator or above (\"Read and change\" access) on the server's site. **Area**: `servers`.",
    params(("id" = uuid::Uuid, Path, description = "The server")),
    request_body(content = inventory::Warranty,
        example = json!({"ends_on": "2028-03-31", "note": "ProSupport 5 years"})),
    responses(
        (status = 200, description = "The warranty as set, `null` once removed", body = Option<inventory::WarrantyView>,
            example = json!({"ends_on": "2028-03-31", "note": "ProSupport 5 years", "updated_at": "2026-11-23T10:00:00Z"})),
        (status = 400, description = "A bad date or a note over 500 characters: `invalid`", body = ErrorBody),
        (status = 403, description = "Read-only on the site: `forbidden`; or outside the key's areas: \
`api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such server, or outside the key's reach: `not_found`", body = ErrorBody),
    ))]
fn set_warranty() {}

#[utoipa::path(get, path = "/api/v1/inventory", tag = "inventory",
    summary = "Search the inventory of the fleet",
    description = "Finds an item across every server the key reaches: \"which servers have OpenSSL below \
3.0.15\" is `kind=software&q=openssl&version_below=3.0.15`, \"which disk models\" is `kind=disk`. Software \
covers Linux packages and Windows updates (`package`, `windows_update`), third-party apps (`winget`, \
`flatpak`, `snap`) and Windows programs (`program`). `q` matches names; for the other kinds, the key and \
every field. Versions compare the way dpkg does, which also suits RPM and Windows versions; with a version \
filter, items without a version are left out. The version of a `system` is its BIOS version, of a `disk` \
its firmware.\n\n\
`format=csv` returns a file to download with every match, up to 100,000.\n\n\
**Who**: any role. **Area**: `servers`.",
    params(inventory::SearchParams),
    responses(
        (status = 200, description = "The matches as JSON, or the CSV file as an attachment", content(
            (inventory::SearchResult = "application/json", example = json!({"hits": [
                {"server_id": ex::SERVER, "hostname": "web-1", "site_id": ex::SITE, "site": "Paris",
                 "source": "package", "name": "openssl", "version": "3.0.14-1~deb12u2",
                 "fields": {"arch": "amd64", "upgradable_version": "3.0.15-1~deb12u1"}}],
                "truncated": false})),
            (String = "text/csv"),
        )),
        (status = 400, description = "An unknown kind or format, `q` too short for software, `limit` out \
of range or an unknown parameter: `invalid`", body = ErrorBody),
    ))]
fn search_inventory() {}
