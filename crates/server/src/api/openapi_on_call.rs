//! On call (ADR-0081) and follow-up (ADR-0082) in the OpenAPI document:
//! merged into it by [`super::build`].

use serde_json::json;
use utoipa::OpenApi;

use super::{Common, ErrorBody};
use crate::api::{follow_up, on_call};

#[derive(OpenApi)]
#[openapi(
    paths(
        list_on_call_schedules,
        create_on_call_schedule,
        update_on_call_schedule,
        delete_on_call_schedule,
        add_on_call_cover,
        remove_on_call_cover,
        list_on_call_people,
        list_follow_ups,
        save_follow_up,
        delete_follow_up,
    ),
    components(schemas(ErrorBody)),
    modifiers(&Common),
    tags(
        (name = "on-call", description = "On call: who is told of each alert of a place in person, besides the \
channels, in turns. A schedule is for everything, one customer or one site; its members take turns, handing \
off every day or every week at a local time in its time zone, and a cover puts someone else on call for a \
period. A gap is a period when the person on call cannot be told (no contact, disabled, removed, no longer \
sees the place); the nearest administrators are told instead. Schedules are managed by the administrators \
of their place, like notification channels: everything takes a global administrator (a key with every \
reach), a customer an administrator of it, a site an administrator of it or of its customer; others get \
404. Only people who see all of the place can be put on a schedule. How each person is reached is theirs \
to set and not in the public API. Every change is in the audit log, with the key's ID. API keys need the \
`alerts` area."),
        (name = "follow-up", description = "Follow-up: who is told next when nobody acknowledges a critical \
incident, step after step, for everything, one customer or one site. A step waits a number of minutes \
without acknowledgement, then tells the person on call of schedules, named people, notification channels \
or the nearest administrators. Where no follow-up is set, the default applies: the nearest administrators, \
after 15 minutes, once. The most precise follow-up of an incident's place applies: its site's, else its \
customer's, else everything's. An incident shows where it is in its follow-up (`GET /api/v1/incidents/{id}`), \
and acknowledging it stops the follow-up. Follow-ups are managed by the administrators of their place, \
like notification channels; others get 404, or 403 when saving. Every change is in the audit log, with \
the key's ID. API keys need the `alerts` area."),
    )
)]
pub(super) struct Doc;

const NOT_ADMIN: &str =
    "Not an administrator: `forbidden`; outside the key's areas: `api_key_scope`";

/// List on-call schedules
///
/// The schedules of what the key's maker administers, by name, each with
/// who is on call now, who is next and the turns of the next 28 days. In
/// full: there are few. Any role may call it; a key whose access is not
/// "Administer" gets an empty list, as being on a schedule is a person's,
/// not a key's.
#[utoipa::path(get, path = "/api/v1/on-call/schedules", tag = "on-call",
    responses(
        (status = 200, description = "The schedules", body = on_call::ScheduleList, example = json!({
            "schedules": [schedule()],
        })),
        (status = 403, description = "No role at all within the key's reach: `forbidden`; `api_key_scope`", body = ErrorBody),
    ))]
fn list_on_call_schedules() {}

/// Add an on-call schedule
///
/// Administrators of its place. The first member is on call from the last
/// hand-off, so someone is on call at once. With `people_per_turn` above
/// 1, that many members are on call together and all are told (ADR-0170). Audited as
/// `on_call.schedule_create`. Not safe to repeat: send `Idempotency-Key`
/// to retry.
#[utoipa::path(post, path = "/api/v1/on-call/schedules", tag = "on-call",
    request_body(content = on_call::ScheduleRequest, example = json!(schedule_request())),
    responses(
        (status = 201, description = "Added", body = on_call::ScheduleView, example = json!(schedule())),
        (status = 400, description = "`invalid`: no name or one over 80 characters, an unknown time zone, a rotation other than `daily` or `weekly`, a time not `HH:MM`, no day for a weekly hand-off, no members, over 50, the same one twice, `people_per_turn` outside 1 to 5 or above the number of members, someone who does not see all of the place, a customer and a site together, an unknown customer or site", body = ErrorBody),
        (status = 403, description = "`forbidden`: not an administrator of its place; `api_key_scope`", body = ErrorBody),
    ))]
