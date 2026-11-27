//! The OpenAPI description of update campaigns (ADR-0033, ADR-0038,
//! ADR-0053). Merged into the document by [`super::openapi::build`]; the
//! router in `mod.rs` serves the routes.

#![allow(dead_code)] // The functions exist for their `#[utoipa::path]`.

use utoipa::OpenApi;

use super::openapi::{Common, ErrorBody};
use super::{campaigns, commands, major_upgrades};
use crate::campaign::{
    CampaignState, FailureThreshold, FailureUnit, MajorUpgradeSpec, PinnedMirror, RunSettings,
    SoftwareItem, SoftwareSpec, SourcesMode, SourcesPin, UpdateSpec, WaveSpec, Window,
};
use crate::commands::major_upgrade::{Check, Progress};

#[derive(OpenApi)]
#[openapi(
    paths(
        list_campaigns, create_campaign, campaign, update_campaign, delete_campaign,
        start_campaign, pause_campaign, resume_campaign, stop_campaign, relaunch_campaign,
        campaign_runs, list_major_upgrades, leapp_preupgrade,
    ),
    components(schemas(
        CampaignState, FailureThreshold, FailureUnit, PinnedMirror, SourcesMode, SourcesPin,
        UpdateSpec, WaveSpec, Window, campaigns::Progress, campaigns::TargetView,
        campaigns::WaveView, campaigns::RelaunchServers, RunSettings, MajorUpgradeSpec, Check,
        Progress, major_upgrades::ServerUpgrade, major_upgrades::MajorUpgradeList, SoftwareSpec,
        SoftwareItem,
    )),
    modifiers(&Common),
    tags(
        (name = "campaigns", description = "Package or agent updates sent to many servers of one \
customer, in batches (`waves`), inside maintenance windows, stopping when a batch fails more than \
its failure threshold. Targets are frozen when the campaign is made; nothing is sent before it is \
started. A campaign is edited at any time: fully before its first batch, only its name, windows \
and failure threshold while a run is under way, and fully once finished, for its next run. A \
finished campaign runs again, all its servers or only those that failed, and keeps each past run \
under `/campaigns/{id}/runs`. It is deleted as a draft or once finished. Every server gets a signed command of its own, `apply_updates` (or `self_update` for an \
agent update campaign, or `software` for a software campaign), queued behind any other command of \
that server and checked by its agent, which runs package updates only when its administrator \
allowed them. On Windows servers an update approves Windows Update classifications \
(`classifications`) and holds updates (`hold`) and apps (`third_party[].hold`). A software campaign \
(`software`) installs or removes software of the catalogue (`/software`): each server gets the \
packages of its own manager (winget on Windows, apt on Debian and Ubuntu, dnf on RHEL and its \
derivatives), where its administrator allowed it (`monitorflow-agent software on`); the others are \
skipped with why. Packages that install from a mirror of the central server get it added to the \
servers' sources first and removed after (`sources` in `add` mode, pinned to the mirror's newest \
published snapshot when the campaign is saved). A `recurring` campaign runs again by itself at the \
first window that opens after each completed run, as \"run again\" does; a failed or stopped run \
ends the recurrence. Campaign commands are made by \
an administrator, not the AI: the risk rules and the approval queue do not apply to them. Reading \
needs a role on the customer as a whole (everything or the customer); creating and acting need an \
administrator role on it. Needs the key area \"updates\".\n\n\
A major upgrade campaign (`major_upgrade`, ADR-0182) moves servers to the next release of their \
distribution, Debian 12 to 13 for now: each server gets a Proxmox or local snapshot first (or none, \
only with `accept_limited_rollback`), then a signed `major_upgrade` command, which its agent runs \
only where the server allowed major upgrades from the web interface. A server is `upgrading` until \
it rebooted, its agent reports the new release and its checks are back to what they were. \
`/major-upgrades` shows the checks of each server beforehand."),
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
                 "include_os": true, "third_party": [], "classifications": [], "hold": []},
        "command_ttl_s": 3600,
        "failure_threshold": {"value": 0, "unit": "count"},
        "windows": [{
            "kind": "weekly", "weekdays": [6], "start": "22:00", "end": "04:00",
            "timezone": "Europe/Paris"
        }],
        "sources": null,
        "agent_version": null,
        "software": null,
        "recurring": false,
        "server_ids": [],
        "wave_specs": [{"size": 1}],
        "wave_count": 2,
        "current_wave": if queued { Some(0) } else { None },
        "previous_runs": 0,
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
/// A stopped campaign scheduled again: its first run is kept.
fn relaunched() -> serde_json::Value {
    let mut campaign = campaign_in("scheduled");
    campaign["previous_runs"] = serde_json::json!(1);
    campaign
}
/// The run of [`stopped`], once it ran again.
fn runs() -> serde_json::Value {
    let run = stopped();
    serde_json::json!({"runs": [{
        "number": 1,
        "state": "stopped",
        "error": run["error"],
        "started_at": run["started_at"],
        "finished_at": run["finished_at"],
        "settings": {
            "name": run["name"],
            "site_id": run["site_id"],
            "server_ids": [],
            "wave_specs": run["wave_specs"],
            "spec": run["spec"],
            "agent_version": null,
            "command_ttl_s": 3600,
            "failure_threshold": run["failure_threshold"],
            "windows": run["windows"],
            "sources": null
        },
        "progress": run["progress"],
        "waves": run["waves"]
    }]})
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
/// Commands are queued only while a maintenance window of `windows` is
/// open, or at any time without windows: `weekly` on chosen weekdays,
/// `monthly` on a day of the month (`{"kind": "monthly", "month_day": 15,
/// ...}`) or a weekday of a week of it (the second Tuesday is
/// `"month_week": 2, "weekday": 2`, the last Friday `"month_week": 5,
/// "weekday": 5`) in chosen `months` (one month for a yearly window), or
/// `once` between two times.
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
snapshot, `agent_version` not like `0.2.0`, package options with `agent_version` or `software`, an \
unknown classification, a bad held update, `unknown catalogue entry <id>`, a software campaign whose \
`sources` replace the servers' own, a mirror of its software without a published snapshot, or \
`recurring` without a weekly, monthly or yearly window or for a major upgrade"),
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
/// What may change depends on where the campaign is (ADR-0173):
///
/// - **Nothing queued yet** (a `draft`, or a `scheduled` or `paused`
///   campaign whose first batch waits for a window): everything.
/// - **A run under way** (a batch was queued, not finished): only `name`,
///   `windows` and `failure_threshold`; they apply to what is still to be
///   queued. Every other field must be as the campaign has it, read with
///   `GET`, or it is 409 naming what differs. Stop it, or wait for it to
///   end, to change the rest.
/// - **Finished** (`stopped`, `completed` or `failed`): everything, for its
///   next run. The run that ended moves to `/campaigns/{id}/runs`, its
///   servers are frozen again into new batches and the campaign is a
///   `draft` again, to start when ready. A change of `name` alone renames
///   it and leaves it as it is. Refused while some of its commands are in
///   flight or package sources are still to be put back, as a deletion.
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
        (status = 409, body = ErrorBody, description = "`conflict`: `cannot change <fields> while \
the campaign runs: ...` for a run under way, `the campaign still has commands in flight: edit it \
once they end` or `the package sources of some servers are not restored yet` for a finished one, \
or a `sources` conflict as for a creation"),
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
/// Any state but a final one → `stopped`: this is how a campaign is
/// cancelled. Commands still `queued` are cancelled; those already sent
/// or running finish. A stopped campaign is not resumed: run it again
/// (see "Run a campaign again"), edit it for its next run, or delete it.
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

/// Run a campaign again
///
/// A `stopped`, `completed` or `failed` campaign runs again with its
/// current settings: its last run moves to `/campaigns/{id}/runs` with its
/// batches and results, its servers are frozen again into batches, and it
/// is `scheduled`, queued at the next maintenance window. With
/// `{"servers": "failed"}`, only the servers whose update failed, expired
/// or was cancelled in the last run, still there, run again, in one batch;
/// the campaign's own targets stay as they are for later runs.
///
/// Refused while some of its commands are in flight or package sources
/// are still to be put back, as a deletion. To change the settings first,
/// edit it: that prepares the next run as a `draft` to start.
///
/// Needs an administrator role on the customer as a whole and the key
/// area "updates". Audited as `campaign.relaunch`, with the number of the
/// run kept.
#[utoipa::path(post, path = "/api/v1/campaigns/{id}/relaunch", tag = "campaigns",
    params(("id" = uuid::Uuid, Path, description = "The campaign")),
    request_body(content = campaigns::RelaunchRequest, example = json!({"servers": "failed"})),
    responses(
        (status = 200, description = "Scheduled again, with its new batches",
            body = campaigns::CampaignDetail, example = relaunched),
        (status = 400, body = ErrorBody, description = "`invalid`: the body, or the settings no \
longer fit, as for a creation (a site or snapshot gone)"),
        (status = 403, body = ErrorBody, description = "`forbidden`: the caller reads the customer \
without administering it as a whole. `api_key_scope`: the key lacks the area"),
        (status = 404, body = ErrorBody, description = "`not_found`: no such campaign, or one of a \
customer the caller does not read as a whole"),
        (status = 409, body = ErrorBody, description = "`conflict`: `cannot relaunch a campaign that \
is <state>` before it is finished, `no server of the campaign failed in its last run`, `the \
campaign still has commands in flight: relaunch it once they end`, or `the package sources of some \
servers are not restored yet`"),
    ))]
