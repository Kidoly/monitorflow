//! The PSA connection in the OpenAPI document (ADR-0164, ADR-0166):
//! merged into it by [`super::openapi::build`].

#![allow(dead_code)] // The functions exist for their `#[utoipa::path]`.

use serde_json::{json, Value};
use utoipa::OpenApi;

use super::openapi::{Common, ErrorBody};
use super::{psa, psa_assets, psa_links};

#[derive(OpenApi)]
#[openapi(
    paths(get_psa_connection, put_psa_connection, delete_psa_connection, test_psa_connection,
          list_psa_clients, list_psa_client_sites, list_psa_site_links, put_psa_site_link, delete_psa_site_link,
          list_psa_ticket_types, list_psa_categories, list_psa_statuses, put_psa_ticket_settings, list_psa_tickets,
          list_psa_priorities, list_psa_customer_links, list_psa_suggestions, put_psa_customer_link,
          delete_psa_customer_link, put_psa_default_client, list_psa_asset_types, list_psa_asset_fields,
          get_psa_asset_settings, put_psa_asset_settings, list_psa_asset_links, put_psa_asset_link,
          delete_psa_asset_link, sync_psa_assets),
    components(schemas(ErrorBody)),
    modifiers(&Common),
    tags((name = "psa", description = "The PSA connection: MonitorFlow calls a PSA's API with credentials \
it keeps, HaloPSA first. One per installation, managed by global administrators (a key with every reach), \
since one PSA serves every customer. In HaloPSA, create an API application (Configuration, Integrations, \
HaloPSA API, View Applications) with the authentication method Client ID and Secret (Client Credentials), \
acting as a dedicated agent, with the permission `all`; MonitorFlow asks for a token on `{base}/auth/token` \
with the scope `all` and calls `{base}/api`. The Client Secret is write-only: it is sealed at rest and never \
answered. Each MonitorFlow site is then matched to a PSA client, and optionally one of its sites, chosen \
from the PSA's lists read through the connection, and each customer to a client, with suggestions by name \
that an administrator confirms; a site of a matched customer without a match of its own uses the customer's \
client, and a default client takes the rest, or no ticket is opened. An incident at the chosen severity then \
opens one ticket for that client, with the chosen ticket type, category and the priority mapped to its \
severity; a silence covering the incident holds the ticket until it ends. An escalation raises the priority, \
and the resolution adds a private note and moves the ticket to the chosen status. Every minute the open \
tickets are read back: one moved to a closed status, or assigned to an agent, acknowledges its incident, and \
one deleted or merged ends the link. When asset sync is on, servers of matched sites and customers become \
PSA assets of the chosen type per system family, matched to existing assets by serial number then host name \
for an administrator to confirm, or created, and updated at most once an hour. MonitorFlow keeps under \
HaloPSA's limit of 700 requests in 5 minutes and pauses on a 429. \
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
            "warning_priority_id": 3,
            "warning_priority_name": "Medium",
            "critical_priority_id": 1,
            "critical_priority_name": "Critical",
            "closed_status_ids": [9],
            "acknowledge_on_assign": true,
        },
        "default_client": {
            "default_client_id": null,
            "default_client_name": null,
            "default_site_id": null,
            "default_site_name": null,
        },
        "assets": {
            "asset_sync": true,
            "asset_inactive_on_removal": false,
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
/// incidents open one (`critical` by default), the status a ticket moves
/// to when its incident is resolved (9, Closed, by default; `null` adds a
/// note only), the priority of warning and critical incidents' tickets
/// (from the ticket type's SLA), the statuses that mean closed, and
/// whether an agent taking a ticket acknowledges its incident. Each is
/// checked against the PSA's lists, which also give the names kept. A
/// ticket already closed in the PSA is never moved at resolution.
/// Audited as `psa.connection.tickets`.
#[utoipa::path(put, path = "/api/v1/psa/ticket-settings", tag = "psa",
    request_body(content = psa::TicketSettingsRequest, example = json!({
        "ticket_type_id": 1,
        "category": "Hardware>Server",
        "min_severity": "critical",
        "resolved_status_id": 9,
        "warning_priority_id": 3,
        "critical_priority_id": 1,
        "closed_status_ids": [9],
        "acknowledge_on_assign": true,
    })),
    responses(
        (status = 200, description = "Saved", body = psa::ConnectionView, example = json!(connection())),
        (status = 400, description = "`invalid`: a ticket type, category, status or priority the PSA does not have, more than 20 closed statuses, or another severity", body = ErrorBody),
        (status = 403, description = "Not a global administrator: `forbidden`; `api_key_scope`", body = ErrorBody),
        (status = 409, description = "`conflict`: no connection is set up", body = ErrorBody),
        (status = 502, description = "`psa_unavailable`: the PSA refused or did not answer", body = ErrorBody),
    ))]
