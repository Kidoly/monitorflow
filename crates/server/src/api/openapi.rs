//! The OpenAPI document of the public API (ADR-0101), generated from the
//! request and response types with `utoipa` and served at
//! `/api/v1/openapi.json`. The functions below only carry each route's
//! description; the router in `mod.rs` serves them.
//!
//! `docs/api/openapi.json` is this document committed. The tests fail
//! when it is out of date (`UPDATE_OPENAPI=1 cargo test -p
//! monitorflow-server openapi` rewrites it), when a documented route does
//! not exist, and when a v1 route is neither documented nor in
//! [`NOT_YET_DOCUMENTED`].

#![allow(dead_code)] // The functions exist for their `#[utoipa::path]`.

use axum::Json;
use serde::Serialize;
use serde_json::json;
use utoipa::openapi::header::HeaderBuilder;
use utoipa::openapi::path::{Operation, ParameterBuilder, ParameterIn};
use utoipa::openapi::schema::{ObjectBuilder, Type};
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::openapi::{OpenApi as Document, RefOr, Required, ResponseBuilder};
use utoipa::{Modify, OpenApi, ToSchema};

use super::openapi_core::{self, ex};
use super::ERROR_CODES;
use super::{approvals, commands, customers, incidents, metrics, servers, silences, sites};

/// Every error: a sentence to show and a stable code to branch on.
#[derive(Serialize, ToSchema)]
#[schema(as = Error)]
pub struct ErrorBody {
    /// Plain words, safe to show.
    pub error: String,
    /// Stable; codes are only ever added.
    pub code: String,
}

/// `GET /api/v1/openapi.json`, without a session: the API's shape is no
/// secret, it is in the source.
pub async fn document() -> Json<Document> {
    Json(build())
}

pub fn build() -> Document {
    let mut doc = ApiDoc::openapi();
    doc.merge(openapi_core::Doc::openapi());
    Common.modify(&mut doc);
    doc
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "MonitorFlow public API",
        version = "1",
        description = include_str!("openapi_intro.md"),
        license(name = "AGPL-3.0-only")
    ),
    paths(
        list_servers, server, list_sites, list_customers, create_customer, create_site,
        list_incidents, incident, acknowledge, list_silences, create_silence, end_silence,
        list_commands, create_command, command, list_approvals, approve, reject,
        query, query_range,
    ),
    components(schemas(
        ErrorBody, incidents::StateFilter, incidents::SeverityFilter, metrics::Aggregation,
        metrics::Rollup, silences::Target,
    )),
    tags(
        (name = "servers", description = "The servers MonitorFlow watches: one machine each, with its agent. \
Lists and details carry the live connection status, open incidents, silences and certificate expiry. \
Area `servers`; any key may list servers within its reach."),
        (name = "sites", description = "Sites group servers and relays, usually one per place or network; \
customers own sites. A single company has one customer. Sites and customers are listed by any key within its \
reach; creating them is in the `administration` area."),
        (name = "alerts", description = "Incidents are opened and resolved by MonitorFlow when a threshold is \
crossed or a server stops reporting; the API reads and acknowledges them. Silences stop notifications about a \
server, relay, site, customer or everything for a while. Area `alerts`."),
        (name = "updates", description = "Commands sent to a server's agent: package updates and agent \
updates, queued, sent as soon as the agent is connected, with their output. Area `updates`."),
        (name = "remediation", description = "Commands the AI proposed to fix an incident that the rules \
held for a person: approve, edit or reject them. The AI's text is untrusted and shown as data. Area \
`remediation`."),
        (name = "metrics", description = "Values a server's sensors reported, now or over a range. You name a \
server, a metric and optionally an instance; MonitorFlow builds the query itself and never accepts raw PromQL. \
Area `servers`."),
    )
)]
struct ApiDoc;

/// What every operation shares: the key, the error codes, the common
/// error answers, rate limits and `Idempotency-Key` on writes.
struct Common;

