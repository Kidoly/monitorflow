//! The fleet part of the OpenAPI document (ADR-0101): enrolling servers
//! and relays with enrollment tokens and install keys, and the hosts that
//! wait for approval. Merged into [`super::build`]; like `openapi.rs`, the
//! functions only carry each route's description.

use serde_json::json;
use utoipa::OpenApi;

use super::super::{install_keys, tokens};
use super::{Common, ErrorBody};

#[derive(OpenApi)]
#[openapi(
    paths(
        list_tokens, create_token, delete_token, list_install_keys, create_install_key,
        revoke_install_key, approve_pending_host, reject_pending_host,
    ),
    modifiers(&Common),
    tags(
        (name = "enrollment", description = "Bringing servers and relays into MonitorFlow. An \
enrollment token is single-use: it enrolls one named server or relay on one site within its validity \
(one hour by default). An install key is reusable: it enrolls any number of servers (or one relay) on one \
site for up to a year, names each after its host, and can make new hosts wait for an administrator's \
approval. Both are secrets shown once, in the answer that creates them; no route returns them again and \
an `Idempotency-Key` replay does not either. Every route needs the administrator role on the site's \
customer, given globally or for that customer: a site's administrator cannot enroll. Through an API key, \
that means access \"Administer\", the \"administration\" area, and a reach of everything or of the customer."),
    )
)]
pub(super) struct FleetDoc;

const TOKEN: &str = "mfe1.q4Rr0xYlq5bS3d9n4oN2c1VZ6sPzT0kQvG8yW7uHjLm.\
3xHq9kQ2v0bR6nWcY1tS8pZ4mJ7fL5dA9gE2uK0iO3w";

fn install() -> serde_json::Value {
    json!({
        "source": "central",
        "custom_url": null,
        "source_from_config": false,
        "custom_url_from_config": false,
        "github_url": "https://downloads.example.com/monitorflow/latest",
        "central": {
            "version": "0.9.2",
            "path": "/agent/0.9.2",
            "sums_sha256": "5f2b8c0e7d1a9f3e6b4c2d8a0e7f1b3c5d9a2e4f6b8c0d1e3f5a7b9c2d4e6f80",
            "installer_sha256": "a1c3e5f7092b4d6f8a0c2e4f6b8d0a2c4e6f8b0d2a4c6e8f0b2d4a6c8e0f2b4d",
            "msi_sha256": null
        }
    })
}

fn address() -> serde_json::Value {
    json!({
        "address": "mf.example.com",
        "default": "mf.example.com",
        "names": ["mf.example.com", "10.0.0.2"],
        "certified": true,
        "upstream_url": "https://mf.example.com:7443",
        "enroll_url": "https://mf.example.com:7445"
    })
}

fn token_view(state: &str, used_at: Option<&str>) -> serde_json::Value {
    json!({
        "id": "0b7f3c5e-2d4a-4e61-9f8b-1a2c3d4e5f60",
        "role": "agent",
        "peer_id": "web-1",
        "site_id": "6f1c2b3a-4d5e-4f60-8a9b-0c1d2e3f4a5b",
        "site_name": "paris",
        "customer_id": "2a3b4c5d-6e7f-4a8b-9c0d-1e2f3a4b5c6d",
        "customer_name": "acme",
        "server_names": [],
        "created_by": "9e8d7c6b-5a4f-4e3d-8c2b-1a0f9e8d7c6b",
        "created_by_username": "alice",
        "created_at": "2026-11-14T09:00:00Z",
        "expires_at": "2026-11-14T10:00:00Z",
        "used_at": used_at,
        "state": state
    })
}

fn created_token() -> serde_json::Value {
    let mut created = token_view("pending", None);
    let extra = json!({
        "token": TOKEN,
        "upstream_url": "https://mf.example.com:7443",
        "enroll_url": "https://mf.example.com:7445",
        "install": install(),
        "address": address()
    });
    created
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    created
}