fn create_on_call_schedule() {}

/// Change an on-call schedule
///
/// Replaces its name, place, time zone, rotation, hand-off and members.
/// Administrators of its place now and of the one it will have. Whoever
/// is on call by the rotation now stays on call until the next hand-off
/// if still a member; if not, the next one in the new list takes over.
/// Members already on it may stay though they could not be added now.
/// Covers stay. Audited as `on_call.schedule_update`. Safe to repeat.
#[utoipa::path(put, path = "/api/v1/on-call/schedules/{id}", tag = "on-call",
    params(("id" = uuid::Uuid, Path, description = "The schedule")),
    request_body(content = on_call::ScheduleRequest, example = json!(schedule_request())),
    responses(
        (status = 200, description = "Changed", body = on_call::ScheduleView, example = json!(schedule())),
        (status = 400, description = "`invalid`: as when adding one", body = ErrorBody),
        (status = 403, description = "`forbidden`: not an administrator of the place it would have; `api_key_scope`", body = ErrorBody),
        (status = 404, description = "`not_found`: no such schedule, or not one the key's maker administers", body = ErrorBody),
    ))]
fn update_on_call_schedule() {}

/// Remove an on-call schedule
///
/// Administrators of its place. Its covers go with it; follow-up steps
/// that name it show it as removed. Audited as `on_call.schedule_delete`.
#[utoipa::path(delete, path = "/api/v1/on-call/schedules/{id}", tag = "on-call",
    params(("id" = uuid::Uuid, Path, description = "The schedule")),
    responses(
        (status = 204, description = "Removed"),
        (status = 403, description = NOT_ADMIN, body = ErrorBody),
        (status = 404, description = "`not_found`: no such schedule, or not one the key's maker administers", body = ErrorBody),
    ))]
fn delete_on_call_schedule() {}

/// Add a cover
///
/// Puts a member, or anyone who could be one, on call for a period,
/// whatever the rotation says: for a holiday or a swap. With several
/// people on call at once, `seat` says whose place it takes, 0 for the
/// first person of the turn. A start absent or past is now. Administrators of the schedule's place. Answers the
/// schedule. Audited as `on_call.override_create`. Not safe to repeat:
/// send `Idempotency-Key` to retry.
#[utoipa::path(post, path = "/api/v1/on-call/schedules/{id}/overrides", tag = "on-call",
    params(("id" = uuid::Uuid, Path, description = "The schedule")),
    request_body(content = on_call::OverrideRequest, example = json!({
        "user_id": "5e8b2d4f-6a1c-4e3b-9d7f-0c2a4b6d8e1f",
        "seat": 0,
        "starts_at": "2026-12-24T18:00:00Z",
        "ends_at": "2026-12-27T08:00:00Z",
    })),
    responses(
        (status = 201, description = "Added; the schedule", body = on_call::ScheduleView, example = json!(schedule())),
        (status = 400, description = "`invalid`: an end not after the start, over 90 days, a start over a year away, a `seat` not below the schedule's `people_per_turn`, someone who does not see all of the place", body = ErrorBody),
        (status = 403, description = NOT_ADMIN, body = ErrorBody),
        (status = 404, description = "`not_found`: no such schedule, or not one the key's maker administers", body = ErrorBody),
    ))]
fn add_on_call_cover() {}

/// Remove a cover
///
/// The rotation applies again for its period. Administrators of the
/// schedule's place. Audited as `on_call.override_delete`.
#[utoipa::path(delete, path = "/api/v1/on-call/overrides/{id}", tag = "on-call",
    params(("id" = uuid::Uuid, Path, description = "The cover, `id` of a schedule's `overrides`")),
    responses(
        (status = 204, description = "Removed"),
        (status = 403, description = NOT_ADMIN, body = ErrorBody),
        (status = 404, description = "`not_found`: no such cover, or its schedule is not one the key's maker administers", body = ErrorBody),
    ))]
