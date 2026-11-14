//! The public API's description of the server-wide settings. Merged into
//! the document by [`super::openapi::build`].

#![allow(dead_code)] // The functions exist for their `#[utoipa::path]`.

use serde::Serialize;
use serde_json::json;
use utoipa::{OpenApi, ToSchema};

use super::openapi::{Common, ErrorBody};
use super::{ai, auto_update, public_address, server_settings, settings};
use crate::auto_update::policy;

#[derive(OpenApi)]
#[openapi(
    paths(
        get_agent_install, set_agent_install, get_public_address, set_public_address, get_ai,
        set_ai, remove_ai_key, test_ai, list_ai_models, get_server_settings,
        set_server_settings, set_retention, get_auto_update, set_auto_update,
    ),
    components(schemas(AiModel, AiTestResult)),
    modifiers(&Common),
    tags(
        (name = "settings", description = "Settings of the whole central server: where install \
commands download the agent, the address hosts reach the server at, AI, session lifetimes, the \
package mirror's address, how long metrics and logs are kept, and automatic agent updates. Values \
the server's configuration file sets win: they are shown as locked and a request to change them \
is refused with 409. Every change is in the audit log with the old and new values. Changes are \
a global administrator's; with a key, access \"Administer\" and reach \"Everything\", in the \
\"administration\" area, except automatic updates, in the \"updates\" area."),
    )
)]
pub(super) struct SettingsDoc;

/// A model of the short list.
#[derive(Serialize, ToSchema)]
pub(super) struct AiModel {
    /// The id to send as `model`.
    id: String,
    /// Its name to show.
    name: String,
    /// When to pick it, in a few words.
    note: String,
}

/// Body of `POST /api/settings/ai/test`.
#[derive(Serialize, ToSchema)]
struct AiTestResult {
    /// The provider answered with this key and model.
    ok: bool,
    /// Why it failed, in plain words; `null` when `ok`.
    error: Option<String>,
}

fn agent_install_example() -> serde_json::Value {
    json!({
        "source": "central",
        "custom_url": null,
        "source_from_config": false,
        "custom_url_from_config": false,
        "github_url": "https://github.com/Kidoly/monitorflow/releases/latest/download",
        "central": {
            "version": "0.4.0",
            "path": "/agent/0.4.0",
            "sums_sha256": "2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae",
            "installer_sha256": "fcde2b2edba56bf408601fb721fe9b5c338d10ee429ea04fae5511b68fbf8fb9",
            "msi_sha256": null
        }
    })
}

/// Read the agent install source
///
/// Where the install commands of new enrollment tokens and install keys
/// download the agent from by default: the project's public releases,
/// this server's release cache (`central`, with the release it holds) or
/// an https directory of the operator's. Any administrator may read it,
/// of a site, a customer or everything, since they make install commands.
#[utoipa::path(get, path = "/api/v1/settings/agent-install", tag = "settings",
    responses((status = 200, description = "The install source", body = settings::AgentInstall,
        example = agent_install_example)))]
fn get_agent_install() {}

/// Set the agent install source
///
/// Replaces both values; install commands made afterwards use them, those
/// already shown do not change. A value the configuration sets may only be
/// sent again unchanged. Audited as `settings.agent_install`. Safe to
/// repeat.
///
/// Errors: 400 `invalid` for a `custom_url` that is not https or has a
/// user, query or fragment, or `custom` without one; 409 `conflict` when
/// it would change what the configuration sets.
#[utoipa::path(put, path = "/api/v1/settings/agent-install", tag = "settings",
    request_body(content = settings::AgentInstallRequest, example = json!({ "source": "central", "custom_url": null })),
    responses(
        (status = 200, description = "The install source as it now stands", body = settings::AgentInstall,
            example = agent_install_example),
        (status = 409, body = ErrorBody, description = "Set by the configuration: `conflict`"),
    ))]
fn set_agent_install() {}

fn public_address_example() -> serde_json::Value {
    json!({
        "address": "monitor.example.com",
        "default": "monitor.example.com",
        "names": ["monitor.example.com", "10.0.0.5"],
        "certified": true,
        "upstream_url": "https://monitor.example.com:7443",
        "enroll_url": "https://monitor.example.com:7445"
    })
}

/// Read the public address
///
/// The address hosts reach this server at in install commands, the names
/// its certificate covers, and whether the address is one of them: agents
/// refuse to connect otherwise. Any administrator may read it.
#[utoipa::path(get, path = "/api/v1/settings/public-address", tag = "settings",
    responses((status = 200, description = "The public address", body = public_address::PublicAddress,
        example = public_address_example)))]
fn get_public_address() {}

