//! The public API's description of dashboards and the data of their
//! panels. Merged into the document by [`super::openapi::build`].

#![allow(dead_code)] // The functions exist for their `#[utoipa::path]`.

use serde_json::json;
use utoipa::OpenApi;

use super::openapi::{Common, ErrorBody};
use super::{dashboard_data, dashboards};

#[derive(OpenApi)]
#[openapi(
    paths(list, detail, create, update, delete, data, catalog),
    modifiers(&Common),
    tags((name = "dashboards", description = "Pages of panels kept for one person (\"Only me\") or shared with a site or a customer. Viewers of a \
site or customer see its dashboards; any role above viewer makes, changes and deletes them; \
anyone who can read makes their own. With a key, the \"dashboards\" area; a key sees, changes and \
deletes its maker's \"Only me\" dashboards whatever its reach, but the data of any panel is only \
ever read within the key's reach. A panel never shows more than the person asking may read: its \
servers are resolved again on every read, and servers they cannot see are counted, not named.\n\n\
A panel's `kind` says how it is drawn: `chart` (lines), `area` (areas, stacked when there is one \
per server), `columns` (a column per step, stacked likewise), `figure` (one number), `gauge` (one \
number on a dial), `top` (a list of the highest or lowest servers), `bars` (the same as \
horizontal bars), `alerts` (open alerts), `heatmap` (a row per server over time, coloured by \
value: the `count` worst servers), `table` (a row per server, a column per metric of `columns`, \
sorted by the first, the `count` first) and `text` (Markdown in `text`, no data). Kinds that draw \
the same data take the same fields and answer the same data shape: `area`, `columns` and \
`heatmap` as `chart`, `gauge` as `figure`, `bars` as `top`; a table answers `table`, a text \
panel `text`. A table column without an instance shows the worst of the server's instances. Optional display fields, ignored by the data: `unit`, `min` and `max` of the scale, \
`thresholds` (`warning`, `critical`) that colour figures, gauges and bars and draw lines on \
charts, `legend` (charts over time only) and `color`, the first colour of the eight of the \
palette. Alerts panels take none of them. Dashboards saved before these kinds and fields existed \
read unchanged.\n\n\
Display links, which show dashboards on a wall screen without signing in, are made, listed and \
turned off only in the web interface, by an administrator of the dashboards' site or customer (or \
of anything for their own). A link's secret is in the URL fragment, shown once; a screen trades it \
for a session of one hour. Whoever holds the URL sees the link's dashboards, up to 12, and \
nothing else: no other page, no settings, no change. Each screen request reads as the link's \
maker with their roles of the moment, so a maker who is disabled or loses the role stops their \
links. A link may expire or never; deleting a dashboard removes it from its links. Because a link \
reads with all its maker's roles and outlives a key, `/api/v1/display-links`, \
`/api/v1/dashboards/{id}/display-links` and `/api/v1/display` answer 404.")),
)]
pub(super) struct DashboardsDoc;

const SITE: &str = "e7f8a9b0-c1d2-4e3f-9a4b-5c6d7e8f9a0b";

fn spec_example() -> serde_json::Value {
    json!({
        "version": 1,
        "range": "24h",
        "panels": [
            {
                "id": "cpu", "title": "CPU, busiest servers", "kind": "chart", "width": 2,
                "servers": { "kind": "all" }, "metric": "monitorflow_cpu_usage_ratio", "combine": "each"
            },
            {
                "id": "disks", "title": "Fullest disks", "kind": "top", "width": 1,
                "servers": { "kind": "list", "ids": ["6f2d1c4b-8a3e-4d7f-9b0c-1e2a3b4c5d6e"] },
                "metric": "monitorflow_disk_used_ratio", "order": "highest", "count": 5
            },
            {
                "id": "servers", "title": "Servers", "kind": "table", "width": 3,
                "servers": { "kind": "all" }, "order": "highest", "count": 20,
                "columns": [
                    { "metric": "monitorflow_cpu_usage_ratio", "instance": "total" },
                    { "metric": "monitorflow_disk_used_ratio" }
                ]
            },
            {
                "id": "cpu-now", "title": "CPU now, busiest server", "kind": "gauge", "width": 1,
                "servers": { "kind": "all" }, "metric": "monitorflow_cpu_usage_ratio", "instance": "total",
                "combine": "max", "thresholds": { "warning": 0.8, "critical": 0.95 }
            },
            { "id": "alerts", "title": "Open alerts", "kind": "alerts", "width": 3, "servers": { "kind": "all" }, "count": 10 }
        ]
    })
}

