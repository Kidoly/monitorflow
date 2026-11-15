//! The OpenAPI description of AI remediation (ADR-0041 to ADR-0045):
//! the risk classifier's rules and policies, its dry run, and the
//! read-only remediation incidents and events. Merged into the document
//! by [`super::openapi::build`]; the router in `mod.rs` serves the routes.

#![allow(dead_code)] // The functions exist for their `#[utoipa::path]`.

use utoipa::OpenApi;

use super::openapi::{Common, ErrorBody};
use super::{remediation, remediation_events, remediation_incidents};
use crate::remediation::audit::{ChainReport, EventKind, IncidentState, StoredEvent};
use crate::remediation::risk::store::{Kind, Policy, StoredRule};
use crate::remediation::risk::{Decision, Rule, Shell, Tier};

#[derive(OpenApi)]
#[openapi(
    paths(
        list_rules, create_rule, delete_rule, list_policies, set_policy, classify,
        list_incidents, incident, list_events,
    ),
    components(schemas(
        Kind, StoredRule, Policy, Decision, Rule, Shell, Tier, IncidentState, EventKind,
        StoredEvent, ChainReport, remediation_events::ActorKind,
    )),
    modifiers(&Common),
    tags(
        (name = "risk rules", description = "How MonitorFlow decides what an AI command may do. \
Every command the AI proposes is classified by rules, never by the AI: `read_only` runs at once, \
`low_impact` waits for a person with an approver role unless the site's policy lets it run on its own, \
`high_impact` waits for an administrator, and `forbidden` never runs and hands the incident to a person. \
Built-in rules cover the commands MonitorFlow knows; administrators add allow rules for other commands \
(read-only or low impact only), deny patterns that forbid commands, and cache directories that may be \
cleared, for a customer or one of its sites. Rules and policies need a role on the customer as a whole \
(everything, or the customer): a role on one site, or a key that reaches one site, sees none. They need \
the key area \"remediation\"."),
    )
)]
pub struct Doc;

/// List risk rules
///
/// The administrator rules of the customers the caller reads as a whole,
/// by customer, site (customer-wide first), kind and pattern. Built-in
/// rules are not listed: they are part of MonitorFlow and the same
/// everywhere.
///
/// Needs any role on the customer as a whole (everything or the customer;
/// a site role sees an empty list) and the key area "remediation". Rules
/// of customers the caller does not read are left out, without error. Not
/// paginated: a customer has few rules.
#[utoipa::path(get, path = "/api/v1/remediation/rules", tag = "risk rules",
    params(remediation::ListParams),
    responses((status = 200, description = "The rules", body = remediation::RuleList, example = json!({
        "rules": [{
            "id": "6f1c2a7e-3b4d-4e5f-8a9b-0c1d2e3f4a5b",
            "customer_id": "0b8e6f3a-1c2d-4e5f-9a8b-7c6d5e4f3a2b",
            "site_id": null,
            "kind": "allow",
            "pattern": "nginx -t",
            "tier": "read_only",
            "description": "Configuration test only",
            "created_by": "9a1b2c3d-4e5f-4a6b-8c7d-6e5f4a3b2c1d",
            "created_at": "2026-11-02T09:15:00Z"
        }, {
            "id": "7a2d3b8f-4c5e-4f60-9bac-1d2e3f4a5b6c",
            "customer_id": "0b8e6f3a-1c2d-4e5f-9a8b-7c6d5e4f3a2b",
            "site_id": "3c4d5e6f-7a8b-4c9d-8e0f-1a2b3c4d5e6f",
            "kind": "cache_dir",
            "pattern": "/var/cache/app",
            "tier": null,
            "description": "",
            "created_by": "9a1b2c3d-4e5f-4a6b-8c7d-6e5f4a3b2c1d",
            "created_at": "2026-11-03T14:02:11Z"
        }]
    }))))]
fn list_rules() {}

