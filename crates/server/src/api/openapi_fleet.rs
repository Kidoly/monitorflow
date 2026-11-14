//! The fleet part of the OpenAPI document (ADR-0101): enrolling servers
//! and relays with enrollment tokens and install keys, and the hosts that
//! wait for approval; relays; the agent releases this server serves;
//! package mirrors. Merged into [`super::build`]; like `openapi.rs`, the
//! functions only carry each route's description.

use serde_json::json;
use utoipa::OpenApi;

use super::super::{
    commands, install_keys, mirrors, relays, release_import, revoke, servers, tokens,
};
use super::{Common, ErrorBody};
use crate::relay_commands;

#[derive(OpenApi)]
#[openapi(
    paths(
        list_tokens, create_token, delete_token, list_install_keys, create_install_key,
        revoke_install_key, approve_pending_host, reject_pending_host,
        list_relays, relay_detail, revoke_relay, update_relay, confirm_relay_names,
        list_agent_releases, import_release, import_release_from_url,
        list_mirrors, mirror_catalog, add_mirrors, mirror_detail, delete_mirror, sync_mirror,
        snapshot_mirror,
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
        (name = "relays", description = "Relays: one per site where servers cannot reach the central \
server, or should not each open a connection to it. A relay forwards its site's agents to the central \
server, can serve package mirrors and releases on its network, and is enrolled like a server. Reading \
relays needs any role on their site; removing, updating and confirming the names of one needs the \
administrator role on its customer. Through an API key, relays are in the \"servers\" area, their \
updates in \"updates\"; listing them is open to any key within its reach."),
        (name = "agent releases", description = "The MonitorFlow releases this server holds and \
serves to install commands and to agent and relay updates, so that servers without Internet access can \
install and update. Every release is checked against the release keys built into the server before it \
is kept. Importing one is for global administrators, since every customer uses the same releases."),
        (name = "mirrors", description = "Package mirrors: copies of the apt and dnf repositories of \
the distributions servers run, kept on this server and on relays, so servers update without reaching \
the Internet and every server of a campaign installs the same packages. A mirror syncs on a schedule; \
a snapshot freezes what a sync copied under a name and never changes, and `current` is the newest one. \
Mirrors are shared by every customer, so every route is for global administrators: through an API key, \
access \"Administer\", reach everything and the \"updates\" area."),
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

fn relay_command(state: &str) -> serde_json::Value {
    let finished = state == "succeeded";
    json!({
        "id": "8d9e0f1a-2b3c-4d4e-8f5a-6b7c8d9e0f1a",
        "relay_id": "1e2f3a4b-5c6d-4e7f-8a9b-0c1d2e3f4a5b",
        "version": "0.9.2",
        "state": state,
        "requested_by": "9e8d7c6b-5a4f-4e3d-8c2b-1a0f9e8d7c6b",
        "issued_at": "2026-11-14T09:30:00Z",
        "expires_at": "2026-11-15T09:30:00Z",
        "finished_at": if finished { json!("2026-11-14T09:31:05Z") } else { json!(null) },
        "stdout": null,
        "error": null
    })
}

fn relay() -> serde_json::Value {
    json!({
        "id": "1e2f3a4b-5c6d-4e7f-8a9b-0c1d2e3f4a5b",
        "peer_id": "relay-paris",
        "name": "relay-paris",
        "site_id": "6f1c2b3a-4d5e-4f60-8a9b-0c1d2e3f4a5b",
        "site_name": "paris",
        "customer_id": "2a3b4c5d-6e7f-4a8b-9c0d-1e2f3a4b5c6d",
        "customer_name": "acme",
        "created_at": "2026-10-02T08:00:00Z",
        "revoked_at": null,
        "status": "online",
        "connected_at": "2026-11-14T06:12:00Z",
        "last_heartbeat": "2026-11-14T09:59:30Z",
        "cert_not_after": "2027-01-12T08:00:00Z",
        "server_names": ["relay.paris.example.com", "10.1.0.5"],
        "pending_names": null,
        "pending_source": null,
        "server_count": 12,
        "connected_agents": 11,
        "mirror_url": "http://10.1.0.5:8090",
        "mirror_reported_at": "2026-11-14T09:00:00Z",
        "relay_version": "0.9.2",
        "last_update": relay_command("succeeded"),
        "offline_alerts": true,
        "site_offline_alerts": true,
        "silenced_until": null
    })
}

/// List relays
///
/// One page of the enrolled relays of the sites the caller reads, sorted
/// by site name then peer ID, with their live connection, version and
/// latest self-update. Removed relays are not listed.
///
/// Paged: `limit` (1 to 1000, default 500) and `cursor`, the `next_cursor`
/// of the previous page.
///
/// Any role reads it. Any API key may list relays within its reach,
/// whatever its areas, to turn names into IDs.
#[utoipa::path(get, path = "/api/v1/relays", tag = "relays", params(servers::ListParams),
    responses(
        (status = 200, body = relays::RelayList, example = json!({
            "relays": [relay()],
            "next_cursor": null
        })),
        (status = 400, body = ErrorBody, description = "Bad `limit` or `cursor`: `invalid`"),
        (status = 403, body = ErrorBody, description = "The caller has no role: `forbidden`"),
    ))]
