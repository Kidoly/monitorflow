//! The OpenAPI description of update campaigns (ADR-0033, ADR-0038,
//! ADR-0053). Merged into the document by [`super::openapi::build`]; the
//! router in `mod.rs` serves the routes.

#![allow(dead_code)] // The functions exist for their `#[utoipa::path]`.

use utoipa::OpenApi;

use super::campaigns;
use super::openapi::{Common, ErrorBody};
use crate::campaign::{
    CampaignState, FailureThreshold, FailureUnit, PinnedMirror, SourcesMode, SourcesPin,
    UpdateSpec, WaveSpec, Window,
};

#[derive(OpenApi)]
#[openapi(
    paths(
        list_campaigns, create_campaign, campaign, update_campaign, delete_campaign,
        start_campaign, pause_campaign, resume_campaign, stop_campaign,
    ),
    components(schemas(
        CampaignState, FailureThreshold, FailureUnit, PinnedMirror, SourcesMode, SourcesPin,
        UpdateSpec, WaveSpec, Window, campaigns::Progress, campaigns::TargetView,
        campaigns::WaveView,
    )),
    modifiers(&Common),
    tags(
        (name = "campaigns", description = "Package or agent updates sent to many servers of one \
customer, in batches (`waves`), inside maintenance windows, stopping when a batch fails more than \
its failure threshold. Targets are frozen when the campaign is made; nothing is sent before it is \
started. A campaign is edited until its first batch is queued and deleted as a draft or once \
finished; to run one again, copy it: read it and create another from its fields. Every server gets a signed command of its own, `apply_updates` (or `self_update` for an \
agent update campaign), queued behind any other command of that server and checked by its agent, \
which runs package updates only when its administrator allowed them. Campaign commands are made by \
an administrator, not the AI: the risk rules and the approval queue do not apply to them. Reading \
needs a role on the customer as a whole (everything or the customer); creating and acting need an \
administrator role on it. Needs the key area \"updates\"."),
    )
)]
pub struct Doc;

/// A campaign of two batches of one server each in `paris`, in `state`:
/// `draft`, `scheduled`, `running`, `paused` or `stopped` (after the
/// first batch).
fn campaign_in(state: &str) -> serde_json::Value {
    let started = (state != "draft").then_some("2026-11-08T22:00:04Z");
    // The first batch was queued, and web-1's command was sent.
    let queued = matches!(state, "running" | "paused" | "stopped");
    let done = state == "stopped";
    let finished = done.then_some("2026-11-08T22:20:31Z");
    let first = match state {
        "running" | "paused" => "running",
        "stopped" => "succeeded",
        _ => "pending",
    };
    let count = |status: &str| -> u32 { u32::from(first == status) };
    let server = |id: &str, name: &str, status: &str, command: Option<&str>| {
        serde_json::json!({
            "server_id": id,
            "peer_id": format!("{name}.paris.acme"),
            "hostname": name,
            "site_id": "3c4d5e6f-7a8b-4c9d-8e0f-1a2b3c4d5e6f",
            "status": status,
            "command_id": command,
            "exit_code": if done { Some(0) } else { None },
            "error": null,
            "finished_at": finished,
            "sources_command_id": null,
            "restore_command_id": null,
            "snapshot_command_id": null
        })
    };
    let mut second = server(
        "2a3b4c5d-6e7f-4a8b-9c0d-1e2f3a4b5c6d",
        "web-2",
        "pending",
        None,
    );
    second["exit_code"] = serde_json::Value::Null;
    second["finished_at"] = serde_json::Value::Null;
    serde_json::json!({
        "id": "4e5f6a7b-8c9d-4e0f-9a1b-2c3d4e5f6a7b",
        "customer_id": "0b8e6f3a-1c2d-4e5f-9a8b-7c6d5e4f3a2b",
        "customer_name": "acme",
        "name": "November updates",
        "site_id": "3c4d5e6f-7a8b-4c9d-8e0f-1a2b3c4d5e6f",
        "state": state,
        "error": done.then_some("stopped by user"),
        "spec": {"packages": [], "security_only": true, "allow_reboot": false,
                 "include_os": true, "third_party": []},
        "command_ttl_s": 3600,
        "failure_threshold": {"value": 0, "unit": "count"},
        "windows": [{
            "kind": "weekly", "weekdays": [6], "start": "22:00", "end": "04:00",
            "timezone": "Europe/Paris"
        }],
        "sources": null,
        "agent_version": null,
        "server_ids": [],
        "wave_specs": [{"size": 1}],
        "wave_count": 2,
        "current_wave": if queued { Some(0) } else { None },
        "progress": {
            "total": 2, "pending": 1 + count("pending"), "in_progress": count("running"),
            "succeeded": count("succeeded"), "failed": 0, "cancelled": 0
        },
        "created_by": "9a1b2c3d-4e5f-4a6b-8c7d-6e5f4a3b2c1d",
        "created_at": "2026-11-06T10:30:00Z",
        "started_at": started,
        "finished_at": finished,
        "waves": [{
            "index": 0,
            "state": if done { "done" } else if queued { "running" } else { "pending" },
            "started_at": if queued { started } else { None },
            "finished_at": finished,
            "progress": {
                "total": 1, "pending": count("pending"), "in_progress": count("running"),
                "succeeded": count("succeeded"), "failed": 0, "cancelled": 0
            },
            "servers": [server(
                "1e2f3a4b-5c6d-4e7f-8a9b-0c1d2e3f4a5b",
                "web-1",
                first,
                queued.then_some("6a7b8c9d-0e1f-4a2b-8c3d-4e5f6a7b8c9d"),
            )]
        }, {
            "index": 1,
            "state": "pending",
            "started_at": null,
            "finished_at": null,
            "progress": {
                "total": 1, "pending": 1, "in_progress": 0, "succeeded": 0, "failed": 0,
                "cancelled": 0
            },
            "servers": [second]
        }]
    })
}

