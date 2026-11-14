//! Integrations in the OpenAPI document (ADR-0102): merged into it by
//! [`super::build`].

use serde_json::json;
use utoipa::OpenApi;

use super::{Common, ErrorBody};
use crate::api::integrations;

#[derive(OpenApi)]
#[openapi(
    paths(
        list_integrations,
        create_integration,
        update_integration,
        delete_integration,
        set_integration_state,
        renew_integration_secret,
        test_integration,
        list_integration_deliveries,
        replay_integration_events,
    ),
    components(schemas(ErrorBody)),
    modifiers(&Common),
    tags((name = "integrations", description = "Integrations: another tool, like a PSA or an automation, \
that gets MonitorFlow's events as one signed JSON `POST` per event: incidents, servers, updates, campaigns \
and approvals, each integration choosing which. The body follows \
[`docs/events/v1.schema.json`](https://github.com/Kidoly/monitorflow/blob/main/docs/events/v1.schema.json); \
the receiver's rules and code to check signatures are in \
[`docs/integrations.md`](https://github.com/Kidoly/monitorflow/blob/main/docs/integrations.md). In short: \
each request carries `X-MonitorFlow-Event`, `X-MonitorFlow-Delivery` (the event's `id`), \
`X-MonitorFlow-Timestamp` and `X-MonitorFlow-Signature`, `sha256=` and the HMAC-SHA256 of \
`{timestamp}.{raw body}` under the integration's signing secret, plus `X-MonitorFlow-Replay: 1` when sent \
again. Delivery is at least once: de-duplicate on the `id`. Anything but a 2xx within 15 seconds is \
retried after 30 seconds, then twice as long each time up to an hour, for 24 hours; an integration that \
accepts nothing for 24 hours is turned off and administrators are told. Events are kept 30 days and can \
be sent again with the same IDs. An integration notifies about everything, one customer or one site, and \
is managed by the administrators of that place, like a notification channel; others get 404. The URL and \
the signing secret are write-only: answers show the URL's origin, as its path and query often hold a \
token, and the secret only once, when it is made. Private and local addresses need a global administrator \
(a key with every reach). Every change is in the audit log, with the key's ID. API keys need the \
`administration` area."))
)]
pub(super) struct Doc;

/// List integrations
///
/// The integrations of what the key's maker administers, oldest first, in
/// full, with every event an integration can ask for. Administrators only.
#[utoipa::path(get, path = "/api/v1/integrations", tag = "integrations",
    responses(
        (status = 200, description = "The integrations", body = integrations::EndpointList, example = json!({
            "endpoints": [integration()],
            "events": [
                "incident.opened", "incident.escalated", "incident.acknowledged", "incident.resolved",
                "server.added", "server.removed", "server.offline", "server.online",
                "update.started", "update.succeeded", "update.failed",
                "campaign.started", "campaign.succeeded", "campaign.failed",
                "approval.waiting", "approval.decided",
            ],
        })),
        (status = 403, description = "Not an administrator: `forbidden`; outside the key's areas: `api_key_scope`", body = ErrorBody),
    ))]
fn list_integrations() {}

/// Add an integration
///
/// Administrators of what it notifies about. Makes its signing secret
/// and answers it this once; events start at once. `allow_private` takes
/// a global administrator. Audited as `integration.create`, with the
/// URL's host only. Not stored for `Idempotency-Key`, as the answer holds
/// the secret: a retry with the same key answers 409 `conflict`, saying
/// it was done; list the integrations to find it, and renew its secret if
/// it was lost.
#[utoipa::path(post, path = "/api/v1/integrations", tag = "integrations",
    request_body(content = integrations::EndpointRequest, example = json!({
        "name": "Ticketing",
        "url": "https://tickets.example.com/hooks/monitorflow?token=abc123",
        "events": ["incident.opened", "incident.resolved"],
        "customer_id": "4f7d1c2e-0a5b-4c3d-9e8f-1a2b3c4d5e6f",
        "site_id": null,
        "allow_private": false,
    })),
    responses(
        (status = 201, description = "Added, with its signing secret", body = integrations::WithSecret, example = json!({
            "endpoint": integration(),
            "secret": SECRET,
        })),
        (status = 400, description = "`invalid`: an empty or long name, a URL that is not http or https or holds a password, an unknown event, a customer and a site together, an unknown customer or site", body = ErrorBody),
        (status = 403, description = "`forbidden`: not an administrator of what it would notify about, or `allow_private` without being a global administrator; `api_key_scope`", body = ErrorBody),
        (status = 409, description = "`conflict`: the same `Idempotency-Key` again, whose answer held the secret; `idempotency_in_progress`", body = ErrorBody),
    ))]
