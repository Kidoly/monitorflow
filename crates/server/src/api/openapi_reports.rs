//! Business services, availability and report schedules (ADR-0155) in the
//! OpenAPI document: merged into it by [`super::openapi::build`]; the
//! router in `mod.rs` serves the routes.

#![allow(dead_code)] // The functions exist for their `#[utoipa::path]`.

use serde_json::{json, Value};
use utoipa::OpenApi;

use super::openapi::{Common, ErrorBody};
use super::openapi_core::ex;
use super::{availability, business_services};

#[derive(OpenApi)]
#[openapi(
    paths(list_services, create_service, update_service, remove_service, availability_report,
        availability_file),
    components(schemas(ErrorBody)),
    modifiers(&Common),
)]
pub struct Doc;

const SERVICE: &str = "5b6c7d8e-9f0a-4b1c-8d2e-3f4a5b6c7d8e";

fn service_request() -> Value {
    json!({
        "name": "Email", "description": "Mail for the whole office",
        "rule": "all", "target": 99.9,
        "members": [
            {"server_id": ex::SERVER},
            {"server_id": ex::SERVER, "check": {"name": "service_postfix", "instance": ""}}
        ]
    })
}

fn service() -> Value {
    json!({
        "id": SERVICE, "customer_id": ex::CUSTOMER, "customer_name": "default",
        "name": "Email", "description": "Mail for the whole office",
        "rule": "all", "at_least": null, "target": 99.9,
        "members": [
            {"server_id": ex::SERVER, "server_name": "mail-1", "device": false, "check": null},
            {"server_id": ex::SERVER, "server_name": "mail-1", "device": false,
             "check": {"name": "service_postfix", "instance": ""}}
        ],
        "created_at": "2026-11-18T09:30:00Z", "updated_at": "2026-11-18T09:30:00Z",
        "can_change": true
    })
}

#[utoipa::path(get, path = "/api/v1/business-services", tag = "alerts",
    summary = "List business services",
    description = "The business services of the customers the key is granted as a whole, by name. A key \
granted only some sites sees none: services belong to a customer.\n\n\
**Who**: any role on the whole customer. **Area**: `alerts`. Unknown parameters are 400.",
    params(business_services::ListParams),
    responses(
        (status = 200, description = "The services", body = business_services::ServiceList,
            example = json!({"services": [service()]})),
        (status = 400, description = "An unknown parameter: `invalid`", body = ErrorBody),
        (status = 404, description = "The customer of the filter does not exist or is outside the key's \
reach: `not_found`", body = ErrorBody),
    ))]
fn list_services() {}

#[utoipa::path(post, path = "/api/v1/business-services", tag = "alerts",
    summary = "Create a business service",
    description = "A business service is a named group with one availability, like \"Email\" or \
\"Website\": servers, devices, or chosen checks of them (`{name, instance}`, as silences name them), all \
of one customer. Its `rule` is `all` (down while any member is down) or `at_least` (down while fewer than \
`at_least` members are up, for members that back each other up). A member under planned maintenance counts \
as up; the time the service would be down only because of such members is its planned maintenance. \
`target` is an SLA target in percent, which reports say was met or missed, with the error budget left.\n\n\
`customer_id` may be left out when there is only one customer. Members are 1 to 200 servers or devices of \
that customer, not removed, each whole or one of its checks.\n\n\
**Who**: operator or above on the whole customer. **Area**: `alerts`. Audited as \
`business_service.create`. **Not safe to repeat**: send `Idempotency-Key` to retry.",
    request_body(content = business_services::ServiceRequest, example = json!(service_request())),
    responses(
        (status = 201, description = "The new service", body = business_services::ServiceView,
            example = json!(service())),
        (status = 400, description = "A name missing or over 80 characters, a description over 500, no \
member or over 200, a member twice, a server of another customer, an unknown check, a rule other than \
`all` or `at_least`, `at_least` missing, with `all`, or over the number of members, a target outside 50 to \
100 or with more than three decimals, or no customer named when there are several: `invalid`",
            body = ErrorBody),
        (status = 403, description = "The key may only read the customer, or has only some of its \
sites: `forbidden`; or outside the key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "The customer does not exist or is outside the key's reach: \
`not_found`", body = ErrorBody),
        (status = 409, description = "The customer has a service of that name: `conflict`",
            body = ErrorBody),
    ))]
fn create_service() {}

#[utoipa::path(put, path = "/api/v1/business-services/{id}", tag = "alerts",
    summary = "Change a business service",
    description = "Replaces every field, as when creating one; without `customer_id`, it stays with its \
customer. Reports of past periods use the new members and rule.\n\n\
**Who**: operator or above on the whole customer. **Area**: `alerts`. Audited as \
`business_service.update`. Safe to repeat.",
    params(("id" = uuid::Uuid, Path, description = "The service")),
    request_body(content = business_services::ServiceRequest, example = json!(service_request())),
    responses(
        (status = 200, description = "Changed", body = business_services::ServiceView,
            example = json!(service())),
        (status = 400, description = "As when creating one: `invalid`", body = ErrorBody),
        (status = 403, description = "The key may only read the customer: `forbidden`; or outside the \
key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such service, or one the key does not see: `not_found`",
            body = ErrorBody),
        (status = 409, description = "The customer has another service of that name: `conflict`",
            body = ErrorBody),
    ))]
