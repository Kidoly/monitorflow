//! The public API's description of user accounts and the license. Merged
//! into the document by [`super::openapi::build`].

#![allow(dead_code)] // The functions exist for their `#[utoipa::path]`.

use chrono::{DateTime, NaiveDate, Utc};
use serde::Serialize;
use serde_json::json;
use utoipa::{OpenApi, ToSchema};
use uuid::Uuid;

use super::openapi::{Common, ErrorBody};
use super::{license, user_admin};

#[derive(OpenApi)]
#[openapi(
    paths(
        list_users, remove_user, set_disabled, grant_role, revoke_role, get_license,
        install_license, remove_license, remove_license_server,
    ),
    components(schemas(LicenseView, LicenseState, LicenseTerms, LicenseTier, AddedServer)),
    modifiers(&Common),
    tags(
        (name = "users", description = "The accounts of people who sign in to the web \
interface, and the roles they hold: viewer, operator, approver or administrator, on everything, \
one customer or one site. Only a global administrator manages them; with a key, access \
\"Administer\", reach \"Everything\" and the \"administration\" area. A key can grant any role, \
administrator of everything included, to an existing account, as its maker can. Inviting a user \
and issuing a password link hand out a credential for a person, so they are left out of the \
public API and done in the web interface. No change may leave the server without an active \
administrator of everything, and nobody may disable or remove their own account. Every change \
is in the audit log."),
        (name = "license", description = "The subscription's license key: its tier (Basic, Standard, \
Premium or Enterprise), the managed and central servers it covers and its dates, checked offline. \
Without one the install is on the free Community tier, with every feature. A valid key moves the \
central server to the tested update channel; an Enterprise key allows additional central servers \
on one database. A global administrator's; \
with a key, access \"Administer\", reach \"Everything\" and the \"administration\" area."),
    ),
)]
pub(super) struct UsersDoc;

/// Body of `GET /api/license`. Without the Enterprise edition in the build
/// only `available` is present.
#[derive(Serialize, ToSchema)]
struct LicenseView {
    /// Whether this build includes the Enterprise edition at all.
    available: bool,
    state: LicenseState,
    /// What the installed key says; `null` without a key or when it no
    /// longer verifies.
    license: Option<LicenseTerms>,
    /// Why an installed key is `invalid`, in plain words.
    problem: Option<String>,
    /// `null` without a key.
    installed_at: Option<DateTime<Utc>>,
    /// User name of who installed it.
    installed_by: Option<String>,
    /// Central servers added under a license, oldest first.
    added: Vec<AddedServer>,
    /// Managed servers now, whose agent is not revoked, to compare with
    /// `license.managed_servers`. Above it nothing changes but a note on
    /// the License page.
    managed_servers_in_use: i64,
}

/// Where the license stands.
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
enum LicenseState {
    /// No license key: a single central server needs none.
    None,
    Valid,
    /// Valid, and expires within 30 days.
    ExpiresSoon,
    Expired,
    /// Stored, but no longer verifies with this build's keys.
    Invalid,
}

/// A paid subscription. Community, the free tier, has no key.
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
enum LicenseTier {
    Basic,
    Standard,
    Premium,
    /// Premium with the Enterprise edition: more than one central server.
    Enterprise,
}

/// What a license key says, once its signature verified.
#[derive(Serialize, ToSchema)]
struct LicenseTerms {
    /// Format of the key: 2, or 1 for a key of ADR-0100.
    format: u32,
    id: Uuid,
    /// The licensee's name.
    customer: String,
    /// `enterprise` when the key includes the Enterprise edition, `core`
    /// otherwise.
    edition: String,
    tier: LicenseTier,
    /// Managed servers the subscription covers; `null` for a format 1 key,
    /// which does not say.
    managed_servers: Option<u32>,
    /// Central servers allowed on the database, the first one included.
    /// Above 1 only for `enterprise`.
    server_instances: u32,
    /// Where to ask for support: an email address or an `https://` URL.
    support: Option<String>,
    issued: NaiveDate,
    /// Valid through this whole day, UTC.
    expires: NaiveDate,
}

/// A central server added under a license.
#[derive(Serialize, ToSchema)]
struct AddedServer {
    /// The name it was started with.
    name: String,
    admitted_at: DateTime<Utc>,
    /// Whether an instance of that name has a recent heartbeat.
    running: bool,
}

