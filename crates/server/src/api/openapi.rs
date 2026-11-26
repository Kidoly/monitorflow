//! The OpenAPI document of the public API (ADR-0101), generated from the
//! request and response types with `utoipa` and served at
//! `/api/v1/openapi.json`. The functions below only carry each route's
//! description; the router in `mod.rs` serves them.
//!
//! `docs/api/openapi.json` is this document committed. The tests fail
//! when it is out of date (`UPDATE_OPENAPI=1 cargo test -p
//! monitorflow-server openapi` rewrites it), when a documented route does
//! not exist, when a route is neither documented nor left out of v1 with
//! a reason ([`super::v1::why_left_out`]), and when the web interface
//! calls a path no route serves (`openapi_web_calls.rs`, ADR-0143).

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

#[path = "openapi_fleet.rs"]
mod fleet;
#[path = "openapi_integrations.rs"]
mod integrations;
#[path = "openapi_notifications.rs"]
mod notifications;
#[path = "openapi_on_call.rs"]
mod on_call;
#[cfg(test)]
#[path = "openapi_web_calls.rs"]
mod web_calls;

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
    doc.merge(super::openapi_system::SystemDoc::openapi());
    doc.merge(super::openapi_settings::SettingsDoc::openapi());
    doc.merge(super::openapi_users::UsersDoc::openapi());
    doc.merge(super::openapi_sso::SsoDoc::openapi());
    doc.merge(super::openapi_dashboards::DashboardsDoc::openapi());
    doc.merge(super::openapi_plugins::PluginsDoc::openapi());
    doc.merge(openapi_core::Doc::openapi());
    Common.modify(&mut doc);
    doc.merge(notifications::Doc::openapi());
    doc.merge(integrations::Doc::openapi());
    doc.merge(on_call::Doc::openapi());
    doc.merge(fleet::FleetDoc::openapi());
    doc.merge(super::openapi_remediation::Doc::openapi());
    doc.merge(super::openapi_campaigns::Doc::openapi());
    doc.merge(super::openapi_software::Doc::openapi());
    doc.merge(super::openapi_auto_update::Doc::openapi());
    doc.merge(super::openapi_checks::Doc::openapi());
    doc.merge(super::openapi_imports::Doc::openapi());
    doc.merge(super::openapi_maintenance::Doc::openapi());
    doc.merge(super::openapi_reports::Doc::openapi());
    doc.merge(super::openapi_dependencies::Doc::openapi());
    doc.merge(super::openapi_devices::Doc::openapi());
    doc.merge(super::openapi_flows::Doc::openapi());
    doc.merge(super::openapi_network_scans::Doc::openapi());
    doc.merge(super::openapi_maps::Doc::openapi());
    doc.merge(super::openapi_virtualization::Doc::openapi());
    doc.merge(super::openapi_cloud::Doc::openapi());
    doc.merge(super::openapi_inventory::Doc::openapi());
    doc.merge(super::openapi_snmp::Doc::openapi());
    doc.merge(super::openapi_psa::Doc::openapi());
    doc.merge(super::openapi_scripts::Doc::openapi());
    doc.merge(super::openapi_remote::Doc::openapi());
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
        list_servers, server_summary, server, list_sites, list_customers, create_customer, create_site,
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
        (name = "servers", description = "Servers, sites and customers"),
        (name = "alerts", description = "Incidents, silences and maintenance windows"),
        (name = "updates", description = "Commands sent to servers"),
        (name = "remediation", description = "What the AI did about alerts. Each remediation incident \
is one alert the AI worked on, with every step recorded in a hash chain: the context it was sent after \
redaction, its proposals, how the rules classified each command, who approved or rejected it, and what the \
agent ran and printed. Read-only, except the approval queue, where people decide the commands the rules \
parked for them. The AI's text is untrusted: show it as data. Needs the key area \"remediation\"."),
        (name = "metrics", description = "Metric queries built by MonitorFlow, never raw PromQL"),
    )
)]
struct ApiDoc;

