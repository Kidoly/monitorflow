//! Custom SNMP checks (ADR-0157) in the OpenAPI document: lookups, MIB
//! files and the names of objects. Merged into it by
//! [`super::openapi::build`].

#![allow(dead_code)] // The functions exist for their `#[utoipa::path]`.

use serde_json::{json, Value};
use utoipa::OpenApi;

use super::openapi::{Common, ErrorBody};
use super::openapi_core::ex;
use super::snmp_library as lib;

#[derive(OpenApi)]
#[openapi(
    paths(list_lookups, get_lookup, create_lookup, update_lookup, delete_lookup, list_mibs,
          upload_mibs, delete_mib, objects),
    components(schemas(ErrorBody, lib::LookupView, lib::MibView, lib::MibFile, lib::OfferedLookup,
                       lib::Suggestion, lib::ObjectView)),
    modifiers(&Common),
    tags((name = "snmp", description = "What custom SNMP checks of devices read (ADR-0157): lookups that \
name the values of an enumerated object and give each a state (`ok`, `warning`, `critical`), built in or a \
customer's own; MIB files a customer uploads to name the objects of its devices; and the names the library \
of common vendor objects and those MIBs give to OIDs. Lookups and MIBs belong to a customer: anyone who \
reads one of its sites reads them, and operators and above of the whole customer change them. Area \
`servers`."))
)]
pub(super) struct Doc;

const LOOKUP: &str = "3c1d8f2a-6b7e-4c90-a1d2-5e6f7a8b9c0d";
const MIB: &str = "8e2f4a6c-1b3d-4e5f-9a7b-0c2d4e6f8a1b";

fn entries() -> Value {
    json!([
        {"value": 1, "name": "Normal", "state": "ok"},
        {"value": 2, "name": "Repairing", "state": "warning"},
        {"value": 11, "name": "Degraded", "state": "critical"},
        {"value": 12, "name": "Crashed", "state": "critical"}
    ])
}

fn lookup() -> Value {
    json!({"id": LOOKUP, "builtin": false, "customer_id": ex::CUSTOMER, "name": "RAID state",
           "entries": entries(), "otherwise": "warning", "used_by": 2,
           "updated_at": "2026-11-19T09:00:00Z", "can_change": true})
}

fn builtin() -> Value {
    json!({"id": "builtin:truth_value", "builtin": true, "customer_id": null,
           "name": "True or false (TruthValue)",
           "entries": [{"value": 1, "name": "True", "state": "ok"}, {"value": 2, "name": "False", "state": "critical"}],
           "otherwise": "warning", "used_by": 0, "updated_at": null, "can_change": false})
}

fn mib() -> Value {
    json!({"id": MIB, "customer_id": ex::CUSTOMER, "module": "TEST-NAS-MIB", "file_name": "TEST-NAS-MIB.txt",
           "objects": 14, "uploaded_at": "2026-11-19T09:00:00Z", "can_change": true})
}

#[utoipa::path(get, path = "/api/v1/snmp-lookups", tag = "snmp",
    summary = "List lookups",
    description = "The built-in lookups first (`builtin:ok_warning_failed`, `builtin:truth_value`, \
`builtin:if_status`, `builtin:device_status`, `builtin:battery_status`, `builtin:enabled`), then the \
lookups of the customers the key reads a site of, by name, each with how many checks name it.\n\n\
**Who**: any role on a site of the customer. **Area**: `servers`. Unknown parameters are 400.",
    params(lib::ListParams),
    responses(
        (status = 200, description = "The lookups", body = lib::LookupList,
            example = json!({"lookups": [builtin(), lookup()]})),
        (status = 400, description = "An unknown parameter: `invalid`", body = ErrorBody),
        (status = 404, description = "The customer of the filter is outside the key's reach: `not_found`",
            body = ErrorBody),
    ))]
fn list_lookups() {}

#[utoipa::path(get, path = "/api/v1/snmp-lookups/{id}", tag = "snmp",
    summary = "Get a lookup",
    description = "One lookup, built in (`builtin:<name>`) or a customer's own.\n\n\
**Who**: any role on a site of the customer. **Area**: `servers`.",
    params(("id" = String, Path, description = "`builtin:<name>` or the lookup's ID")),
    responses(
        (status = 200, description = "The lookup", body = lib::LookupView, example = json!(lookup())),
        (status = 404, description = "No such lookup, or one the key does not see: `not_found`",
            body = ErrorBody),
    ))]
fn get_lookup() {}