fn draft() -> serde_json::Value {
    campaign_in("draft")
}
fn scheduled() -> serde_json::Value {
    campaign_in("scheduled")
}
fn running() -> serde_json::Value {
    campaign_in("running")
}
fn paused() -> serde_json::Value {
    campaign_in("paused")
}
fn stopped() -> serde_json::Value {
    campaign_in("stopped")
}
fn list_example() -> serde_json::Value {
    let mut campaign = campaign_in("running");
    campaign.as_object_mut().unwrap().remove("waves");
    serde_json::json!({"campaigns": [campaign], "next_cursor": null})
}

/// List campaigns
///
/// The campaigns of the customers the caller reads as a whole, newest
/// first, without their batches and servers (read one for those).
///
/// Needs any role on the customer as a whole (everything or the customer;
/// a role on one site, or a key that reaches one site, sees none) and the
/// key area "updates". Paginated with `limit` and `cursor`; unknown
/// parameters and bad values are 400.
#[utoipa::path(get, path = "/api/v1/campaigns", tag = "campaigns",
    params(campaigns::ListParams),
    responses((status = 200, description = "The campaigns", body = campaigns::CampaignList, example = list_example)))]
fn list_campaigns() {}

/// Create a campaign
///
/// Makes a `draft` campaign: nothing is sent to any server until it is
/// started. Its servers are frozen now: those of `server_ids` and the
/// unrevoked servers of `site_id`, all of the customer, sorted by host
/// name. A server added to the site later is not added to the campaign.
/// They are split into batches by `waves`, in order: explicit server
/// lists first, then `{"size": n}` and `{"percent": p}` (of all servers,
/// rounded up) take the next servers by host name; the servers left form
/// one last batch. Without `waves`, one batch.
///
/// Once started, each server gets one signed command, sent when its agent
/// is connected and has no other command in flight:
///
/// - a package update (`apply_updates` with `packages`, `security_only`
///   and `allow_reboot`; empty `packages` upgrades everything). A server
///   whose agent does not accept package updates is left out of its
///   batch. Where it can, MonitorFlow takes a local snapshot of the server
///   first (`snapshot_command_id`). With `sources`, the server's package
///   sources are first pointed at the pinned mirror snapshots and put back
///   after the update (`sources_command_id`, `restore_command_id`); the
///   central server needs mirrors and `mirror_public_url` for that;
/// - or, with `agent_version`, an agent update (`self_update`) to that
///   version, which takes no package options.
///
/// These commands come from an administrator, so they are not classified
/// by the risk rules and need no approval.
///
/// To copy a campaign, read it and send its `name`, `customer_id`,
/// `site_id`, `server_ids`, `wave_specs` (as `waves`), the fields of
/// `spec`, `command_ttl_s`, `failure_threshold`, `windows`, `sources` and
/// `agent_version` here: the copy is a new draft, with the servers of the
/// site as they are now.
///
/// Needs an administrator role on the customer as a whole and the key
/// area "updates". Audited as `campaign.create`. Not safe to repeat: send
/// `Idempotency-Key` to retry.
#[utoipa::path(post, path = "/api/v1/campaigns", tag = "campaigns",
    request_body(content = campaigns::CreateRequest, example = json!({
        "customer_id": "0b8e6f3a-1c2d-4e5f-9a8b-7c6d5e4f3a2b",
        "name": "November updates",
        "site_id": "3c4d5e6f-7a8b-4c9d-8e0f-1a2b3c4d5e6f",
        "waves": [{"size": 1}],
        "security_only": true,
        "windows": [{
            "kind": "weekly", "weekdays": [6], "start": "22:00", "end": "04:00",
            "timezone": "Europe/Paris"
        }]
    })),
    responses(
        (status = 201, description = "The draft, with its batches and servers",
            body = campaigns::CampaignDetail, example = draft),
        (status = 400, body = ErrorBody, description = "`invalid`, with `error` saying why: not the \
expected JSON, no `site_id` nor `server_ids`, `unknown site <id>` or \
`unknown server <id>` (not of the customer), `server <id> is revoked`, a bad name, `command_ttl_s` \
outside 60 to 604800, a bad package name, batch, window, time zone, failure threshold or pinned \
snapshot, `agent_version` not like `0.2.0`, or package options with `agent_version`"),
        (status = 403, body = ErrorBody, description = "`forbidden`: the caller reads the customer \
without administering it as a whole. `api_key_scope`: the key lacks the area"),
        (status = 404, body = ErrorBody, description = "`not_found`: a customer the caller does not read"),
        (status = 409, body = ErrorBody, description = "`conflict`, with `sources` only: \
`mirror_public_url is not configured`, `mirrors are not enabled on this server`, or the mirrors' \
signing key cannot be used"),
    ))]