/// Set the public address
///
/// A DNS name or IP address, without a port: the commands use the
/// server's published ports. `null` or empty goes back to the first name
/// of the certificate. An address the certificate does not cover is
/// accepted and `certified` says so. Affects install commands made
/// afterwards only. Audited as `settings.public_address`. Safe to repeat.
///
/// Errors: 400 `invalid` for a port or a malformed name.
#[utoipa::path(put, path = "/api/v1/settings/public-address", tag = "settings",
    request_body(content = public_address::SetRequest, example = json!({ "address": "monitor.example.com" })),
    responses((status = 200, description = "The public address as it now stands",
        body = public_address::PublicAddress, example = public_address_example)))]
fn set_public_address() {}

fn ai_example() -> serde_json::Value {
    json!({
        "enabled": true,
        "from_config": false,
        "provider": "openai_compatible",
        "has_key": false,
        "base_url": "http://llm.example.com:8080/v1",
        "model": "local-model",
        "models": [{ "id": "model-large", "name": "Model Large", "note": "Recommended: the best diagnoses" }]
    })
}

/// Read the AI settings
///
/// Whether AI is on, with which provider and model, and whether a key is
/// stored. The key is never shown. Every signed-in user may read this,
/// whatever their role, since approvals say when AI is off; `base_url` is
/// `null` for all but global administrators.
#[utoipa::path(get, path = "/api/v1/settings/ai", tag = "settings",
    responses((status = 200, description = "The AI settings", body = ai::AiView, example = ai_example)))]
fn get_ai() {}

/// Set the AI settings
///
/// Replaces everything but the key, which absent or empty keeps the
/// stored one. The stored key belongs to its provider, and for
/// `openai_compatible` to its `base_url`: changing either forgets it, and
/// sending the old key to a new address requires typing it again. Turning
/// AI on, or changing what it uses while on, first sends a test request
/// to the provider with these settings. The key is stored encrypted.
/// Audited as `settings.ai`, with `key_changed` but never the key.
///
/// Errors: 400 `invalid` for an unknown model, a bad address, a missing
/// key or a test that failed, in plain words; 409 `conflict` when the
/// configuration sets AI.
#[utoipa::path(put, path = "/api/v1/settings/ai", tag = "settings",
    request_body(content = ai::SetRequest, example = json!({
        "provider": "openai_compatible",
        "base_url": "http://llm.example.com:8080/v1",
        "model": "local-model",
        "enabled": true
    })),
    responses(
        (status = 200, description = "The AI settings as they now stand", body = ai::AiView, example = ai_example),
        (status = 409, body = ErrorBody, description = "Set by the configuration: `conflict`"),
    ))]
fn set_ai() {}

/// Remove the AI key
///
/// Forgets the stored key, which turns AI off. Audited as `settings.ai`.
/// Safe to repeat. 409 `conflict` when the configuration sets AI.
#[utoipa::path(delete, path = "/api/v1/settings/ai", tag = "settings",
    responses(
        (status = 200, description = "The AI settings as they now stand", body = ai::AiView, example = json!({
            "enabled": false, "from_config": false, "provider": "anthropic", "has_key": false,
            "base_url": null, "model": "model-large",
            "models": [{ "id": "model-large", "name": "Model Large", "note": "Recommended: the best diagnoses" }]
        })),
        (status = 409, body = ErrorBody, description = "Set by the configuration: `conflict`"),
    ))]
fn remove_ai_key() {}

/// Test AI settings
///
/// Sends one small request to the provider with the settings given, the
/// stored ones filling what is left out (the key only for the same
/// provider and address), and answers whether it worked. Saves nothing.
/// The central server connects to `base_url` as given, so only a global
/// administrator may call it; the stored key is never sent to an address
/// other than the one it was entered for.
#[utoipa::path(post, path = "/api/v1/settings/ai/test", tag = "settings",
    request_body(content = ai::TestRequest, example = json!({
        "provider": "openai_compatible", "base_url": "http://llm.example.com:8080/v1", "model": "local-model"
    })),
    responses((status = 200, description = "Whether it worked", body = AiTestResult,
        example = json!({ "ok": false, "error": "The server at llm.example.com did not answer within 30 seconds." }))))]
fn test_ai() {}

/// List a provider's models
///
/// Asks the provider of the settings given, completed as for a test, for
/// the models it offers. Saves nothing. `models` is empty and `error`
/// says why when the provider could not say.
#[utoipa::path(post, path = "/api/v1/settings/ai/models", tag = "settings",
    request_body(content = ai::TestRequest, example = json!({
        "provider": "openai_compatible", "base_url": "http://llm.example.com:8080/v1"
    })),
    responses((status = 200, description = "The models offered", body = ai::ModelList,
        example = json!({ "models": ["local-model", "local-model-large"], "error": null }))))]