#[utoipa::path(post, path = "/api/v1/snmp-lookups", tag = "snmp",
    summary = "Create a lookup",
    description = "A lookup names the values of an enumerated object (a RAID state, an HA role, a power \
source) and gives each a state, which a custom check's channel writes as its `_state` series: 0 OK, 1 \
warning, 2 critical. An entry is an integer (`value`), a range (`value` and `max`) or an exact text \
(`text`), with a `name` of 1 to 64 characters and a `state`; `otherwise` is the state of a value no entry \
names (`warning` by default). 1 to 100 entries. `customer_id` may be left out when there is only one \
customer. Checks name it by its ID in `lookupN`.\n\n\
**Who**: operator or above on the whole customer. **Area**: `servers`. Audited as `snmp_lookup.create`. \
**Not safe to repeat**: send `Idempotency-Key` to retry.",
    request_body(content = lib::LookupRequest, example = json!({"name": "RAID state", "entries": entries()})),
    responses(
        (status = 201, description = "The new lookup", body = lib::LookupView, example = json!(lookup())),
        (status = 400, description = "A name missing or over 64 characters, no entry or over 100, an entry \
that is not one value, one range or one text, a range that ends before it starts, an unknown state, or no \
customer named when there are several: `invalid`", body = ErrorBody),
        (status = 403, description = "The key may only read the customer, or has only some of its sites: \
`forbidden`; or outside the key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "The customer is outside the key's reach: `not_found`", body = ErrorBody),
        (status = 409, description = "The customer has a lookup of that name: `conflict`", body = ErrorBody),
    ))]
fn create_lookup() {}

#[utoipa::path(put, path = "/api/v1/snmp-lookups/{id}", tag = "snmp",
    summary = "Change a lookup",
    description = "Replaces its name and entries; it stays with its customer. Probes running checks that \
name it get the change within seconds. A built-in lookup cannot change.\n\n\
**Who**: operator or above on the whole customer. **Area**: `servers`. Audited as `snmp_lookup.update`. \
Safe to repeat.",
    params(("id" = uuid::Uuid, Path, description = "The lookup")),
    request_body(content = lib::LookupRequest, example = json!({"name": "RAID state", "entries": entries()})),
    responses(
        (status = 200, description = "Changed", body = lib::LookupView, example = json!(lookup())),
        (status = 400, description = "As when creating one, a built-in lookup, or another customer: \
`invalid`", body = ErrorBody),
        (status = 403, description = "The key may only read the customer: `forbidden`; or outside the \
key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such lookup, or one the key does not see: `not_found`",
            body = ErrorBody),
        (status = 409, description = "The customer has another lookup of that name: `conflict`",
            body = ErrorBody),
    ))]
fn update_lookup() {}

#[utoipa::path(delete, path = "/api/v1/snmp-lookups/{id}", tag = "snmp",
    summary = "Remove a lookup",
    description = "Removes a lookup no check names; change those checks first.\n\n\
**Who**: operator or above on the whole customer. **Area**: `servers`. Audited as `snmp_lookup.delete`.",
    params(("id" = uuid::Uuid, Path, description = "The lookup")),
    responses(
        (status = 204, description = "Removed"),
        (status = 400, description = "A built-in lookup: `invalid`", body = ErrorBody),
        (status = 403, description = "The key may only read the customer: `forbidden`; or outside the \
key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such lookup, or one the key does not see: `not_found`",
            body = ErrorBody),
        (status = 409, description = "Checks name it: `conflict`", body = ErrorBody),
    ))]
fn delete_lookup() {}

#[utoipa::path(get, path = "/api/v1/snmp-mibs", tag = "snmp",
    summary = "List uploaded MIBs",
    description = "The MIB modules of the customers the key reads a site of, by module name, with the \
number of objects each names.\n\n\
**Who**: any role on a site of the customer. **Area**: `servers`.",
    params(lib::ListParams),
    responses(
        (status = 200, description = "The modules", body = lib::MibList, example = json!({"mibs": [mib()]})),
        (status = 400, description = "An unknown parameter: `invalid`", body = ErrorBody),
        (status = 404, description = "The customer of the filter is outside the key's reach: `not_found`",
            body = ErrorBody),
    ))]
fn list_mibs() {}