fn create_campaign() {}

/// Read a campaign
///
/// The campaign with its batches (`waves`) in order and, in each, its
/// servers with the status of their command: `pending` (no command yet),
/// `queued`, `sent`, `running`, `succeeded`, `failed`, `expired`,
/// `cancelled`, or `skipped` (no command, with `error` saying why, such as
/// `server revoked`). Poll it to follow a running campaign; the command of
/// each server is under `/commands/{command_id}`.
///
/// Needs any role on the campaign's customer as a whole and the key area
/// "updates".
#[utoipa::path(get, path = "/api/v1/campaigns/{id}", tag = "campaigns",
    params(("id" = uuid::Uuid, Path, description = "The campaign")),
    responses(
        (status = 200, description = "The campaign and its servers", body = campaigns::CampaignDetail, example = running),
        (status = 404, body = ErrorBody, description = "`not_found`: no such campaign, or one of a \
customer the caller does not read as a whole"),
    ))]
fn campaign() {}

/// Edit a campaign
///
/// Replaces the whole campaign with the body, which is the body of a
/// creation: fields left out take their defaults, not the campaign's
/// values. Its servers are frozen again into batches, as at creation. The
/// state, who made it and when stay.
///
/// Only a campaign that queued nothing yet changes: a `draft`, or a
/// `scheduled` or `paused` one whose first batch is still waiting for a
/// maintenance window. Once a batch was queued, or once finished, it is
/// 409: copy it into a new campaign instead (see "Create a campaign").
///
/// Needs an administrator role on the campaign's customer, and on the
/// customer of the body when it moves it, as a whole, and the key area
/// "updates". Audited as `campaign.update`.
#[utoipa::path(put, path = "/api/v1/campaigns/{id}", tag = "campaigns",
    params(("id" = uuid::Uuid, Path, description = "The campaign")),
    request_body(content = campaigns::CreateRequest, example = json!({
        "customer_id": "0b8e6f3a-1c2d-4e5f-9a8b-7c6d5e4f3a2b",
        "name": "November updates",
        "site_id": "3c4d5e6f-7a8b-4c9d-8e0f-1a2b3c4d5e6f",
        "waves": [{"size": 1}],
        "security_only": true,
        "windows": [{
            "kind": "weekly", "weekdays": [6], "start": "22:00", "end": "04:00",
            "timezone": "Europe/Paris"
        }]
    })),
    responses(
        (status = 200, description = "The campaign as edited, with its batches and servers",
            body = campaigns::CampaignDetail, example = draft),
        (status = 400, body = ErrorBody, description = "`invalid`, as for a creation"),
        (status = 403, body = ErrorBody, description = "`forbidden`: the caller reads the customer \
without administering it as a whole. `api_key_scope`: the key lacks the area"),
        (status = 404, body = ErrorBody, description = "`not_found`: no such campaign, or one of a \
customer the caller does not read as a whole"),
        (status = 409, body = ErrorBody, description = "`conflict`: `cannot edit a campaign that is \
<state>` once it is finished, `cannot edit a campaign that queued commands already: duplicate it \
instead`, or a `sources` conflict as for a creation"),
    ))]