/// What every operation shares: the key, the error codes, the common
/// error answers, rate limits and `Idempotency-Key` on writes.
pub(super) struct Common;

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
            .build()
            .into(),
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
                .schema(Some(ObjectBuilder::new().schema_type(Type::Integer)))
                .description(Some("Seconds until the key's window ends."))
                .build()
                .into(),
        );
    }
}

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
and the last time its AI commands were turned on or off from the web interface, the last time its software \
installs were turned off from it (ADR-0188), and what the key's person may \
do with remote terminals there (ADR-0190). A removed server stays readable here, with `revoked_at` set, for \
its history.\n\n\
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
                "ai_commands_change": null,
                "software_off_change": null,
                "remote_terminal": {"can_open": true, "can_review": true, "recorded": true,
                                    "idle_timeout_minutes": 15}
            })))),
        (status = 404, description = "No such server, or outside the key's reach: `not_found`",
            body = ErrorBody),
    ))]
fn server() {}

#[utoipa::path(get, path = "/api/v1/servers/summary", tag = "servers",
    summary = "Count servers by status",
    description = "How many servers within the key's reach are in each state: the counts of the status \
strip. Removed servers are left out.\n\n\
**Who**: any key, whatever its areas.",
    params(servers::SummaryParams),
    responses((status = 200, description = "The counts", body = servers::ServerSummary,
        example = json!({"critical": 1, "warning": 2, "acknowledged": 0, "silenced": 1, "offline": 0,
            "ok": 38, "total": 42}))))]
fn server_summary() {}

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

#[utoipa::path(get, path = "/api/v1/incidents", tag = "alerts",
    summary = "List incidents",
    description = "One page of the incidents of the servers and relays within the key's reach: open ones \
first, then the newest first. An incident is opened when a threshold is crossed (`metric` is then the \
metric) or when a server or relay stops reporting (`metric` is `offline`).\n\n\
**Who**: any role. **Area**: `alerts`.\n\n\
**Filters**, all optional and ANDed: `state`, `severity` (the current one), `server_id`, `site_id`, `from` \
and `to` (incidents active at some point between them), and `group` (the incidents one notification sent \
together, from the link in that notification). A server or site outside the key's reach gives an empty \
list. Unknown parameters are 400.\n\n\
**Pages**: `limit` incidents per page, 1 to 500, 100 by default; pass `next_cursor` as `cursor` with the \
same filters for the next page, `null` on the last one.\n\n\
A PSA polling for new alerts can ask for `state=open` every minute, and acknowledge what it took.",
    params(incidents::ListParams),
    responses(
        (status = 200, description = "One page of incidents", body = incidents::IncidentList,
            example = json!({"incidents": [ex::incident()], "next_cursor": null})),
        (status = 400, description = "A bad filter value, an unknown parameter, `from` after `to`, \
`limit` out of range or a bad `cursor`: `invalid`", body = ErrorBody),
    ))]
fn list_incidents() {}

#[utoipa::path(get, path = "/api/v1/incidents/{id}", tag = "alerts",
    summary = "Get an incident",
    description = "One incident with its timeline (`events`, in order), the notifications that sent it \
together with others, where it was acknowledged, and its follow-up: who is told next if nobody \
acknowledges it.\n\n\
**Who**: any role on the site of its server or relay. **Area**: `alerts`.",
    params(("id" = uuid::Uuid, Path, description = "The incident")),
    responses(
        (status = 200, description = "The incident and its timeline", body = incidents::IncidentDetail,
            example = json!(ex::merge(ex::incident(), json!({
                "events": [
                    {"id": 1201, "kind": "opened", "severity": "warning", "value": 0.88,
                     "occurred_at": "2026-11-14T08:41:00Z", "recorded_at": "2026-11-14T08:41:02Z",
                     "details": {}},
                    {"id": 1207, "kind": "escalated", "severity": "critical", "value": 0.96,
                     "occurred_at": "2026-11-14T09:02:00Z", "recorded_at": "2026-11-14T09:02:01Z",
                     "details": {}},
                    {"id": 1208, "kind": "followed_up", "severity": "critical", "value": null,
                     "occurred_at": "2026-11-14T09:17:00Z", "recorded_at": "2026-11-14T09:17:00Z",
                     "details": {"step": 1, "steps": 2, "minutes": 15, "told": ["bob"]}}
                ],
                "notification_groups": [
                    {"id": ex::GROUP, "incidents": 2, "created_at": "2026-11-14T09:02:05Z"}
                ],
                "acknowledged_via": null,
                "follow_up": {
                    "state": "waiting", "stop_reason": null, "step": 1, "steps": 2,
                    "started_at": "2026-11-14T09:02:05Z", "next_at": "2026-11-14T09:32:05Z",
                    "next": ["carol"], "policy": "customer"
                }
            })))),
        (status = 404, description = "No such incident, or outside the key's reach: `not_found`",
            body = ErrorBody),
    ))]