#[utoipa::path(post, path = "/api/v1/snmp-mibs", tag = "snmp",
    summary = "Upload MIB files",
    description = "Reads 1 to 20 MIB files (SMIv1 or SMIv2, at most 1 MiB and 20,000 objects each) and \
keeps what they name: each object's OID, kind, type, named values, units and the start of its \
description; the files themselves are not kept. The reader never fetches an imported module and runs \
nothing. Imports resolve against the standard modules MonitorFlow knows, the customer's modules uploaded \
before, and the other files of the same upload, in any order; a file that needs a module nobody uploaded \
is refused, and the answer names the modules to add. A module uploaded again replaces the one of the same \
name. The answer offers each object with named values as a lookup, its states guessed from the names, for \
`POST /api/v1/snmp-lookups`.\n\n\
**Who**: operator or above on the whole customer. **Area**: `servers`. Audited as `snmp_mib.upload`, one \
record per module. Safe to repeat.",
    request_body(content = lib::MibUpload, example = json!({"files": [
        {"name": "TEST-NAS-MIB.txt", "text": "TEST-NAS-MIB DEFINITIONS ::= BEGIN\nIMPORTS enterprises, OBJECT-TYPE FROM SNMPv2-SMI;\ntestNas OBJECT IDENTIFIER ::= { enterprises 99999 }\nsystemStatus OBJECT-TYPE\n  SYNTAX INTEGER { normal(1), failed(2) }\n  MAX-ACCESS read-only\n  STATUS current\n  DESCRIPTION \"Whether the system is well.\"\n  ::= { testNas 1 }\nEND\n"}
    ]})),
    responses(
        (status = 201, description = "What was added", body = lib::Uploaded, example = json!({
            "mibs": [mib()],
            "offered_lookups": [{"name": "systemStatus", "module": "TEST-NAS-MIB", "entries": [
                {"value": 1, "name": "normal", "state": "ok"}, {"value": 2, "name": "failed", "state": "critical"}]}]
        })),
        (status = 400, description = "No file or over 20, a file over 1 MiB or with over 20,000 objects, \
one that is not a MIB or does not read (the answer says which file and why), modules to upload too, or \
no customer named when there are several: `invalid`", body = ErrorBody),
        (status = 403, description = "The key may only read the customer: `forbidden`; or outside the \
key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "The customer is outside the key's reach: `not_found`", body = ErrorBody),
        (status = 413, description = "The body is over the size of 20 files"),
    ))]
fn upload_mibs() {}

#[utoipa::path(delete, path = "/api/v1/snmp-mibs/{id}", tag = "snmp",
    summary = "Remove an uploaded MIB",
    description = "Removes the module and the names it gave. Checks keep their OIDs and keep running; \
modules that imported it keep what they named.\n\n\
**Who**: operator or above on the whole customer. **Area**: `servers`. Audited as `snmp_mib.delete`.",
    params(("id" = uuid::Uuid, Path, description = "The module")),
    responses(
        (status = 204, description = "Removed"),
        (status = 403, description = "The key may only read the customer: `forbidden`; or outside the \
key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such module, or one the key does not see: `not_found`",
            body = ErrorBody),
    ))]
fn delete_mib() {}

#[utoipa::path(get, path = "/api/v1/snmp-objects", tag = "snmp",
    summary = "Find or name SNMP objects",
    description = "With `q`: objects whose name contains it (without case) or whose OID starts with it, \
from the library of common vendor objects (net-snmp, Synology, APC, Fortinet, Cisco, IF-MIB) and the \
customers' MIBs, at most 200, each with the channel it suggests (kind, unit, scaling, a built-in lookup, \
and for a column its table and a column naming the rows). With `oids`: up to 50 OIDs, each named by the \
longest known OID it starts with, the rest as its instance (`ifInOctets.3`).\n\n\
**Who**: any role on a site of the customer. **Area**: `servers`.",
    params(lib::ObjectParams),
    responses(
        (status = 200, description = "The objects, or the names", body = lib::ObjectList, example = json!({
            "objects": [{"name": "fgSysSesCount.0", "module": "Fortinet (FORTINET-FORTIGATE-MIB)", "source": "library",
                         "oid": "1.3.6.1.4.1.12356.101.4.1.8.0", "kind": "scalar", "syntax": null, "enumeration": [],
                         "units": "sessions", "description": "Open sessions",
                         "suggestion": {"type": "gauge", "unit": "sessions", "multiply": 1.0, "divide": 1.0,
                                        "lookup": null, "table": null, "name_column": null}}],
            "names": {}
        })),
        (status = 400, description = "Neither or both of `q` and `oids`, `q` over 128 characters, an OID \
that is not numeric, or over 50: `invalid`", body = ErrorBody),
        (status = 404, description = "The customer is outside the key's reach: `not_found`", body = ErrorBody),
    ))]
fn objects() {}
