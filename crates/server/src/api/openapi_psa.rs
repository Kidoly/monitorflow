//! The PSA connection in the OpenAPI document (ADR-0164, ADR-0166):
//! merged into it by [`super::openapi::build`].

#![allow(dead_code)] // The functions exist for their `#[utoipa::path]`.

use serde_json::{json, Value};
use utoipa::OpenApi;

use super::openapi::{Common, ErrorBody};
use super::psa;

#[derive(OpenApi)]
#[openapi(
    paths(get_psa_connection, put_psa_connection, delete_psa_connection, test_psa_connection,
          list_psa_clients, list_psa_client_sites, list_psa_site_links, put_psa_site_link, delete_psa_site_link,
          list_psa_ticket_types, list_psa_categories, list_psa_statuses, put_psa_ticket_settings, list_psa_tickets),
    components(schemas(ErrorBody)),
    modifiers(&Common),
    tags((name = "psa", description = "The PSA connection: MonitorFlow calls a PSA's API with credentials \
it keeps, HaloPSA first. One per installation, managed by global administrators (a key with every reach), \
since one PSA serves every customer. In HaloPSA, create an API application (Configuration, Integrations, \
HaloPSA API, View Applications) with the authentication method Client ID and Secret (Client Credentials), \
acting as a dedicated agent, with the permission `all`; MonitorFlow asks for a token on `{base}/auth/token` \
with the scope `all` and calls `{base}/api`. The Client Secret is write-only: it is sealed at rest and never \
answered. Each MonitorFlow site is then matched to a PSA client, and optionally one of its sites, chosen \
from the PSA's lists read through the connection; incidents of a site without a match open no ticket. An incident at the chosen severity then opens one \
ticket for its site's client, with the chosen ticket type and category, and its resolution adds a private \
note and moves the ticket to the chosen status. \
When the PSA refuses or does not answer, the answer is 502 `psa_unavailable` with its own words. Every change is in the audit log, with the key's ID. API keys need the `administration` area."))
)]
pub(super) struct Doc;

fn connection() -> Value {
    json!({
        "kind": "halopsa",
        "base_url": "https://halo.example.com",
        "tenant": null,
        "client_id": "5b1d0c4e-7a2f-4e6b-9c3d-2f1e0a9b8c7d",
        "secret_error": null,
        "enabled": true,
        "agent_name": "MonitorFlow",
        "last_success_at": "2026-11-22T09:30:00Z",
        "last_error": null,
        "last_error_at": null,
        "created_at": "2026-11-22T09:00:00Z",
        "updated_at": "2026-11-22T09:00:00Z",
        "tickets": {
            "ticket_type_id": 1,
            "ticket_type_name": "Incident",
            "category": "Hardware>Server",
            "min_severity": "critical",
            "resolved_status_id": 9,
            "resolved_status_name": "Closed",
        },
    })
}

/// Read the PSA connection
///
/// The connection without its Client Secret, or `null` when none is set
/// up. Global administrators only.
#[utoipa::path(get, path = "/api/v1/psa/connection", tag = "psa",
    responses(
        (status = 200, description = "The connection, or `null`", body = psa::ConnectionState, example = json!({
            "connection": connection(),
        })),
        (status = 403, description = "Not a global administrator: `forbidden`; `api_key_scope`", body = ErrorBody),
    ))]
fn get_psa_connection() {}

