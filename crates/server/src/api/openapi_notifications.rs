//! Notification channels in the OpenAPI document (ADR-0060, ADR-0077,
//! ADR-0078): merged into it by [`super::build`].

use serde_json::json;
use utoipa::OpenApi;

use super::{Common, ErrorBody};
use crate::api::notifications;

#[derive(OpenApi)]
#[openapi(
    paths(
        list_notification_channels,
        create_notification_channel,
        update_notification_channel,
        delete_notification_channel,
        test_notification_channel,
    ),
    components(schemas(ErrorBody)),
    modifiers(&Common),
    tags((name = "notifications", description = "Notification channels: where messages to people go \
when an incident opens, gets worse or ends, a command waits for approval, an update fails, a backup fails \
or a new version is out. A channel is an email address list sent through a mail server, a webhook, a \
chat channel (`slack`, `teams`, `discord`) or a plugin's, and notifies about everything, one customer or \
one site. Only administrators reach channels, and only those of what they administer: everything takes a \
global administrator (a key with every reach), a customer an administrator of it, a site an administrator \
of it or of its customer; others answer 404. Secrets are write-only: the mail password, signing secret, \
chat webhook URL or plugin secret settings go in `secret` and never come back, a channel says only \
`has_secret`, and a webhook shows only its URL's origin, as its path and query often hold a token. A \
channel saved by anyone but a global administrator only sends to public addresses. Every change is in \
the audit log, with the key's ID. API keys need the `alerts` area. For machines rather than people, use \
an integration."))
)]
pub(super) struct Doc;

/// List notification channels
///
/// The channels of what the key's maker administers, oldest first, in
/// full: there are few. Administrators only.
#[utoipa::path(get, path = "/api/v1/notifications/channels", tag = "notifications",
    responses(
        (status = 200, description = "The channels", body = notifications::ChannelList, example = json!({
            "channels": [channel_email(), channel_webhook()],
        })),
        (status = 403, description = "Not an administrator: `forbidden`; outside the key's areas: `api_key_scope`", body = ErrorBody),
    ))]
fn list_notification_channels() {}

/// Add a notification channel
///
/// Administrators of what it notifies about: everything, the customer of
/// `customer_id` or the site of `site_id`. The channel starts hearing of
/// what it covers at once; send a test to check it. Audited as
/// `notification.channel_create`, without the secret and with only the
/// host of a webhook. Not safe to repeat: send `Idempotency-Key` to retry.
#[utoipa::path(post, path = "/api/v1/notifications/channels", tag = "notifications",
    request_body(content = notifications::ChannelRequest, example = json!({
        "kind": "email",
        "settings": {
            "host": "smtp.example.com", "port": 587, "username": "alerts@example.com",
            "from": "", "to": ["ops@example.com"],
        },
        "secret": "mail-password",
        "customer_id": "4f7d1c2e-0a5b-4c3d-9e8f-1a2b3c4d5e6f",
        "site_id": null,
        "follow_up_only": false,
    })),
    responses(
        (status = 201, description = "Added", body = notifications::ChannelView, example = json!(channel_email())),
        (status = 400, description = "`invalid`: unknown kind, settings that do not fit it (no recipient, a URL with a password, a chat URL of another service…), a secret over 1024 bytes, a customer and a site together, an unknown customer or site, a plugin not turned on there", body = ErrorBody),
        (status = 403, description = "`forbidden`: not an administrator of what it would notify about; `api_key_scope`", body = ErrorBody),
    ))]
fn create_notification_channel() {}