fn user_example() -> serde_json::Value {
    json!({
        "id": "5b0e3c2a-1d4f-4e6b-8a9c-7f1e2d3c4b5a",
        "username": "alice",
        "state": "active",
        "created_at": "2026-09-01T08:00:00Z",
        "roles": [
            { "role": "operator", "customer_id": "c1a2b3c4-d5e6-4f70-8a9b-0c1d2e3f4a5b", "site_id": null },
            { "role": "administrator", "customer_id": null, "site_id": "e7f8a9b0-c1d2-4e3f-9a4b-5c6d7e8f9a0b" }
        ],
        "link_expires_at": null
    })
}

/// List users
///
/// Every account, sorted by user name, with its state, its roles and the
/// expiry of an unused password link. Password hashes and links are
/// never shown. Not paged: the list stays small.
#[utoipa::path(get, path = "/api/v1/users", tag = "users",
    responses((status = 200, description = "Every account", body = user_admin::UserList,
        example = json!({ "users": [user_example()] }))))]
fn list_users() {}

/// Remove a user
///
/// Deletes the account with its roles, sessions and password link; API
/// keys it made stop working. What it did stays in the audit log and the
/// history it left. Audited as `user.remove` with the user name.
///
/// Errors: 404 `not_found` for an unknown ID; 409 `conflict` for the
/// caller's own account (the key's maker) or the last active
/// administrator of everything.
#[utoipa::path(delete, path = "/api/v1/users/{id}", tag = "users",
    params(("id" = uuid::Uuid, Path, description = "The user's ID")),
    responses(
        (status = 204, description = "Removed"),
        (status = 409, body = ErrorBody, description = "Own account or last administrator: `conflict`"),
    ))]
fn remove_user() {}

/// Disable or enable a user
///
/// Disabling ends the user's sessions at once, voids their password link
/// and stops the API keys they made; enabling lets them sign in again
/// and their unexpired keys work again. Audited as `user.disable` or
/// `user.enable` when the state changes; sending the current state
/// changes nothing, so it is safe to repeat.
///
/// Errors: 404 `not_found` for an unknown ID; 409 `conflict` for
/// disabling the caller's own account or the last active administrator
/// of everything.
#[utoipa::path(put, path = "/api/v1/users/{id}/disabled", tag = "users",
    params(("id" = uuid::Uuid, Path, description = "The user's ID")),
    request_body(content = user_admin::DisabledRequest, example = json!({ "disabled": true })),
    responses(
        (status = 200, description = "The user as they now stand", body = user_admin::UserView,
            example = disabled_example),
        (status = 409, body = ErrorBody, description = "Own account or last administrator: `conflict`"),
    ))]
fn set_disabled() {}

fn disabled_example() -> serde_json::Value {
    let mut user = user_example();
    user["state"] = json!("disabled");
    user
}

/// Grant a role
///
/// Gives the user a role on everything (both IDs absent), on one customer
/// and all its sites, or on one site. Takes effect at the user's next
/// request, for their sessions and API keys alike. Granting a role the
/// user already holds changes nothing, so it is safe to repeat. Audited
/// as `user.grant`.
///
/// Errors: 400 `invalid` for both a customer and a site, or one that does
/// not exist; 404 `not_found` for an unknown user.
#[utoipa::path(post, path = "/api/v1/users/{id}/roles", tag = "users",
    params(("id" = uuid::Uuid, Path, description = "The user's ID")),
    request_body(content = user_admin::GrantRequest, example = json!({
        "role": "operator", "customer_id": "c1a2b3c4-d5e6-4f70-8a9b-0c1d2e3f4a5b"
    })),
    responses((status = 200, description = "The user with their roles", body = user_admin::UserView,
        example = user_example)))]
fn grant_role() {}

