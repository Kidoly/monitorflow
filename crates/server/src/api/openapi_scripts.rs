//! The OpenAPI description of the script library, its runs and the
//! automation rules (ADR-0189). Merged into the document by
//! [`super::openapi::build`]; the router in `mod.rs` serves the routes.

#![allow(dead_code)] // The functions exist for their `#[utoipa::path]`.

use serde_json::{json, Value};
use utoipa::OpenApi;

use super::openapi::{Common, ErrorBody};
use super::openapi_core::ex;
use super::{automation_rules, scripts};
use crate::scripts::automation::{EventKind, Firing};
use crate::scripts::run::{RunState, Target};
use crate::scripts::{Interpreter, Parameter, ParameterType};

#[derive(OpenApi)]
#[openapi(
    paths(
        list_scripts, create_script, script, update_script, delete_script, run_script,
        list_runs, script_run, approve_run, reject_run, cancel_run,
        list_rules, create_rule, rule, update_rule, delete_rule,
    ),
    components(schemas(
        Interpreter, Parameter, ParameterType, Target, RunState, EventKind, Firing,
        scripts::ScriptView, scripts::VersionView, scripts::Counts, scripts::RunServer,
    )),
    modifiers(&Common),
    tags(
        (name = "scripts", description = "The script library (ADR-0189): shell (`sh`, Linux) and \
PowerShell (Windows) scripts kept in MonitorFlow, with typed parameters, every version kept, and the \
least role that may run them. A run sends one version to one server, a chosen group of servers, a \
site or a business service, now or at a set time, as a signed `run_script` command each agent checks. \
Agents run scripts as their own unprivileged user, never as root, and only where the host's \
administrator allowed it at install (`MF_ALLOW_SCRIPTS=1` on Linux, `ALLOW_SCRIPTS=1` or \
`monitorflow-agent.exe scripts on` on Windows): the central server cannot turn it on. Parameters reach \
the script as environment variables `MF_PARAM_<NAME>`, never in its text. Every run is classified by \
the risk rules of AI commands, line by line: a forbidden line refuses it; it runs at once when the \
person who asked may approve its tier on every server's site, and otherwise waits in the approval \
queue for 24 hours. Each server's output (the last 64 KiB of stdout and stderr) is kept with its \
command. Automation rules run a script when an incident opens, a check fails or a server joins, \
approved as AI commands are. Needs the key area \"remediation\"."),
    )
)]
pub struct Doc;

const SCRIPT: &str = "5e1f2a3b-4c5d-4e6f-8a7b-9c0d1e2f3a4b";
const RUN: &str = "7a8b9c0d-1e2f-4a3b-8c4d-5e6f7a8b9c0d";
const RULE: &str = "3c4d5e6f-7a8b-4c9d-8e0f-1a2b3c4d5e6f";
const EVENT: &str = "9e8d7c6b-5a4f-4e3d-8c2b-1a0f9e8d7c6b";
const BODY: &str =
    "#!/bin/sh\nfind /var/cache/app -mindepth 1 -mtime +\"$MF_PARAM_DAYS\" -delete\ndf -h /var\n";

fn parameters() -> Value {
    json!([{
        "name": "days", "label": "Older than (days)", "type": "integer", "required": true,
        "default": "7", "choices": []
    }])
}

fn script_view() -> Value {
    json!({
        "id": SCRIPT, "customer_id": ex::CUSTOMER, "name": "clear-app-cache",
        "description": "Deletes cached files older than the given number of days.",
        "interpreter": "sh", "run_role": "operator", "ai_allowed": true, "timeout_s": 600,
        "version": 2, "parameters": parameters(),
        "created_at": "2026-12-01T09:00:00Z", "updated_at": "2026-12-03T14:12:00Z",
        "deleted_at": null, "can_manage": true
    })
}