fn list_relays() {}

/// Read a relay
///
/// One relay, removed or not, with the certificate issued to it last, the
/// servers whose latest connection came through it and its 10 latest
/// self-updates, newest first.
///
/// Any role on the relay's site. Through an API key, the "servers" area.
#[utoipa::path(get, path = "/api/v1/relays/{id}", tag = "relays",
    params(("id" = uuid::Uuid, Path, description = "The relay's `id`")),
    responses(
        (status = 200, body = relays::RelayDetail, example = json!({
            "id": "1e2f3a4b-5c6d-4e7f-8a9b-0c1d2e3f4a5b",
            "peer_id": "relay-paris",
            "name": "relay-paris",
            "site_id": "6f1c2b3a-4d5e-4f60-8a9b-0c1d2e3f4a5b",
            "site_name": "paris",
            "customer_id": "2a3b4c5d-6e7f-4a8b-9c0d-1e2f3a4b5c6d",
            "customer_name": "acme",
            "created_at": "2026-10-02T08:00:00Z",
            "revoked_at": null,
            "status": "online",
            "connected_at": "2026-11-14T06:12:00Z",
            "last_heartbeat": "2026-11-14T09:59:30Z",
            "cert_not_after": "2027-01-12T08:00:00Z",
            "server_names": ["relay.paris.example.com", "10.1.0.5"],
            "pending_names": null,
            "pending_source": null,
            "server_count": 1,
            "connected_agents": 1,
            "mirror_url": null,
            "mirror_reported_at": null,
            "relay_version": "0.9.2",
            "last_update": relay_command("succeeded"),
            "offline_alerts": true,
            "site_offline_alerts": true,
            "silenced_until": null,
            "certificate": {
                "serial": "3f9a1c0b7e2d",
                "not_before": "2026-11-13T08:00:00Z",
                "not_after": "2027-01-12T08:00:00Z",
                "issued_at": "2026-11-13T08:00:00Z",
                "revoked": false,
                "revoked_at": null,
                "revocation_reason": null
            },
            "servers": [{
                "id": "5a6b7c8d-9e0f-4a1b-8c2d-3e4f5a6b7c8d",
                "peer_id": "web-1",
                "hostname": "web-1",
                "status": "online"
            }],
            "updates": [relay_command("succeeded")]
        })),
        (status = 403, body = ErrorBody, description = "The caller has no role: `forbidden`; a \
key outside the \"servers\" area: `api_key_scope`"),
        (status = 404, body = ErrorBody, description = "No such relay, or of a site the caller \
cannot read: `not_found`"),
    ))]
fn relay_detail() {}

