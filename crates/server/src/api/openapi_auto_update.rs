//! The OpenAPI description of the progress of automatic updates of agents
//! and relays (ADR-0055). Merged into the document by
//! [`super::openapi::build`]; the router in `mod.rs` serves the routes.

#![allow(dead_code)] // The functions exist for their `#[utoipa::path]`.

use utoipa::OpenApi;

use super::auto_update;
use super::openapi::{Common, ErrorBody};
use crate::auto_update::policy::{Settings, SitePhase};
use crate::auto_update::store::{Kind, Rollout, RolloutState};
use crate::auto_update::PeerState;

#[derive(OpenApi)]
#[openapi(
    paths(auto_update_status, auto_update_peers, resume_rollout),
    components(schemas(
        Settings, SitePhase, Kind, Rollout, RolloutState, auto_update::KindView,
        auto_update::Totals, auto_update::SiteView, PeerState, auto_update::PeerList,
        auto_update::PeerView, auto_update::HistoryRollout, auto_update::Outcome,
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

/// List the agents or relays behind a total
///
/// The servers (for `agent`) or relays (for `relay`) counted in one of the
/// totals of `GET /api/v1/auto-update`, classified the same way, so `total` equals
/// that count: `up_to_date`, `waiting`, `pending` (an update in progress)
/// or `failed` (since the rollout started or was resumed). With `site_id`,
/// only that site's. Failed ones come newest first with the error their
/// update reported; the others by customer, site and name. A waiting agent
/// with `waiting_for_relay` goes once its relay is on the target.
///
/// Needs a global administrator role and the key area "updates".
#[utoipa::path(get, path = "/api/v1/auto-update/{kind}/peers", tag = "automatic updates",
    params(
        ("kind" = Kind, Path, description = "`agent` or `relay`; anything else is 404"),
        auto_update::PeersParams,
    ),
    responses(
        (status = 200, description = "One page of the peers in that state", body = auto_update::PeerList, example = json!({
            "target": "0.3.1",
            "total": 1,
            "peers": [{
                "id": "5e6f7a8b-9c0d-4e1f-8a2b-3c4d5e6f7a8b",
                "name": "web-01",
                "site_id": "3c4d5e6f-7a8b-4c9d-8e0f-1a2b3c4d5e6f",
                "site_name": "paris",
                "customer_id": "1a2b3c4d-5e6f-4a7b-8c9d-0e1f2a3b4c5d",
                "customer_name": "acme",
                "version": "0.3.0",
                "state": "failed",
                "waiting_for_relay": false,
                "error": "the new version did not connect; rolled back",
                "finished_at": "2026-11-10T08:12:40Z"
            }]
        })),
        (status = 400, body = ErrorBody, description = "`invalid`: a missing or unknown `state`, \
a `site_id` that is not a UUID, or `limit` outside 1 to 500"),
        (status = 403, body = ErrorBody, description = "`forbidden`: not a global administrator. \
`api_key_scope`: the key lacks the area"),
        (status = 404, body = ErrorBody, description = "`not_found`: `kind` is not `agent` or `relay`"),
    ))]
fn auto_update_peers() {}

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