fn script_detail() -> Value {
    json!({
        "script": script_view(), "body": BODY,
        "versions": [
            {"version": 2, "body": BODY, "parameters": parameters(),
             "note": "Show the free space after", "created_by": ex::USER,
             "created_by_name": "alice", "created_at": "2026-12-03T14:12:00Z"},
            {"version": 1, "body": "find /var/cache/app -mindepth 1 -mtime +7 -delete\n",
             "parameters": [], "note": "", "created_by": ex::USER, "created_by_name": "alice",
             "created_at": "2026-12-01T09:00:00Z"}
        ]
    })
}

fn script_request() -> Value {
    json!({
        "customer_id": ex::CUSTOMER, "name": "clear-app-cache",
        "description": "Deletes cached files older than the given number of days.",
        "interpreter": "sh", "run_role": "operator", "ai_allowed": true, "timeout_s": 600,
        "body": BODY, "parameters": parameters(), "note": "Show the free space after"
    })
}

fn run_view(state: &str) -> Value {
    let dispatched = state == "dispatched";
    let waiting = state == "awaiting_approval";
    json!({
        "id": RUN, "script_id": SCRIPT, "script_name": "clear-app-cache", "version": 2,
        "interpreter": "sh", "customer_id": ex::CUSTOMER, "parameters": {"days": "14"},
        "target": {"kind": "site", "site_id": ex::SITE},
        "requested_by": ex::USER, "requested_by_name": "bob",
        "rule_id": null, "rule_name": null, "trigger": null,
        "state": state, "tier": "high_impact",
        "rule": {"source": "builtin", "id": "default.unknown"},
        "reason": "line 2: `find` with -delete outside a cache directory is not known",
        "scheduled_at": "2026-12-04T22:00:00Z", "created_at": "2026-12-04T10:15:00Z",
        "expires_at": if waiting { json!("2026-12-05T10:15:00Z") } else { Value::Null },
        "decided_by": if dispatched { json!(ex::USER) } else { Value::Null },
        "decided_by_name": if dispatched { json!("alice") } else { Value::Null },
        "decided_at": if dispatched { json!("2026-12-04T11:02:00Z") } else { Value::Null },
        "decision_reason": null,
        "dispatched_at": if dispatched { json!("2026-12-04T22:00:02Z") } else { Value::Null },
        "counts": if dispatched {
            json!({"servers": 2, "pending": 0, "succeeded": 1, "failed": 1})
        } else {
            json!({"servers": 2, "pending": 2, "succeeded": 0, "failed": 0})
        },
        "can_approve": waiting, "can_reject": waiting, "can_cancel": waiting
    })
}

fn run_detail(state: &str) -> Value {
    let dispatched = state == "dispatched";
    let servers = if dispatched {
        json!([
            {"server_id": ex::SERVER, "hostname": "web-1", "site_id": ex::SITE,
             "status": "succeeded", "command_id": ex::COMMAND, "exit_code": 0,
             "stdout": "Filesystem  Size  Used Avail Use% Mounted on\n/dev/sda3    40G   21G   17G  56% /var\n",
             "stderr": "", "error": null, "started_at": "2026-12-04T22:00:03Z",
             "finished_at": "2026-12-04T22:00:05Z"},
            {"server_id": ex::SERVER_2, "hostname": "web-2", "site_id": ex::SITE,
             "status": "skipped", "command_id": null, "exit_code": null, "stdout": null,
             "stderr": null,
             "error": "library scripts are off on this server; run the installer there with MF_ALLOW_SCRIPTS=1",
             "started_at": null, "finished_at": null}
        ])
    } else {
        json!([
            {"server_id": ex::SERVER, "hostname": "web-1", "site_id": ex::SITE,
             "status": "waiting", "command_id": null, "exit_code": null, "stdout": null,
             "stderr": null, "error": null, "started_at": null, "finished_at": null},
            {"server_id": ex::SERVER_2, "hostname": "web-2", "site_id": ex::SITE,
             "status": "waiting", "command_id": null, "exit_code": null, "stdout": null,
             "stderr": null, "error": null, "started_at": null, "finished_at": null}
        ])
    };
    json!({"run": run_view(state), "body": BODY, "servers": servers})
}