fn key_view(state: &str) -> serde_json::Value {
    json!({
        "id": "4c5d6e7f-8a9b-4c0d-9e1f-2a3b4c5d6e7f",
        "role": "agent",
        "site_id": "6f1c2b3a-4d5e-4f60-8a9b-0c1d2e3f4a5b",
        "site_name": "paris",
        "customer_id": "2a3b4c5d-6e7f-4a8b-9c0d-1e2f3a4b5c6d",
        "customer_name": "acme",
        "created_by_username": "alice",
        "created_at": "2026-11-14T09:00:00Z",
        "expires_at": "2026-12-14T09:00:00Z",
        "max_hosts": 50,
        "host_count": 0,
        "require_approval": true,
        "revoked_at": null,
        "state": state
    })
}

fn created_key() -> serde_json::Value {
    let mut created = key_view("active");
    let extra = json!({
        "token": TOKEN,
        "upstream_url": "https://mf.example.com:7443",
        "enroll_url": "https://mf.example.com:7445",
        "install": install(),
        "address": address()
    });
    created
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    created
}

/// List enrollment tokens
///
/// The single-use tokens of the sites whose customer the caller
/// administers, newest first, in every state, without their secret. Records
/// that install keys leave when they enroll a host are not listed.
///
/// Paged: `limit` (1 to 500, default 100) and `cursor`, the `next_cursor`
/// of the previous page. `state` keeps only `pending`, `used` or `expired`
/// tokens. Unknown query parameters are refused.
///
/// Needs the administrator role on a customer, or global: 403 `forbidden`
/// otherwise, also for a site's administrator.
#[utoipa::path(get, path = "/api/v1/tokens", tag = "enrollment", params(tokens::ListParams),
    responses(
        (status = 200, body = tokens::TokenList, example = json!({
            "tokens": [token_view("used", Some("2026-11-14T09:04:12Z"))],
            "next_cursor": null
        })),
        (status = 400, body = ErrorBody, description = "Bad `state`, `limit` or `cursor`, or an \
unknown parameter: `invalid`"),
        (status = 403, body = ErrorBody, description = "The caller administers no customer: \
`forbidden`; a key outside the \"administration\" area: `api_key_scope`"),
    ))]
fn list_tokens() {}

/// Create an enrollment token
///
/// Makes a single-use token that enrolls one server (`agent`) or relay
/// (`relay`) under the ID `peer_id` on the site `site_id`. The answer is
/// the only time the secret `token` is shown: MonitorFlow stores its hash,
/// no route returns it again, and a token that is lost is revoked and made
/// again. It comes with what the install commands need: the addresses to
/// enroll at and connect to, and where installers download the agent from.
///
/// For a relay, `server_names` are the names and addresses its agents reach
/// it at; they go on its certificate. A relay token without them leaves
/// the relay waiting for an administrator to confirm its names (`POST
/// /api/v1/relays/{id}/names`).
///
/// Needs the administrator role on the site's customer. A site the caller
/// cannot read is 404. Audited as `enrollment_token.create`, without the
/// secret.
///
/// Not safe to repeat: each call makes another token. With
/// `Idempotency-Key`, a repeat does not make a second one but answers 409
/// `conflict` with `Idempotent-Replayed: true`, saying the secret is shown
/// only once: the first answer is not stored.
#[utoipa::path(post, path = "/api/v1/tokens", tag = "enrollment",
    request_body(content = tokens::CreateRequest, examples(
        ("agent" = (summary = "A server", value = json!({
            "role": "agent",
            "peer_id": "web-1",
            "site_id": "6f1c2b3a-4d5e-4f60-8a9b-0c1d2e3f4a5b",
            "ttl_s": 3600
        }))),
        ("relay" = (summary = "A relay with its names", value = json!({
            "role": "relay",
            "peer_id": "relay-paris",
            "site_id": "6f1c2b3a-4d5e-4f60-8a9b-0c1d2e3f4a5b",
            "server_names": ["relay.paris.example.com", "10.1.0.5"]
        }))),
    )),
    responses(
        (status = 201, body = tokens::CreatedToken, example = json!(created_token())),
        (status = 400, body = ErrorBody, description = "A field is missing, unknown or out of \
range, `peer_id` is not a valid ID, `server_names` on an agent token or a name that is not a DNS name \
or IP address: `invalid`"),
        (status = 403, body = ErrorBody, description = "The caller does not administer the site's \
customer: `forbidden`; a key outside the \"administration\" area: `api_key_scope`"),
        (status = 404, body = ErrorBody, description = "No such site, or not readable by the \
caller: `not_found`"),
        (status = 409, body = ErrorBody, description = "`peer_id` is taken by another server or \
relay, belongs to a peer removed from another site, or a relay name is the central server's or another \
site's relay's; or an `Idempotency-Key` replay: `conflict`"),
    ))]