impl Modify for Common {
    fn modify(&self, doc: &mut Document) {
        let components = doc.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "api_key",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .bearer_format("mfk_")
                    .description(Some(
                        "An API key made under Admin › API keys: `Authorization: Bearer mfk_…`. \
                         It acts as its maker, narrowed to its reach, access and areas.",
                    ))
                    .build(),
            ),
        );
        components.schemas.insert(
            "Error".into(),
            ObjectBuilder::new()
                .description(Some(
                    "Every error: a sentence to show and a stable code to branch on.",
                ))
                .property(
                    "error",
                    ObjectBuilder::new()
                        .schema_type(Type::String)
                        .description(Some(
                            "Plain words, safe to show; may change between versions.",
                        )),
                )
                .property(
                    "code",
                    ObjectBuilder::new()
                        .schema_type(Type::String)
                        .description(Some("Stable; codes are only ever added."))
                        .enum_values(Some(ERROR_CODES)),
                )
                .required("error")
                .required("code")
                .examples([json!({"error": "not found", "code": "not_found"})])
                .into(),
        );
        doc.security = Some(vec![utoipa::openapi::security::SecurityRequirement::new(
            "api_key",
            Vec::<String>::new(),
        )]);
        for item in doc.paths.paths.values_mut() {
            let writes = [
                &mut item.post,
                &mut item.put,
                &mut item.delete,
                &mut item.patch,
            ];
            for operation in writes.into_iter().flatten() {
                idempotency_key(operation);
            }
            let all = [
                &mut item.get,
                &mut item.post,
                &mut item.put,
                &mut item.delete,
                &mut item.patch,
            ];
            for operation in all.into_iter().flatten() {
                errors(operation);
            }
        }
    }
}

fn idempotency_key(operation: &mut Operation) {
    operation.parameters.get_or_insert_with(Vec::new).push(
        ParameterBuilder::new()
            .name("Idempotency-Key")
            .parameter_in(ParameterIn::Header)
            .required(Required::False)
            .description(Some(
                "1 to 255 visible ASCII characters, a UUID usually. The same request again \
                 within 24 hours gets the first answer, with `Idempotent-Replayed: true`, \
                 instead of running twice.",
            ))
            .schema(Some(
                ObjectBuilder::new()
                    .schema_type(Type::String)
                    .min_length(Some(1))
                    .max_length(Some(255)),
            ))
            .build(),
    );
    let responses = &mut operation.responses.responses;
    if let Some(RefOr::T(conflict)) = responses.get_mut("409") {
        conflict.description.push_str(
            "; or `idempotency_in_progress` while the first request with this \
             `Idempotency-Key` is still running",
        );
    } else {
        responses.insert(
            "409".into(),
            error(
                "The first request with this `Idempotency-Key` is still running: \
                 `idempotency_in_progress`",
            ),
        );
    }
    responses.insert(
        "422".into(),
        error("This `Idempotency-Key` was used for another request: `idempotency_key_reused`"),
    );
}

/// An error answer with the [`ErrorBody`] schema.
fn error(description: &str) -> RefOr<utoipa::openapi::Response> {
    RefOr::T(
        ResponseBuilder::new()
            .description(description)
            .content(
                "application/json",
                utoipa::openapi::ContentBuilder::new()
                    .schema(Some(utoipa::openapi::Ref::from_schema_name("Error")))
                    .build(),
            )
            .build(),
    )
}