fn rule_view() -> Value {
    json!({
        "id": RULE, "name": "Clear the cache when /var fills", "customer_id": ex::CUSTOMER,
        "site_id": null, "event": "incident_opened", "metric": "monitorflow_disk_used_ratio",
        "script_id": SCRIPT, "script_name": "clear-app-cache", "parameters": {"days": "3"},
        "enabled": true, "cooldown_minutes": 60,
        "created_at": "2026-12-04T09:00:00Z", "updated_at": "2026-12-04T09:00:00Z",
        "can_manage": true
    })
}

fn rule_request() -> Value {
    json!({
        "name": "Clear the cache when /var fills", "customer_id": ex::CUSTOMER,
        "event": "incident_opened", "metric": "monitorflow_disk_used_ratio",
        "script_id": SCRIPT, "parameters": {"days": "3"}, "enabled": true,
        "cooldown_minutes": 60
    })
}

fn rule_detail() -> Value {
    json!({
        "rule": rule_view(),
        "firings": [
            {"event_id": EVENT, "server_id": ex::SERVER, "hostname": "web-1", "run_id": RUN,
             "skipped": null, "created_at": "2026-12-04T13:40:05Z"}
        ]
    })
}

#[utoipa::path(get, path = "/api/v1/remediation/scripts", tag = "scripts",
    summary = "List library scripts",
    description = "The live scripts the caller can use: those of every customer and those of the \
customers the caller reads, by name. With `customer_id`, those that customer's servers can run.\n\n\
**Who**: any role. **Area**: `remediation`.",
    params(scripts::ListParams),
    responses(
        (status = 200, description = "The scripts", body = scripts::ScriptList,
            example = json!({"scripts": [script_view()]})),
        (status = 400, description = "An unknown parameter: `invalid`", body = ErrorBody),
        (status = 404, description = "A customer the key does not see: `not_found`", body = ErrorBody),
    ))]
fn list_scripts() {}

#[utoipa::path(post, path = "/api/v1/remediation/scripts", tag = "scripts",
    summary = "Add a script",
    description = "Adds a script and its first version. A script of every customer (`customer_id` \
null) is for administrators of everything; a customer's own is for administrators of that customer \
as a whole. `run_role` is the least role that may run it; `ai_allowed` lets the AI propose it as a fix \
during remediation, classified and approved like its other commands.\n\n\
**Who**: administrator, as above. **Area**: `remediation`. Audited as `script.create`.",
    request_body(content = scripts::ScriptRequest, example = json!(script_request())),
    responses(
        (status = 201, description = "Added", body = scripts::ScriptDetail,
            example = json!(script_detail())),
        (status = 400, description = "A bad name, an empty or too long text, bad parameters, a viewer \
`run_role` or a timeout out of range: `invalid`", body = ErrorBody),
        (status = 403, description = "Not an administrator there: `forbidden`; or outside the key's \
areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "A customer the key does not see: `not_found`", body = ErrorBody),
        (status = 409, description = "A live script of that name exists already: `conflict`",
            body = ErrorBody),
    ))]
fn create_script() {}

#[utoipa::path(get, path = "/api/v1/remediation/scripts/{id}", tag = "scripts",
    summary = "Get a script and its versions",
    description = "The script, its current text and every version, newest first. A deleted script \
is still shown, with `deleted_at`, for its runs.\n\n**Who**: any role reading its customer. **Area**: \
`remediation`.",
    params(("id" = uuid::Uuid, Path, description = "The script")),
    responses(
        (status = 200, description = "The script", body = scripts::ScriptDetail,
            example = json!(script_detail())),
        (status = 404, description = "No such script, or one the key does not see: `not_found`",
            body = ErrorBody),
    ))]
fn script() {}