/// Set up or change the PSA connection
///
/// Sets the address, the tenant of a hosted instance, the Client ID and
/// the Client Secret, and whether MonitorFlow uses the connection. Saving
/// tests nothing: test first with the same body. An empty or absent
/// `client_secret` keeps the stored one, only for the same address. A new
/// address or Client ID clears the last success and error. Audited as
/// `psa.connection.create` or `psa.connection.update`, without the secret.
/// Safe to repeat.
#[utoipa::path(put, path = "/api/v1/psa/connection", tag = "psa",
    request_body(content = psa::ConnectionRequest, example = json!({
        "kind": "halopsa",
        "base_url": "https://halo.example.com",
        "tenant": null,
        "client_id": "5b1d0c4e-7a2f-4e6b-9c3d-2f1e0a9b8c7d",
        "client_secret": "the application's secret",
        "enabled": true,
    })),
    responses(
        (status = 200, description = "Saved", body = psa::ConnectionView, example = json!(connection())),
        (status = 400, description = "`invalid`: an address that is not http or https or holds a user, a missing Client ID, a missing secret for a new address, another `kind`", body = ErrorBody),
        (status = 403, description = "Not a global administrator: `forbidden`; `api_key_scope`", body = ErrorBody),
    ))]
fn put_psa_connection() {}

/// Remove the PSA connection
///
/// MonitorFlow stops calling the PSA. Audited as `psa.connection.delete`.
#[utoipa::path(delete, path = "/api/v1/psa/connection", tag = "psa",
    responses(
        (status = 204, description = "Removed"),
        (status = 403, description = "Not a global administrator: `forbidden`; `api_key_scope`", body = ErrorBody),
        (status = 404, description = "`not_found`: no connection is set up", body = ErrorBody),
    ))]
fn delete_psa_connection() {}

/// Test the PSA connection
///
/// Signs in to the PSA and reads the agent the application acts as,
/// read-only: no ticket is created. The body's values are tested, the
/// stored ones filling what it leaves out, so a form can be tested before
/// it is saved; `{}` tests the stored connection and records the outcome
/// on it. A failure is answered with 200 and the PSA's own words.
#[utoipa::path(post, path = "/api/v1/psa/connection/test", tag = "psa",
    request_body(content = psa::ConnectionRequest, example = json!({
        "base_url": "https://halo.example.com",
        "client_id": "5b1d0c4e-7a2f-4e6b-9c3d-2f1e0a9b8c7d",
        "client_secret": "the application's secret",
    })),
    responses(
        (status = 200, description = "Whether it worked", body = psa::TestResult, example = json!({
            "ok": false,
            "agent_name": null,
            "error": "halo.example.com answered 401 Unauthorized to the sign-in: invalid_client. Check the Client ID and Secret, and that the application uses Client Credentials.",
        })),
        (status = 400, description = "`invalid`: as when saving", body = ErrorBody),
        (status = 403, description = "Not a global administrator: `forbidden`; `api_key_scope`", body = ErrorBody),
    ))]
fn test_psa_connection() {}

/// Search the PSA's clients
///
/// The PSA's active clients whose name holds `search`, by name, at most
/// 50, read through the connection. Global administrators only.
#[utoipa::path(get, path = "/api/v1/psa/clients", tag = "psa",
    params(psa::ClientSearch),
    responses(
        (status = 200, description = "The clients", body = psa::RecordList, example = json!({
            "records": [{"id": 10, "name": "Example Industries"}, {"id": 11, "name": "Example Logistics"}],
        })),
        (status = 403, description = "Not a global administrator: `forbidden`; `api_key_scope`", body = ErrorBody),
        (status = 409, description = "`conflict`: no connection is set up", body = ErrorBody),
        (status = 502, description = "`psa_unavailable`: the PSA refused or did not answer; `error` has its words", body = ErrorBody),
    ))]
fn list_psa_clients() {}

/// List a PSA client's sites
///
/// The sites of one PSA client, by name. Global administrators only.
#[utoipa::path(get, path = "/api/v1/psa/clients/{id}/sites", tag = "psa",
    params(("id" = i64, Path, description = "The PSA client's id")),
    responses(
        (status = 200, description = "Its sites", body = psa::RecordList, example = json!({
            "records": [{"id": 100, "name": "Main office"}, {"id": 101, "name": "Warehouse"}],
        })),
        (status = 403, description = "Not a global administrator: `forbidden`; `api_key_scope`", body = ErrorBody),
        (status = 409, description = "`conflict`: no connection is set up", body = ErrorBody),
        (status = 502, description = "`psa_unavailable`: the PSA refused or did not answer", body = ErrorBody),
    ))]
