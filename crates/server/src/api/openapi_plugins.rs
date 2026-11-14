//! The public API's description of plugins and their publishers.
//! Merged into the document by [`super::openapi::build`].

#![allow(dead_code)] // The functions exist for their `#[utoipa::path]`.

use serde_json::json;
use utoipa::{OpenApi, ToSchema};

use super::openapi::{Common, ErrorBody};
use super::plugins;

#[derive(OpenApi)]
#[openapi(
    paths(
        list, inspect, install, usable, set_places, set_enabled, remove, publishers,
        add_publisher, remove_publisher,
    ),
    components(schemas(PluginFile)),
    modifiers(&Common),
    tags((name = "plugins", description = "Signed WebAssembly plugins that add checks, run by \
agents, and notification channels, run by the central server, in a sandbox that reaches only the \
hosts its manifest names. Installing, updating, placing, turning off and removing plugins, and \
trusting the publisher keys that sign them, are a global administrator's: a plugin runs on the \
central server or on the agents it is given to. With a key, access \"Administer\", reach \
\"Everything\" and the \"administration\" area. Which plugins may be used where is read by \
anyone who can read, limited to the places they see. Every change is in the audit log.")),
)]
pub(super) struct PluginsDoc;

/// A plugin file as `monitorflow-plugin pack` writes it, at most 3 MiB.
#[derive(ToSchema)]
#[schema(value_type = String, format = Binary)]
struct PluginFile(Vec<u8>);

const PLUGIN: &str = "4d3c2b1a-0f9e-4d8c-b7a6-5e4d3c2b1a0f";
const PUBLISHER: &str = "8e7d6c5b-4a39-4281-9f0e-1d2c3b4a5968";
/// The first 8 bytes of the key's SHA-256, hex.
const FINGERPRINT: &str = "3fa1c09e5b7d2e64";

fn settings_example() -> serde_json::Value {
    json!([
        { "key": "url", "label": "Webhook address", "type": "url", "required": true },
        { "key": "token", "label": "Token", "type": "secret", "required": true, "help": "Given by the receiving service" }
    ])
}

fn plugin_example() -> serde_json::Value {
    json!({
        "id": PLUGIN,
        "name": "chat-relay",
        "kind": "channel",
        "version": "1.2.0",
        "title": "Chat relay",
        "description": "Posts alerts to a chat room through a webhook.",
        "official": false,
        "publisher": "Acme tools",
        "fingerprint": FINGERPRINT,
        "capabilities": ["Sends notifications from the central server", "Connects to the address in its Webhook address setting"],
        "settings": settings_example(),
        "enabled": true,
        "places": [{ "customer_id": "c1a2b3c4-d5e6-4f70-8a9b-0c1d2e3f4a5b", "customer_name": "acme", "site_id": null, "site_name": null }],
        "channels": 2,
        "checks": 0,
        "installed_at": "2026-10-20T10:00:00Z",
        "updated_at": "2026-11-05T09:30:00Z"
    })
}

fn inspection_example() -> serde_json::Value {
    json!({
        "name": "chat-relay",
        "kind": "channel",
        "version": "1.3.0",
        "title": "Chat relay",
        "description": "Posts alerts to a chat room through a webhook.",
        "official": false,
        "publisher": "Acme tools",
        "fingerprint": FINGERPRINT,
        "capabilities": ["Sends notifications from the central server", "Connects to the address in its Webhook address setting"],
        "settings": settings_example(),
        "sha256": "0f4e3d2c1b0a99887766554433221100ffeeddccbbaa99887766554433221100",
        "update": { "from_version": "1.2.0", "adds": [], "drops": [] }
    })
}

/// List installed plugins
///
/// Every installed plugin with its publisher, what it asks for, where it
/// may be used and how many channels and checks use it.
#[utoipa::path(get, path = "/api/v1/plugins", tag = "plugins",
    responses((status = 200, description = "Installed plugins", body = plugins::PluginList,
        example = json!({ "plugins": [plugin_example()] }))))]
fn list() {}

/// Inspect a plugin file
///
/// Reads and verifies the file sent as the body, without storing
/// anything: its manifest, its publisher, what it asks for, the SHA-256
/// to install it with, and what an update would add or drop. Send the
/// raw file as `application/octet-stream`.
///
/// Errors: 400 `invalid` for a file that does not verify, is signed by a
/// key not trusted yet (the message gives its fingerprint), or cannot
/// replace the installed version (not newer, another publisher or
/// kind).
#[utoipa::path(post, path = "/api/v1/plugins/inspect", tag = "plugins",
    request_body(content = PluginFile, content_type = "application/octet-stream"),
    responses((status = 200, description = "What the file is", body = plugins::Inspection,
        example = inspection_example)))]
fn inspect() {}

/// Install or update a plugin
///
/// Installs the plugin of the body, which must be the file inspected:
/// `sha256` is the one inspection answered. A new plugin is used nowhere
/// until places are set (201); an update keeps its places and settings,
/// and the new version replaces the old one wherever it is used (200).
/// Audited as `plugin.install` or `plugin.update` with the version,
/// fingerprint, SHA-256 and capabilities. Installing the same file again
/// is refused as not newer.
///
/// Errors: 400 `invalid` as for an inspection, or when the body is not
/// the file of `sha256`.
#[utoipa::path(post, path = "/api/v1/plugins", tag = "plugins",
    params(plugins::InstallQuery),
    request_body(content = PluginFile, content_type = "application/octet-stream"),
    responses(
        (status = 201, description = "Installed", body = plugins::PluginView, example = plugin_example),
        (status = 200, description = "Updated", body = plugins::PluginView, example = plugin_example),
    ))]