#[utoipa::path(put, path = "/api/v1/remediation/scripts/{id}", tag = "scripts",
    summary = "Change a script",
    description = "Replaces every field. A new text or new parameters make a new version, the earlier \
ones are kept; other fields change in place. `customer_id` must stay what it is. Runs already made \
keep the version they named.\n\n\
**Who**: administrator, as for adding. **Area**: `remediation`. Audited as `script.update`.",
    params(("id" = uuid::Uuid, Path, description = "The script")),
    request_body(content = scripts::ScriptRequest, example = json!(script_request())),
    responses(
        (status = 200, description = "Changed", body = scripts::ScriptDetail,
            example = json!(script_detail())),
        (status = 400, description = "As for adding, or another `customer_id`: `invalid`",
            body = ErrorBody),
        (status = 403, description = "Not an administrator there: `forbidden`", body = ErrorBody),
        (status = 404, description = "No such script: `not_found`", body = ErrorBody),
        (status = 409, description = "Deleted, or the name is taken: `conflict`", body = ErrorBody),
    ))]
fn update_script() {}

#[utoipa::path(delete, path = "/api/v1/remediation/scripts/{id}", tag = "scripts",
    summary = "Delete a script",
    description = "The script leaves the library and the AI's list; its versions and runs are kept, \
and runs scheduled but not sent yet skip their servers. Refused while automation rules run it.\n\n\
**Who**: administrator, as for adding. **Area**: `remediation`. Audited as `script.delete`.",
    params(("id" = uuid::Uuid, Path, description = "The script")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 403, description = "Not an administrator there: `forbidden`", body = ErrorBody),
        (status = 404, description = "No such script: `not_found`", body = ErrorBody),
        (status = 409, description = "Automation rules run it: `conflict`", body = ErrorBody),
    ))]
fn delete_script() {}

#[utoipa::path(post, path = "/api/v1/remediation/scripts/{id}/runs", tag = "scripts",
    summary = "Run a script",
    description = "Runs the script on one server or a group of servers (`servers`), every server of \
a site (`site`) or of a business service (`business_service`), all of one customer, now or at \
`scheduled_at`. The risk rules classify it for each server's site: forbidden is 400; otherwise it is \
`scheduled` when the caller may approve its tier on every site (approver for `read_only` and \
`low_impact`, administrator for `high_impact`), else `awaiting_approval` until an approver decides, \
for 24 hours at most. When its time comes, each server gets a signed `run_script` command; a server \
that removed, runs another system, has scripts turned off, or that a rule now forbids is `skipped` \
with the reason. A command waits an hour for an offline server.\n\n\
**Who**: the script's `run_role` on every server's site. **Area**: `remediation`. Audited as \
`script.run`.",
    params(("id" = uuid::Uuid, Path, description = "The script")),
    request_body(content = scripts::RunRequest, examples(
        ("site" = (summary = "Every server of a site, tonight", value = json!({
            "target": {"kind": "site", "site_id": ex::SITE}, "parameters": {"days": "14"},
            "scheduled_at": "2026-12-04T22:00:00Z"}))),
        ("servers" = (summary = "Two servers, now", value = json!({
            "target": {"kind": "servers", "server_ids": [ex::SERVER, ex::SERVER_2]},
            "parameters": {"days": "14"}}))),
    )),
    responses(
        (status = 201, description = "Made", body = scripts::RunDetail,
            example = json!(run_detail("awaiting_approval"))),
        (status = 400, description = "No server, servers of several customers, another customer's \
script, a missing or bad parameter, an unknown version, a time over a year ahead, or forbidden by \
the risk rules: `invalid`", body = ErrorBody),
        (status = 403, description = "Below the script's `run_role` on a server's site: `forbidden`",
            body = ErrorBody),
        (status = 404, description = "No such script, or a server, site or business service the key \
does not see: `not_found`", body = ErrorBody),
        (status = 409, description = "The script was deleted: `conflict`", body = ErrorBody),
    ))]
fn run_script() {}

#[utoipa::path(get, path = "/api/v1/remediation/script-runs", tag = "scripts",
    summary = "List script runs",
    description = "Runs the caller reads every server of, newest first, with how their servers are \
doing. `state=awaiting_approval` is the approval queue of scripts.\n\n\
**Who**: any role. **Area**: `remediation`.",
    params(scripts::RunParams),
    responses(
        (status = 200, description = "The runs", body = scripts::RunList,
            example = json!({"runs": [run_view("awaiting_approval")]})),
        (status = 400, description = "An unknown parameter or state: `invalid`", body = ErrorBody),
        (status = 404, description = "A server the key does not see: `not_found`", body = ErrorBody),
    ))]