fn relaunch_campaign() {}

/// List a campaign's past runs
///
/// Every run that ended before the campaign was edited or run again,
/// newest first, each with the settings it ran with and its batches and
/// servers as they ended. The current or last run is the campaign itself
/// (`GET /campaigns/{id}`).
///
/// Needs any role on the campaign's customer as a whole and the key area
/// "updates".
#[utoipa::path(get, path = "/api/v1/campaigns/{id}/runs", tag = "campaigns",
    params(("id" = uuid::Uuid, Path, description = "The campaign")),
    responses(
        (status = 200, description = "Its past runs", body = campaigns::RunList, example = runs),
        (status = 404, body = ErrorBody, description = "`not_found`: no such campaign, or one of a \
customer the caller does not read as a whole"),
    ))]
fn campaign_runs() {}

/// List the servers that can move to the next release
///
/// For each server the caller sees on a release MonitorFlow can upgrade
/// from (Debian 12, Ubuntu LTS, RHEL 8 and 9, Rocky Linux 8, AlmaLinux 8
/// and 9), or with a major upgrade recorded: the next
/// release, every check before upgrading to it, the rollback it would
/// get, and the progress of its last upgrade (ADR-0182). The central
/// server's checks come first (`allowed`: the server allows major upgrades
/// from the web interface; `agent_version`: its agent knows the target;
/// `mirror`: the mirrors carry the target when the server uses them; on
/// Ubuntu and the RHEL family, a `warning` when they do not, as the
/// campaign puts the server's own sources back for the upgrade and keeps
/// them until the mirrors carry the new release), then the agent's
/// (`up_to_date`, `held_packages`, `broken_packages`, `free_space`,
/// `reboot_pending`, `sources`, `release_offered` on Ubuntu, `leapp` on the
/// RHEL family, `third_party_sources`). A `warning` never blocks; `ready`
/// is false while any check `failed`. The agent runs its checks again just
/// before the upgrade. Upgrades themselves are campaigns with
/// `major_upgrade`; `leapp preupgrade` alone is `POST
/// /servers/{id}/major-upgrade/preupgrade`, whose report comes back in
/// `progress` with `preupgrade_only`.
///
/// Needs any role on the servers' sites and the key area "updates".
#[utoipa::path(get, path = "/api/v1/major-upgrades", tag = "campaigns",
    params(major_upgrades::Filter),
    responses(
        (status = 200, description = "The servers and their next release",
            body = major_upgrades::MajorUpgradeList, example = json!({"servers": [{
                "server_id": "6f1c2a7e-3b4d-4e8f-9a0b-1c2d3e4f5a6b",
                "hostname": "web-1",
                "site_id": "2b7e1516-28ae-4d2a-a6f7-15880928cf4f",
                "customer_id": "0b8e6f3a-1c2d-4e5f-9a8b-7c6d5e4f3a2b",
                "release": "debian 12",
                "release_label": "Debian 12 (bookworm)",
                "target": "debian 13",
                "target_label": "Debian 13 (trixie)",
                "ready": true,
                "checks": [
                    {"name": "allowed", "state": "ok", "detail": "The server allows major upgrades from the web interface.", "items": []},
                    {"name": "agent_version", "state": "ok", "detail": "The agent knows the next release.", "items": []},
                    {"name": "mirror", "state": "ok", "detail": "The server takes its packages from its own sources.", "items": []},
                    {"name": "up_to_date", "state": "ok", "detail": "every package is up to date", "items": []},
                    {"name": "held_packages", "state": "ok", "detail": "no package is held", "items": []},
                    {"name": "broken_packages", "state": "ok", "detail": "no broken package", "items": []},
                    {"name": "free_space", "state": "ok", "detail": "free: / and /var and /boot 18.2 GiB", "items": []},
                    {"name": "reboot_pending", "state": "ok", "detail": "no reboot pending", "items": []},
                    {"name": "sources", "state": "ok", "detail": "3 entries move to the new release", "items": []},
                    {"name": "third_party_sources", "state": "warning",
                     "detail": "1 third-party sources are switched off for the upgrade and left off",
                     "items": ["/etc/apt/sources.list.d/docker.list: deb https://download.docker.com/linux/debian bookworm stable"]}
                ],
                "rollback": {"available": true, "reason": null, "kind": null,
                             "detail": "LVM snapshot of / and /var", "left_out": null, "method": "lvm"},
                "progress": null,
                "reported_at": "2026-11-23T06:00:00Z",
                "can_upgrade": true,
                "can_preupgrade": false
            }]})),
        (status = 400, body = ErrorBody, description = "`invalid`: an unknown parameter or a bad ID"),
        (status = 403, body = ErrorBody, description = "`forbidden`: the caller has no role. \
`api_key_scope`: the key lacks the area"),
    ))]