fn create_integration() {}

/// Change an integration
///
/// Replaces its name, URL, events, what it notifies about and
/// `allow_private`. Administrators of what it notifies about now and of
/// what it will. A `url` left empty keeps the stored one. Another URL
/// gets a new signing secret, answered this once, so the old secret never
/// signs for another receiver; the old one stops at once. Anyone but a
/// global administrator keeps `allow_private` only with the same URL.
/// Audited as `integration.update`. Safe to repeat; with
/// `Idempotency-Key`, the answer is kept 24 hours, a new secret included.
#[utoipa::path(put, path = "/api/v1/integrations/{id}", tag = "integrations",
    params(("id" = uuid::Uuid, Path, description = "The integration")),
    request_body(content = integrations::EndpointRequest, example = json!({
        "name": "Ticketing",
        "url": "",
        "events": null,
        "customer_id": "4f7d1c2e-0a5b-4c3d-9e8f-1a2b3c4d5e6f",
        "site_id": null,
        "allow_private": false,
    })),
    responses(
        (status = 200, description = "Changed; `secret` is set when the URL changed", body = integrations::Updated, example = json!({
            "endpoint": integration(),
            "secret": null,
        })),
        (status = 400, description = "`invalid`: as when adding one", body = ErrorBody),
        (status = 403, description = "`forbidden`: not an administrator of what it would notify about, or `allow_private` for a new URL without being a global administrator; `api_key_scope`", body = ErrorBody),
        (status = 404, description = "`not_found`: no such integration, or not one the key's maker administers", body = ErrorBody),
    ))]
fn update_integration() {}

/// Remove an integration
///
/// Administrators of what it notifies about. Its pending deliveries and
/// its delivery log go with it. Audited as `integration.delete`.
#[utoipa::path(delete, path = "/api/v1/integrations/{id}", tag = "integrations",
    params(("id" = uuid::Uuid, Path, description = "The integration")),
    responses(
        (status = 204, description = "Removed"),
        (status = 403, description = "Not an administrator: `forbidden`; `api_key_scope`", body = ErrorBody),
        (status = 404, description = "`not_found`: no such integration, or not one the key's maker administers", body = ErrorBody),
    ))]
fn delete_integration() {}

/// Pause or turn on an integration
///
/// `paused` stops new events for it; deliveries already queued wait. `on`
/// resumes a paused or turned off one, and its day of failures starts
/// again: what it missed while not on is not queued, so send it again
/// with the replay. Administrators of what it notifies about. Audited as
/// `integration.state`. Safe to repeat.
#[utoipa::path(post, path = "/api/v1/integrations/{id}/state", tag = "integrations",
    params(("id" = uuid::Uuid, Path, description = "The integration")),
    request_body(content = integrations::StateRequest, example = json!({ "state": "on" })),
    responses(
        (status = 200, description = "Its new state", body = integrations::EndpointView, example = json!(integration())),
        (status = 400, description = "`invalid`: a state other than `on` or `paused`", body = ErrorBody),
        (status = 403, description = "Not an administrator: `forbidden`; `api_key_scope`", body = ErrorBody),
        (status = 404, description = "`not_found`: no such integration, or not one the key's maker administers", body = ErrorBody),
    ))]
fn set_integration_state() {}

/// Make a new signing secret
///
/// Replaces the signing secret and answers the new one this once; the old
/// one stops working at once, retries included. Administrators of what it
/// notifies about. Audited as `integration.secret_renew`. Not stored for
/// `Idempotency-Key`: a retry with the same key answers 409 `conflict`;
/// call it again without the header for another secret.
#[utoipa::path(post, path = "/api/v1/integrations/{id}/secret", tag = "integrations",
    params(("id" = uuid::Uuid, Path, description = "The integration")),
    responses(
        (status = 200, description = "The new signing secret", body = integrations::WithSecret, example = json!({
            "endpoint": integration(),
            "secret": SECRET,
        })),
        (status = 403, description = "Not an administrator: `forbidden`; `api_key_scope`", body = ErrorBody),
        (status = 404, description = "`not_found`: no such integration, or not one the key's maker administers", body = ErrorBody),
        (status = 409, description = "`conflict`: the same `Idempotency-Key` again, whose answer held the secret; `idempotency_in_progress`", body = ErrorBody),
    ))]
fn renew_integration_secret() {}