fn put_psa_ticket_settings() {}

/// List tickets opened for incidents
///
/// The 50 most recently changed: created, closed when their incident
/// resolved, skipped because the site has no PSA client or the incident
/// opened and resolved during a silence, refused by the PSA with its
/// words, held by a silence, or ended because the ticket was deleted or
/// merged in the PSA. Global administrators only.
#[utoipa::path(get, path = "/api/v1/psa/tickets", tag = "psa",
    responses(
        (status = 200, description = "The tickets", body = psa::TicketList, example = json!({
            "tickets": [{
                "incident_id": "7c9e6679-7425-40de-944b-e07fc1f90ae7",
                "ticket_id": 5001,
                "state": "open",
                "psa_status_id": 2,
                "checked_at": "2026-11-22T10:05:00Z",
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

/// List the priorities of a ticket type
///
/// The priorities of the ticket type's SLA, by their number within it,
/// which is what a ticket's priority names; absent `ticket_type_id` means
/// the connection's ticket type. Global administrators only.
#[utoipa::path(get, path = "/api/v1/psa/priorities", tag = "psa",
    params(psa::PriorityQuery),
    responses(
        (status = 200, description = "The priorities", body = psa::RecordList, example = json!({
            "records": [{"id": 1, "name": "Critical"}, {"id": 2, "name": "High"}, {"id": 3, "name": "Medium"}],
        })),
        (status = 403, description = "Not a global administrator: `forbidden`; `api_key_scope`", body = ErrorBody),
        (status = 409, description = "`conflict`: no connection is set up", body = ErrorBody),
        (status = 502, description = "`psa_unavailable`: the PSA refused or did not answer", body = ErrorBody),
    ))]
fn list_psa_priorities() {}

fn customer_link() -> Value {
    json!({
        "customer_id": "4f7d1c2e-0a5b-4c3d-9e8f-1a2b3c4d5e6f",
        "customer_name": "Example Industries",
        "client_id": 10,
        "client_name": "Example Industries SAS",
        "linked_at": "2026-11-25T10:00:00Z",
        "site_count": 2,
        "matched_sites": 1,
    })
}

/// List customers and their PSA match
///
/// Every MonitorFlow customer, by name, with the PSA client it is matched
/// to, or `null`s, and how many of its sites have a match of their own.
/// Read from MonitorFlow only. Global administrators only.
#[utoipa::path(get, path = "/api/v1/psa/customer-links", tag = "psa",
    responses(
        (status = 200, description = "The customers", body = psa_links::CustomerLinkList, example = json!({
            "customers": [customer_link()],
        })),
        (status = 403, description = "Not a global administrator: `forbidden`; `api_key_scope`", body = ErrorBody),
    ))]
fn list_psa_customer_links() {}

/// Suggest PSA clients for customers
///
/// For each customer without a match, the PSA client whose name reads the
/// same once case, accents, punctuation and company suffixes like SAS or
/// Ltd are left out, when exactly one does. Reads up to 3,000 clients
/// from the PSA. Nothing is matched until each suggestion is confirmed
/// with `PUT /api/v1/psa/customer-links/{customer_id}`.
#[utoipa::path(get, path = "/api/v1/psa/customer-links/suggestions", tag = "psa",
    responses(
        (status = 200, description = "The suggestions", body = psa_links::SuggestionList, example = json!({
            "suggestions": [{
                "customer_id": "4f7d1c2e-0a5b-4c3d-9e8f-1a2b3c4d5e6f",
                "customer_name": "Example Industries",
                "client_id": 10,
                "client_name": "Example Industries SAS",
            }],
        })),
        (status = 403, description = "Not a global administrator: `forbidden`; `api_key_scope`", body = ErrorBody),
        (status = 409, description = "`conflict`: no connection is set up", body = ErrorBody),
        (status = 502, description = "`psa_unavailable`: the PSA refused or did not answer", body = ErrorBody),
    ))]