fn create_token() {}

/// Revoke an enrollment token
///
/// Deletes a token that was not used, expired or not, so it can enroll
/// nothing. The audit log keeps its record (`enrollment_token.revoke`). A
/// used token cannot be revoked: remove the server or relay it enrolled
/// instead.
///
/// Needs the administrator role on the token's customer. Safe to repeat in
/// effect: once deleted, the token is 404.
#[utoipa::path(delete, path = "/api/v1/tokens/{id}", tag = "enrollment",
    params(("id" = uuid::Uuid, Path, description = "The token's `id`")),
    responses(
        (status = 204, description = "Revoked"),
        (status = 403, body = ErrorBody, description = "The caller does not administer the token's \
customer: `forbidden`; a key outside the \"administration\" area: `api_key_scope`"),
        (status = 404, body = ErrorBody, description = "No such token, already revoked, or of a \
site the caller cannot read: `not_found`"),
        (status = 409, body = ErrorBody, description = "The token was already used: `conflict`"),
    ))]
fn delete_token() {}

/// List install keys and pending hosts
///
/// The 100 newest install keys of the sites whose customer the caller
/// administers, in every state, without their secret; and every host
/// waiting for approval on those sites, oldest first. Not paged; older
/// keys stay in the audit log.
///
/// Needs the administrator role on a customer, or global: 403 `forbidden`
/// otherwise, also for a site's administrator.
#[utoipa::path(get, path = "/api/v1/install-keys", tag = "enrollment",
    responses(
        (status = 200, body = install_keys::KeyList, example = json!({
            "keys": [key_view("active")],
            "pending_hosts": [{
                "peer_id": "web-2",
                "hostname": "web-2",
                "site_id": "6f1c2b3a-4d5e-4f60-8a9b-0c1d2e3f4a5b",
                "site_name": "paris",
                "customer_name": "acme",
                "install_key_id": "4c5d6e7f-8a9b-4c0d-9e1f-2a3b4c5d6e7f",
                "source": "192.0.2.10",
                "created_at": "2026-11-14T09:12:40Z"
            }]
        })),
        (status = 403, body = ErrorBody, description = "The caller administers no customer: \
`forbidden`; a key outside the \"administration\" area: `api_key_scope`"),
    ))]
fn list_install_keys() {}