fn view_example() -> serde_json::Value {
    json!({
        "id": "9a8b7c6d-5e4f-4a3b-8c2d-1e0f9a8b7c6d",
        "name": "Paris overview",
        "shared_with": { "kind": "site", "id": SITE, "name": "paris" },
        "panels": 3,
        "updated_at": "2026-11-13T16:20:00Z",
        "updated_by_username": "alice",
        "can_edit": true,
        "can_link": false,
        "spec": spec_example(),
        "revision": 4
    })
}

/// List dashboards
///
/// The dashboards the caller sees, by name: their own, those of the sites
/// they read, and those of customers they see as a whole. Without the
/// panels; read one for them. Not paged: the list stays small.
#[utoipa::path(get, path = "/api/v1/dashboards", tag = "dashboards",
    responses((status = 200, description = "The dashboards the caller sees", body = dashboards::DashboardList,
        example = json!({ "dashboards": [{
            "id": "9a8b7c6d-5e4f-4a3b-8c2d-1e0f9a8b7c6d", "name": "Paris overview",
            "shared_with": { "kind": "site", "id": SITE, "name": "paris" }, "panels": 3,
            "updated_at": "2026-11-13T16:20:00Z", "updated_by_username": "alice",
            "can_edit": true, "can_link": false
        }] }))))]
fn list() {}

/// Read a dashboard
///
/// With its panels and its `revision`, to send back when changing it. 404
/// `not_found` for one the caller does not see.
#[utoipa::path(get, path = "/api/v1/dashboards/{id}", tag = "dashboards",
    params(("id" = uuid::Uuid, Path, description = "The dashboard's ID")),
    responses((status = 200, description = "The dashboard", body = dashboards::DashboardView, example = view_example)))]
fn detail() {}

/// Create a dashboard
///
/// `shared_with` personal needs only read access; a site or customer
/// needs a role above viewer on it. Every site, customer
/// and server a panel names must be readable by the caller and within the
/// sharing. Audited as `dashboard.create`. Not safe to repeat: send
/// `Idempotency-Key` to retry.
///
/// Errors: 400 `invalid` for a bad name or spec, or a panel naming
/// something unknown, unreadable or outside the sharing, in one message
/// that does not say which; 403 `forbidden` for a site or customer the
/// caller only reads; 404 `not_found` for one they do not see.
#[utoipa::path(post, path = "/api/v1/dashboards", tag = "dashboards",
    request_body(content = dashboards::DashboardRequest, example = json!({
        "name": "Paris overview", "shared_with": { "kind": "site", "id": SITE }, "spec": spec_example()
    })),
    responses((status = 201, description = "Created", body = dashboards::DashboardView, example = view_example)))]
fn create() {}

/// Change a dashboard
///
/// Replaces its name, sharing and panels, given the `revision` the edit
/// started from. Needs the right to change it as it is shared now and, if
/// the sharing changes, as it will be. Audited as `dashboard.update`.
///
/// Errors: as for creating it, and 400 `invalid` without `revision`; 409
/// `conflict` when someone saved it since that revision: read it again.
#[utoipa::path(put, path = "/api/v1/dashboards/{id}", tag = "dashboards",
    params(("id" = uuid::Uuid, Path, description = "The dashboard's ID")),
    request_body(content = dashboards::DashboardRequest, example = json!({
        "name": "Paris overview", "shared_with": { "kind": "site", "id": SITE }, "spec": spec_example(), "revision": 4
    })),
    responses(
        (status = 200, description = "The dashboard as it now stands", body = dashboards::DashboardView,
            example = view_example),
        (status = 409, body = ErrorBody, description = "Changed since `revision`: `conflict`"),
    ))]
fn update() {}

/// Delete a dashboard
///
/// Also takes it off the display links that showed it. Audited as
/// `dashboard.delete`. 404 `not_found` for one the caller does not see,
/// 403 `forbidden` for one they may only read.
#[utoipa::path(delete, path = "/api/v1/dashboards/{id}", tag = "dashboards",
    params(("id" = uuid::Uuid, Path, description = "The dashboard's ID")),
    responses((status = 204, description = "Deleted")))]