fn list_psa_suggestions() {}

/// Match a customer to a PSA client
///
/// Its sites without a match of their own open tickets for this client,
/// and a site's own match must name it. Checked with the PSA, which also
/// gives the name kept; a PSA client matches one customer at most.
/// Audited as `psa.link.update`. Safe to repeat.
#[utoipa::path(put, path = "/api/v1/psa/customer-links/{customer_id}", tag = "psa",
    params(("customer_id" = uuid::Uuid, Path, description = "The MonitorFlow customer")),
    request_body(content = psa_links::CustomerLinkRequest, example = json!({"client_id": 10})),
    responses(
        (status = 200, description = "Matched", body = psa_links::CustomerLink, example = json!(customer_link())),
        (status = 400, description = "`invalid`: the PSA has no such client", body = ErrorBody),
        (status = 403, description = "Not a global administrator: `forbidden`; `api_key_scope`", body = ErrorBody),
        (status = 404, description = "`not_found`: no such customer", body = ErrorBody),
        (status = 409, description = "`conflict`: no connection is set up, the client is matched to another customer, or a site of the customer is matched to another client", body = ErrorBody),
        (status = 502, description = "`psa_unavailable`: the PSA refused or did not answer", body = ErrorBody),
    ))]
fn put_psa_customer_link() {}

/// Unmatch a customer
///
/// Its sites without a match of their own fall back to the default
/// client. Audited as `psa.link.delete`.
#[utoipa::path(delete, path = "/api/v1/psa/customer-links/{customer_id}", tag = "psa",
    params(("customer_id" = uuid::Uuid, Path, description = "The MonitorFlow customer")),
    responses(
        (status = 204, description = "Unmatched"),
        (status = 403, description = "Not a global administrator: `forbidden`; `api_key_scope`", body = ErrorBody),
        (status = 404, description = "`not_found`: the customer is not matched", body = ErrorBody),
    ))]
fn delete_psa_customer_link() {}

/// Choose the default PSA client
///
/// The PSA client, and optionally one of its sites, that incidents of
/// sites matched to nothing (neither the site nor its customer) open
/// tickets for; `client_id: null` opens no ticket for them, the default.
/// Checked with the PSA. Audited as `psa.connection.default`.
#[utoipa::path(put, path = "/api/v1/psa/default-client", tag = "psa",
    request_body(content = psa_links::DefaultClientRequest, example = json!({"client_id": 10, "psa_site_id": 100})),
    responses(
        (status = 200, description = "Saved", body = psa::ConnectionView, example = json!(connection())),
        (status = 400, description = "`invalid`: the PSA has no such client or site, or the site belongs to another client", body = ErrorBody),
        (status = 403, description = "Not a global administrator: `forbidden`; `api_key_scope`", body = ErrorBody),
        (status = 409, description = "`conflict`: no connection is set up", body = ErrorBody),
        (status = 502, description = "`psa_unavailable`: the PSA refused or did not answer", body = ErrorBody),
    ))]
fn put_psa_default_client() {}

/// List the PSA's asset types
///
/// Active asset types, by name, read through the connection. Global
/// administrators only.
#[utoipa::path(get, path = "/api/v1/psa/asset-types", tag = "psa",
    responses(
        (status = 200, description = "The asset types", body = psa::RecordList, example = json!({
            "records": [{"id": 3, "name": "Server"}, {"id": 4, "name": "Workstation"}],
        })),
        (status = 403, description = "Not a global administrator: `forbidden`; `api_key_scope`", body = ErrorBody),
        (status = 409, description = "`conflict`: no connection is set up", body = ErrorBody),
        (status = 502, description = "`psa_unavailable`: the PSA refused or did not answer", body = ErrorBody),
    ))]
