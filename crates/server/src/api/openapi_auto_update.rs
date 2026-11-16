//! The OpenAPI description of the progress of automatic updates of agents
//! and relays (ADR-0055). Merged into the document by
//! [`super::openapi::build`]; the router in `mod.rs` serves the routes.

#![allow(dead_code)] // The functions exist for their `#[utoipa::path]`.

use utoipa::OpenApi;

use super::auto_update;
use super::openapi::{Common, ErrorBody};
use crate::auto_update::policy::{Settings, SitePhase};
use crate::auto_update::store::{Kind, Rollout, RolloutState};

#[derive(OpenApi)]
#[openapi(
    paths(auto_update_status, resume_rollout),
    components(schemas(
        Settings, SitePhase, Kind, Rollout, RolloutState, auto_update::KindView,
        auto_update::Totals, auto_update::SiteView, auto_update::HistoryRollout,
        auto_update::Outcome,
    )),
    modifiers(&Common),
    tags(
        (name = "automatic updates", description = "Agents and relays update themselves to the \
newest stable release the central server holds, never newer than the server: per site, one server \
first, then a quarter of the site at a time, never the host's packages and never a restart of the \
host. A rollout halts everywhere after too many failures until a global administrator resumes it. \
These routes need a global administrator and the key area \"updates\"; the settings themselves are \
under `/settings/auto-update`."),
    )
)]
pub struct Doc;

/// Read the progress of automatic updates
///
/// The settings as they apply, the central server's version (agents and
/// relays never move past it), and for agents and for relays: the newest
/// stable release held, the `target` they move to, its rollout, totals,
/// each site's phase and totals, and the last 10 rollouts with their
/// `outcome`. Relays are updated before the agents behind them.
///
/// MonitorFlow sends each server or relay that needs it a signed
/// `self_update` command, with no person's approval: one per site first
/// (`canary`), then a quarter of the site at a time (`rolling`). A site
/// whose first update failed is `blocked` for that version. Each server is
/// tried once per version. Only the agent or relay restarts, never the
/// host, and its watchdog rolls back a version that does not connect.
///
/// Needs a global administrator role and the key area "updates".
#[utoipa::path(get, path = "/api/v1/auto-update", tag = "automatic updates",
    responses(
        (status = 200, description = "The settings and each rollout", body = auto_update::StatusView, example = json!({
            "settings": {
                "enabled": true, "enabled_locked": false, "paused": false, "failure_threshold": 2
            },
            "server_version": "0.3.1",
            "agents": {
                "latest": "0.3.1",
                "target": "0.3.1",
                "rollout": {
                    "kind": "agent", "version": "0.3.1", "state": "rolling",
                    "started_at": "2026-11-10T08:00:00Z", "halted_at": null, "reason": null,
                    "counted_from": "2026-11-10T08:00:00Z"
                },
                "totals": {
                    "up_to_date": 3, "waiting": 4, "waiting_for_relay": 0, "pending": 1,
                    "succeeded": 3, "failed": 0
                },
                "sites": [{
                    "id": "3c4d5e6f-7a8b-4c9d-8e0f-1a2b3c4d5e6f",
                    "name": "paris",
                    "customer_name": "acme",
                    "auto_update": null,
                    "enabled": true,
                    "phase": "rolling",
                    "totals": {
                        "up_to_date": 3, "waiting": 4, "waiting_for_relay": 0, "pending": 1,
                        "succeeded": 3, "failed": 0
                    }
                }],
                "history": [{
                    "kind": "agent", "version": "0.3.1", "state": "rolling",
                    "started_at": "2026-11-10T08:00:00Z", "halted_at": null, "reason": null,
                    "counted_from": "2026-11-10T08:00:00Z", "outcome": "rolling"
                }, {
                    "kind": "agent", "version": "0.3.0", "state": "rolling",
                    "started_at": "2026-11-02T08:00:00Z", "halted_at": null, "reason": null,
                    "counted_from": "2026-11-02T08:00:00Z", "outcome": "superseded"
                }]
            },
            "relays": {
                "latest": "0.3.1",
                "target": "0.3.1",
                "rollout": null,
                "totals": {
                    "up_to_date": 1, "waiting": 0, "waiting_for_relay": 0, "pending": 0,
                    "succeeded": 0, "failed": 0
                },
                "sites": [{
                    "id": "3c4d5e6f-7a8b-4c9d-8e0f-1a2b3c4d5e6f",
                    "name": "paris",
                    "customer_name": "acme",
                    "auto_update": null,
                    "enabled": true,
                    "phase": null,
                    "totals": {
                        "up_to_date": 1, "waiting": 0, "waiting_for_relay": 0, "pending": 0,
                        "succeeded": 0, "failed": 0
                    }
                }],
                "history": []
            }
        })),
        (status = 403, body = ErrorBody, description = "`forbidden`: not a global administrator. \
`api_key_scope`: the key lacks the area"),
    ))]
fn auto_update_status() {}

/// Resume a halted rollout
///
/// A rollout halts everywhere once `failure_threshold` of its updates
/// failed. Resuming it, once the cause is fixed, counts failures again
/// from now, and each blocked site gets a new first server among those
/// not tried yet; `self_update` commands are sent again at the next pass
/// (every 5 minutes by default), unless automatic updates are paused.
///
/// Needs a global administrator role and the key area "updates". Audited
/// as `auto_update.resume`. Resuming again is 409.
#[utoipa::path(post, path = "/api/v1/auto-update/rollouts/{kind}/{version}/resume",
    tag = "automatic updates",
    params(
        ("kind" = Kind, Path, description = "`agent` or `relay`; anything else is 404"),
        ("version" = String, Path, description = "The rollout's version, like `0.3.1`"),
    ),
    responses(
        (status = 200, description = "The rollout, rolling again", body = Rollout, example = json!({
            "kind": "agent", "version": "0.3.1", "state": "rolling",
            "started_at": "2026-11-10T08:00:00Z", "halted_at": null, "reason": null,
            "counted_from": "2026-11-12T09:41:17Z"
        })),
        (status = 403, body = ErrorBody, description = "`forbidden`: not a global administrator. \
`api_key_scope`: the key lacks the area"),
        (status = 404, body = ErrorBody, description = "`not_found`: `kind` is not `agent` or `relay`"),
        (status = 409, body = ErrorBody, description = "`conflict`: `this rollout is not halted`, \
also for a version that has no rollout"),
    ))]
fn resume_rollout() {}