fn incident() {}

#[utoipa::path(post, path = "/api/v1/incidents/acknowledge", tag = "alerts",
    summary = "Acknowledge incidents",
    description = "Says someone is on these incidents, so the follow-up stops and nobody else is told next. \
The incidents stay open until what they measure recovers; acknowledging changes nothing else.\n\n\
**Who**: operator or above (\"Read and change\" access) on the site of every incident. **Area**: `alerts`. \
They are acknowledged as the key's maker.\n\n\
The answer lists the incidents this request acknowledged. The others were resolved, or acknowledged \
already, by anyone. All are checked before anything is written: one that is not found makes the whole \
request 404, one the key may only read 403. Each one acknowledged is audited as `incident.acknowledge`. \
Safe to repeat: a second time acknowledges nothing more.",
    request_body(content = incidents::AcknowledgeRequest,
        example = json!({"incident_ids": [ex::INCIDENT, ex::INCIDENT_2]})),
    responses(
        (status = 200, description = "What this request acknowledged", body = incidents::Acknowledged,
            example = json!({"acknowledged": [ex::INCIDENT]})),
        (status = 400, description = "No incident, or more than 500: `invalid`", body = ErrorBody),
        (status = 403, description = "The key may only read the site of one of them: `forbidden`; or \
outside the key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "One of them does not exist or is outside the key's reach: \
`not_found`", body = ErrorBody),
    ))]
fn acknowledge() {}

#[utoipa::path(get, path = "/api/v1/silences", tag = "alerts",
    summary = "List silences",
    description = "The silences that are active or yet to start, the first to start first, in full. A \
silence stops notifications about a server, a relay, a site, a customer or everything until it ends, \
or about chosen `checks` of a server or site; incidents still open and show, and what is still open at its \
end is sent then. A maintenance window's running occurrence is listed with its `window_id`; its coming ones \
are in `GET /api/v1/maintenance-windows`.\n\n\
**Who**: any role. **Area**: `alerts`. The key sees the silences of the sites it reaches, of their \
customers, and those covering everything.\n\n\
**Filter**: at most one of `server_id`, `relay_id` and `site_id`, for the silences covering it: its own, \
its site's, its customer's and those covering everything. Unknown parameters are 400.",
    params(silences::ListParams),
    responses(
        (status = 200, description = "Active and upcoming silences", body = silences::SilenceList,
            example = json!({"silences": [ex::silence()]})),
        (status = 400, description = "More than one filter, or an unknown parameter: `invalid`",
            body = ErrorBody),
        (status = 404, description = "The server, relay or site of the filter does not exist or is \
outside the key's reach: `not_found`", body = ErrorBody),
    ))]
fn list_silences() {}

