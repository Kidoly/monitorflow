//! The public API's description of sign-in providers (ADR-0145). Merged
//! into the document by [`super::openapi::build`]. The sign-in itself is
//! for people in a browser and stays out of v1, under `auth`.

#![allow(dead_code)] // The functions exist for their `#[utoipa::path]`.

use serde_json::json;
use utoipa::OpenApi;

use super::identity_providers as idp;
use super::openapi::{Common, ErrorBody};
use crate::sso::config::{LdapSettings, OidcSettings};

#[derive(OpenApi)]
#[openapi(
    paths(list, create, detail, update, remove, test, groups, set_groups),
    components(schemas(OidcSettings, LdapSettings)),
    modifiers(&Common),
    tags(
        (name = "sign-in", description = "Sign-in providers: OpenID Connect providers \
(Entra ID, Google, Okta, Keycloak, authentik and any other that follows the standard) and LDAP \
directories over LDAPS or StartTLS only. People sign in through them with the provider's own \
password and second factor; their account is made at their first sign-in, and its roles are \
those its groups give under \"Groups and roles\", replaced at each sign-in and as soon as the \
groups change. Someone in no mapped group cannot sign in. One administrator of everything with \
a password kept by MonitorFlow always remains, so a provider that is down never locks everyone \
out. A provider is turned on only once its connection test passes. Secrets are stored \
encrypted and never returned. Only a global administrator manages these; with a key, access \
\"Administer\", reach \"Everything\" and the \"administration\" area. Every change is in the \
audit log. Signing in itself is done in a browser and is not part of this API."),
    ),
)]
pub(super) struct SsoDoc;

fn oidc_settings() -> serde_json::Value {
    json!({
        "kind": "oidc",
        "issuer": "https://id.example.com/realms/acme",
        "client_id": "monitorflow",
        "scopes": "openid profile email groups",
        "groups_claim": "groups",
        "ca_pem": null
    })
}

fn ldap_settings() -> serde_json::Value {
    json!({
        "kind": "ldap",
        "url": "ldaps://ldap.example.com",
        "starttls": false,
        "ca_pem": null,
        "bind_dn": "cn=monitorflow,ou=services,dc=example,dc=com",
        "user_base_dn": "ou=people,dc=example,dc=com",
        "user_filter": "(|(uid={username})(sAMAccountName={username}))",
        "group_base_dn": "ou=groups,dc=example,dc=com",
        "group_filter": "(|(member={dn})(uniqueMember={dn})(memberUid={username}))",
        "group_name_attribute": "cn",
        "nested_groups": true
    })
}

fn provider_example() -> serde_json::Value {
    json!({
        "id": "0c4b7f0e-8a51-4f3e-9d0a-2b1c3d4e5f60",
        "kind": "oidc",
        "name": "Keycloak",
        "enabled": true,
        "has_secret": true,
        "settings": oidc_settings(),
        "mappings": [
            { "group": "monitorflow-admins", "role": "administrator", "customer_id": null, "site_id": null },
            { "group": "helpdesk", "role": "viewer", "customer_id": null, "site_id": null }
        ],
        "accounts": 12,
        "created_at": "2026-11-17T08:00:00Z",
        "updated_at": "2026-11-17T08:05:00Z"
    })
}

fn list_example() -> serde_json::Value {
    json!({
        "providers": [provider_example()],
        "redirect_uri": "https://monitor.example.com/api/auth/oidc/callback",
        "local_administrators": 1
    })
}

fn groups_example() -> serde_json::Value {
    json!({ "mappings": [
        { "group": "monitorflow-admins", "role": "administrator", "customer_id": null, "site_id": null },
        { "group": "paris-ops", "role": "operator", "customer_id": null, "site_id": "e7f8a9b0-c1d2-4e3f-9a4b-5c6d7e8f9a0b" }
    ]})
}

/// List sign-in providers
///
/// Every provider, oldest first (the order directories are tried in),
/// with its groups and how many accounts sign in through it, the redirect
/// URI to enter in an OpenID Connect provider, and how many local
/// administrators keep a password.
#[utoipa::path(get, path = "/api/v1/identity-providers", tag = "sign-in",
    responses((status = 200, description = "The providers", body = idp::ProviderList, example = list_example)))]
fn list() {}

/// Add a sign-in provider
///
/// `settings.kind` is `oidc` or `ldap`. An `ldap://` address needs
/// `starttls`; plain LDAP is refused. The secret is the client secret,
/// absent for a public client, or the service account's password; it is
/// stored encrypted and never returned. `enabled: true` tests the
/// provider first, as `POST /api/v1/identity-providers/test` does.
/// Audited as `identity_provider.create`, never with the secret.
///
/// Errors: 400 `invalid` for settings that do not hold or a test that
/// failed, with the reason; 409 `conflict` for a name another provider
/// has.
#[utoipa::path(post, path = "/api/v1/identity-providers", tag = "sign-in",
    request_body(content = idp::ProviderRequest, example = json!({
        "name": "Keycloak", "enabled": false, "secret": "client-secret", "settings": oidc_settings()
    })),
    responses(
        (status = 201, description = "The provider", body = idp::ProviderView, example = provider_example),
        (status = 409, body = ErrorBody, description = "Name taken: `conflict`"),
    ))]