fn update_service() {}

#[utoipa::path(delete, path = "/api/v1/business-services/{id}", tag = "alerts",
    summary = "Remove a business service",
    description = "Removes the service; reports no longer show it, for past periods too. Its members \
are not touched.\n\n\
**Who**: operator or above on the whole customer. **Area**: `alerts`. Audited as \
`business_service.remove`.",
    params(("id" = uuid::Uuid, Path, description = "The service")),
    responses(
        (status = 204, description = "Removed"),
        (status = 403, description = "The key may only read the customer: `forbidden`; or outside the \
key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such service, or one the key does not see: `not_found`",
            body = ErrorBody),
    ))]
fn remove_service() {}

fn report() -> Value {
    let outage = json!({
        "start": "2026-10-14T03:10:00Z", "end": "2026-10-14T03:52:00Z", "seconds": 2520.0,
        "ongoing": false,
        "causes": [{"kind": "check", "server_id": ex::SERVER, "server_name": "mail-1",
                    "check": "service_postfix", "instance": "", "via": null}]
    });
    let numbers = json!({
        "availability": 99.9059, "counted_seconds": 2_678_400.0, "down_seconds": 2520.0,
        "maintenance_seconds": 7200.0, "outages": 1, "longest_seconds": 2520.0
    });
    json!({
        "customer_id": null, "customer_name": null,
        "from": "2026-10-01T00:00:00Z", "to": "2026-11-01T00:00:00Z",
        "generated_at": "2026-11-01T06:00:00Z",
        "summary": {"services": 1, "services_met": 1, "services_missed": 0, "machines": 1,
                    "outages": 1, "down_seconds": 2520.0, "maintenance_seconds": 7200.0},
        "services": [{
            "id": SERVICE, "name": "Email", "customer_id": ex::CUSTOMER, "rule": "all",
            "at_least": null, "target": 99.9, "met": true, "budget_seconds": 158.4,
            "members": [{"server_id": ex::SERVER, "server_name": "mail-1", "check": null}],
            "numbers": numbers, "outage_list": [outage]
        }],
        "machines": [{
            "id": ex::SERVER, "name": "mail-1", "device": false, "site_id": ex::SITE,
            "site_name": "paris", "customer_id": ex::CUSTOMER, "numbers": numbers,
            "outage_list": [outage]
        }],
        "checks": [{
            "server_id": ex::SERVER, "server_name": "mail-1", "name": "service_postfix",
            "instance": "", "numbers": numbers, "outage_list": [outage]
        }],
        "maintenance": [{
            "start": "2026-10-05T01:00:00Z", "end": "2026-10-05T03:00:00Z",
            "reason": "Patches", "servers": ["mail-1"], "checks": [], "window": true
        }]
    })
}

#[utoipa::path(get, path = "/api/v1/availability", tag = "alerts",
    summary = "Availability over a period",
    description = "How available the business services, servers, devices and checks the key reads were \
over a period, from one hour to 13 months: this month so far when `from` and `to` are absent.\n\n\
**What counts as down**: any time a critical incident was open: a check failing at its critical \
threshold, a server offline, or a server unreachable because what it is reached through was down (an \
offline relay). Warnings never count. **Planned maintenance**, time under a silence or a maintenance \
window, is given apart (`maintenance_seconds`) and left out of `counted_seconds`; a silence counts from \
when it started or was made, whichever is later. `availability` is the share of `counted_seconds` that \
was up, in percent, or `null` when nothing was counted. An outage is one unbroken stretch of down time, \
with its causes.\n\n\
Services give their SLA `target`, whether it was `met`, and `budget_seconds`: the down time the target \
allowed minus the down time there was, negative once exceeded. `checks` lists only checks that had \
outages. Computed from the incident history on each request, so a past period gives the same numbers \
later, unless something was removed since.\n\n\
**Who**: any role. With `customer_id`, only that customer's sites the key reads; business services only \
of customers the key is granted as a whole. **Area**: `alerts`.",
    params(availability::Params),
    responses(
        (status = 200, description = "The report", body = crate::availability::report::Report,
            example = json!(report())),
        (status = 400, description = "A period shorter than an hour, longer than 13 months or starting \
in the future, or an unknown parameter: `invalid`", body = ErrorBody),
        (status = 404, description = "The customer does not exist or is outside the key's reach: \
`not_found`", body = ErrorBody),
    ))]
fn availability_report() {}

#[utoipa::path(get, path = "/api/v1/availability/report", tag = "alerts",
    summary = "Download an availability report",
    description = "The same report as a file to keep or send: `pdf`, a document to send to a customer \
as it is (a summary in plain words, then the details), or `csv`, one row per service, server, device \
and check with its numbers, then one row per outage and per planned maintenance, with a `kind` column. \
CSV numbers are plain (seconds, dot decimals); words are in `language`; times are in `time_zone`.\n\n\
**Who**: any role, as above. **Area**: `alerts`.",
    params(availability::FileParams),
    responses(
        (status = 200, description = "The file, as an attachment", content(
            (String = "text/csv"),
            (Vec<u8> = "application/pdf"),
        )),
        (status = 400, description = "An unknown format, language or time zone, or a period as above: \
`invalid`", body = ErrorBody),
        (status = 404, description = "The customer does not exist or is outside the key's reach: \
`not_found`", body = ErrorBody),
    ))]
fn availability_file() {}