fn list_psa_asset_types() {}

/// List an asset type's fields
///
/// The fields of one asset type, by name, which can receive a server's
/// host name, serial number, maker and model; besides them,
/// `inventory_number` is the asset tag. Global administrators only.
#[utoipa::path(get, path = "/api/v1/psa/asset-types/{id}/fields", tag = "psa",
    params(("id" = i64, Path, description = "The PSA asset type's id")),
    responses(
        (status = 200, description = "Its fields", body = psa::RecordList, example = json!({
            "records": [{"id": 13, "name": "Manufacturer"}, {"id": 14, "name": "Model"}, {"id": 12, "name": "Serial Number"}],
        })),
        (status = 403, description = "Not a global administrator: `forbidden`; `api_key_scope`", body = ErrorBody),
        (status = 409, description = "`conflict`: no connection is set up", body = ErrorBody),
        (status = 502, description = "`psa_unavailable`: the PSA refused or did not answer", body = ErrorBody),
    ))]
fn list_psa_asset_fields() {}

fn asset_settings() -> Value {
    json!({
        "asset_sync": true,
        "asset_inactive_on_removal": false,
        "types": [{
            "family": "linux",
            "type_id": 3,
            "type_name": "Server",
            "hostname_field": "inventory_number",
            "serial_field": "12",
            "manufacturer_field": "13",
            "model_field": "14",
        }],
    })
}

/// Read the asset sync's settings
///
/// Whether servers are synced as PSA assets, whether a removed server's
/// asset is marked inactive, and each system family's asset type and the
/// fields that receive the host name, serial number, maker and model.
#[utoipa::path(get, path = "/api/v1/psa/asset-settings", tag = "psa",
    responses(
        (status = 200, description = "The settings", body = psa_assets::AssetSettingsView, example = json!(asset_settings())),
        (status = 403, description = "Not a global administrator: `forbidden`; `api_key_scope`", body = ErrorBody),
        (status = 409, description = "`conflict`: no connection is set up", body = ErrorBody),
    ))]
fn get_psa_asset_settings() {}

/// Choose how servers become assets
///
/// Turns the asset sync on or off, and sets each system family's asset
/// type (`linux`, `windows`; a family without one is not synced) and
/// where each value goes: `inventory_number` or a field id of the type,
/// as text, or `null`. The sync is one way: servers of matched sites and
/// customers become assets of their client and site; an existing asset
/// found by serial number, then host name, is proposed for confirmation,
/// otherwise one is created with an External Link back to the server.
/// MonitorFlow never deletes assets. Audited as `psa.connection.assets`.
#[utoipa::path(put, path = "/api/v1/psa/asset-settings", tag = "psa",
    request_body(content = psa_assets::AssetSettingsRequest, example = json!({
        "asset_sync": true,
        "asset_inactive_on_removal": false,
        "types": [{"family": "linux", "type_id": 3, "hostname_field": "inventory_number",
                   "serial_field": "12", "manufacturer_field": "13", "model_field": "14"}],
    })),
    responses(
        (status = 200, description = "Saved", body = psa_assets::AssetSettingsView, example = json!(asset_settings())),
        (status = 400, description = "`invalid`: another family, two types for one family, a type or field the PSA does not have, or the sync turned on without a type", body = ErrorBody),
        (status = 403, description = "Not a global administrator: `forbidden`; `api_key_scope`", body = ErrorBody),
        (status = 409, description = "`conflict`: no connection is set up", body = ErrorBody),
        (status = 502, description = "`psa_unavailable`: the PSA refused or did not answer", body = ErrorBody),
    ))]
fn put_psa_asset_settings() {}