/// Send a test event
///
/// Posts a signed `test` event (`X-MonitorFlow-Event: test`, an empty
/// object as details) to the URL now, whatever the state, and answers how
/// it went. A failure is still a 200. It is not stored: it is not in the
/// delivery log and does not count as a failure. Not audited.
/// Administrators of what it notifies about.
#[utoipa::path(post, path = "/api/v1/integrations/{id}/test", tag = "integrations",
    params(("id" = uuid::Uuid, Path, description = "The integration")),
    responses(
        (status = 200, description = "Tried", body = integrations::TestResult, example = json!({
            "ok": false,
            "status": 401,
            "error": "tickets.example.com answered 401 Unauthorized: check the signing secret there.",
        })),
        (status = 403, description = "Not an administrator: `forbidden`; `api_key_scope`", body = ErrorBody),
        (status = 404, description = "`not_found`: no such integration, or not one the key's maker administers", body = ErrorBody),
    ))]
fn test_integration() {}

/// List deliveries
///
/// The integration's deliveries, newest first, 50 at a time: pass
/// `next_before` as `before` for older ones. Deliveries go with their
/// events after 30 days. Administrators of what it notifies about.
#[utoipa::path(get, path = "/api/v1/integrations/{id}/deliveries", tag = "integrations",
    params(("id" = uuid::Uuid, Path, description = "The integration"), integrations::DeliveryParams),
    responses(
        (status = 200, description = "A page of deliveries", body = integrations::DeliveryList, example = json!({
            "deliveries": [{
                "id": 48213,
                "event_id": "0b5f7c4e-2b8a-4a35-9d38-6a1f2f0c8e11",
                "event": "incident.opened",
                "occurred_at": "2026-11-13T03:10:00Z",
                "state": "pending",
                "replay": false,
                "attempts": 3,
                "last_status": 503,
                "last_error": "tickets.example.com answered 503 Service Unavailable.",
                "last_attempt_at": "2026-11-13T03:13:31Z",
                "next_at": "2026-11-13T03:15:31Z",
                "finished_at": null,
            }],
            "next_before": null,
        })),
        (status = 400, description = "`invalid`: `before` is not a number", body = ErrorBody),
        (status = 403, description = "Not an administrator: `forbidden`; `api_key_scope`", body = ErrorBody),
        (status = 404, description = "`not_found`: no such integration, or not one the key's maker administers", body = ErrorBody),
    ))]
fn list_integration_deliveries() {}

/// Send events again
///
/// Queues one event, by `event_id`, or every event from `from` to `to`
/// that the integration asks for and notifies about, again, oldest
/// first, with the same IDs and `X-MonitorFlow-Replay: 1`. Events are
/// kept 30 days. They go out while the integration is on. Administrators
/// of what it notifies about. Audited as `integration.replay`. Not safe
/// to repeat: each call queues them again; send `Idempotency-Key` to
/// retry.
#[utoipa::path(post, path = "/api/v1/integrations/{id}/replay", tag = "integrations",
    params(("id" = uuid::Uuid, Path, description = "The integration")),
    request_body(content = integrations::ReplayRequest, example = json!({
        "from": "2026-11-12T00:00:00Z",
        "to": "2026-11-13T08:00:00Z",
    })),
    responses(
        (status = 200, description = "Queued", body = integrations::ReplayResult, example = json!({ "queued": 17 })),
        (status = 400, description = "`invalid`: both or neither of `event_id` and `from`, `to` without `from`, `from` not before `to`, a range over 30 days", body = ErrorBody),
        (status = 403, description = "Not an administrator: `forbidden`; `api_key_scope`", body = ErrorBody),
        (status = 404, description = "`not_found`: no such integration, not one the key's maker administers, or an `event_id` that is unknown, older than 30 days, or not one it asks for and notifies about", body = ErrorBody),
    ))]
fn replay_integration_events() {}

/// A made-up signing secret, in its real shape.
const SECRET: &str = "mfwh_3f9a1c7e5b2d4f6a8c0e1b3d5f7a9c2e4b6d8f0a1c3e5b7d9f2a4c6e8b0d1f3a";

fn integration() -> serde_json::Value {
    json!({
        "id": "2d7c9a4e-1f3b-4e5d-8a6c-0b2e4d6f8a1c",
        "name": "Ticketing",
        "origin": "https://tickets.example.com",
        "events": ["incident.opened", "incident.resolved"],
        "customer_id": "4f7d1c2e-0a5b-4c3d-9e8f-1a2b3c4d5e6f",
        "customer_name": "acme",
        "site_id": null,
        "site_name": null,
        "allow_private": false,
        "state": "on",
        "created_at": "2026-11-03T10:00:00Z",
        "failing_since": "2026-11-13T03:10:02Z",
        "last_success_at": "2026-11-13T02:58:40Z",
        "pending": 1,
        "last_error": "tickets.example.com answered 503 Service Unavailable.",
    })
}