#[utoipa::path(post, path = "/api/v1/silences", tag = "alerts",
    summary = "Create a silence",
    description = "Stops notifications about a server, a relay, a site, a customer or everything, from \
`starts_at` (now when absent or past) until `ends_at`, at most 30 days later. Use it around a deployment \
or maintenance. With `checks` (1 to 20, a server or a site only), only the incidents those checks write \
are held: the server going offline and its other checks still notify, and the server is not shown as \
silenced. Each check must run there: built in, or set on the server, its site, or a server of the site.\n\n\
**Who**, by `scope`: for `server`, `relay` or `site`, operator or above on its site; for `customer`, \
operator or above on the customer as a whole; for `all`, a global administrator. **Area**: `alerts`. \
`target_id` is required, except for `all`, which takes none.\n\n\
`reason` (1 to 200 characters) shows to others, so they know what the silence is for. Audited as \
`silence.create`. The same request again while its silence runs or waits (same target, `ends_at` and \
`reason`) answers that silence with 200 and makes no second one. Otherwise **not safe to repeat**: each \
request makes a silence; send `Idempotency-Key` to retry.",
    request_body(content = silences::CreateRequest, example = json!({
        "scope": "server", "target_id": ex::SERVER,
        "starts_at": "2026-11-14T22:00:00Z", "ends_at": "2026-11-15T00:00:00Z",
        "reason": "Kernel upgrade and reboot"
    })),
    responses(
        (status = 201, description = "The new silence", body = silences::SilenceView,
            example = json!(ex::silence())),
        (status = 200, description = "The same silence, already there: nothing was created",
            body = silences::SilenceView, example = json!(ex::silence())),
        (status = 400, description = "No `target_id` (or one for `all`), no reason or a reason over 200 \
characters, an end not after the start, or longer than 30 days, checks for another scope, unknown checks \
or over 20: `invalid`", body = ErrorBody),
        (status = 403, description = "The key may only read the target, or may not silence \
everything: `forbidden`; or outside the key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "The target does not exist or is outside the key's reach: \
`not_found`", body = ErrorBody),
    ))]
fn create_silence() {}

#[utoipa::path(post, path = "/api/v1/silences/{id}/end", tag = "alerts",
    summary = "End a silence",
    description = "Ends a silence now, or calls it off if it has not started. What it held back and is \
still open is sent within seconds. For an occurrence of a maintenance window, this skips that one \
window; the next comes as planned.\n\n\
**Who**: whoever could create it: operator or above on its site, or on its customer as a whole, or a \
global administrator for one covering everything. **Area**: `alerts`.\n\n\
Audited as `silence.end`. A second request is 409, as the silence is over.",
    params(("id" = uuid::Uuid, Path, description = "The silence")),
    responses(
        (status = 204, description = "Ended"),
        (status = 403, description = "The key may not end it: `forbidden`; or outside the key's \
areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such silence, or one the key does not see: `not_found`",
            body = ErrorBody),
        (status = 409, description = "The silence is already over: `conflict`", body = ErrorBody),
    ))]
fn end_silence() {}

#[utoipa::path(get, path = "/api/v1/servers/{id}/commands", tag = "updates",
    summary = "List a server's commands",
    description = "One page of the commands sent to a server, the newest first, without their output: \
package and agent updates, package source changes, AI commands and snapshots, and campaign steps. Read one for its \
output.\n\n\
**Who**: any role on the server's site. **Area**: `updates`.\n\n\
**Pages**: `limit` commands per page, 1 to 500, 100 by default; pass `next_cursor` as `cursor` for the \
next page, `null` on the last one. Unknown parameters are 400.",
    params(("id" = uuid::Uuid, Path, description = "The server"), commands::ListParams),
    responses(
        (status = 200, description = "One page of commands", body = commands::CommandList,
            example = json!({"commands": [ex::command()], "next_cursor": null})),
        (status = 400, description = "`limit` out of range, a bad `cursor` or an unknown parameter: \
`invalid`", body = ErrorBody),
        (status = 404, description = "No such server, or outside the key's reach: `not_found`",
            body = ErrorBody),
    ))]
fn list_commands() {}