fn list_ai_models() {}

fn server_settings_example() -> serde_json::Value {
    json!({
        "session_lifetime": { "value": 43200, "default": 43200, "locked": false },
        "session_idle_timeout": { "value": 1800, "default": 1800, "locked": false },
        "mirror_public_url": { "value": "http://mirror.example.com:7480", "default": null, "locked": false },
        "retention": {
            "metrics": { "value": "90d", "locked": false },
            "logs": { "value": "30d", "locked": false },
            "default_metrics": "90d",
            "default_logs": "30d",
            "max_days": 3650,
            "updater_alive": true,
            "applying": false,
            "error": null
        }
    })
}

/// Read the server settings
///
/// Session lifetimes of the web interface, the package mirror's address,
/// and how long the stores keep metrics and logs. `retention` is `null`
/// outside the compose deployment, where the updater does not manage the
/// stores.
#[utoipa::path(get, path = "/api/v1/settings/server", tag = "settings",
    responses((status = 200, description = "The server settings", body = server_settings::ServerSettings,
        example = server_settings_example)))]
fn get_server_settings() {}

/// Set the server settings
///
/// Replaces all three values; `null` is the default. Open sessions follow
/// a shorter lifetime at once, and a longer one applies from the next
/// sign-in. Audited as `settings.server`. Safe to repeat.
///
/// Errors: 400 `invalid` for a value out of range, an idle timeout longer
/// than the lifetime or a bad URL; 409 `conflict` when it would change
/// what the configuration sets.
#[utoipa::path(put, path = "/api/v1/settings/server", tag = "settings",
    request_body(content = server_settings::SetRequest, example = json!({
        "session_lifetime": 43200, "session_idle_timeout": 1800, "mirror_public_url": "http://mirror.example.com:7480"
    })),
    responses(
        (status = 200, description = "The server settings as they now stand", body = server_settings::ServerSettings,
            example = server_settings_example),
        (status = 409, body = ErrorBody, description = "Set by the configuration: `conflict`"),
    ))]
fn set_server_settings() {}

/// Change how long metrics and logs are kept
///
/// Asks the updater to restart the stores with the new retention; `null`
/// leaves a store alone. Data older than a shorter retention is deleted
/// by the store and cannot be recovered. Answers at once with
/// `retention.applying` true; follow it with `GET
/// /api/v1/settings/server`. Audited as `settings.retention`.
///
/// Errors: 400 `invalid` outside 1 to 3650 days; 409 `conflict` without
/// an updater, while it is not running or another change is applied, or
/// for a store the deployment's `.env` sets.
#[utoipa::path(put, path = "/api/v1/settings/retention", tag = "settings",
    request_body(content = server_settings::RetentionBody, example = json!({ "metrics_days": 180, "logs_days": null })),
    responses(
        (status = 202, description = "Asked of the updater", body = server_settings::ServerSettings,
            example = server_settings_example),
        (status = 409, body = ErrorBody, description = "No updater, a change under way, or set by `.env`: `conflict`"),
    ))]
fn set_retention() {}

/// Read the automatic update settings
///
/// Whether agents and relays update by themselves to the newest stable
/// release the server holds, by default for sites that do not choose,
/// whether that is paused, and how many failures halt a rollout. A global
/// administrator's; with a key, the "updates" area.
#[utoipa::path(get, path = "/api/v1/settings/auto-update", tag = "settings",
    responses((status = 200, description = "The settings as they apply", body = policy::Settings,
        example = json!({ "enabled": true, "enabled_locked": false, "paused": false, "failure_threshold": 2 }))))]
fn get_auto_update() {}

/// Change the automatic update settings
///
/// Changes the fields sent and keeps the others. Pausing stops new
/// automatic update commands from being queued; those already sent run.
/// Audited as `settings.auto_update`. Safe to repeat. With a key, the
/// "updates" area.
///
/// Errors: 400 `invalid` for a threshold outside 1 to 1000; 409
/// `conflict` for `enabled` while the configuration sets it.
#[utoipa::path(put, path = "/api/v1/settings/auto-update", tag = "settings",
    request_body(content = auto_update::SettingsRequest, example = json!({ "paused": true })),
    responses(
        (status = 200, description = "The settings as they now apply", body = policy::Settings,
            example = json!({ "enabled": true, "enabled_locked": false, "paused": true, "failure_threshold": 2 })),
        (status = 409, body = ErrorBody, description = "Set by the configuration: `conflict`"),
    ))]
fn set_auto_update() {}
