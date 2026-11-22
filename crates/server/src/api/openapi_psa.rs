//! The PSA connection in the OpenAPI document (ADR-0164, ADR-0166):
//! merged into it by [`super::openapi::build`].

#![allow(dead_code)] // The functions exist for their `#[utoipa::path]`.

use serde_json::{json, Value};
use utoipa::OpenApi;

use super::openapi::{Common, ErrorBody};
use super::psa;

#[derive(OpenApi)]
#[openapi(
    paths(get_psa_connection, put_psa_connection, delete_psa_connection, test_psa_connection),
    components(schemas(ErrorBody)),
    modifiers(&Common),
    tags((name = "psa", description = "The PSA connection: MonitorFlow calls a PSA's API with credentials \
it keeps, HaloPSA first. One per installation, managed by global administrators (a key with every reach), \
since one PSA serves every customer. In HaloPSA, create an API application (Configuration, Integrations, \
HaloPSA API, View Applications) with the authentication method Client ID and Secret (Client Credentials), \
acting as a dedicated agent, with the permission `all`; MonitorFlow asks for a token on `{base}/auth/token` \
with the scope `all` and calls `{base}/api`. The Client Secret is write-only: it is sealed at rest and never \
answered. Every change is in the audit log, with the key's ID. API keys need the `administration` area."))
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