#[utoipa::path(post, path = "/api/v1/servers/{id}/commands", tag = "updates",
    summary = "Queue a command",
    description = "Queues a command for the server's agent. It is sent as soon as the agent is connected and \
has no other command running, and the agent may start it until `ttl_s` seconds have passed (60 to 604800, \
one hour by default); after that it is `expired`. Poll `GET /api/v1/commands/{id}` for its end.\n\n\
- `apply_updates` installs package updates: `packages` (up to 1000 names; empty or absent for every \
update), `security_only` and `allow_reboot`. Package names are letters, digits and `.+_:-`, not starting \
with `-`; on Windows servers, KB numbers (`KB5034441`) or update IDs. On Windows servers, \
`classifications` approves Windows Update classifications by English name (`Security Updates`, \
`Critical Updates`, `Definition Updates`, `Update Rollups`, `Updates`, `Feature Packs`, `Service Packs`, \
`Tools`, `Upgrades`; empty: every one but `Upgrades`), `hold` lists updates never installed, and each \
`third_party` entry may `hold` apps. An agent too old for them gets a 400 rather than an update that \
ignores them.\n\
- `software` installs or removes software from the catalogue (`/software`) on a Windows server whose \
administrator allowed it (`monitorflow-agent.exe software on`): `software` is `{\"action\": \"install\" | \
\"remove\", \"catalog_ids\": [id]}`, with `allow_reboot`.\n\
- `self_update` installs another agent version: `version` (like `0.9.3`, required), `allow_downgrade`, and \
`health_timeout_s` (30 to 3600, 300 by default), how long the new agent has to connect before the old \
one comes back.\n\n\
Fields of the other kind are 400. The agent refuses kinds it does not accept (`command_kinds` of the \
server); the command then ends `failed`.\n\n\
**Who**: an administrator of the server's customer as a whole: a global administrator or one of that \
customer. A site administrator may not, and neither may a key that reaches one site. **Area**: \
`updates`.\n\n\
Audited as `command.create`. **Not safe to repeat**: each request queues a command; send \
`Idempotency-Key` to retry.",
    params(("id" = uuid::Uuid, Path, description = "The server")),
    request_body(content = commands::CreateRequest, examples(
        ("apply_updates" = (summary = "Install two package updates",
            value = json!({"kind": "apply_updates", "packages": ["openssl", "libssl3"], "ttl_s": 3600}))),
        ("self_update" = (summary = "Update the agent",
            value = json!({"kind": "self_update", "version": "0.9.3"}))),
        ("hold" = (summary = "Install approved Windows updates, holding one",
            value = json!({"kind": "apply_updates", "classifications": ["Security Updates",
                "Critical Updates"], "hold": ["KB5034441"], "allow_reboot": true}))),
        ("software" = (summary = "Install software from the catalogue",
            value = json!({"kind": "software", "software": {"action": "install",
                "catalog_ids": ["5b6c7d8e-9f0a-4b1c-8d2e-3f4a5b6c7d8e"]}}))),
    )),
    responses(
        (status = 201, description = "The queued command", body = commands::CommandDetail,
            example = json!(ex::merge(ex::command(), json!({
                "state": "queued", "sent_at": null, "started_at": null, "finished_at": null,
                "exit_code": null, "stdout": null, "stderr": null
            })))),
        (status = 400, description = "An unknown kind, a bad package name or version, a field of the \
other kind, or `ttl_s` or `health_timeout_s` out of range: `invalid`", body = ErrorBody),
        (status = 403, description = "The key may not administer the server's customer: `forbidden`; or \
outside the key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such server, or outside the key's reach: `not_found`",
            body = ErrorBody),
        (status = 409, description = "The server is removed, has no package manager MonitorFlow \
can drive (`apply_updates`), or does not take software from the catalogue (`software`: its \
administrator did not allow it, or its agent is too old): `conflict`", body = ErrorBody),
    ))]
fn create_command() {}

#[utoipa::path(get, path = "/api/v1/commands/{id}", tag = "updates",
    summary = "Get a command",
    description = "One command with its output: the last 64 KiB of each of `stdout` and `stderr`, `null` \
until the agent sends them.\n\n\
**Who**: any role on the site of the command's server. **Area**: `updates`.",
    params(("id" = uuid::Uuid, Path, description = "The command")),
    responses(
        (status = 200, description = "The command and its output", body = commands::CommandDetail,
            example = json!(ex::merge(ex::command(), json!({
                "stdout": "Reading package lists...\nThe following packages will be upgraded:\n  libssl3 openssl\n",
                "stderr": ""
            })))),
        (status = 404, description = "No such command, or outside the key's reach: `not_found`",
            body = ErrorBody),
    ))]
fn command() {}