/// Remove a relay
///
/// "Remove" in the web interface. Marks the relay revoked, revokes its
/// certificates and those of every peer obtained through it or renewed
/// from them, publishes them in the revocation list and closes the relay's
/// stream and those it forwards. Its unused enrollment tokens are deleted,
/// its open offline incidents resolved, and it leaves every list until it
/// enrolls again. Servers behind it lose their connection until they reach
/// the central server or another relay. Audited as `relay.revoke` with the
/// serials.
///
/// The body is required: send `{}` to give no reason. Needs the
/// administrator role on the relay's customer. Not undone by repeating: a
/// second call is 409.
#[utoipa::path(post, path = "/api/v1/relays/{id}/revoke", tag = "relays",
    params(("id" = uuid::Uuid, Path, description = "The relay's `id`")),
    request_body(content = revoke::RevokeRequest, example = json!({ "reason": "cessation_of_operation" })),
    responses(
        (status = 200, body = revoke::Revoked, example = json!({
            "revoked_at": "2026-11-14T10:05:00Z",
            "reason": "cessation_of_operation",
            "serials": ["3f9a1c0b7e2d", "1b2c3d4e5f60"],
            "issued_through": [{ "serial": "1b2c3d4e5f60", "peer_role": "agent", "peer_id": "web-1" }],
            "closed_streams": 2
        })),
        (status = 400, body = ErrorBody, description = "An unknown `reason` or field: `invalid`"),
        (status = 403, body = ErrorBody, description = "The caller does not administer the relay's \
customer: `forbidden`; a key outside the \"servers\" area: `api_key_scope`"),
        (status = 404, body = ErrorBody, description = "No such relay, or of a site the caller \
cannot read: `not_found`"),
        (status = 409, body = ErrorBody, description = "Already removed: `conflict`"),
    ))]
fn revoke_relay() {}

/// Update a relay
///
/// Queues a self-update of the relay to `version`. The relay downloads the
/// release from this server when it holds it, installs it, restarts and
/// must connect back within `health_timeout_s`, or it rolls back. The
/// command waits up to 24 hours for an offline relay; its progress shows in
/// `last_update` and in the relay's `updates`. Audited as `relay.update`.
///
/// Needs the administrator role on the relay's customer. Through an API
/// key, the "updates" area. One update at a time: while one is in flight,
/// another is 409, so repeating is safe.
#[utoipa::path(post, path = "/api/v1/relays/{id}/update", tag = "relays",
    params(("id" = uuid::Uuid, Path, description = "The relay's `id`")),
    request_body(content = relays::UpdateRequest, example = json!({
        "version": "0.9.2",
        "health_timeout_s": 300
    })),
    responses(
        (status = 202, body = relay_commands::RelayCommand, example = json!(relay_command("queued"))),
        (status = 400, body = ErrorBody, description = "`version` is not like `0.9.2`, \
`health_timeout_s` is out of range, or a field is unknown: `invalid`"),
        (status = 403, body = ErrorBody, description = "The caller does not administer the relay's \
customer: `forbidden`; a key outside the \"updates\" area: `api_key_scope`"),
        (status = 404, body = ErrorBody, description = "No such relay, or of a site the caller \
cannot read: `not_found`"),
        (status = 409, body = ErrorBody, description = "The relay is removed, or has an update in \
flight: `conflict`"),
    ))]
fn update_relay() {}

/// Confirm a relay's names
///
/// A relay enrolled without names (by an install key, or a token without
/// `server_names`) waits, speaking for no agent, with the names it
/// reported in `pending_names`. This sets the names its certificate
/// carries for agents; the relay collects that certificate within seconds
/// and starts forwarding its site's agents. Audited as
/// `relay.confirm_names`.
///
/// Needs the administrator role on the relay's customer. Through an API
/// key, the "servers" area.
#[utoipa::path(post, path = "/api/v1/relays/{id}/names", tag = "relays",
    params(("id" = uuid::Uuid, Path, description = "The relay's `id`")),
    request_body(content = relays::ConfirmNames, example = json!({
        "names": ["relay.paris.example.com", "10.1.0.5"]
    })),
    responses(
        (status = 204, description = "Confirmed"),
        (status = 400, body = ErrorBody, description = "No name, more than 16, or one that is not \
a usable DNS name or IP address: `invalid`"),
        (status = 403, body = ErrorBody, description = "The caller does not administer the relay's \
customer: `forbidden`; a key outside the \"servers\" area: `api_key_scope`"),
        (status = 404, body = ErrorBody, description = "No such relay, or of a site the caller \
cannot read: `not_found`"),
        (status = 409, body = ErrorBody, description = "The relay is removed or not waiting for \
names, or a name is the central server's or another site's relay's: `conflict`"),
    ))]