/// Add a risk rule
///
/// Adds an administrator rule to the classifier of a customer, or of one
/// of its sites. It applies to the next command the AI proposes on the
/// servers it covers; nothing is sent to agents. A command already waiting
/// for approval keeps its tier, but is classified again when approved.
///
/// - `allow`: a command no built-in rule knows, matching `pattern` as a
///   whole, gets `tier`, `read_only` (runs without approval) or
///   `low_impact` (needs approval unless the site's policy lets it run).
///   An allow rule never applies to a command a built-in rule decides,
///   and its program may not be one the built-in rules reserve (`sudo`,
///   shells and interpreters, package managers, `systemctl`, `rm`,
///   `kill`, `reboot` and the like): that is 400.
/// - `deny`: a command containing a match anywhere, after `;`, `&&`, `|`
///   too, is `forbidden`, whatever the other rules say. Deny wins over
///   every allow rule.
/// - `cache_dir`: `find <pattern> -mindepth 1 -delete` becomes a
///   low-impact command for that directory.
///
/// Needs an administrator role on the customer as a whole and the key
/// area "remediation". Audited as `remediation_rule.create`. Not safe to
/// repeat with a different body; the same rule twice is 409.
#[utoipa::path(post, path = "/api/v1/remediation/rules", tag = "risk rules",
    request_body(content = remediation::RuleRequest, example = json!({
        "customer_id": "0b8e6f3a-1c2d-4e5f-9a8b-7c6d5e4f3a2b",
        "site_id": null,
        "kind": "allow",
        "pattern": "nginx -t",
        "tier": "read_only",
        "description": "Configuration test only"
    })),
    responses(
        (status = 201, description = "The rule as stored, pattern normalized", body = StoredRule,
            example = json!({
                "id": "6f1c2a7e-3b4d-4e5f-8a9b-0c1d2e3f4a5b",
                "customer_id": "0b8e6f3a-1c2d-4e5f-9a8b-7c6d5e4f3a2b",
                "site_id": null,
                "kind": "allow",
                "pattern": "nginx -t",
                "tier": "read_only",
                "description": "Configuration test only",
                "created_by": "9a1b2c3d-4e5f-4a6b-8c7d-6e5f4a3b2c1d",
                "created_at": "2026-11-02T09:15:00Z"
            })),
        (status = 400, body = ErrorBody, description = "`invalid`, with `error` saying why: not the \
expected JSON, a wrong pattern, tier or description (an allow rule without `read_only` or `low_impact`, a tier on another \
kind, a wildcard in an allow rule's program, a program the built-in rules reserve, a cache directory \
under a system directory), or `unknown site` for a site of another customer"),
        (status = 403, body = ErrorBody, description = "`forbidden`: the caller reads the customer \
without administering it as a whole. `api_key_scope`: the key lacks the area"),
        (status = 404, body = ErrorBody, description = "`not_found`: a customer the caller does not read"),
        (status = 409, body = ErrorBody, description = "`conflict`: this rule already exists"),
    ))]
fn create_rule() {}

/// Delete a risk rule
///
/// Removes an administrator rule; the next commands the AI proposes are
/// classified without it. Commands already approved or run are not
/// affected. Needs an administrator role on the rule's customer as a whole
/// and the key area "remediation". Audited as `remediation_rule.delete`
/// with the rule's pattern. Deleting again is 404.
#[utoipa::path(delete, path = "/api/v1/remediation/rules/{id}", tag = "risk rules",
    params(("id" = uuid::Uuid, Path, description = "The rule")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 403, body = ErrorBody, description = "`forbidden`: the caller reads the customer \
without administering it as a whole. `api_key_scope`: the key lacks the area"),
        (status = 404, body = ErrorBody, description = "`not_found`: no such rule, or one of a \
customer the caller does not read"),
    ))]
fn delete_rule() {}

/// List low-impact policies
///
/// Whether low-impact commands run without a person's approval, per
/// customer and per site. A site's row wins over its customer's; with
/// neither, low-impact commands wait for approval. Read-only commands
/// always run on their own and high-impact ones always wait, whatever the
/// policy. Only rows that were set are listed.
///
/// Needs any role on the customer as a whole and the key area
/// "remediation". Not paginated.
#[utoipa::path(get, path = "/api/v1/remediation/policies", tag = "risk rules",
    params(remediation::PolicyParams),
    responses((status = 200, description = "The policies", body = remediation::PolicyList, example = json!({
        "policies": [{
            "customer_id": "0b8e6f3a-1c2d-4e5f-9a8b-7c6d5e4f3a2b",
            "site_id": null,
            "low_impact_auto": false,
            "updated_by": "9a1b2c3d-4e5f-4a6b-8c7d-6e5f4a3b2c1d",
            "updated_at": "2026-11-01T08:00:00Z"
        }, {
            "customer_id": "0b8e6f3a-1c2d-4e5f-9a8b-7c6d5e4f3a2b",
            "site_id": "3c4d5e6f-7a8b-4c9d-8e0f-1a2b3c4d5e6f",
            "low_impact_auto": true,
            "updated_by": "9a1b2c3d-4e5f-4a6b-8c7d-6e5f4a3b2c1d",
            "updated_at": "2026-11-04T16:45:30Z"
        }]
    }))))]