fn errors(operation: &mut Operation) {
    for (status, description) in [
        (
            "400",
            "A parameter or the body is wrong: `invalid`, or `invalid_request`",
        ),
        (
            "401",
            "No key, or a key that is unknown, expired or turned off, or whose maker is \
             disabled: `unauthenticated`",
        ),
        (
            "403",
            "The key's access or its maker's roles do not allow it: `forbidden`; the route is \
             outside the key's areas: `api_key_scope`",
        ),
        ("404", "Not found, or outside the key's reach: `not_found`"),
        (
            "429",
            "Over the key's rate limit: `rate_limited`; `Retry-After` says in how many seconds \
             to try again",
        ),
        ("500", "Something failed on the central server: `internal`"),
    ] {
        let responses = &mut operation.responses.responses;
        if !responses.contains_key(status) {
            responses.insert(status.into(), error(description));
        }
    }
    if let Some(RefOr::T(limited)) = operation.responses.responses.get_mut("429") {
        limited.headers.insert(
            "Retry-After".into(),
            HeaderBuilder::new()
                .schema(ObjectBuilder::new().schema_type(Type::Integer))
                .description(Some("Seconds until the key's window ends."))
                .build(),
        );
    }
}

// Routes reachable through v1 that the document does not describe yet,
// in the order ADR-0101 gives. Moving one into the document is additive.
pub const NOT_YET_DOCUMENTED: &[&str] = &[
    "/api/notifications/",
    "/api/integrations",
    "/api/on-call/",
    "/api/follow-up/",
    "/api/tokens",
    "/api/install-keys",
    "/api/pending-hosts/",
    "/api/sources",
    "/api/mirrors",
    "/api/remediation/rules",
    "/api/remediation/policies",
    "/api/remediation/classify",
    "/api/remediation/incidents",
    "/api/remediation/events",
    "/api/settings/",
    "/api/users",
    "/api/backups",
    "/api/certificate-authority",
    "/api/campaigns",
    "/api/thresholds",
    "/api/sensor_configs",
    "/api/dashboards",
    "/api/dashboard-data",
    "/api/dashboard-metrics",
    "/api/display-links",
    "/api/relays",
    "/api/agent-releases",
    "/api/server-update",
    "/api/license",
    "/api/plugins",
    "/api/auto-update",
    "/api/health",
];

// Sub-routes of servers and sites not documented yet.
pub const NOT_YET_DOCUMENTED_UNDER: &[&str] = &[
    "/sensors",
    "/sensor_configs",
    "/thresholds",
    "/metrics",
    "/logs",
    "/packages",
    "/rollback",
    "/sources",
    "/revoke",
    "/proxmox",
    "/offline-alerts",
    "/check-interval",
    "/agent-auto-update",
    "/ai-commands",
];

#[utoipa::path(get, path = "/api/v1/servers", tag = "servers",
    summary = "List servers",
    description = "One page of the servers within the key's reach, sorted by site name, then hostname, then \
name. Removed servers are left out; read one by its ID to see it.\n\n\
**Who**: any key, whatever its areas. A key whose maker has no role at all is 403.\n\n\
**Pages**: `limit` servers per page, 1 to 1000, 500 by default. Pass `next_cursor` as `cursor` for the \
next page; it is `null` on the last one. Other query parameters are ignored.\n\n\
`status` is `online` while the agent has a connection open, directly or through a relay. A connected agent \
that has no server yet (waiting for approval) is not listed.",
    params(servers::ListParams),
    responses(
        (status = 200, description = "One page of servers", body = servers::ServerList,
            example = json!({"servers": [ex::server()], "next_cursor": "WyJwYXJpcyIsIndlYi0xIiwid2ViLTEiXQ"})),
        (status = 400, description = "`limit` out of range, or a `cursor` that is not one MonitorFlow \
gave: `invalid`", body = ErrorBody),
    ))]
fn list_servers() {}

#[utoipa::path(get, path = "/api/v1/servers/{id}", tag = "servers",
    summary = "Get a server",
    description = "One server: the fields of a listing row, plus the certificate MonitorFlow issued it last \
and the last time its AI commands were turned on or off from the web interface. A removed server stays \
readable here, with `revoked_at` set, for its history.\n\n\
**Who**: any role on the server's site. **Area**: `servers`.",
    params(("id" = uuid::Uuid, Path, description = "The server")),
    responses(
        (status = 200, description = "The server", body = servers::ServerDetail,
            example = json!(ex::merge(ex::server(), json!({
                "certificate": {
                    "serial": "3f2a9c41d07e", "not_before": "2026-11-14T06:12:40Z",
                    "not_after": "2027-02-12T06:12:40Z", "issued_at": "2026-11-14T06:12:40Z",
                    "revoked": false, "revoked_at": null, "revocation_reason": null
                },
                "ai_commands_change": null
            })))),
        (status = 404, description = "No such server, or outside the key's reach: `not_found`",
            body = ErrorBody),
    ))]