fn asset_link() -> Value {
    json!({
        "server_id": "1b2c3d4e-5f60-4718-9a0b-1c2d3e4f5a6b",
        "hostname": "web-01",
        "site_name": "paris",
        "state": "proposed",
        "asset_id": 7001,
        "asset_name": "WEB-01",
        "matched_by": "serial",
        "deactivated": false,
        "error": null,
        "pushed_at": null,
        "updated_at": "2026-11-25T10:01:00Z",
    })
}

/// List servers and their PSA assets
///
/// Servers the asset sync has looked at, proposed matches first, then
/// refusals, then the most recently changed, at most 200, with counts of
/// all of them by state. Global administrators only.
#[utoipa::path(get, path = "/api/v1/psa/asset-links", tag = "psa",
    responses(
        (status = 200, description = "The servers", body = psa_assets::AssetLinkList, example = json!({
            "servers": [asset_link()],
            "proposed": 1,
            "linked": 40,
            "failed": 0,
        })),
        (status = 403, description = "Not a global administrator: `forbidden`; `api_key_scope`", body = ErrorBody),
    ))]
fn list_psa_asset_links() {}

/// Confirm or choose a server's asset
///
/// `asset_id` set to the proposed asset confirms it; another asset of the
/// server's client chooses that one instead (checked with the PSA); `null`
/// creates a new asset at the next pass. The asset is then updated, with
/// an External Link back to the server. Audited as `psa.asset.link`.
#[utoipa::path(put, path = "/api/v1/psa/asset-links/{server_id}", tag = "psa",
    params(("server_id" = uuid::Uuid, Path, description = "The server")),
    request_body(content = psa_assets::AssetLinkRequest, example = json!({"asset_id": 7001})),
    responses(
        (status = 200, description = "Saved", body = psa_assets::AssetLinkView, example = json!({
            "server_id": "1b2c3d4e-5f60-4718-9a0b-1c2d3e4f5a6b",
            "hostname": "web-01",
            "site_name": "paris",
            "state": "linked",
            "asset_id": 7001,
            "asset_name": "WEB-01",
            "matched_by": "serial",
            "deactivated": false,
            "error": null,
            "pushed_at": null,
            "updated_at": "2026-11-25T10:02:00Z",
        })),
        (status = 400, description = "`invalid`: the PSA has no such asset, or it belongs to another client", body = ErrorBody),
        (status = 403, description = "Not a global administrator: `forbidden`; `api_key_scope`", body = ErrorBody),
        (status = 404, description = "`not_found`: no such server", body = ErrorBody),
        (status = 409, description = "`conflict`: no connection is set up, the server's site and customer have no PSA client, or the asset is another server's", body = ErrorBody),
        (status = 502, description = "`psa_unavailable`: the PSA refused or did not answer", body = ErrorBody),
    ))]
fn put_psa_asset_link() {}

/// Forget a server's asset
///
/// In MonitorFlow only: the asset stays in the PSA, and the next pass
/// matches the server again. Audited as `psa.asset.unlink`.
#[utoipa::path(delete, path = "/api/v1/psa/asset-links/{server_id}", tag = "psa",
    params(("server_id" = uuid::Uuid, Path, description = "The server")),
    responses(
        (status = 204, description = "Forgotten"),
        (status = 403, description = "Not a global administrator: `forbidden`; `api_key_scope`", body = ErrorBody),
        (status = 404, description = "`not_found`: the server has no asset", body = ErrorBody),
    ))]
fn delete_psa_asset_link() {}

/// Sync assets now
///
/// Linked servers, and those the PSA refused, are pushed again at the next
/// passes instead of within the hour, 20 servers a minute.
#[utoipa::path(post, path = "/api/v1/psa/asset-links/sync", tag = "psa",
    responses(
        (status = 200, description = "Queued", body = psa_assets::SyncQueued, example = json!({"queued": 41})),
        (status = 403, description = "Not a global administrator: `forbidden`; `api_key_scope`", body = ErrorBody),
        (status = 409, description = "`conflict`: no connection is set up", body = ErrorBody),
    ))]
fn sync_psa_assets() {}