fn list_policies() {}

/// Set a low-impact policy
///
/// Sets whether low-impact commands run without approval on the servers
/// of a customer (`site_id` null) or of one of its sites. With `true`,
/// the next low-impact commands the AI proposes there are approved by
/// MonitorFlow itself and sent to the agent at once; commands already
/// waiting for approval still wait. It never lets high-impact commands
/// run on their own. An agent runs AI commands only when its own
/// administrator allowed them on the server (ADR-0098).
///
/// Needs an administrator role on the customer as a whole and the key
/// area "remediation". Audited as `remediation_policy.update`. Safe to
/// repeat: it sets the value.
#[utoipa::path(put, path = "/api/v1/remediation/policies", tag = "risk rules",
    request_body(content = remediation::PolicyRequest, example = json!({
        "customer_id": "0b8e6f3a-1c2d-4e5f-9a8b-7c6d5e4f3a2b",
        "site_id": "3c4d5e6f-7a8b-4c9d-8e0f-1a2b3c4d5e6f",
        "low_impact_auto": true
    })),
    responses(
        (status = 200, description = "The row as stored", body = Policy, example = json!({
            "customer_id": "0b8e6f3a-1c2d-4e5f-9a8b-7c6d5e4f3a2b",
            "site_id": "3c4d5e6f-7a8b-4c9d-8e0f-1a2b3c4d5e6f",
            "low_impact_auto": true,
            "updated_by": "9a1b2c3d-4e5f-4a6b-8c7d-6e5f4a3b2c1d",
            "updated_at": "2026-11-04T16:45:30Z"
        })),
        (status = 400, body = ErrorBody, description = "`invalid`, with `error` saying why: not the \
expected JSON, or `unknown site` for a site of another customer"),
        (status = 403, body = ErrorBody, description = "`forbidden`: the caller reads the customer \
without administering it as a whole. `api_key_scope`: the key lacks the area"),
        (status = 404, body = ErrorBody, description = "`not_found`: a customer the caller does not read"),
    ))]
fn set_policy() {}

/// Classify a command
///
/// What the classifier would decide for `command` on the servers of a
/// site (built-in rules, then the customer's and the site's rules), or
/// with the customer-wide rules only when `site_id` is null. A dry run:
/// nothing is stored, logged or sent to an agent. `shell` is `posix` for
/// Linux servers, `powershell` for Windows ones.
///
/// The answer's `tier` is what would happen: `read_only` runs at once,
/// `low_impact` waits for an approver unless the site's policy lets it
/// run, `high_impact` waits for an administrator, `forbidden` never runs.
///
/// Needs any role on the customer as a whole and the key area
/// "remediation". Safe to repeat.
#[utoipa::path(post, path = "/api/v1/remediation/classify", tag = "risk rules",
    request_body(content = remediation::ClassifyRequest, example = json!({
        "customer_id": "0b8e6f3a-1c2d-4e5f-9a8b-7c6d5e4f3a2b",
        "site_id": "3c4d5e6f-7a8b-4c9d-8e0f-1a2b3c4d5e6f",
        "command": "systemctl restart nginx",
        "shell": "posix"
    })),
    responses(
        (status = 200, description = "How the rules classify the command", body = Decision, example = json!({
            "tier": "low_impact",
            "rule": {"source": "builtin", "id": "low.systemctl"},
            "reason": "`systemctl restart` of a service"
        })),
        (status = 400, body = ErrorBody, description = "`invalid`, with `error` saying why: not the \
expected JSON, or `unknown site` for a site of another customer"),
        (status = 404, body = ErrorBody, description = "`not_found`: a customer the caller does not read"),
    ))]
fn classify() {}