fn confirm_relay_names() {}

/// List agent releases
///
/// The releases this server holds and can serve to agents and relays,
/// for install commands and updates, newest first, and the newest one that
/// is not a pre-release.
///
/// Any signed-in user, even without a role. Through an API key, the
/// "updates" area.
#[utoipa::path(get, path = "/api/v1/agent-releases", tag = "agent releases",
    responses(
        (status = 200, body = commands::AgentReleases, example = json!({
            "versions": ["0.10.0-rc.1", "0.9.2", "0.9.1"],
            "latest": "0.9.2"
        })),
        (status = 403, body = ErrorBody, description = "A key outside the \"updates\" area: \
`api_key_scope`"),
    ))]
fn list_agent_releases() {}

/// The files of a release, for `POST /api/agent-releases/import`.
#[derive(utoipa::ToSchema)]
#[schema(as = ReleaseUpload)]
struct ReleaseUpload {
    /// One part per file, each named by its file name in the release
    /// (`filename` of the part): `SHA256SUMS`, `SHA256SUMS.sig` and
    /// `VERSION` are required, with at least one agent binary; any of
    /// `monitorflow-agent-linux-x86_64`, `monitorflow-agent-linux-aarch64`,
    /// `monitorflow-agent-windows-x86_64.exe`,
    /// `monitorflow-agent-windows-x86_64.msi`, `install.sh`,
    /// `monitorflow-agent.service`, `monitorflow-agent-sudoers`,
    /// `monitorflow-relay-linux-x86_64`, `monitorflow-relay-linux-aarch64`,
    /// `monitorflow-relay.service` and `monitorflow-relay-sudoers` may
    /// follow. Other files are ignored.
    #[schema(value_type = Vec<String>, format = Binary)]
    files: Vec<Vec<u8>>,
}

/// Import a release from files
///
/// For servers without Internet access: imports a release from its files,
/// sent as `multipart/form-data` up to 2 GiB in all. The signature of
/// `SHA256SUMS` is checked against the release keys built into the server
/// and every file against its checksum, as `agent-release import` does on
/// the command line. The release is then served to install commands and
/// agent and relay updates. Audited as `agent_release.import`.
///
/// Global administrators only: the release cache serves every customer.
/// Through an API key: access "Administer", reach everything, the
/// "updates" area. One import at a time. `Idempotency-Key` is ignored on
/// this route: importing the same release again replaces it with the same
/// files.
#[utoipa::path(post, path = "/api/v1/agent-releases/import", tag = "agent releases",
    request_body(content = ReleaseUpload, content_type = "multipart/form-data"),
    responses(
        (status = 200, body = release_import::ImportResult, example = json!({
            "version": "0.9.2",
            "missing": ["monitorflow-agent-windows-x86_64.exe", "monitorflow-agent-windows-x86_64.msi"]
        })),
        (status = 400, body = ErrorBody, description = "Not a multipart form: `invalid_request`. \
A required file is absent, the signature does not verify, a checksum does not match, no agent binary, \
or the upload stopped: `invalid`"),
        (status = 403, body = ErrorBody, description = "Not a global administrator: `forbidden`; a \
key outside the \"updates\" area: `api_key_scope`"),
        (status = 409, body = ErrorBody, description = "Another import is running: `conflict`"),
    ))]
fn import_release() {}