fn server() {}

#[utoipa::path(get, path = "/api/v1/sites", tag = "sites",
    summary = "List sites",
    description = "Every site within the key's reach, sorted by name, in full: sites are few, so this list \
has no pages. Counts leave out removed servers and relays.\n\n\
**Who**: any key, whatever its areas. A key whose maker has no role at all is 403.",
    responses(
        (status = 200, description = "The sites", body = sites::SiteList,
            example = json!({"sites": [ex::site(), ex::merge(ex::site(), json!({
                "id": ex::SITE_2, "name": "lyon", "server_count": 4, "relay_count": 0,
                "agent_auto_update": false, "agent_auto_update_effective": false
            }))]})),
    ))]
fn list_sites() {}

#[utoipa::path(get, path = "/api/v1/customers", tag = "sites",
    summary = "List customers",
    description = "The customers the key sees, sorted by name, in full: those it reaches as a whole, and \
those with a site it reaches. `site_count` counts only the sites the key reaches. A single company has one \
customer.\n\n\
**Who**: any key, whatever its areas. A key whose maker has no role at all is 403.",
    responses(
        (status = 200, description = "The customers", body = customers::CustomerList,
            example = json!({"customers": [{"id": ex::CUSTOMER, "name": "acme", "site_count": 2}]})),
    ))]
fn list_customers() {}

#[utoipa::path(post, path = "/api/v1/customers", tag = "sites",
    summary = "Create a customer",
    description = "Adds a customer, without sites; add them with `POST /api/v1/customers/{id}/sites`.\n\n\
**Who**: a global administrator; the key must reach everything with \"Administer\" access. **Area**: \
`administration`.\n\n\
Names are 1 to 64 characters, without control characters or spaces at either end, and unique. Audited as \
`customer.create`. Sending the same name twice is refused with 409, so a retry never makes two customers.",
    request_body(content = customers::NameRequest, example = json!({"name": "acme"})),
    responses(
        (status = 201, description = "The new customer", body = customers::CustomerView,
            example = json!({"id": ex::CUSTOMER, "name": "acme", "site_count": 0})),
        (status = 400, description = "The name is empty, too long, or has control characters or \
spaces at either end: `invalid`", body = ErrorBody),
        (status = 403, description = "The key or its maker is not a global administrator: \
`forbidden`; or outside the key's areas: `api_key_scope`", body = ErrorBody),
        (status = 409, description = "A customer has this name already: `conflict`", body = ErrorBody),
    ))]
fn create_customer() {}

#[utoipa::path(post, path = "/api/v1/customers/{id}/sites", tag = "sites",
    summary = "Create a site",
    description = "Adds a site to a customer. Servers join it when they are installed with one of its \
install commands. Sites never move between customers.\n\n\
**Who**: an administrator of the customer as a whole: a global administrator, or one of that customer. A \
site administrator may not, and neither may a key that reaches one site. **Area**: `administration`.\n\n\
Names are 1 to 64 characters, without control characters or spaces at either end, and unique across all \
customers: the name is the `site` label of every metric. Audited as `site.create`. Sending the same name \
twice is refused with 409.",
    params(("id" = uuid::Uuid, Path, description = "The customer")),
    request_body(content = customers::NameRequest, example = json!({"name": "paris"})),
    responses(
        (status = 201, description = "The new site", body = sites::SiteView,
            example = json!(ex::merge(ex::site(), json!({"server_count": 0, "relay_count": 0})))),
        (status = 400, description = "The name is empty, too long, or has control characters or \
spaces at either end: `invalid`", body = ErrorBody),
        (status = 403, description = "The key sees the customer but may not administer it: \
`forbidden`; or outside the key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such customer, or one the key does not see: `not_found`",
            body = ErrorBody),
        (status = 409, description = "A site has this name already, in any customer: `conflict`",
            body = ErrorBody),
    ))]