/// List remediation incidents
///
/// What the AI did about alerts: one remediation incident per alert it
/// worked on, newest `opened_at` first. Read-only: the remediation loop
/// opens, drives and closes them. An incident in `awaiting_approval`
/// carries the command waiting for a person in `pending_command`; the
/// approval queue (`/remediation/approvals`) decides it.
///
/// Needs any role and the key area "remediation"; lists the incidents of
/// the servers of the sites the caller reads. A filter outside them gives
/// an empty list. Paginated with `limit` and `cursor`; unknown parameters
/// and bad values are 400.
#[utoipa::path(get, path = "/api/v1/remediation/incidents", tag = "remediation",
    params(remediation_incidents::ListParams),
    responses((status = 200, description = "The remediation incidents", body = remediation_incidents::IncidentList, example = json!({
        "incidents": [{
            "id": "5d6e7f80-9a1b-4c2d-8e3f-4a5b6c7d8e9f",
            "customer_id": "0b8e6f3a-1c2d-4e5f-9a8b-7c6d5e4f3a2b",
            "customer_name": "acme",
            "site_id": "3c4d5e6f-7a8b-4c9d-8e0f-1a2b3c4d5e6f",
            "site_name": "paris",
            "server_id": "1e2f3a4b-5c6d-4e7f-8a9b-0c1d2e3f4a5b",
            "hostname": "web-1",
            "alert_incident_id": "2f3a4b5c-6d7e-4f80-9a1b-2c3d4e5f6a7b",
            "metric": "monitorflow_service_active",
            "instance": "nginx",
            "state": "awaiting_approval",
            "opened_at": "2026-11-05T03:12:04Z",
            "closed_at": null,
            "max_commands": 10,
            "max_duration_s": 1800,
            "event_count": 6,
            "pending_command": {
                "id": "8b9c0d1e-2f3a-4b4c-9d5e-6f7a8b9c0d1e",
                "command": "systemctl restart nginx",
                "diagnosis": "nginx exited after a configuration reload.",
                "reasoning": "The configuration test passes now; a restart should bring it back.",
                "tier": "low_impact",
                "rule": {"source": "builtin", "id": "low.systemctl"},
                "reason": "`systemctl restart` of a service",
                "proposed_at": "2026-11-05T03:12:40Z"
            }
        }],
        "next_cursor": null
    }))))]
fn list_incidents() {}