fn remove_on_call_cover() {}

/// List who can be on call
///
/// The people who can be put on a schedule of a place, or named in a
/// follow-up step: enabled users with a role on the site or its customer,
/// and, for a global administrator, those with a global role. Administrators
/// of the place.
#[utoipa::path(get, path = "/api/v1/on-call/people", tag = "on-call", params(on_call::PeopleParams),
    responses(
        (status = 200, description = "The people", body = on_call::PeopleList, example = json!({
            "people": [
                { "id": "5e8b2d4f-6a1c-4e3b-9d7f-0c2a4b6d8e1f", "username": "alice", "has_contact": true },
                { "id": "8c1e3a5b-7d9f-4b2c-a6e8-1f3d5b7a9c2e", "username": "bob", "has_contact": false },
            ],
        })),
        (status = 400, description = "`invalid_request`: an unknown parameter or a bad ID; `invalid`: a customer and a site together", body = ErrorBody),
        (status = 403, description = "`forbidden`: not an administrator of the place; `api_key_scope`", body = ErrorBody),
    ))]
fn list_on_call_people() {}

/// List follow-ups
///
/// The follow-ups of what the key's maker administers, with the default
/// steps and the schedules and channels a step may name. Administrators
/// only.
#[utoipa::path(get, path = "/api/v1/follow-up/policies", tag = "follow-up",
    responses(
        (status = 200, description = "The follow-ups", body = follow_up::PolicyList, example = json!({
            "policies": [follow_up_policy()],
            "default_steps": [{
                "after_minutes": 15, "on_call": [], "users": [], "channels": [], "administrators": true,
            }],
            "schedules": [{
                "id": SCHEDULE, "name": "acme night shift",
                "customer_id": CUSTOMER, "site_id": null,
            }],
            "channels": [{
                "id": "6a1d9e3f-7b4c-4a2e-8f5d-3c1b2a4e6d8f", "name": "a webhook",
                "customer_id": null, "site_id": "9b2e6a41-3c7d-4e8f-a1b2-c3d4e5f60718",
            }],
        })),
        (status = 403, description = NOT_ADMIN, body = ErrorBody),
    ))]
fn list_follow_ups() {}

/// Set the follow-up of a place
///
/// Creates the follow-up of everything, a customer or a site, or replaces
/// its steps. Administrators of the place. Steps may name only schedules
/// and channels the key's maker administers that cover all of the place,
/// and people who could be on call for it; people already named may
/// stay. Each list is kept once, in order. Running follow-ups take the
/// new steps at their next one. Audited as `follow_up.policy_create` or
/// `follow_up.policy_update`. Safe to repeat.
#[utoipa::path(put, path = "/api/v1/follow-up/policies", tag = "follow-up",
    request_body(content = follow_up::PolicyRequest, example = json!({
        "customer_id": CUSTOMER,
        "site_id": null,
        "steps": [
            { "after_minutes": 10, "on_call": [SCHEDULE] },
            { "after_minutes": 20, "users": ["8c1e3a5b-7d9f-4b2c-a6e8-1f3d5b7a9c2e"], "administrators": true },
        ],
    })),
    responses(
        (status = 200, description = "Saved", body = follow_up::PolicyView, example = json!(follow_up_policy())),
        (status = 400, description = "`invalid`: over 10 steps, a wait outside 1 to 1440 minutes, a step that tells nobody, a schedule or channel that does not cover all of the place or is not the maker's to name, someone who does not see all of the place, a customer and a site together, an unknown customer or site", body = ErrorBody),
        (status = 403, description = "`forbidden`: not an administrator of the place; `api_key_scope`", body = ErrorBody),
    ))]