fn list_psa_client_sites() {}

fn site_link() -> Value {
    json!({
        "site_id": "9a3c1e52-6b7d-4f8e-a1b2-c3d4e5f60718",
        "site_name": "paris",
        "customer_id": "4f7d1c2e-0a5b-4c3d-9e8f-1a2b3c4d5e6f",
        "customer_name": "Example Industries",
        "client_id": 10,
        "client_name": "Example Industries",
        "psa_site_id": 100,
        "psa_site_name": "Main office",
        "linked_at": "2026-11-22T10:00:00Z",
    })
}

/// List sites and their PSA match
///
/// Every MonitorFlow site, by customer and name, with the PSA client and
/// site it is matched to, or `null`s. Read from MonitorFlow only. Global
/// administrators only.
#[utoipa::path(get, path = "/api/v1/psa/site-links", tag = "psa",
    responses(
        (status = 200, description = "The sites", body = psa::SiteLinkList, example = json!({
            "sites": [site_link()],
        })),
        (status = 403, description = "Not a global administrator: `forbidden`; `api_key_scope`", body = ErrorBody),
    ))]
fn list_psa_site_links() {}

/// Match a site to a PSA client
///
/// Sets the PSA client, and optionally one of its sites, that the site's
/// incidents open tickets for. Both are checked with the PSA, which also
/// gives the names kept; a PSA site must belong to the client and match
/// one MonitorFlow site at most. Audited as `psa.link.update`. Safe to
/// repeat.
#[utoipa::path(put, path = "/api/v1/psa/site-links/{site_id}", tag = "psa",
    params(("site_id" = uuid::Uuid, Path, description = "The MonitorFlow site")),
    request_body(content = psa::SiteLinkRequest, example = json!({"client_id": 10, "psa_site_id": 100})),
    responses(
        (status = 200, description = "Matched", body = psa::SiteLink, example = json!(site_link())),
        (status = 400, description = "`invalid`: the PSA has no such client or site, or the site belongs to another client", body = ErrorBody),
        (status = 403, description = "Not a global administrator: `forbidden`; `api_key_scope`", body = ErrorBody),
        (status = 404, description = "`not_found`: no such MonitorFlow site", body = ErrorBody),
        (status = 409, description = "`conflict`: no connection is set up, or the PSA site is matched to another site", body = ErrorBody),
        (status = 502, description = "`psa_unavailable`: the PSA refused or did not answer", body = ErrorBody),
    ))]
fn put_psa_site_link() {}

/// Unmatch a site
///
/// The site's incidents open no more tickets. Audited as
/// `psa.link.delete`.
#[utoipa::path(delete, path = "/api/v1/psa/site-links/{site_id}", tag = "psa",
    params(("site_id" = uuid::Uuid, Path, description = "The MonitorFlow site")),
    responses(
        (status = 204, description = "Unmatched"),
        (status = 403, description = "Not a global administrator: `forbidden`; `api_key_scope`", body = ErrorBody),
        (status = 404, description = "`not_found`: the site is not matched", body = ErrorBody),
    ))]
fn delete_psa_site_link() {}

/// List the PSA's ticket types
///
/// Ticket types used for tickets (not opportunities or projects), by
/// name, read through the connection. Global administrators only.
#[utoipa::path(get, path = "/api/v1/psa/ticket-types", tag = "psa",
    responses(
        (status = 200, description = "The ticket types", body = psa::RecordList, example = json!({
            "records": [{"id": 3, "name": "Change Request"}, {"id": 1, "name": "Incident"}],
        })),
        (status = 403, description = "Not a global administrator: `forbidden`; `api_key_scope`", body = ErrorBody),
        (status = 409, description = "`conflict`: no connection is set up", body = ErrorBody),
        (status = 502, description = "`psa_unavailable`: the PSA refused or did not answer", body = ErrorBody),
    ))]