/// Import a release from a URL
///
/// Like the upload, but the server downloads the release's files from the
/// directory `url`, over http or https: the signature protects the files,
/// not the transport. Files the directory does not have (404) are skipped;
/// the required ones are then reported missing. The download stops after
/// 60 seconds without data, or beyond 2 GiB. Audited as
/// `agent_release.import`, with only the URL's host.
///
/// Global administrators only. Through an API key: access "Administer",
/// reach everything, the "updates" area. One import at a time; repeating
/// imports the same release again.
#[utoipa::path(post, path = "/api/v1/agent-releases/import-url", tag = "agent releases",
    request_body(content = release_import::UrlRequest, example = json!({
        "url": "https://downloads.example.com/monitorflow/v0.9.2/"
    })),
    responses(
        (status = 200, body = release_import::ImportResult, example = json!({
            "version": "0.9.2",
            "missing": []
        })),
        (status = 400, body = ErrorBody, description = "`url` is not an http or https URL, a file \
could not be downloaded, or the files are not a genuine release: `invalid`"),
        (status = 403, body = ErrorBody, description = "Not a global administrator: `forbidden`; a \
key outside the \"updates\" area: `api_key_scope`"),
        (status = 409, body = ErrorBody, description = "Another import is running: `conflict`"),
    ))]
fn import_release_from_url() {}

fn run_summary(kind: &str, state: &str) -> serde_json::Value {
    let running = state == "running";
    json!({
        "id": "7a8b9c0d-1e2f-4a3b-8c4d-5e6f7a8b9c0d",
        "kind": kind,
        "trigger": "manual",
        "requested_by": "user:9e8d7c6b-5a4f-4e3d-8c2b-1a0f9e8d7c6b",
        "state": state,
        "snapshot_id": null,
        "error": null,
        "started_at": "2026-11-14T03:00:00Z",
        "finished_at": if running { json!(null) } else { json!("2026-11-14T03:12:40Z") }
    })
}

fn mirror() -> serde_json::Value {
    json!({
        "id": "3c4d5e6f-7a8b-4c9d-8e0f-1a2b3c4d5e6f",
        "name": "debian-12",
        "family": "apt",
        "upstream_url": "https://deb.debian.org/debian",
        "distribution": "bookworm",
        "components": ["main", "non-free-firmware"],
        "architectures": ["amd64"],
        "sync_interval_s": 86400,
        "auto_snapshot": true,
        "created_at": "2026-10-01T12:00:00Z",
        "deleting_at": null,
        "delete_error": null,
        "current_snapshot": "20261114T031240Z",
        "last_run": run_summary("sync", "succeeded")
    })
}

/// List mirrors
///
/// Every package mirror of this install, by name, with its newest
/// published snapshot and its last run. Mirrors are shared by every
/// customer.
///
/// Global administrators only. Through an API key: access "Administer",
/// reach everything, the "updates" area.
#[utoipa::path(get, path = "/api/v1/mirrors", tag = "mirrors",
    responses(
        (status = 200, body = mirrors::MirrorList, example = json!({
            "mirrors": [mirror()],
            "keep_snapshots": 3
        })),
        (status = 403, body = ErrorBody, description = "Not a global administrator: `forbidden`; a \
key outside the \"updates\" area: `api_key_scope`"),
    ))]
fn list_mirrors() {}

/// List the distributions mirrors can carry
///
/// The catalog of distributions a mirror can be added for, whether each is
/// added already and whether there is room for it, with the state of the
/// mirror tools and of the disk the mirrors live on.
///
/// Global administrators only, as for the list.
#[utoipa::path(get, path = "/api/v1/mirrors/catalog", tag = "mirrors",
    responses(
        (status = 200, body = mirrors::Catalog, example = json!({
            "tools": { "available": true, "problem": null },
            "disk": { "free_bytes": 412316860416u64, "total_bytes": 1000204886016u64 },
            "distributions": [
                { "id": "debian-12", "name": "Debian 12", "family": "apt",
                  "size_bytes": 128849018880u64, "added": true, "problem": null },
                { "id": "rocky-9", "name": "Rocky Linux 9", "family": "dnf",
                  "size_bytes": 85899345920u64, "added": false, "problem": null }
            ]
        })),
        (status = 403, body = ErrorBody, description = "Not a global administrator: `forbidden`; a \
key outside the \"updates\" area: `api_key_scope`"),
    ))]
fn mirror_catalog() {}