fn create_site() {}

#[utoipa::path(get, path = "/api/v1/incidents", tag = "alerts", params(incidents::ListParams),
    responses((status = 200, body = incidents::IncidentList)))]
fn list_incidents() {}

#[utoipa::path(get, path = "/api/v1/incidents/{id}", tag = "alerts", params(("id" = uuid::Uuid, Path)),
    responses((status = 200, body = incidents::IncidentDetail)))]
fn incident() {}

/// Says someone is on it, so nobody else is told next. Acknowledging again
/// changes nothing.
#[utoipa::path(post, path = "/api/v1/incidents/acknowledge", tag = "alerts",
    request_body = incidents::AcknowledgeRequest,
    responses((status = 200, body = incidents::Acknowledged)))]
fn acknowledge() {}

#[utoipa::path(get, path = "/api/v1/silences", tag = "alerts", params(silences::ListParams),
    responses((status = 200, body = silences::SilenceList)))]
fn list_silences() {}

/// Not safe to repeat: send `Idempotency-Key` to retry.
#[utoipa::path(post, path = "/api/v1/silences", tag = "alerts", request_body = silences::CreateRequest,
    responses((status = 201, body = silences::SilenceView)))]
fn create_silence() {}

#[utoipa::path(post, path = "/api/v1/silences/{id}/end", tag = "alerts", params(("id" = uuid::Uuid, Path)),
    responses((status = 204, description = "Ended")))]
fn end_silence() {}

#[utoipa::path(get, path = "/api/v1/servers/{id}/commands", tag = "updates",
    params(("id" = uuid::Uuid, Path), commands::ListParams),
    responses((status = 200, body = commands::CommandList)))]
fn list_commands() {}

/// Queues a command for the server's agent. Not safe to repeat: send
/// `Idempotency-Key` to retry.
#[utoipa::path(post, path = "/api/v1/servers/{id}/commands", tag = "updates",
    params(("id" = uuid::Uuid, Path)), request_body = commands::CreateRequest,
    responses((status = 201, body = commands::CommandDetail), (status = 409, body = ErrorBody)))]
fn create_command() {}

#[utoipa::path(get, path = "/api/v1/commands/{id}", tag = "updates", params(("id" = uuid::Uuid, Path)),
    responses((status = 200, body = commands::CommandDetail)))]
fn command() {}

#[utoipa::path(get, path = "/api/v1/remediation/approvals", tag = "remediation",
    params(approvals::ListParams), responses((status = 200, body = approvals::PendingList)))]
fn list_approvals() {}

#[utoipa::path(post, path = "/api/v1/remediation/approvals/{id}/approve", tag = "remediation",
    params(("id" = uuid::Uuid, Path)), request_body = approvals::ApproveRequest,
    responses((status = 200, body = approvals::DecisionView), (status = 409, body = ErrorBody)))]
fn approve() {}

#[utoipa::path(post, path = "/api/v1/remediation/approvals/{id}/reject", tag = "remediation",
    params(("id" = uuid::Uuid, Path)), request_body = approvals::RejectRequest,
    responses((status = 200, body = approvals::DecisionView), (status = 409, body = ErrorBody)))]
fn reject() {}