fn list_psa_ticket_types() {}

/// List the PSA's categories
///
/// First-level categories as a ticket's category takes them, by name.
/// Some PSA instances refuse a ticket without one. Global administrators
/// only.
#[utoipa::path(get, path = "/api/v1/psa/categories", tag = "psa",
    responses(
        (status = 200, description = "The categories", body = psa::CategoryList, example = json!({
            "categories": ["Hardware>Server", "Network"],
        })),
        (status = 403, description = "Not a global administrator: `forbidden`; `api_key_scope`", body = ErrorBody),
        (status = 409, description = "`conflict`: no connection is set up", body = ErrorBody),
        (status = 502, description = "`psa_unavailable`: the PSA refused or did not answer", body = ErrorBody),
    ))]
fn list_psa_categories() {}

/// List the PSA's ticket statuses
///
/// Ticket statuses by id; in HaloPSA, 1 is New and 9 Closed on every
/// instance. Global administrators only.
#[utoipa::path(get, path = "/api/v1/psa/statuses", tag = "psa",
    responses(
        (status = 200, description = "The statuses", body = psa::RecordList, example = json!({
            "records": [{"id": 1, "name": "New"}, {"id": 9, "name": "Closed"}],
        })),
        (status = 403, description = "Not a global administrator: `forbidden`; `api_key_scope`", body = ErrorBody),
        (status = 409, description = "`conflict`: no connection is set up", body = ErrorBody),
        (status = 502, description = "`psa_unavailable`: the PSA refused or did not answer", body = ErrorBody),
    ))]
fn list_psa_statuses() {}

/// Choose what tickets are opened
///
/// The ticket type and category of new tickets, the least severity whose
/// incidents open one (`critical` by default), and the status a ticket
/// moves to when its incident is resolved (9, Closed, by default; `null`
/// adds a note only). Each is checked against the PSA's lists, which also
/// give the names kept. Audited as `psa.connection.tickets`.
#[utoipa::path(put, path = "/api/v1/psa/ticket-settings", tag = "psa",
    request_body(content = psa::TicketSettingsRequest, example = json!({
        "ticket_type_id": 1,
        "category": "Hardware>Server",
        "min_severity": "critical",
        "resolved_status_id": 9,
    })),
    responses(
        (status = 200, description = "Saved", body = psa::ConnectionView, example = json!(connection())),
        (status = 400, description = "`invalid`: a ticket type, category or status the PSA does not have, or another severity", body = ErrorBody),
        (status = 403, description = "Not a global administrator: `forbidden`; `api_key_scope`", body = ErrorBody),
        (status = 409, description = "`conflict`: no connection is set up", body = ErrorBody),
        (status = 502, description = "`psa_unavailable`: the PSA refused or did not answer", body = ErrorBody),
    ))]
fn put_psa_ticket_settings() {}

/// List tickets opened for incidents
///
/// The 50 most recently changed: created, closed when their incident
/// resolved, skipped because the site has no PSA client, or refused by
/// the PSA with its words. Global administrators only.
#[utoipa::path(get, path = "/api/v1/psa/tickets", tag = "psa",
    responses(
        (status = 200, description = "The tickets", body = psa::TicketList, example = json!({
            "tickets": [{
                "incident_id": "7c9e6679-7425-40de-944b-e07fc1f90ae7",
                "ticket_id": 5001,
                "state": "open",
                "error": null,
                "metric": "disk_used_percent",
                "severity": "critical",
                "server_name": "web-01",
                "site_name": "paris",
                "created_at": "2026-11-22T10:00:05Z",
                "updated_at": "2026-11-22T10:00:05Z",
                "closed_at": null,
            }],
        })),
        (status = 403, description = "Not a global administrator: `forbidden`; `api_key_scope`", body = ErrorBody),
    ))]
fn list_psa_tickets() {}