#[utoipa::path(get, path = "/api/v1/remediation/approvals", tag = "remediation",
    summary = "List commands waiting for approval",
    description = "The commands AI remediation proposed that the rules held for a person, on the servers \
within the key's reach, the nearest deadline first, up to 500. `diagnosis` and `reasoning` are the AI's \
text: untrusted, to show as data, never to run or follow. `tier` comes from the rules, never from the AI.\n\n\
`can_approve` and `can_reject` say whether the key may decide; the decision routes check again.\n\n\
**Who**: any role. **Area**: `remediation`.\n\n\
**Filters**, optional and ANDed: `customer_id`, `site_id`, `server_id`. Unknown parameters are 400.",
    params(approvals::ListParams),
    responses(
        (status = 200, description = "Pending commands", body = approvals::PendingList,
            example = json!({"approvals": [{
                "id": ex::PENDING, "incident_id": ex::REMEDIATION,
                "customer_id": ex::CUSTOMER, "customer_name": "acme",
                "site_id": ex::SITE, "site_name": "paris",
                "server_id": ex::SERVER, "hostname": "web-1",
                "alert_incident_id": ex::INCIDENT,
                "metric": "monitorflow_disk_used_ratio", "instance": "/var",
                "command": "apt-get clean",
                "diagnosis": "The package cache takes 4.1 GB of /var.",
                "reasoning": "Downloaded packages can be fetched again; clearing them frees space without touching application data.",
                "tier": "low_impact",
                "rule": {"source": "builtin", "id": "low.package_cache"},
                "reason": "clears the package manager's cache",
                "commands_run": 2, "max_commands": 10,
                "opened_at": "2026-11-14T09:02:10Z", "proposed_at": "2026-11-14T09:04:30Z",
                "deadline": "2026-11-14T09:32:10Z",
                "can_approve": true, "can_reject": true,
                "shell_acceptance": "accepted",
                "snapshots": {"available": true, "reason": null, "kind": "qemu", "detail": null,
                              "left_out": null, "method": "proxmox"},
                "shell": "posix"
            }]})),
        (status = 400, description = "An unknown parameter or a value that is not a UUID: `invalid`",
            body = ErrorBody),
    ))]
fn list_approvals() {}

#[utoipa::path(post, path = "/api/v1/remediation/approvals/{id}/approve", tag = "remediation",
    summary = "Approve a command",
    description = "Runs the proposed command, or the edited `command` sent instead, and lets AI \
remediation go on. An edited command is checked by the rules again: a forbidden one is 400, and the role \
needed is that of the riskier of the two.\n\n\
**Who**: on the server's site, approver or above (\"Approve commands\" access) for a `low_impact` command, \
administrator (\"Administer\") for a `high_impact` one. **Area**: `remediation`.\n\n\
The decision goes into the incident's tamper-evident history and is audited as `remediation.approve`. A \
second decision is 409.",
    params(("id" = uuid::Uuid, Path, description = "The pending command, `id` of the listing")),
    request_body(content = approvals::ApproveRequest, examples(
        ("as_proposed" = (summary = "Run it as proposed", value = json!({}))),
        ("edited" = (summary = "Run an edited command",
            value = json!({"command": "apt-get autoclean"}))),
    )),
    responses(
        (status = 200, description = "Approved", body = approvals::DecisionView,
            example = json!({"id": ex::PENDING, "incident_id": ex::REMEDIATION, "state": "approved",
                             "command": "apt-get autoclean", "edited": true})),
        (status = 400, description = "The edited command is empty, over 4096 bytes, has NUL bytes or \
is forbidden by the rules: `invalid`", body = ErrorBody),
        (status = 403, description = "The key's access is too low for this command's tier: \
`forbidden`; or outside the key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such pending command, or outside the key's reach: \
`not_found`", body = ErrorBody),
        (status = 409, description = "Already decided, the incident no longer waits, its deadline \
passed, or the server's agent refuses AI commands: `conflict`", body = ErrorBody),
    ))]
fn approve() {}