fn list_major_upgrades() {}

/// Run leapp preupgrade alone on a server
///
/// Queues a `leapp_preupgrade` command for the next release of a server of
/// the RHEL family (RHEL, Rocky Linux, AlmaLinux), before any campaign
/// (ADR-0182). Its agent's root helper checks the signed command, that the
/// server allows major upgrades from the web interface, that leapp and its
/// data are installed and that no upgrade is running, then runs `leapp
/// preupgrade` alone in the upgrade's unit: it writes leapp's report and
/// changes nothing on the server. The report comes back in the server's
/// `progress` of `GET /major-upgrades`, with `preupgrade_only` set:
/// `done` when leapp found no inhibitor, `failed` with the inhibitors'
/// titles otherwise. The command succeeds once leapp started; it waits an
/// hour for an offline server. Audited as `command.create`.
///
/// Needs the administrator role on the server's customer as a whole, never
/// a site's, as for a major upgrade campaign. Through an API key, the
/// "updates" area. `can_preupgrade` of `GET /major-upgrades` says whether
/// the caller may run it now.
#[utoipa::path(post, path = "/api/v1/servers/{id}/major-upgrade/preupgrade", tag = "campaigns",
    params(("id" = uuid::Uuid, Path, description = "The server's `id`")),
    responses(
        (status = 201, description = "Command queued", body = commands::CommandDetail, example = json!({
            "id": "b1c2d3e4-f5a6-4b7c-8d9e-0f1a2b3c4d5e",
            "server_id": "6f1c2a7e-3b4d-4e8f-9a0b-1c2d3e4f5a6b",
            "kind": "leapp_preupgrade",
            "parameters": {"from": "rocky 8", "to": "rocky 9"},
            "state": "queued",
            "issued_at": "2026-11-27T10:00:00Z",
            "expires_at": "2026-11-27T11:00:00Z",
            "sent_at": null,
            "started_at": null,
            "finished_at": null,
            "exit_code": null,
            "error": null,
            "requested_by": "0d1e2f3a-4b5c-4d6e-8f7a-9b0c1d2e3f4a",
            "stdout": "",
            "stderr": ""
        })),
        (status = 400, body = ErrorBody, description = "`invalid`: a bad ID"),
        (status = 403, body = ErrorBody, description = "`forbidden`: the caller does not administer \
the server's customer as a whole. `api_key_scope`: the key lacks the area"),
        (status = 404, body = ErrorBody, description = "`not_found`: no such server, or of a site the \
caller cannot read"),
        (status = 409, body = ErrorBody, description = "`conflict`, saying why in a sentence: the \
server is not of the RHEL family, MonitorFlow knows no next release for it, it does not allow major \
upgrades from the web interface, its agent predates this command, or it is removed"),
    ))]
fn leapp_preupgrade() {}