fn create() {}

/// Read a sign-in provider
///
/// One provider, as in the list: its settings without the secret, its
/// groups and how many accounts sign in through it.
#[utoipa::path(get, path = "/api/v1/identity-providers/{id}", tag = "sign-in",
    params(("id" = uuid::Uuid, Path, description = "The provider's ID")),
    responses((status = 200, description = "The provider", body = idp::ProviderView, example = provider_example)))]
fn detail() {}

/// Change a sign-in provider
///
/// Replaces its name, settings and whether it is on; its kind stays. An
/// empty secret keeps the stored one while the issuer or directory
/// address stays the same, and is required again otherwise. Turning it
/// on, or changing it while on, tests it first. Turning it off ends the
/// sessions of its accounts. Audited as `identity_provider.update` with
/// the old and new settings and `secret_changed`.
///
/// Errors: 400 `invalid` as for adding, or for another kind; 409
/// `conflict` for a name taken.
#[utoipa::path(put, path = "/api/v1/identity-providers/{id}", tag = "sign-in",
    params(("id" = uuid::Uuid, Path, description = "The provider's ID")),
    request_body(content = idp::ProviderRequest, example = json!({
        "name": "Directory", "enabled": true, "settings": ldap_settings()
    })),
    responses(
        (status = 200, description = "The provider as it now stands", body = idp::ProviderView, example = provider_example),
        (status = 409, body = ErrorBody, description = "Name taken: `conflict`"),
    ))]
fn update() {}

/// Remove a sign-in provider
///
/// Its accounts stay, with their history, but lose their roles, their
/// sessions and any way to sign in; remove them or give them a password
/// link under Users. Audited as `identity_provider.delete`.
#[utoipa::path(delete, path = "/api/v1/identity-providers/{id}", tag = "sign-in",
    params(("id" = uuid::Uuid, Path, description = "The provider's ID")),
    responses((status = 204, description = "Removed")))]
fn remove() {}

/// Test a sign-in provider
///
/// Checks settings without saving them. OpenID Connect: the discovery
/// document, the signing keys, and the client ID and secret, by trading
/// a code that does not exist. LDAP: the TLS connection, the service
/// account and the bases; with `username` and `password`, the whole
/// sign-in, the groups found and, with `id`, the roles they give. With
/// `id` and no secret, the stored secret is used, only for the address it
/// was entered for. A failed test is a 200 with `ok: false` and the
/// reason.
#[utoipa::path(post, path = "/api/v1/identity-providers/test", tag = "sign-in",
    request_body(content = idp::TestRequest, example = json!({
        "id": "0c4b7f0e-8a51-4f3e-9d0a-2b1c3d4e5f60", "settings": ldap_settings(),
        "username": "alice", "password": "her password"
    })),
    responses((status = 200, description = "What the test found", body = idp::TestResult, example = json!({
        "ok": true, "error": null, "oidc": null,
        "ldap": { "active_directory": false, "dn": "uid=alice,ou=people,dc=example,dc=com",
                  "groups": ["monitorflow-admins", "staff"] },
        "roles": [{ "role": "administrator", "customer_id": null, "site_id": null }]
    }))))]
fn test() {}

/// Read the groups of a sign-in provider
///
/// Which role each group gives, and where: everything (both IDs null), a
/// customer with its sites, or a site.
#[utoipa::path(get, path = "/api/v1/identity-providers/{id}/groups", tag = "sign-in",
    params(("id" = uuid::Uuid, Path, description = "The provider's ID")),
    responses((status = 200, description = "The groups and their roles", body = idp::Mappings, example = groups_example)))]
fn groups() {}

/// Replace the groups of a sign-in provider
///
/// The whole list at once, at most 500. Group names are compared without
/// regard to case. The roles of every account of the provider are
/// replaced at once from the groups it had at its last sign-in; an
/// account left without any loses its sessions and cannot sign in.
/// Audited as `identity_provider.groups`.
///
/// Errors: 400 `invalid` for an empty group, both a customer and a site,
/// or one that does not exist.
#[utoipa::path(put, path = "/api/v1/identity-providers/{id}/groups", tag = "sign-in",
    params(("id" = uuid::Uuid, Path, description = "The provider's ID")),
    request_body(content = idp::Mappings, example = groups_example),
    responses((status = 200, description = "The groups as they now stand", body = idp::Mappings, example = groups_example)))]
fn set_groups() {}