fn update_campaign() {}

/// Delete a campaign
///
/// Deletes a `draft`, or a `stopped`, `completed` or `failed` campaign,
/// with its batches, servers and windows. Its commands stay in the
/// history under `/commands/{id}`, without their `campaign_id`. A
/// campaign in progress is stopped first.
///
/// Refused while some of its commands are still `queued`, `sent` or
/// `running` (stopping lets the commands already sent finish), or while
/// the package sources of some of its servers are still to be put back
/// after pinned snapshots.
///
/// Needs an administrator role on the customer as a whole and the key
/// area "updates". Audited as `campaign.delete`, with its name, state and
/// number of servers.
#[utoipa::path(delete, path = "/api/v1/campaigns/{id}", tag = "campaigns",
    params(("id" = uuid::Uuid, Path, description = "The campaign")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 403, body = ErrorBody, description = "`forbidden`: the caller reads the customer \
without administering it as a whole. `api_key_scope`: the key lacks the area"),
        (status = 404, body = ErrorBody, description = "`not_found`: no such campaign, or one of a \
customer the caller does not read as a whole"),
        (status = 409, body = ErrorBody, description = "`conflict`: `cannot delete a campaign that is \
<state>` for one `scheduled`, `running` or `paused` (stop it first), `the campaign still has \
commands in flight: delete it once they end`, or `the package sources of some servers are not \
restored yet`"),
    ))]
fn delete_campaign() {}

/// Start a campaign
///
/// `draft` → `scheduled`. The campaign runner then queues the first
/// batch's commands as soon as a maintenance window is open for at least
/// 60 more seconds (at once without windows), with an expiry no later
/// than the window's end, and the campaign becomes `running`. Each next
/// batch starts when every server of the one before has a result, unless
/// more of them failed than `failure_threshold`: the campaign is then
/// `failed` and its queued commands are cancelled. Past the last batch it
/// is `completed`.
///
/// Needs an administrator role on the customer as a whole and the key
/// area "updates". Audited as `campaign.start`. Starting again is 409.
#[utoipa::path(post, path = "/api/v1/campaigns/{id}/start", tag = "campaigns",
    params(("id" = uuid::Uuid, Path, description = "The campaign")),
    responses(
        (status = 200, description = "Started, or scheduled for its start time", body = campaigns::CampaignDetail, example = scheduled),
        (status = 403, body = ErrorBody, description = "`forbidden`: the caller reads the customer \
without administering it as a whole. `api_key_scope`: the key lacks the area"),
        (status = 404, body = ErrorBody, description = "`not_found`: no such campaign, or one of a \
customer the caller does not read as a whole"),
        (status = 409, body = ErrorBody, description = "`conflict`: `cannot start a campaign that is \
<state>`; only a `draft` starts"),
    ))]