/// Create an install key
///
/// Makes a reusable key for the install commands of a site. An `agent` key
/// enrolls up to `max_hosts` servers (no limit by default) until it expires
/// (30 days by default), each named after its host name; with
/// `require_approval`, each new host waits in `pending_hosts` until an
/// administrator approves it. A `relay` key enrolls one relay within a day
/// by default; `max_hosts` and `require_approval` are ignored for it, and
/// the relay waits for an administrator to confirm its names.
///
/// The answer is the only time the secret `token` is shown: MonitorFlow
/// stores its hash and no route returns it again. It comes with what the
/// install commands need, as for an enrollment token.
///
/// Needs the administrator role on the site's customer. A site the caller
/// cannot read is 404. Audited as `install_key.create`, without the secret.
///
/// Not safe to repeat: each call makes another key. With
/// `Idempotency-Key`, a repeat answers 409 `conflict` with
/// `Idempotent-Replayed: true` instead of making a second key or showing
/// the secret again.
#[utoipa::path(post, path = "/api/v1/install-keys", tag = "enrollment",
    request_body(content = install_keys::CreateRequest, examples(
        ("agent" = (summary = "Servers, with approval", value = json!({
            "role": "agent",
            "site_id": "6f1c2b3a-4d5e-4f60-8a9b-0c1d2e3f4a5b",
            "ttl_s": 2592000,
            "max_hosts": 50,
            "require_approval": true
        }))),
        ("relay" = (summary = "One relay", value = json!({
            "role": "relay",
            "site_id": "6f1c2b3a-4d5e-4f60-8a9b-0c1d2e3f4a5b"
        }))),
    )),
    responses(
        (status = 201, body = install_keys::CreatedKey, example = json!(created_key())),
        (status = 400, body = ErrorBody, description = "A field is missing, unknown or out of \
range: `invalid`"),
        (status = 403, body = ErrorBody, description = "The caller does not administer the site's \
customer: `forbidden`; a key outside the \"administration\" area: `api_key_scope`"),
        (status = 404, body = ErrorBody, description = "No such site, or not readable by the \
caller: `not_found`"),
        (status = 409, body = ErrorBody, description = "An `Idempotency-Key` replay: `conflict`"),
    ))]
fn create_install_key() {}

/// Revoke an install key
///
/// The key enrolls no more hosts. Hosts it enrolled keep working, and hosts
/// waiting for approval still wait. The key stays listed as `revoked`.
/// Audited as `install_key.revoke`.
///
/// Needs the administrator role on the key's customer.
#[utoipa::path(delete, path = "/api/v1/install-keys/{id}", tag = "enrollment",
    params(("id" = uuid::Uuid, Path, description = "The key's `id`")),
    responses(
        (status = 204, description = "Revoked"),
        (status = 403, body = ErrorBody, description = "The caller does not administer the key's \
customer: `forbidden`; a key outside the \"administration\" area: `api_key_scope`"),
        (status = 404, body = ErrorBody, description = "No such key, or of a site the caller \
cannot read: `not_found`"),
        (status = 409, body = ErrorBody, description = "Already revoked: `conflict`"),
    ))]
fn revoke_install_key() {}

/// Approve a pending host
///
/// A host enrolled by a key that requires approval is accepted: its next
/// connection goes through and it appears in the server list. A server
/// that was removed and enrolled again under the same ID is back. Audited
/// as `pending_host.approve`.
///
/// Needs the administrator role on the host's customer.
#[utoipa::path(post, path = "/api/v1/pending-hosts/{peer_id}/approve", tag = "enrollment",
    params(("peer_id" = String, Path, description = "The `peer_id` of an entry of \
`pending_hosts`")),
    responses(
        (status = 204, description = "Approved"),
        (status = 403, body = ErrorBody, description = "The caller does not administer the host's \
customer: `forbidden`; a key outside the \"administration\" area: `api_key_scope`"),
        (status = 404, body = ErrorBody, description = "No such pending host, or of a site the \
caller cannot read: `not_found`"),
        (status = 409, body = ErrorBody, description = "Already approved or rejected: `conflict`"),
    ))]
fn approve_pending_host() {}

/// Reject a pending host
///
/// Revokes the host's certificates and closes its connection, so it can
/// never connect under this ID.
/// Audited as `pending_host.reject` with the revoked serials.
///
/// Needs the administrator role on the host's customer.
#[utoipa::path(post, path = "/api/v1/pending-hosts/{peer_id}/reject", tag = "enrollment",
    params(("peer_id" = String, Path, description = "The `peer_id` of an entry of \
`pending_hosts`")),
    responses(
        (status = 204, description = "Rejected"),
        (status = 403, body = ErrorBody, description = "The caller does not administer the host's \
customer: `forbidden`; a key outside the \"administration\" area: `api_key_scope`"),
        (status = 404, body = ErrorBody, description = "No such pending host, or of a site the \
caller cannot read: `not_found`"),
        (status = 409, body = ErrorBody, description = "Already approved or rejected: `conflict`"),
    ))]
fn reject_pending_host() {}