/// Change a notification channel
///
/// Replaces its kind, settings, what it notifies about and
/// `follow_up_only`; the secret
/// only when one is given. Administrators of what it notifies about now
/// and of what it will. A stored secret stays only with the place it was
/// typed for: changing the mail server, its port or username, or a
/// webhook's URL, or giving a new chat URL, needs the secret again, or
/// the answer is 400. A webhook `url` left empty keeps the stored one,
/// which the API never shows. Audited as `notification.channel_update`.
/// Safe to repeat.
#[utoipa::path(put, path = "/api/v1/notifications/channels/{id}", tag = "notifications",
    params(("id" = uuid::Uuid, Path, description = "The channel")),
    request_body(content = notifications::ChannelRequest, example = json!({
        "kind": "webhook",
        "settings": { "url": "" },
        "customer_id": null,
        "site_id": "9b2e6a41-3c7d-4e8f-a1b2-c3d4e5f60718",
        "follow_up_only": true,
    })),
    responses(
        (status = 200, description = "Changed", body = notifications::ChannelView, example = json!(channel_webhook())),
        (status = 400, description = "`invalid`: as when adding one, or a new destination without its secret", body = ErrorBody),
        (status = 403, description = "`forbidden`: not an administrator of what it would notify about; `api_key_scope`", body = ErrorBody),
        (status = 404, description = "`not_found`: no such channel, or not one the key's maker administers", body = ErrorBody),
    ))]
fn update_notification_channel() {}

/// Remove a notification channel
///
/// Administrators of what it notifies about. Follow-up steps that name it
/// show it as removed. Audited as `notification.channel_delete`.
#[utoipa::path(delete, path = "/api/v1/notifications/channels/{id}", tag = "notifications",
    params(("id" = uuid::Uuid, Path, description = "The channel")),
    responses(
        (status = 204, description = "Removed"),
        (status = 403, description = "Not an administrator: `forbidden`; `api_key_scope`", body = ErrorBody),
        (status = 404, description = "`not_found`: no such channel, or not one the key's maker administers", body = ErrorBody),
    ))]
fn delete_notification_channel() {}

/// Send a test message
///
/// Sends a test through the channel now and answers whether it went
/// through, with the error in plain words. A failure is still a 200. The
/// outcome becomes the channel's `last_sent_at` or `last_error`. Not
/// audited. Administrators of what it notifies about.
#[utoipa::path(post, path = "/api/v1/notifications/channels/{id}/test", tag = "notifications",
    params(("id" = uuid::Uuid, Path, description = "The channel")),
    responses(
        (status = 200, description = "Tried", body = notifications::TestResult, example = json!({
            "ok": false,
            "error": "smtp.example.com refused the password for alerts@example.com.",
        })),
        (status = 403, description = "Not an administrator: `forbidden`; `api_key_scope`", body = ErrorBody),
        (status = 404, description = "`not_found`: no such channel, or not one the key's maker administers", body = ErrorBody),
    ))]
fn test_notification_channel() {}

fn channel_email() -> serde_json::Value {
    json!({
        "id": "0c8f3b6e-5d2a-4f1e-9b7c-2d4e6f8a0b1c",
        "kind": "email",
        "settings": {
            "host": "smtp.example.com", "port": 587, "username": "alerts@example.com",
            "from": "", "to": ["ops@example.com"],
        },
        "has_secret": true,
        "customer_id": "4f7d1c2e-0a5b-4c3d-9e8f-1a2b3c4d5e6f",
        "customer_name": "acme",
        "site_id": null,
        "site_name": null,
        "created_at": "2026-11-02T09:00:00Z",
        "last_sent_at": "2026-11-12T22:41:07Z",
        "last_error": null,
        "last_error_at": null,
        "follow_up_only": false,
    })
}

fn channel_webhook() -> serde_json::Value {
    json!({
        "id": "6a1d9e3f-7b4c-4a2e-8f5d-3c1b2a4e6d8f",
        "kind": "webhook",
        "settings": { "origin": "https://hooks.example.com" },
        "has_secret": true,
        "customer_id": "4f7d1c2e-0a5b-4c3d-9e8f-1a2b3c4d5e6f",
        "customer_name": "acme",
        "site_id": "9b2e6a41-3c7d-4e8f-a1b2-c3d4e5f60718",
        "site_name": "paris",
        "created_at": "2026-11-05T14:30:00Z",
        "last_sent_at": null,
        "last_error": "hooks.example.com answered 503 Service Unavailable.",
        "last_error_at": "2026-11-13T03:12:44Z",
        "follow_up_only": true,
    })
}