/// Revoke a role
///
/// Removes one grant, given in the body exactly as listed. Takes effect at
/// the user's next request. Revoking a grant the user does not hold
/// changes nothing. Audited as `user.revoke`.
///
/// Errors: 400 `invalid` for both a customer and a site; 404 `not_found`
/// for an unknown user; 409 `conflict` when it would leave no active
/// administrator of everything.
#[utoipa::path(delete, path = "/api/v1/users/{id}/roles", tag = "users",
    params(("id" = uuid::Uuid, Path, description = "The user's ID")),
    request_body(content = user_admin::GrantRequest, example = json!({
        "role": "administrator", "site_id": "e7f8a9b0-c1d2-4e3f-9a4b-5c6d7e8f9a0b"
    })),
    responses(
        (status = 200, description = "The user with their roles", body = user_admin::UserView,
            example = json!({
                "id": "5b0e3c2a-1d4f-4e6b-8a9c-7f1e2d3c4b5a", "username": "alice", "state": "active",
                "created_at": "2026-09-01T08:00:00Z",
                "roles": [{ "role": "operator", "customer_id": "c1a2b3c4-d5e6-4f70-8a9b-0c1d2e3f4a5b", "site_id": null }],
                "link_expires_at": null
            })),
        (status = 409, body = ErrorBody, description = "Last administrator: `conflict`"),
    ))]
fn revoke_role() {}

fn license_example() -> serde_json::Value {
    json!({
    "available": true,
    "state": "valid",
    "license": {
        "format": 2,
        "id": "0b6e4c1a-7f3d-4e2b-9a8c-5d1f0e2b3c4a",
        "customer": "Acme",
        "edition": "enterprise",
        "tier": "enterprise",
        "managed_servers": 250,
        "server_instances": 3,
        "support": "support@example.com",
        "issued": "2026-10-01",
        "expires": "2027-09-30"
    },
    "problem": null,
    "installed_at": "2026-10-02T08:12:44Z",
    "installed_by": "admin",
    "added": [{ "name": "central-2", "admitted_at": "2026-10-02T08:20:01Z", "running": true }],
    "managed_servers_in_use": 212
    })
}

/// Read the license
///
/// Whether this build includes the Enterprise edition, the installed
/// license key's state and terms (tier, customer, servers covered,
/// support contact, dates), the central servers added under it and the
/// managed servers now. No key means the free Community tier.
/// Without the Enterprise edition only `available: false` is answered.
#[utoipa::path(get, path = "/api/v1/license", tag = "license",
    responses((status = 200, description = "The license", body = LicenseView, example = license_example)))]
fn get_license() {}

/// Install a license key
///
/// Installs the key in place of any other and answers the new state. A key
/// that does not verify with this build's public keys, or has expired, is
/// refused. Audited as `license.install` with the licensee, the tier, the
/// numbers of managed and central servers and the expiry. Safe to repeat
/// with the same key.
///
/// Errors: 400 `invalid` for a key that does not verify or has expired;
/// 409 `conflict` when the build has no Enterprise edition.
#[utoipa::path(put, path = "/api/v1/license", tag = "license",
    request_body(content = license::InstallRequest, example = json!({
        "text": "-----BEGIN MONITORFLOW LICENSE-----\neyJmb3JtYXQiOjEsImN1c3RvbWVyIjoiQWNtZSJ9\n-----END MONITORFLOW LICENSE-----\n-----BEGIN MONITORFLOW LICENSE SIGNATURE-----\nMEUCIQDx2b4kZ0n8vJ1yQ3pT6w9sR2fH5aL7cE0dG4iK8mN1oA==\n-----END MONITORFLOW LICENSE SIGNATURE-----"
    })),
    responses(
        (status = 200, description = "Installed; the license as it now stands", body = LicenseView, example = license_example),
        (status = 409, body = ErrorBody, description = "No Enterprise edition in this build: `conflict`"),
    ))]
fn install_license() {}

/// Remove the license key
///
/// Central servers already added keep running; no new one can be added.
/// Audited as `license.remove`. 404 `not_found` when no key is installed
/// or the build has no Enterprise edition.
#[utoipa::path(delete, path = "/api/v1/license", tag = "license",
    responses((status = 204, description = "Removed")))]
fn remove_license() {}

/// Free an added central server's place
///
/// Forgets the additional central server `name`, freeing its place under
/// the license. Stop it first: it is added again at its next start if a
/// place is free, and refused otherwise.
/// Audited as `server_instance.remove`. 404 `not_found` for a name never
/// added, or without the Enterprise edition.
#[utoipa::path(delete, path = "/api/v1/license/servers/{name}", tag = "license",
    params(("name" = String, Path, description = "An `added[].name`")),
    responses((status = 204, description = "Removed")))]
fn remove_license_server() {}