#[utoipa::path(get, path = "/api/v1/metrics/query", tag = "metrics",
    summary = "Read a metric now",
    description = "The value of one metric of one server at `time` (now by default): one series per \
instance, or one with `agg`. `query` in the answer is the query MonitorFlow ran, for reference.\n\n\
`metric` is `monitorflow_` followed by a sensor's metric name, such as `monitorflow_cpu_usage_ratio`; \
`GET /api/v1/servers/{id}/metrics` lists those a server reported. `instance` is what the sensor watches, \
like a disk or a service. `lookback` (1 to 172800 seconds) takes the last value within that window, for \
checks that run less often than every few minutes.\n\n\
**Who**: any role on the server's site. **Area**: `servers`.\n\n\
Answers hold at most 500 series. Unknown parameters are 400.",
    params(metrics::InstantParams),
    responses(
        (status = 200, description = "The values", body = metrics::QueryResult,
            example = json!({
                "query": "monitorflow_disk_used_ratio{peer=\"web-1\",site=\"paris\"}",
                "series": [
                    {"labels": {"__name__": "monitorflow_disk_used_ratio", "instance": "/",
                                "peer": "web-1", "site": "paris"},
                     "points": [[1763112600, 0.41]]},
                    {"labels": {"__name__": "monitorflow_disk_used_ratio", "instance": "/var",
                                "peer": "web-1", "site": "paris"},
                     "points": [[1763112600, 0.97]]}
                ]
            })),
        (status = 400, description = "A bad metric name, instance or `lookback`, an unknown \
parameter, more than 500 series, or a query the metrics database refused: `invalid`", body = ErrorBody),
        (status = 404, description = "No such server, or outside the key's reach: `not_found`",
            body = ErrorBody),
        (status = 502, description = "The metrics database is unreachable or not configured: \
`metrics_unavailable`", body = ErrorBody),
        (status = 504, description = "The metrics database did not answer in time: `metrics_timeout`",
            body = ErrorBody),
    ))]
fn query() {}

#[utoipa::path(get, path = "/api/v1/metrics/query_range", tag = "metrics",
    summary = "Read a metric over time",
    description = "One metric of one server from `start` to `end` (the last hour by default), a point \
every `step` seconds (by default the range over 720, at least 1). Points are `[unix seconds, value]`; a \
value that is not a finite number is `null`.\n\n\
Without `rollup`, a point is the last sample before it, so a short spike between two points of a long \
range does not show. With `rollup` (`avg`, `max`, `min`, `last`), each point summarizes its series over \
the step before it, before `agg` combines the series: `rollup=max` keeps the spikes.\n\n\
**Who**: any role on the server's site. **Area**: `servers`.\n\n\
Ranges are at most 31 days and under 11,000 points per series; answers hold at most 500 series. Unknown \
parameters are 400.",
    params(metrics::RangeParams),
    responses(
        (status = 200, description = "The series", body = metrics::QueryResult,
            example = json!({
                "query": "max(max_over_time(monitorflow_cpu_usage_ratio{peer=\"web-1\",site=\"paris\",instance=\"total\"}[300s]))",
                "series": [
                    {"labels": {},
                     "points": [[1763109000, 0.22], [1763109300, 0.35], [1763109600, 0.91],
                                [1763109900, 0.30]]}
                ]
            })),
        (status = 400, description = "A bad metric name or instance, `start` not before `end`, a range \
over 31 days, too many points, a bad `step`, an unknown parameter, more than 500 series, or a query the \
metrics database refused: `invalid`", body = ErrorBody),
        (status = 404, description = "No such server, or outside the key's reach: `not_found`",
            body = ErrorBody),
        (status = 502, description = "The metrics database is unreachable or not configured: \
`metrics_unavailable`", body = ErrorBody),
        (status = 504, description = "The metrics database did not answer in time: `metrics_timeout`",
            body = ErrorBody),
    ))]