fn list_runs() {}

#[utoipa::path(get, path = "/api/v1/remediation/script-runs/{id}", tag = "scripts",
    summary = "Get a script run and its output",
    description = "The run, the exact text it runs, and each server's state and output (the last \
64 KiB of stdout and stderr, the exit code, or why it was skipped).\n\n\
**Who**: any role reading every server of the run. **Area**: `remediation`.",
    params(("id" = uuid::Uuid, Path, description = "The run")),
    responses(
        (status = 200, description = "The run", body = scripts::RunDetail,
            example = json!(run_detail("dispatched"))),
        (status = 404, description = "No such run, or one the key does not see: `not_found`",
            body = ErrorBody),
    ))]
fn script_run() {}

#[utoipa::path(post, path = "/api/v1/remediation/script-runs/{id}/approve", tag = "scripts",
    summary = "Approve a script run",
    description = "Lets a run waiting for approval go: it is sent at once, or at its time. It is \
classified again first: a run the rules now forbid is 409, and the role needed is that of the higher \
of its two tiers.\n\n\
**Who**: approver (`read_only`, `low_impact`) or administrator (`high_impact`) on every server's \
site. **Area**: `remediation`. Audited as `script_run.approve`.",
    params(("id" = uuid::Uuid, Path, description = "The run")),
    responses(
        (status = 200, description = "Approved", body = scripts::RunDetail,
            example = json!(run_detail("dispatched"))),
        (status = 403, description = "Access too low for its tier: `forbidden`", body = ErrorBody),
        (status = 404, description = "No such run: `not_found`", body = ErrorBody),
        (status = 409, description = "Not waiting for approval, or now forbidden: `conflict`",
            body = ErrorBody),
    ))]
fn approve_run() {}

#[utoipa::path(post, path = "/api/v1/remediation/script-runs/{id}/reject", tag = "scripts",
    summary = "Reject a script run",
    description = "Drops a run waiting for approval, saying why.\n\n\
**Who**: approver or above on every server's site. **Area**: `remediation`. Audited as \
`script_run.reject`.",
    params(("id" = uuid::Uuid, Path, description = "The run")),
    request_body(content = scripts::RejectRequest,
        example = json!({"reason": "Not during the sale; schedule it for Sunday night."})),
    responses(
        (status = 200, description = "Rejected", body = scripts::RunDetail,
            example = json!(run_detail("rejected"))),
        (status = 400, description = "No reason, or one over 1000 characters: `invalid`",
            body = ErrorBody),
        (status = 403, description = "Not an approver there: `forbidden`", body = ErrorBody),
        (status = 404, description = "No such run: `not_found`", body = ErrorBody),
        (status = 409, description = "Not waiting for approval: `conflict`", body = ErrorBody),
    ))]
fn reject_run() {}

#[utoipa::path(post, path = "/api/v1/remediation/script-runs/{id}/cancel", tag = "scripts",
    summary = "Cancel a script run",
    description = "Cancels a run that waits for approval or for its time. Commands already sent are \
not recalled.\n\n\
**Who**: the person who asked for it, or an approver or above on every server's site. **Area**: \
`remediation`. Audited as `script_run.cancel`.",
    params(("id" = uuid::Uuid, Path, description = "The run")),
    responses(
        (status = 200, description = "Cancelled", body = scripts::RunDetail,
            example = json!(run_detail("cancelled"))),
        (status = 403, description = "Neither its requester nor an approver: `forbidden`",
            body = ErrorBody),
        (status = 404, description = "No such run: `not_found`", body = ErrorBody),
        (status = 409, description = "Sent, or ended already: `conflict`", body = ErrorBody),
    ))]
fn cancel_run() {}

