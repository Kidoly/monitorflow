//! Maintenance windows (ADR-0151) in the OpenAPI document: merged into it
//! by [`super::openapi::build`]; the router in `mod.rs` serves the routes.

#![allow(dead_code)] // The functions exist for their `#[utoipa::path]`.

use serde_json::{json, Value};
use utoipa::OpenApi;

use super::maintenance_windows;
use super::openapi::{Common, ErrorBody};
use super::openapi_core::ex;

#[derive(OpenApi)]
#[openapi(
    paths(list_windows, create_window, update_window, remove_window),
    components(schemas(ErrorBody)),
    modifiers(&Common),
)]
pub struct Doc;

const WINDOW: &str = "7c1d2e3f-4a5b-4c6d-8e9f-0a1b2c3d4e5f";

fn request() -> Value {
    json!({
        "site_id": ex::SITE, "server_ids": [ex::SERVER],
        "checks": [{"name": "service_postgresql", "instance": ""}],
        "repeat": "weekly", "weekdays": ["sunday"],
        "start_time": "02:00", "duration_minutes": 120, "time_zone": "Europe/Paris",
        "reason": "Database patches"
    })
}

fn window() -> Value {
    json!({
        "id": WINDOW, "site_id": ex::SITE, "site_name": "paris", "customer_id": ex::CUSTOMER,
        "whole_site": false, "servers": [{"id": ex::SERVER, "hostname": "web-1"}],
        "checks": [{"name": "service_postgresql", "instance": ""}],
        "repeat": "weekly", "every": 1, "weekdays": ["sunday"],
        "month_day": null, "month_week": null,
        "months": [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
        "start_time": "02:00", "duration_minutes": 120, "time_zone": "Europe/Paris",
        "starts_on": "2026-11-18", "ends_on": null, "reason": "Database patches",
        "active_until": null,
        "next": [
            {"starts_at": "2026-11-22T01:00:00Z", "ends_at": "2026-11-22T03:00:00Z"},
            {"starts_at": "2026-11-29T01:00:00Z", "ends_at": "2026-11-29T03:00:00Z"},
            {"starts_at": "2026-12-06T01:00:00Z", "ends_at": "2026-12-06T03:00:00Z"}
        ],
        "created_by_username": "alice", "created_at": "2026-11-18T09:30:00Z",
        "updated_at": "2026-11-18T09:30:00Z", "can_change": true
    })
}

#[utoipa::path(get, path = "/api/v1/maintenance-windows", tag = "alerts",
    summary = "List maintenance windows",
    description = "The maintenance windows of the sites the key reads, the next to come first, in full. A \
maintenance window is a silence that comes back on its own every day, week or month, at local times in a \
time zone, for a site, chosen servers of it, or chosen checks of those: notifications are held while one \
runs, exactly as a silence holds them, and what is still open at its end is sent then. It needs no renewing.\n\n\
Each window lists its next three windows (`next`, the running one first) and, while one runs, when it ends \
(`active_until`). While one runs, its servers' `silence` says `window: true`, and its incidents \
`in_maintenance: true`.\n\n\
**Who**: any role. **Area**: `alerts`.\n\n\
**Filter**: at most one of `site_id` and `server_id`, for the windows of the site, or those covering the \
server (its whole site's, and those naming it). Unknown parameters are 400.",
    params(maintenance_windows::ListParams),
    responses(
        (status = 200, description = "The windows", body = maintenance_windows::WindowList,
            example = json!({"windows": [window()]})),
        (status = 400, description = "Both filters, or an unknown parameter: `invalid`", body = ErrorBody),
        (status = 404, description = "The site or server of the filter does not exist or is outside the \
key's reach: `not_found`", body = ErrorBody),
    ))]
fn list_windows() {}

#[utoipa::path(post, path = "/api/v1/maintenance-windows", tag = "alerts",
    summary = "Create a maintenance window",
    description = "A silence that comes back on its own:\n\n\
- `daily`: every `every` days from `starts_on`;\n\
- `weekly`: on `weekdays`, every `every` weeks from the week of `starts_on`;\n\
- `monthly`: on `month_day` (a month without that day has no window), or on the one weekday of \
`weekdays` in week `month_week` of the month (5 is the last: \"the last Friday\"), in `months` (every \
month when absent).\n\n\
Each window starts at `start_time`, local to `time_zone`, and lasts `duration_minutes` (at most 7 days). \
Daylight saving is followed: a start that happens twice when clocks go back is the first, one skipped \
when they go forward is read with the offset before the change, and the end is the local start plus the \
duration, so \"22:00 for 8 hours\" ends at 06:00 local time on the nights clocks change.\n\n\
Without `server_ids`, it covers the whole site, servers added later included. With `checks`, only the \
incidents those checks write: the server going offline and its other checks still notify.\n\n\
**Who**: operator or above on the site. **Area**: `alerts`. Audited as `maintenance_window.create`. \
**Not safe to repeat**: each request makes a window; send `Idempotency-Key` to retry.",
    request_body(content = maintenance_windows::WindowRequest, example = json!(request())),
    responses(
        (status = 201, description = "The new window", body = maintenance_windows::WindowView,
            example = json!(window())),
        (status = 400, description = "An unknown repeat, weekday, month or time zone, a time not \
`HH:MM`, no weekday for a weekly window, neither or both of `month_day` and `month_week`, not one \
weekday for a monthly window by week, a duration over 7 days, a last day before the first, servers of \
another site or over 500, unknown checks or over 20, or a reason missing or over 200 characters: \
`invalid`", body = ErrorBody),
        (status = 403, description = "The key may only read the site: `forbidden`; or outside the key's \
areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "The site does not exist or is outside the key's reach: \
`not_found`", body = ErrorBody),
    ))]
fn create_window() {}

#[utoipa::path(put, path = "/api/v1/maintenance-windows/{id}", tag = "alerts",
    summary = "Change a maintenance window",
    description = "Replaces every field, as when creating one. The running window, if any, ends and the \
new rule's are written at once, so what it held stays held when the new rule still covers it, and is sent \
within seconds otherwise.\n\n\
**Who**: operator or above on its site now and on the one it will have. **Area**: `alerts`. Audited as \
`maintenance_window.update`. Safe to repeat.",
    params(("id" = uuid::Uuid, Path, description = "The window")),
    request_body(content = maintenance_windows::WindowRequest, example = json!(request())),
    responses(
        (status = 200, description = "Changed", body = maintenance_windows::WindowView,
            example = json!(window())),
        (status = 400, description = "As when creating one: `invalid`", body = ErrorBody),
        (status = 403, description = "The key may only read one of the sites: `forbidden`; or outside \
the key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such window, or one the key does not see: `not_found`",
            body = ErrorBody),
    ))]
fn update_window() {}

#[utoipa::path(delete, path = "/api/v1/maintenance-windows/{id}", tag = "alerts",
    summary = "Remove a maintenance window",
    description = "No more windows. The running one ends now, and what it held back and is still open is \
sent within seconds. To skip one window only, end its silence (`POST /api/v1/silences/{id}/end`): the \
window comes back as planned.\n\n\
**Who**: operator or above on its site. **Area**: `alerts`. Audited as `maintenance_window.remove`.",
    params(("id" = uuid::Uuid, Path, description = "The window")),
    responses(
        (status = 204, description = "Removed"),
        (status = 403, description = "The key may only read its site: `forbidden`; or outside the \
key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such window, removed already, or one the key does not see: \
`not_found`", body = ErrorBody),
    ))]
fn remove_window() {}