fn query_range() {}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    use super::super::v1;
    use super::*;

    const ROUTER: &str = include_str!("mod.rs");

    /// `(METHOD, /api/… path)` of the router's `.route(...)` calls.
    fn routes() -> BTreeSet<(String, String)> {
        let mut routes = BTreeSet::new();
        for (start, _) in ROUTER.match_indices(".route(") {
            let call = &ROUTER[start + ".route(".len()..];
            let mut depth = 1;
            let end = call
                .char_indices()
                .find(|&(_, c)| {
                    depth += match c {
                        '(' => 1,
                        ')' => -1,
                        _ => 0,
                    };
                    depth == 0
                })
                .map_or(call.len(), |(i, _)| i);
            let call = &call[..end];
            let Some(path) = call.split('"').nth(1) else {
                continue;
            };
            for method in ["get", "post", "put", "delete", "patch"] {
                let used = call.match_indices(&format!("{method}(")).any(|(i, _)| {
                    !call[..i]
                        .chars()
                        .next_back()
                        .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == ':')
                });
                if used {
                    routes.insert((method.to_uppercase(), path.to_owned()));
                }
            }
        }
        assert!(routes.len() > 100, "{routes:?}");
        routes
    }

    fn documented() -> BTreeSet<(String, String)> {
        let doc = build();
        let mut ops = BTreeSet::new();
        for (path, item) in &doc.paths.paths {
            let path = path.replacen("/api/v1/", "/api/", 1);
            for (method, op) in [
                ("GET", &item.get),
                ("POST", &item.post),
                ("PUT", &item.put),
                ("DELETE", &item.delete),
            ] {
                if op.is_some() {
                    ops.insert((method.to_owned(), path.clone()));
                }
            }
        }
        ops
    }

    #[test]
    fn documented_routes_exist() {
        let routes = routes();
        for op in documented() {
            assert!(routes.contains(&op), "{op:?} is documented but not routed");
        }
    }

    #[test]
    fn every_v1_route_is_documented_or_listed() {
        let documented = documented();
        for (method, path) in routes() {
            if !path.starts_with("/api/")
                || v1::left_out(&method, &path)
                || documented.contains(&(method.clone(), path.clone()))
            {
                continue;
            }
            let listed = NOT_YET_DOCUMENTED.iter().any(|p| path.starts_with(p))
                || (["/api/servers/{id}", "/api/sites/{id}", "/api/relays/{id}"]
                    .iter()
                    .any(|p| path.starts_with(p))
                    && NOT_YET_DOCUMENTED_UNDER.iter().any(|s| path.contains(s)));
            assert!(
                listed,
                "{method} {path} is in v1 but neither documented in api/openapi.rs nor in NOT_YET_DOCUMENTED"
            );
        }
    }

    #[test]
    fn the_committed_document_is_up_to_date() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/api/openapi.json");
        let generated = format!("{}\n", build().to_pretty_json().unwrap());
        if std::env::var_os("UPDATE_OPENAPI").is_some() {
            std::fs::write(&path, &generated).unwrap();
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            committed == generated,
            "docs/api/openapi.json is out of date; run `UPDATE_OPENAPI=1 cargo test -p monitorflow-server openapi` and review the change"
        );
    }

    #[test]
    fn every_reference_resolves_and_names_are_unique() {
        let doc = serde_json::to_value(build()).unwrap();
        let text = doc.to_string();
        let schemas = doc["components"]["schemas"].as_object().unwrap();
        for (i, _) in text.match_indices("#/components/schemas/") {
            let name: String = text[i + 21..].chars().take_while(|c| *c != '"').collect();
            assert!(
                schemas.contains_key(&name),
                "{name} is referenced but not defined"
            );
        }
        // Two types of one name would silently share a schema.
        for name in ["CreateRequest", "State", "Target", "Status"] {
            assert!(
                !schemas.contains_key(name),
                "{name}: give it a unique `#[schema(as = …)]`"
            );
        }
    }

    #[test]
    fn errors_list_every_code() {
        let doc = serde_json::to_value(build()).unwrap();
        let codes = &doc["components"]["schemas"]["Error"]["properties"]["code"]["enum"];
        assert_eq!(codes.as_array().unwrap().len(), ERROR_CODES.len());
        let op = &doc["paths"]["/api/v1/silences"]["post"];
        assert!(op["parameters"].to_string().contains("Idempotency-Key"));
        assert_eq!(
            op["responses"]["429"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/Error"
        );
    }
}