fn save_follow_up() {}

/// Remove a follow-up
///
/// The next follow-up out applies again to its place: its customer's,
/// everything's, or the default. Administrators of the place. Audited as
/// `follow_up.policy_delete`.
#[utoipa::path(delete, path = "/api/v1/follow-up/policies/{id}", tag = "follow-up",
    params(("id" = uuid::Uuid, Path, description = "The follow-up")),
    responses(
        (status = 204, description = "Removed"),
        (status = 403, description = NOT_ADMIN, body = ErrorBody),
        (status = 404, description = "`not_found`: no such follow-up, or not one the key's maker administers", body = ErrorBody),
    ))]
fn delete_follow_up() {}

const CUSTOMER: &str = "4f7d1c2e-0a5b-4c3d-9e8f-1a2b3c4d5e6f";
const SCHEDULE: &str = "3b6d9f1a-2c4e-4a7b-8d0f-5e7a9c1b3d6f";

fn schedule_request() -> serde_json::Value {
    json!({
        "name": "acme night shift",
        "customer_id": CUSTOMER,
        "site_id": null,
        "time_zone": "Europe/Paris",
        "rotation": "weekly",
        "handoff_time": "09:00",
        "handoff_day": 1,
        "members": ["5e8b2d4f-6a1c-4e3b-9d7f-0c2a4b6d8e1f", "8c1e3a5b-7d9f-4b2c-a6e8-1f3d5b7a9c2e"],
        "people_per_turn": 1,
    })
}

fn schedule() -> serde_json::Value {
    let alice = "5e8b2d4f-6a1c-4e3b-9d7f-0c2a4b6d8e1f";
    let bob = "8c1e3a5b-7d9f-4b2c-a6e8-1f3d5b7a9c2e";
    let turn = |starts: &str, ends: &str, id: &str, name: &str, problem: Option<&str>| {
        json!({
            "seat": 0, "starts_at": starts, "ends_at": ends, "user_id": id, "username": name,
            "override_id": null, "problem": problem,
        })
    };
    let now = turn(
        "2026-11-09T08:00:00Z",
        "2026-11-16T08:00:00Z",
        alice,
        "alice",
        None,
    );
    let next = turn(
        "2026-11-16T08:00:00Z",
        "2026-11-23T08:00:00Z",
        bob,
        "bob",
        Some("no_contact"),
    );
    json!({
        "id": SCHEDULE,
        "name": "acme night shift",
        "customer_id": CUSTOMER,
        "customer_name": "acme",
        "site_id": null,
        "site_name": null,
        "time_zone": "Europe/Paris",
        "rotation": "weekly",
        "handoff_time": "09:00",
        "handoff_day": 1,
        "members": [
            { "user_id": alice, "username": "alice", "problem": null },
            { "user_id": bob, "username": "bob", "problem": "no_contact" },
        ],
        "people_per_turn": 1,
        "overrides": [],
        "now": now,
        "next": next,
        "now_all": [now],
        "next_all": [next],
        "turns": [now, next],
        "fallback": ["carol"],
        "mail_host": "smtp.example.com",
        "can_manage": true,
    })
}

fn follow_up_policy() -> serde_json::Value {
    json!({
        "id": "7f2a4c6e-8b1d-4f3a-9c5e-2d4f6a8b0c1e",
        "customer_id": CUSTOMER,
        "customer_name": "acme",
        "site_id": null,
        "site_name": null,
        "steps": [
            {
                "after_minutes": 10, "on_call": [{ "id": SCHEDULE, "name": "acme night shift" }],
                "users": [], "channels": [], "administrators": false,
            },
            {
                "after_minutes": 20, "on_call": [],
                "users": [{ "id": "8c1e3a5b-7d9f-4b2c-a6e8-1f3d5b7a9c2e", "name": "bob" }],
                "channels": [], "administrators": true,
            },
        ],
        "updated_at": "2026-11-10T16:20:00Z",
    })
}