fn delete() {}

/// Read the data of panels
///
/// What the panels given show now, over `range`, for the caller: the
/// servers of each panel are those it names that the caller reads, within
/// `within`. Pass a dashboard's `spec.panels` and `spec.range`, and its
/// sharing as `within`, to show it as its page does; panels need not be
/// saved. Points are aligned to the step and answers are cached 15
/// seconds. A panel whose query fails answers `kind: error` while the
/// others still show. Changes nothing.
///
/// Errors: 400 `invalid` for a bad panel; 502 `metrics_unavailable` or 504
/// `metrics_timeout` when the metrics store fails as a whole.
#[utoipa::path(post, path = "/api/v1/dashboard-data", tag = "dashboards",
    request_body(content = dashboard_data::DataRequest, example = json!({
        "range": "24h", "within": { "kind": "site", "id": SITE }, "panels": spec_example()["panels"]
    })),
    responses(
        (status = 200, description = "Every panel's data", body = dashboard_data::DataResponse, example = json!({
            "start": 1_763_049_600, "end": 1_763_136_000, "step": 300, "at": 1_763_136_120, "now": 1_763_136_127,
            "panels": [
                { "id": "cpu", "servers": 4, "unavailable": 0, "kind": "chart", "series": [
                    { "label": "web-1", "server_id": "6f2d1c4b-8a3e-4d7f-9b0c-1e2a3b4c5d6e",
                      "points": [[1_763_049_600, 0.125], [1_763_049_900, 0.141], [1_763_050_200, null]] }
                ] },
                { "id": "disks", "servers": 1, "unavailable": 0, "kind": "top", "rows": [
                    { "server_id": "6f2d1c4b-8a3e-4d7f-9b0c-1e2a3b4c5d6e", "hostname": "web-1", "instance": "/var", "value": 0.812 }
                ] },
                { "id": "servers", "servers": 4, "unavailable": 0, "kind": "table", "rows": [
                    { "server_id": "6f2d1c4b-8a3e-4d7f-9b0c-1e2a3b4c5d6e", "hostname": "web-1", "values": [0.125, 0.812] }
                ] },
                { "id": "cpu-now", "servers": 4, "unavailable": 0, "kind": "figure", "value": 0.83 },
                { "id": "alerts", "servers": 4, "unavailable": 1, "kind": "alerts", "alerts": [
                    { "id": "2b3c4d5e-6f70-4a1b-9c2d-3e4f5a6b7c8d", "server_id": "6f2d1c4b-8a3e-4d7f-9b0c-1e2a3b4c5d6e",
                      "hostname": "web-1", "metric": "monitorflow_disk_used_ratio", "instance": "/var",
                      "severity": "warning", "opened_at": "2026-11-14T15:02:00Z",
                      "silenced_until": null, "in_maintenance": false }
                ] }
            ]
        })),
        (status = 502, body = ErrorBody, description = "The metrics store fails: `metrics_unavailable`"),
        (status = 504, body = ErrorBody, description = "The metrics store is too slow: `metrics_timeout`"),
    ))]
fn data() {}

/// List metrics for panels
///
/// The `monitorflow_*` metrics reported in the last day by the servers
/// the caller reads, optionally of one site or customer, and the
/// instances (disks, cores, interfaces) of `metric` when given. At most
/// 1000 of each. Changes nothing.
///
/// Errors: 400 `invalid` for a metric name that is not a MonitorFlow
/// metric; 404 `not_found` for a site the caller does not read; 502
/// `metrics_unavailable` when the metrics store fails.
#[utoipa::path(get, path = "/api/v1/dashboard-metrics", tag = "dashboards",
    params(dashboard_data::CatalogParams),
    responses(
        (status = 200, description = "Metrics and instances", body = dashboard_data::Catalog, example = json!({
            "metrics": ["monitorflow_cpu_usage_ratio", "monitorflow_disk_used_ratio", "monitorflow_memory_available_ratio"],
            "instances": ["/", "/var"]
        })),
        (status = 502, body = ErrorBody, description = "The metrics store fails: `metrics_unavailable`"),
    ))]
fn catalog() {}