/// Add a distribution to the mirrors
///
/// Creates the mirrors a distribution of the catalog needs (its main and
/// security repositories, for instance) that do not exist yet. The
/// scheduler starts syncing them within a minute; servers can use them
/// after the first sync. Creates the mirrors' signing key on first use.
/// Each mirror created is audited as `mirror.create`.
///
/// Global administrators only, as for the list. Safe to repeat: a second
/// call is 409 once every mirror exists.
#[utoipa::path(post, path = "/api/v1/mirrors", tag = "mirrors",
    request_body(content = mirrors::AddRequest, example = json!({ "distribution": "debian-12" })),
    responses(
        (status = 201, description = "The mirrors created", body = mirrors::MirrorList,
            example = json!({
                "mirrors": [{
                    "id": "3c4d5e6f-7a8b-4c9d-8e0f-1a2b3c4d5e6f",
                    "name": "debian-12",
                    "family": "apt",
                    "upstream_url": "https://deb.debian.org/debian",
                    "distribution": "bookworm",
                    "components": ["main", "non-free-firmware"],
                    "architectures": ["amd64"],
                    "sync_interval_s": 86400,
                    "auto_snapshot": true,
                    "created_at": "2026-11-14T10:00:00Z",
                    "deleting_at": null,
                    "delete_error": null,
                    "current_snapshot": null,
                    "last_run": null
                }],
                "keep_snapshots": 3
            })),
        (status = 400, body = ErrorBody, description = "Not a distribution of the catalog: \
`invalid`"),
        (status = 403, body = ErrorBody, description = "Not a global administrator: `forbidden`; a \
key outside the \"updates\" area: `api_key_scope`"),
        (status = 409, body = ErrorBody, description = "Already added, the mirror tools are not \
available, too little free space, or the signing key could not be made: `conflict`"),
    ))]
fn add_mirrors() {}

/// Read a mirror
///
/// One mirror with its 100 newest snapshots and why each is kept, its 10
/// latest runs with their output, and what keeps it from being deleted.
///
/// Global administrators only, as for the list.
#[utoipa::path(get, path = "/api/v1/mirrors/{id}", tag = "mirrors",
    params(("id" = uuid::Uuid, Path, description = "The mirror's `id`")),
    responses(
        (status = 200, body = mirrors::MirrorDetail, example = json!({
            "id": "3c4d5e6f-7a8b-4c9d-8e0f-1a2b3c4d5e6f",
            "name": "debian-12",
            "family": "apt",
            "upstream_url": "https://deb.debian.org/debian",
            "distribution": "bookworm",
            "components": ["main", "non-free-firmware"],
            "architectures": ["amd64"],
            "sync_interval_s": 86400,
            "auto_snapshot": true,
            "created_at": "2026-10-01T12:00:00Z",
            "deleting_at": null,
            "delete_error": null,
            "current_snapshot": "20261114T031240Z",
            "last_run": run_summary("sync", "succeeded"),
            "snapshots": [{
                "id": "e5f6a7b8-c9d0-4e1f-8a2b-3c4d5e6f7a8b",
                "mirror_id": "3c4d5e6f-7a8b-4c9d-8e0f-1a2b3c4d5e6f",
                "name": "20261114T031240Z",
                "state": "published",
                "size_bytes": 121332826112u64,
                "error": null,
                "created_at": "2026-11-14T03:12:40Z",
                "published_at": "2026-11-14T03:13:02Z",
                "kept": "current"
            }],
            "keep_snapshots": 3,
            "runs": [{
                "id": "7a8b9c0d-1e2f-4a3b-8c4d-5e6f7a8b9c0d",
                "mirror_id": "3c4d5e6f-7a8b-4c9d-8e0f-1a2b3c4d5e6f",
                "kind": "sync",
                "trigger": "schedule",
                "requested_by": null,
                "state": "succeeded",
                "snapshot_id": "e5f6a7b8-c9d0-4e1f-8a2b-3c4d5e6f7a8b",
                "output": "Mirror `debian-12` has been updated successfully.\n",
                "error": null,
                "started_at": "2026-11-14T03:00:00Z",
                "finished_at": "2026-11-14T03:12:40Z"
            }],
            "used_by": {
                "campaigns": [],
                "servers": [{ "id": "5a6b7c8d-9e0f-4a1b-8c2d-3e4f5a6b7c8d", "hostname": "web-1" }]
            }
        })),
        (status = 403, body = ErrorBody, description = "Not a global administrator: `forbidden`; a \
key outside the \"updates\" area: `api_key_scope`"),
        (status = 404, body = ErrorBody, description = "No such mirror: `not_found`"),
    ))]