fn start_campaign() {}

/// Pause a campaign
///
/// `scheduled` or `running` → `paused`: nothing more is queued, and the
/// campaign's commands still `queued` (not yet sent to the agent) are
/// cancelled; those already sent or running finish and count. Their
/// servers get a new command when the campaign is resumed.
///
/// Needs an administrator role on the customer as a whole and the key
/// area "updates". Audited as `campaign.pause`. Pausing a paused campaign
/// is 409.
#[utoipa::path(post, path = "/api/v1/campaigns/{id}/pause", tag = "campaigns",
    params(("id" = uuid::Uuid, Path, description = "The campaign")),
    responses(
        (status = 200, description = "Paused", body = campaigns::CampaignDetail, example = paused),
        (status = 403, body = ErrorBody, description = "`forbidden`: the caller reads the customer \
without administering it as a whole. `api_key_scope`: the key lacks the area"),
        (status = 404, body = ErrorBody, description = "`not_found`: no such campaign, or one of a \
customer the caller does not read as a whole"),
        (status = 409, body = ErrorBody, description = "`conflict`: `cannot pause a campaign that is \
<state>`; only `scheduled` and `running` pause"),
    ))]
fn pause_campaign() {}

/// Resume a campaign
///
/// `paused` → `running`, or `scheduled` if no batch was queued yet. The
/// runner queues commands again, within the maintenance windows.
///
/// Needs an administrator role on the customer as a whole and the key
/// area "updates". Audited as `campaign.resume`. Resuming a campaign that
/// is not paused is 409.
#[utoipa::path(post, path = "/api/v1/campaigns/{id}/resume", tag = "campaigns",
    params(("id" = uuid::Uuid, Path, description = "The campaign")),
    responses(
        (status = 200, description = "Resumed", body = campaigns::CampaignDetail, example = running),
        (status = 403, body = ErrorBody, description = "`forbidden`: the caller reads the customer \
without administering it as a whole. `api_key_scope`: the key lacks the area"),
        (status = 404, body = ErrorBody, description = "`not_found`: no such campaign, or one of a \
customer the caller does not read as a whole"),
        (status = 409, body = ErrorBody, description = "`conflict`: `cannot resume a campaign that is \
<state>`; only `paused` resumes"),
    ))]
fn resume_campaign() {}

/// Stop a campaign
///
/// Any state but a final one → `stopped`, for good: this is how a
/// campaign is cancelled. Commands still `queued` are cancelled; those
/// already sent or running finish. A stopped campaign cannot be started
/// again: copy it into another, or delete it.
///
/// Needs an administrator role on the customer as a whole and the key
/// area "updates". Audited as `campaign.stop`. Stopping a `stopped`,
/// `completed` or `failed` campaign is 409.
#[utoipa::path(post, path = "/api/v1/campaigns/{id}/stop", tag = "campaigns",
    params(("id" = uuid::Uuid, Path, description = "The campaign")),
    responses(
        (status = 200, description = "Stopped", body = campaigns::CampaignDetail, example = stopped),
        (status = 403, body = ErrorBody, description = "`forbidden`: the caller reads the customer \
without administering it as a whole. `api_key_scope`: the key lacks the area"),
        (status = 404, body = ErrorBody, description = "`not_found`: no such campaign, or one of a \
customer the caller does not read as a whole"),
        (status = 409, body = ErrorBody, description = "`conflict`: `cannot stop a campaign that is \
<state>`"),
    ))]
fn stop_campaign() {}