#[utoipa::path(get, path = "/api/v1/remediation/automation-rules", tag = "scripts",
    summary = "List automation rules",
    description = "Rules whose place (everything, a customer or a site) the caller reads, by name. \
A rule runs its script on a server when an incident opens there (`incident_opened`, of any metric but \
`offline`, or of `metric`), when its check `metric` fails (`check_failed`), or when it joins \
(`server_joined`, a first enrollment). Each run is classified as an AI command: read-only runs at \
once, low impact when the site lets AI changes run on their own, the rest waits for approval. At \
most one run per server per `cooldown_minutes`, and 50 per rule per hour.\n\n\
**Who**: any role. **Area**: `remediation`.",
    params(automation_rules::ListParams),
    responses(
        (status = 200, description = "The rules", body = automation_rules::RuleList,
            example = json!({"rules": [rule_view()]})),
        (status = 400, description = "An unknown parameter: `invalid`", body = ErrorBody),
        (status = 404, description = "A customer the key does not see: `not_found`", body = ErrorBody),
    ))]
fn list_rules() {}

#[utoipa::path(post, path = "/api/v1/remediation/automation-rules", tag = "scripts",
    summary = "Add an automation rule",
    description = "Adds a rule. It takes only events after it was made. Its script must be one of \
every customer, or the customer's own for a rule of that customer or one of its sites; the values \
given must suit its parameters.\n\n\
**Who**: administrator of the place: of everything, of the customer as a whole, or of the site. \
**Area**: `remediation`. Audited as `automation_rule.create`.",
    request_body(content = automation_rules::RuleRequest, example = json!(rule_request())),
    responses(
        (status = 201, description = "Added", body = automation_rules::RuleDetail,
            example = json!({"rule": rule_view(), "firings": []})),
        (status = 400, description = "No name, both a customer and a site, a missing or stray \
metric, a cooldown out of range, another customer's script or bad parameters: `invalid`",
            body = ErrorBody),
        (status = 403, description = "Not an administrator of the place: `forbidden`", body = ErrorBody),
        (status = 404, description = "A place or script the key does not see: `not_found`",
            body = ErrorBody),
    ))]
fn create_rule() {}

#[utoipa::path(get, path = "/api/v1/remediation/automation-rules/{id}", tag = "scripts",
    summary = "Get an automation rule and its last firings",
    description = "The rule and the last 50 events it took, with the run each made or why it made \
none (cooldown, hourly limit, a script deleted or for another system).\n\n\
**Who**: any role reading its place. **Area**: `remediation`.",
    params(("id" = uuid::Uuid, Path, description = "The rule")),
    responses(
        (status = 200, description = "The rule", body = automation_rules::RuleDetail,
            example = json!(rule_detail())),
        (status = 404, description = "No such rule, or one the key does not see: `not_found`",
            body = ErrorBody),
    ))]
fn rule() {}

#[utoipa::path(put, path = "/api/v1/remediation/automation-rules/{id}", tag = "scripts",
    summary = "Change an automation rule",
    description = "Replaces every field, as for adding; `enabled: false` pauses it.\n\n\
**Who**: administrator of the place it has and of the one it gets. **Area**: `remediation`. Audited \
as `automation_rule.update`.",
    params(("id" = uuid::Uuid, Path, description = "The rule")),
    request_body(content = automation_rules::RuleRequest, example = json!(rule_request())),
    responses(
        (status = 200, description = "Changed", body = automation_rules::RuleDetail,
            example = json!(rule_detail())),
        (status = 400, description = "As for adding: `invalid`", body = ErrorBody),
        (status = 403, description = "Not an administrator of the place: `forbidden`", body = ErrorBody),
        (status = 404, description = "No such rule: `not_found`", body = ErrorBody),
    ))]
fn update_rule() {}

#[utoipa::path(delete, path = "/api/v1/remediation/automation-rules/{id}", tag = "scripts",
    summary = "Delete an automation rule",
    description = "The rule stops; the runs it made are kept.\n\n\
**Who**: administrator of the place. **Area**: `remediation`. Audited as `automation_rule.delete`.",
    params(("id" = uuid::Uuid, Path, description = "The rule")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 403, description = "Not an administrator of the place: `forbidden`", body = ErrorBody),
        (status = 404, description = "No such rule: `not_found`", body = ErrorBody),
    ))]
fn delete_rule() {}
