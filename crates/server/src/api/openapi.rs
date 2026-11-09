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
use utoipa::openapi::path::{Operation, ParameterBuilder, ParameterIn};
use utoipa::openapi::schema::{ObjectBuilder, Type};
use utoipa::openapi::security::{Http, HttpAuthScheme, SecurityScheme};
use utoipa::openapi::{OpenApi as Document, RefOr, Required, ResponseBuilder};
use utoipa::{Modify, OpenApi, ToSchema};

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
    ApiDoc::openapi()
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "MonitorFlow public API",
        version = "1",
        description = "Scripts and integrations use `/api/v1` with an API key made under Admin › API keys: \
`Authorization: Bearer mfk_…`. A key acts as the administrator who made it, within its reach (everything, \
a customer or a site), its access and its areas. Lists that can grow take `limit` and `cursor` and answer \
`next_cursor`, null on the last page. Every error has `error`, plain words, and `code`, stable. Writes may \
carry `Idempotency-Key`; the same request again within 24 hours gets the first answer with \
`Idempotent-Replayed: true`. A key makes up to 600 requests a minute; answers carry `RateLimit-Limit`, \
`RateLimit-Remaining` and `RateLimit-Reset`, and 429 carries `Retry-After`. See ADR-0101.",
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
    modifiers(&Common),
    tags(
        (name = "servers", description = "Servers, sites and customers"),
        (name = "alerts", description = "Incidents and silences"),
        (name = "updates", description = "Commands sent to servers"),
        (name = "remediation", description = "Commands the AI proposed, waiting for a person"),
        (name = "metrics", description = "Metric queries built by MonitorFlow, never raw PromQL"),
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
            SecurityScheme::Http(Http::new(HttpAuthScheme::Bearer)),
        );
        components.schemas.insert(
            "Error".into(),
            ObjectBuilder::new()
                .property("error", ObjectBuilder::new().schema_type(Type::String))
                .property(
                    "code",
                    ObjectBuilder::new()
                        .schema_type(Type::String)
                        .enum_values(Some(ERROR_CODES)),
                )
                .required("error")
                .required("code")
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
                 within 24 hours gets the first answer instead of running twice.",
            ))
            .schema(Some(
                ObjectBuilder::new()
                    .schema_type(Type::String)
                    .max_length(Some(255)),
            ))
            .build(),
    );
}

fn errors(operation: &mut Operation) {
    for (status, description) in [
        (
            "400",
            "Invalid parameters or body: `invalid` or `invalid_request`",
        ),
        (
            "401",
            "No key, or a key that is unknown, expired or turned off: `unauthenticated`",
        ),
        (
            "403",
            "The key's role or areas do not allow it: `forbidden` or `api_key_scope`",
        ),
        ("404", "Not found, or outside the key's reach: `not_found`"),
        (
            "429",
            "Over the key's rate limit: `rate_limited`, with `Retry-After`",
        ),
    ] {
        operation
            .responses
            .responses
            .entry(status.into())
            .or_insert_with(|| {
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
            });
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
    "/api/customers/{id}",
    "/api/metrics/",
    "/api/silences/{id}",
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

#[utoipa::path(get, path = "/api/v1/servers", tag = "servers", params(servers::ListParams),
    responses((status = 200, body = servers::ServerList)))]
fn list_servers() {}

#[utoipa::path(get, path = "/api/v1/servers/{id}", tag = "servers", params(("id" = uuid::Uuid, Path)),
    responses((status = 200, body = servers::ServerDetail)))]
fn server() {}

#[utoipa::path(get, path = "/api/v1/sites", tag = "servers",
    responses((status = 200, body = sites::SiteList)))]
fn list_sites() {}

#[utoipa::path(get, path = "/api/v1/customers", tag = "servers",
    responses((status = 200, body = customers::CustomerList)))]
fn list_customers() {}

#[utoipa::path(post, path = "/api/v1/customers", tag = "servers", request_body = customers::NameRequest,
    responses((status = 201, body = customers::CustomerView), (status = 409, body = ErrorBody)))]
fn create_customer() {}

#[utoipa::path(post, path = "/api/v1/customers/{id}/sites", tag = "servers",
    params(("id" = uuid::Uuid, Path)), request_body = customers::NameRequest,
    responses((status = 201, body = sites::SiteView), (status = 409, body = ErrorBody)))]
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

#[utoipa::path(get, path = "/api/v1/metrics/query", tag = "metrics", params(metrics::InstantParams),
    responses((status = 200, body = metrics::QueryResult)))]
fn query() {}

#[utoipa::path(get, path = "/api/v1/metrics/query_range", tag = "metrics", params(metrics::RangeParams),
    responses((status = 200, body = metrics::QueryResult)))]
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