#[utoipa::path(post, path = "/api/v1/remediation/approvals/{id}/reject", tag = "remediation",
    summary = "Reject a command",
    description = "Drops the proposed command. `reason` (1 to 1000 characters) goes back to the AI, which \
may propose something else or hand the incident to a person.\n\n\
**Who**: approver or above (\"Approve commands\" access) on the server's site, whatever the tier. **Area**: \
`remediation`.\n\n\
The decision goes into the incident's history and is audited as `remediation.reject`. A second decision \
is 409.",
    params(("id" = uuid::Uuid, Path, description = "The pending command, `id` of the listing")),
    request_body(content = approvals::RejectRequest,
        example = json!({"reason": "Packages are cached for an offline install tomorrow; free space elsewhere."})),
    responses(
        (status = 200, description = "Rejected", body = approvals::DecisionView,
            example = json!({"id": ex::PENDING, "incident_id": ex::REMEDIATION, "state": "rejected",
                             "command": "apt-get clean", "edited": false})),
        (status = 400, description = "No reason, or one over 1000 characters: `invalid`",
            body = ErrorBody),
        (status = 403, description = "The key's access is below \"Approve commands\": `forbidden`; or \
outside the key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such pending command, or outside the key's reach: \
`not_found`", body = ErrorBody),
        (status = 409, description = "Already decided, the incident no longer waits, or its deadline \
passed: `conflict`", body = ErrorBody),
    ))]
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
    pub(super) fn routes() -> BTreeSet<(String, String)> {
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

    /// `(METHOD, /api/… path)` of the operations in the document.
    pub(super) fn documented() -> BTreeSet<(String, String)> {
        let doc = build();
        let mut ops = BTreeSet::new();
        for (path, item) in &doc.paths.paths {
            let path = path.replacen("/api/v1/", "/api/", 1);
            for (method, op) in [
                ("GET", &item.get),
                ("POST", &item.post),
                ("PUT", &item.put),
                ("DELETE", &item.delete),
                ("PATCH", &item.patch),
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

    /// Every route is in the public API and its document, or left out
    /// of v1 with a reason; never both (ADR-0143).
    #[test]
    fn every_route_is_documented_or_left_out_with_a_reason() {
        let documented = documented();
        for (method, path) in routes() {
            if !path.starts_with("/api/") {
                continue;
            }
            let op = (method.clone(), path.clone());
            match v1::why_left_out(&method, &path) {
                Some(why) => {
                    assert!(!why.is_empty(), "{method} {path}: say why it is left out");
                    assert!(
                        !documented.contains(&op),
                        "{method} {path} is left out of v1 but documented"
                    );
                }
                None => assert!(
                    documented.contains(&op),
                    "{method} {path} is in the public API but not documented: describe it in \
                     an api/openapi_*.rs file (everything the web interface does must be in the API)"
                ),
            }
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

    /// What an integrator needs of each operation: a summary, a
    /// description, and an example of each body it sends or gets back.
    #[test]
    fn every_operation_is_described_with_examples() {
        let doc = serde_json::to_value(build()).unwrap();
        // Only JSON bodies need one: a multipart upload has no useful example.
        let has_example = |content: &serde_json::Value| {
            content.get("application/json").is_none()
                || content["application/json"].get("example").is_some()
                || content["application/json"].get("examples").is_some()
        };
        for (path, item) in doc["paths"].as_object().unwrap() {
            for (method, op) in item.as_object().unwrap() {
                let at = format!("{method} {path}");
                for field in ["summary", "description"] {
                    let text = op[field].as_str().unwrap_or_default();
                    assert!(!text.is_empty(), "{at} has no {field}");
                }
                assert!(
                    !op["summary"].as_str().unwrap().contains('\n'),
                    "{at}: keep the summary to one line"
                );
                if let Some(body) = op.get("requestBody") {
                    assert!(has_example(&body["content"]), "{at}: no request example");
                }
                for (status, response) in op["responses"].as_object().unwrap() {
                    // OpenAPI requires it; utoipa 6 no longer writes an empty one.
                    assert!(
                        response.get("$ref").is_some() || response.get("description").is_some(),
                        "{at}: no description for {status}"
                    );
                    if status.starts_with('2') {
                        if let Some(content) = response.get("content") {
                            assert!(has_example(content), "{at}: no example for {status}");
                        }
                    }
                }
            }
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