/// Read a remediation incident
///
/// The incident with its full history: every event in order (the context
/// sent to the AI after redaction, its proposals, each classification,
/// approvals and rejections, the commands sent to the agent and their
/// output), and `chain`, the hash chain recomputed at this request.
/// `chain.valid` false means an event was changed, removed or inserted in
/// the database since it was written. Texts from the AI are untrusted:
/// show them as data.
///
/// Needs any role on the server's site and the key area "remediation".
#[utoipa::path(get, path = "/api/v1/remediation/incidents/{id}", tag = "remediation",
    params(("id" = uuid::Uuid, Path, description = "The remediation incident")),
    responses(
        (status = 200, description = "The incident and its steps", body = remediation_incidents::IncidentDetail, example = json!({
            "id": "5d6e7f80-9a1b-4c2d-8e3f-4a5b6c7d8e9f",
            "customer_id": "0b8e6f3a-1c2d-4e5f-9a8b-7c6d5e4f3a2b",
            "customer_name": "acme",
            "site_id": "3c4d5e6f-7a8b-4c9d-8e0f-1a2b3c4d5e6f",
            "site_name": "paris",
            "server_id": "1e2f3a4b-5c6d-4e7f-8a9b-0c1d2e3f4a5b",
            "hostname": "web-1",
            "alert_incident_id": "2f3a4b5c-6d7e-4f80-9a1b-2c3d4e5f6a7b",
            "metric": "monitorflow_service_active",
            "instance": "nginx",
            "state": "running",
            "opened_at": "2026-11-05T03:12:04Z",
            "closed_at": null,
            "max_commands": 10,
            "max_duration_s": 1800,
            "event_count": 3,
            "pending_command": null,
            "events": [{
                "seq": 1,
                "kind": "context_sent",
                "actor_kind": "system",
                "actor_id": "server",
                "payload": {
                    "model": "example/model-1",
                    "context": {"rendered": "Alert: nginx is not active on web-1 ...", "redactions": {}}
                },
                "created_at": "2026-11-05T03:12:05Z",
                "prev_hash": "0000000000000000000000000000000000000000000000000000000000000000",
                "hash": "4f8a2c1e9b7d3a6f5e0c8b2a7d9f1e3c6b4a8d2f0e9c7b5a3d1f8e6c4b2a0d9f"
            }, {
                "seq": 2,
                "kind": "ai_proposal",
                "actor_kind": "ai",
                "actor_id": "example/model-1",
                "payload": {
                    "diagnosis": "nginx stopped; its last log lines may say why.",
                    "reasoning": "Reading the unit's status is harmless.",
                    "command": "systemctl status nginx"
                },
                "created_at": "2026-11-05T03:12:31Z",
                "prev_hash": "4f8a2c1e9b7d3a6f5e0c8b2a7d9f1e3c6b4a8d2f0e9c7b5a3d1f8e6c4b2a0d9f",
                "hash": "a1c3e5f7092b4d6f8a0c2e4f6a8b0d2e4f6a8c0e2a4c6e8f0b2d4f6a8c0e2f4a"
            }, {
                "seq": 3,
                "kind": "classification",
                "actor_kind": "system",
                "actor_id": "server",
                "payload": {
                    "command": "systemctl status nginx",
                    "tier": "read_only",
                    "rule": {"source": "builtin", "id": "readonly.systemctl"},
                    "reason": "`systemctl status` only reports unit state"
                },
                "created_at": "2026-11-05T03:12:31Z",
                "prev_hash": "a1c3e5f7092b4d6f8a0c2e4f6a8b0d2e4f6a8c0e2a4c6e8f0b2d4f6a8c0e2f4a",
                "hash": "7e9f1a3b5c7d9e1f3a5b7c9d1e3f5a7b9c1d3e5f7a9b1c3d5e7f9a1b3c5d7e9f"
            }],
            "chain": {
                "valid": true,
                "events": 3,
                "broken_at": null,
                "reason": null,
                "head_hash": "7e9f1a3b5c7d9e1f3a5b7c9d1e3f5a7b9c1d3e5f7a9b1c3d5e7f9a1b3c5d7e9f"
            }
        })),
        (status = 404, body = ErrorBody, description = "`not_found`: no such incident, or one on a \
site the caller does not read"),
    ))]
fn incident() {}

/// List remediation events
///
/// The remediation audit log: the events of every remediation incident on
/// the sites the caller reads, newest first, across incidents. Each event
/// carries its incident's customer, site, server, metric and instance.
/// `payload` leaves out the context sent to the AI (`context.rendered`)
/// and command output (`stdout`, `stderr`); read the incident for them
/// and to verify its chain.
///
/// Needs any role and the key area "remediation". Filters are ANDed; a
/// filter outside the caller's sites gives an empty list. Paginated with
/// `limit` and `cursor`; unknown parameters and bad values are 400.
#[utoipa::path(get, path = "/api/v1/remediation/events", tag = "remediation",
    params(remediation_events::ListParams),
    responses((status = 200, description = "The events", body = remediation_events::EventList, example = json!({
        "events": [{
            "incident_id": "5d6e7f80-9a1b-4c2d-8e3f-4a5b6c7d8e9f",
            "seq": 2,
            "kind": "approval",
            "actor_kind": "user",
            "actor_id": "9a1b2c3d-4e5f-4a6b-8c7d-6e5f4a3b2c1d",
            "actor_name": "alice",
            "payload": {"command": "systemctl restart nginx"},
            "created_at": "2026-11-05T03:13:02Z",
            "hash": "a1c3e5f7092b4d6f8a0c2e4f6a8b0d2e4f6a8c0e2a4c6e8f0b2d4f6a8c0e2f4a",
            "customer_id": "0b8e6f3a-1c2d-4e5f-9a8b-7c6d5e4f3a2b",
            "customer_name": "acme",
            "site_id": "3c4d5e6f-7a8b-4c9d-8e0f-1a2b3c4d5e6f",
            "site_name": "paris",
            "server_id": "1e2f3a4b-5c6d-4e7f-8a9b-0c1d2e3f4a5b",
            "hostname": "web-1",
            "metric": "monitorflow_service_active",
            "instance": "nginx"
        }],
        "next_cursor": "MjAyNi0xMS0wNVQwMzoxMzowMlo"
    }))))]
fn list_events() {}