fn install() {}

/// List plugins usable here
///
/// Plugins turned on, with the places the caller can see where they may
/// be used, for making channels and checks. A plugin with no place the
/// caller sees is left out. Any role may read it; with a key, the
/// "administration" area.
#[utoipa::path(get, path = "/api/v1/plugins/usable", tag = "plugins",
    responses((status = 200, description = "Plugins the caller may use", body = plugins::UsableList,
        example = json!({ "plugins": [{
            "name": "chat-relay", "kind": "channel", "version": "1.2.0", "title": "Chat relay",
            "description": "Posts alerts to a chat room through a webhook.",
            "capabilities": ["Sends notifications from the central server", "Connects to the address in its Webhook address setting"],
            "settings": settings_example(), "everywhere": false,
            "customer_ids": ["c1a2b3c4-d5e6-4f70-8a9b-0c1d2e3f4a5b"], "site_ids": []
        }] }))))]
fn usable() {}

/// Set where a plugin may be used
///
/// Replaces the places: everything (both IDs absent), customers or
/// sites, at most 200; an empty list means nowhere. Channels and checks
/// already using it are not removed. Audited as `plugin.places`. Safe to
/// repeat.
///
/// Errors: 400 `invalid` for a place with both IDs, more than 200, or a
/// customer or site that does not exist; 404 `not_found` for an unknown
/// plugin.
#[utoipa::path(put, path = "/api/v1/plugins/{id}/places", tag = "plugins",
    params(("id" = uuid::Uuid, Path, description = "The plugin's ID")),
    request_body(content = plugins::PlacesRequest, example = json!({
        "places": [{ "customer_id": "c1a2b3c4-d5e6-4f70-8a9b-0c1d2e3f4a5b" }, { "site_id": "e7f8a9b0-c1d2-4e3f-9a4b-5c6d7e8f9a0b" }]
    })),
    responses((status = 200, description = "The plugin as it now stands", body = plugins::PluginView,
        example = plugin_example)))]
fn set_places() {}

/// Turn a plugin on or off
///
/// Turns every use of the plugin on or off at once: its channels send
/// nothing and its checks do not run while it is off. Audited as
/// `plugin.enable` or `plugin.disable`, also when it already was. 404
/// `not_found` for an unknown plugin.
#[utoipa::path(put, path = "/api/v1/plugins/{id}/enabled", tag = "plugins",
    params(("id" = uuid::Uuid, Path, description = "The plugin's ID")),
    request_body(content = plugins::EnabledRequest, example = json!({ "enabled": false })),
    responses((status = 200, description = "The plugin as it now stands", body = plugins::PluginView,
        example = plugin_example)))]
fn set_enabled() {}

/// Remove a plugin
///
/// Deletes the plugin and its places. Audited as `plugin.remove`.
///
/// Errors: 404 `not_found` for an unknown plugin; 409 `conflict` while a
/// channel or a check uses it: remove those first, or turn it off.
#[utoipa::path(delete, path = "/api/v1/plugins/{id}", tag = "plugins",
    params(("id" = uuid::Uuid, Path, description = "The plugin's ID")),
    responses(
        (status = 204, description = "Removed"),
        (status = 409, body = ErrorBody, description = "Still used: `conflict`"),
    ))]
fn remove() {}

fn publisher_example() -> serde_json::Value {
    json!({
        "id": PUBLISHER,
        "name": "Acme tools",
        "public_key": "q5Z0rJt4YQm2bVnE8cW1xLp9sKd3fHg6uA7iO0yT2eQ=",
        "fingerprint": FINGERPRINT,
        "plugins": ["chat-relay"],
        "created_at": "2026-10-20T09:55:00Z"
    })
}

/// List trusted publishers
///
/// The publisher keys trusted to sign plugins, with the installed plugins
/// each signed, and the fingerprints of the official keys built in.
#[utoipa::path(get, path = "/api/v1/plugins/publishers", tag = "plugins",
    responses((status = 200, description = "Trusted publishers", body = plugins::PublisherList,
        example = json!({ "publishers": [publisher_example()], "official": [] }))))]
fn publishers() {}

/// Trust a publisher
///
/// Trusts a public key to sign plugins, so they can be installed. Check
/// its fingerprint with its publisher first: whatever it signs may run on
/// the central server or on agents. Audited as `plugin_publisher.add`.
///
/// Errors: 400 `invalid` for a name not 1 to 80 characters or a key that
/// does not parse; 409 `conflict` for a key trusted already or built in.
#[utoipa::path(post, path = "/api/v1/plugins/publishers", tag = "plugins",
    request_body(content = plugins::PublisherRequest, example = json!({
        "name": "Acme tools", "public_key": "q5Z0rJt4YQm2bVnE8cW1xLp9sKd3fHg6uA7iO0yT2eQ="
    })),
    responses(
        (status = 201, description = "Trusted", body = plugins::PublisherView, example = publisher_example),
        (status = 409, body = ErrorBody, description = "Trusted already or built in: `conflict`"),
    ))]
fn add_publisher() {}

/// Stop trusting a publisher
///
/// Audited as `plugin_publisher.remove`.
///
/// Errors: 404 `not_found` for an unknown ID; 409 `conflict` while a
/// plugin it signed is installed.
#[utoipa::path(delete, path = "/api/v1/plugins/publishers/{id}", tag = "plugins",
    params(("id" = uuid::Uuid, Path, description = "The publisher's ID")),
    responses(
        (status = 204, description = "Removed"),
        (status = 409, body = ErrorBody, description = "A plugin it signed is installed: `conflict`"),
    ))]
fn remove_publisher() {}