fn mirror_detail() {}

/// Delete a mirror
///
/// Starts deleting a mirror no server or unfinished campaign uses, with its
/// snapshots and the copies relays hold. The answer comes at once, with
/// `deleting_at` set; the deletion is over when the mirror's detail
/// answers 404, and `delete_error` says why while it is not. Calling again
/// after a failure tries again at once. Audited as `mirror.delete`, then
/// `mirror.deleted`.
///
/// Global administrators only, as for the list.
#[utoipa::path(delete, path = "/api/v1/mirrors/{id}", tag = "mirrors",
    params(("id" = uuid::Uuid, Path, description = "The mirror's `id`")),
    responses(
        (status = 202, description = "Being deleted", body = mirrors::MirrorView),
        (status = 403, body = ErrorBody, description = "Not a global administrator: `forbidden`; a \
key outside the \"updates\" area: `api_key_scope`"),
        (status = 404, body = ErrorBody, description = "No such mirror: `not_found`"),
        (status = 409, body = ErrorBody, description = "Servers or campaigns still use it (the \
sentence names them), or what uses it could not be checked: `conflict`"),
    ))]
fn delete_mirror() {}

/// Sync a mirror now
///
/// Starts copying the upstream repository now rather than at the next
/// scheduled time. The answer comes at once with the run, which goes on in
/// the background; follow it in the mirror's detail. Audited as
/// `mirror.sync`.
///
/// Global administrators only, as for the list. Not safe to repeat while
/// it runs: a second call is 409.
#[utoipa::path(post, path = "/api/v1/mirrors/{id}/sync", tag = "mirrors",
    params(("id" = uuid::Uuid, Path, description = "The mirror's `id`")),
    responses(
        (status = 202, body = mirrors::RunSummary, example = json!(run_summary("sync", "running"))),
        (status = 403, body = ErrorBody, description = "Not a global administrator: `forbidden`; a \
key outside the \"updates\" area: `api_key_scope`"),
        (status = 404, body = ErrorBody, description = "No such mirror: `not_found`"),
        (status = 409, body = ErrorBody, description = "The mirror is busy with another run or \
retention, or is being deleted: `conflict`"),
    ))]
fn sync_mirror() {}

/// Publish a snapshot of a mirror
///
/// Publishes what the last successful sync copied as a snapshot named
/// `name`, which servers can then be pointed at and which never changes.
/// The answer comes at once with the run; follow it in the mirror's
/// detail. Audited as `mirror.snapshot`.
///
/// Global administrators only, as for the list.
#[utoipa::path(post, path = "/api/v1/mirrors/{id}/snapshots", tag = "mirrors",
    params(("id" = uuid::Uuid, Path, description = "The mirror's `id`")),
    request_body(content = mirrors::SnapshotRequest, example = json!({ "name": "before-upgrade" })),
    responses(
        (status = 202, body = mirrors::RunSummary, example = json!(run_summary("snapshot", "running"))),
        (status = 400, body = ErrorBody, description = "An invalid `name`, or an unknown field: \
`invalid`. A name another snapshot of the mirror has makes the run fail instead."),
        (status = 403, body = ErrorBody, description = "Not a global administrator: `forbidden`; a \
key outside the \"updates\" area: `api_key_scope`"),
        (status = 404, body = ErrorBody, description = "No such mirror: `not_found`"),
        (status = 409, body = ErrorBody, description = "The mirror is busy, has no successful sync \
yet, or is being deleted: `conflict`"),
    ))]
fn snapshot_mirror() {}
