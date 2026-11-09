//! Follow-up (ADR-0082): who is told next when nobody acknowledges a
//! critical incident, and the links that acknowledge from a message.
//!
//! A follow-up is managed like a channel or a schedule of the same place
//! (ADR-0078): by the administrators of what it covers. Its steps may name
//! only schedules and channels the editor manages that cover the whole
//! place, and people who could be on call for it (ADR-0081). The link in a
//! personal message acknowledges its incidents as that person, without a
//! session: the page that opens it asks for a click, and a GET does nothing.

use std::collections::HashMap;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{PgConnection, PgExecutor, PgPool, Row};
use uuid::Uuid;

use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::incidents::IncidentView;
use super::notifications::{manages, require_administrator};
use super::on_call::{candidates, coverage_of};
use super::one_time::OneTimeToken;
use super::sensors::{body, parse_id};
use super::{ApiError, ApiState};
use crate::notify::follow_up::{
    self, default_steps, label, minutes, policy_for, recipients, Policy, Step, DETAILS,
    MAX_AFTER_MINUTES, MAX_STEPS,
};
use crate::notify::message::event_text;
use crate::notify::on_call::{self, Directory, Schedule};
use crate::notify::{self, Channel, Coverage};

const LINK_GONE: &str =
    "This link no longer works: it expired. Open the alert in MonitorFlow to acknowledge it.";

/// Where an incident is in its follow-up.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct FollowUpView {
    /// `waiting` for the next step at `next_at`, `silenced` until a silence
    /// ends then waiting, or `stopped`.
    pub state: &'static str,
    /// When stopped: `acknowledged`, `resolved`, `not_critical`, or
    /// `finished` once every step was told.
    pub stop_reason: Option<String>,
    /// Steps told so far, and how many there are now.
    pub step: u32,
    pub steps: u32,
    /// When the first message went out.
    pub started_at: DateTime<Utc>,
    /// When the next step is told if nobody acknowledges.
    pub next_at: Option<DateTime<Utc>>,
    /// Who it would tell then, people first; empty when nobody can be.
    pub next: Vec<String>,
    /// Whose follow-up applies: `default`, `everything`, `customer` or
    /// `site`.
    pub policy: &'static str,
}

fn policy_kind(policy: Option<&Policy>) -> &'static str {
    match policy.map(|p| p.coverage) {
        None => "default",
        Some(Coverage::Everything) => "everything",
        Some(Coverage::Customer(_)) => "customer",
        Some(Coverage::Site(_)) => "site",
    }
}

/// `(started_at, step, next_at, stop_reason)` of `incident_follow_ups`.
type StoredFollowUp = (DateTime<Utc>, i32, Option<DateTime<Utc>>, Option<String>);

/// The follow-up of `incident`, `None` when none started.
pub async fn follow_up_view(
    pool: &PgPool,
    incident: &IncidentView,
) -> Result<Option<FollowUpView>, ApiError> {
    let row: Option<StoredFollowUp> = sqlx::query_as(
        "SELECT started_at, step, next_at, stop_reason FROM incident_follow_ups
             WHERE incident_id = $1",
    )
    .bind(incident.id)
    .fetch_optional(pool)
    .await?;
    let Some((started_at, step, next_at, stop_reason)) = row else {
        return Ok(None);
    };
    let mut conn = pool.acquire().await?;
    let policies = follow_up::policies(&mut *conn).await?;
    let place = (Some(incident.customer_id), Some(incident.site_id));
    let policy = policy_for(&policies, place.0, place.1);
    let steps = policy.map_or_else(default_steps, |p| p.steps.clone());
    let told = usize::try_from(step).unwrap_or(0);
    let mut view = FollowUpView {
        state: "stopped",
        stop_reason,
        step: u32::try_from(step).unwrap_or(0),
        steps: u32::try_from(steps.len()).unwrap_or(0),
        started_at,
        next_at: None,
        next: Vec::new(),
        policy: policy_kind(policy),
    };
    if let (Some(at), Some(next)) = (next_at, steps.get(told)) {
        // A silence puts the step off until it ends, plus the step's wait.
        let (state, at) = match incident.silenced_until {
            Some(end) => ("silenced", at.max(end + minutes(next.after_minutes))),
            None => ("waiting", at),
        };
        let directory = Directory::load(&mut conn).await?;
        let schedules = on_call::schedules(&mut conn, at, None).await?;
        let channels = notify::channels(&mut *conn).await?;
        let who = recipients(next, &schedules, &directory, &channels, place, at);
        view.state = state;
        view.next_at = Some(at);
        view.next = follow_up::names(&who, &directory, &channels);
    }
    Ok(Some(view))
}

/// Something a step names, with its name now; `null` once removed.
#[derive(Debug, Clone, Serialize)]
pub struct Named {
    pub id: Uuid,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StepView {
    pub after_minutes: u32,
    pub on_call: Vec<Named>,
    pub users: Vec<Named>,
    pub channels: Vec<Named>,
    pub administrators: bool,
}

/// A follow-up as the screen shows it.
#[derive(Debug, Clone, Serialize)]
pub struct PolicyView {
    pub id: Uuid,
    /// The customer it covers, or the customer of its site; `null` for
    /// everything.
    pub customer_id: Option<Uuid>,
    pub customer_name: Option<String>,
    pub site_id: Option<Uuid>,
    pub site_name: Option<String>,
    pub steps: Vec<StepView>,
    pub updated_at: DateTime<Utc>,
}

/// A schedule or channel a step may name.
#[derive(Debug, Clone, Serialize)]
pub struct Choice {
    pub id: Uuid,
    pub name: String,
    pub customer_id: Option<Uuid>,
    pub site_id: Option<Uuid>,
}

/// Body of `GET /api/follow-up/policies`.
#[derive(Debug, Serialize)]
pub struct PolicyList {
    /// Those the user manages.
    pub policies: Vec<PolicyView>,
    /// What applies where nobody set one.
    pub default_steps: Vec<StepView>,
    /// Schedules and channels the user manages, for the form.
    pub schedules: Vec<Choice>,
    pub channels: Vec<Choice>,
}

/// Body of `PUT /api/follow-up/policies`: the steps of one place, replacing
/// any.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyRequest {
    #[serde(default)]
    pub customer_id: Option<Uuid>,
    #[serde(default)]
    pub site_id: Option<Uuid>,
    /// In order; empty tells nobody after the first message.
    pub steps: Vec<Step>,
}

/// Names of what steps name.
struct Names {
    schedules: HashMap<Uuid, String>,
    users: HashMap<Uuid, String>,
    channels: HashMap<Uuid, String>,
}

async fn names(conn: &mut PgConnection) -> Result<Names, sqlx::Error> {
    let schedules = sqlx::query_as("SELECT id, name FROM on_call_schedules")
        .fetch_all(&mut *conn)
        .await?
        .into_iter()
        .collect();
    let users = sqlx::query_as("SELECT id, username FROM users")
        .fetch_all(&mut *conn)
        .await?
        .into_iter()
        .collect();
    let channels = notify::channels(&mut *conn)
        .await?
        .iter()
        .map(|c| (c.id, label(&c.destination)))
        .collect();
    Ok(Names {
        schedules,
        users,
        channels,
    })
}

fn step_view(step: &Step, names: &Names) -> StepView {
    let named = |ids: &[Uuid], map: &HashMap<Uuid, String>| {
        ids.iter()
            .map(|id| Named {
                id: *id,
                name: map.get(id).cloned(),
            })
            .collect()
    };
    StepView {
        after_minutes: step.after_minutes,
        on_call: named(&step.on_call, &names.schedules),
        users: named(&step.users, &names.users),
        channels: named(&step.channels, &names.channels),
        administrators: step.administrators,
    }
}

/// The views of the follow-ups `user` manages, or only `only`.
async fn views(
    conn: &mut PgConnection,
    user: &CurrentUser,
    only: Option<Uuid>,
) -> Result<Vec<PolicyView>, ApiError> {
    let names = names(&mut *conn).await?;
    let rows = sqlx::query(
        "SELECT f.id, f.site_id, coalesce(f.customer_id, s.customer_id) AS customer_id,
                c.name AS customer_name, s.name AS site_name, f.steps, f.updated_at,
                f.customer_id AS own_customer
         FROM follow_up_policies f
         LEFT JOIN sites s ON s.id = f.site_id
         LEFT JOIN customers c ON c.id = coalesce(f.customer_id, s.customer_id)
         WHERE $1::uuid IS NULL OR f.id = $1
         ORDER BY c.name NULLS FIRST, s.name NULLS FIRST",
    )
    .bind(only)
    .fetch_all(&mut *conn)
    .await?;
    let mut out = Vec::new();
    for row in rows {
        let site_id: Option<Uuid> = row.try_get("site_id")?;
        let Some(coverage) = Coverage::from_columns(row.try_get("own_customer")?, site_id) else {
            continue;
        };
        if !manages(user, coverage) {
            continue;
        }
        let steps: Vec<Step> = serde_json::from_value(row.try_get("steps")?).unwrap_or_default();
        out.push(PolicyView {
            id: row.try_get("id")?,
            customer_id: row.try_get("customer_id")?,
            customer_name: row.try_get("customer_name")?,
            site_id,
            site_name: row.try_get("site_name")?,
            steps: steps.iter().map(|s| step_view(s, &names)).collect(),
            updated_at: row.try_get("updated_at")?,
        });
    }
    Ok(out)
}

/// `GET /api/follow-up/policies`: administrators; those of what they
/// administer, the default, and what their steps may name.
pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<PolicyList>, ApiError> {
    require_administrator(&user)?;
    let mut conn = state.pool.acquire().await?;
    let policies = views(&mut conn, &user, None).await?;
    let names = names(&mut conn).await?;
    let schedules = on_call::schedules(&mut conn, Utc::now(), None)
        .await?
        .into_iter()
        .filter(|s| manages(&user, s.coverage))
        .map(|s| choice(s.id, &names.schedules, s.coverage))
        .collect();
    let channels = notify::channels(&mut *conn)
        .await?
        .into_iter()
        .filter(|c| manages(&user, c.coverage))
        .map(|c| choice(c.id, &names.channels, c.coverage))
        .collect();
    Ok(Json(PolicyList {
        policies,
        default_steps: default_steps()
            .iter()
            .map(|s| step_view(s, &names))
            .collect(),
        schedules,
        channels,
    }))
}

fn choice(id: Uuid, names: &HashMap<Uuid, String>, coverage: Coverage) -> Choice {
    let (customer_id, site_id) = coverage.columns();
    Choice {
        id,
        name: names.get(&id).cloned().unwrap_or_default(),
        customer_id,
        site_id,
    }
}

/// Checks `steps` for `coverage`; `current` are the steps saved before,
/// whose people may stay though they could not be added now.
async fn validate(
    conn: &mut PgConnection,
    user: &CurrentUser,
    coverage: Coverage,
    steps: &[Step],
    current: &[Step],
) -> Result<(), ApiError> {
    if steps.len() > MAX_STEPS {
        return Err(ApiError::Invalid(format!("At most {MAX_STEPS} steps.")));
    }
    let directory = Directory::load(&mut *conn).await?;
    match coverage {
        Coverage::Customer(id) if !directory.site_customer.values().any(|c| *c == id) => {
            let exists: Option<Uuid> = sqlx::query_scalar("SELECT id FROM customers WHERE id = $1")
                .bind(id)
                .fetch_optional(&mut *conn)
                .await?;
            if exists.is_none() {
                return Err(ApiError::Invalid("unknown customer or site".into()));
            }
        }
        Coverage::Site(id) if !directory.site_customer.contains_key(&id) => {
            return Err(ApiError::Invalid("unknown customer or site".into()));
        }
        _ => {}
    }
    let schedules: Vec<Schedule> = on_call::schedules(&mut *conn, Utc::now(), None).await?;
    let channels: Vec<Channel> = notify::channels(&mut *conn).await?;
    let people = candidates(&mut *conn, user, coverage).await?;
    for (index, step) in steps.iter().enumerate() {
        let n = index + 1;
        if !(1..=MAX_AFTER_MINUTES).contains(&step.after_minutes) {
            return Err(ApiError::Invalid(format!(
                "Step {n}: wait between 1 and {MAX_AFTER_MINUTES} minutes."
            )));
        }
        if step.on_call.is_empty()
            && step.users.is_empty()
            && step.channels.is_empty()
            && !step.administrators
        {
            return Err(ApiError::Invalid(format!(
                "Step {n} tells nobody: choose who it tells, or remove it."
            )));
        }
        for id in &step.on_call {
            let found = schedules.iter().find(|s| s.id == *id);
            if !found.is_some_and(|s| {
                manages(user, s.coverage) && directory.contains(s.coverage, coverage)
            }) {
                return Err(ApiError::Invalid(format!(
                    "Step {n}: choose schedules that cover all of this place."
                )));
            }
        }
        for id in &step.channels {
            let found = channels.iter().find(|c| c.id == *id);
            if !found.is_some_and(|c| {
                manages(user, c.coverage) && directory.contains(c.coverage, coverage)
            }) {
                return Err(ApiError::Invalid(format!(
                    "Step {n}: choose channels that cover all of this place."
                )));
            }
        }
        for id in &step.users {
            let kept = current.iter().any(|s| s.users.contains(id));
            if !kept && !people.iter().any(|p| p.id == *id) {
                return Err(ApiError::Invalid(format!(
                    "Step {n}: only people who see all of this place can be told."
                )));
            }
        }
    }
    Ok(())
}

/// Each list once, in order.
fn normalized(steps: Vec<Step>) -> Vec<Step> {
    let once = |mut ids: Vec<Uuid>| {
        let mut seen = std::collections::HashSet::new();
        ids.retain(|id| seen.insert(*id));
        ids
    };
    steps
        .into_iter()
        .map(|s| Step {
            on_call: once(s.on_call),
            users: once(s.users),
            channels: once(s.channels),
            ..s
        })
        .collect()
}

async fn record(
    db: impl PgExecutor<'_>,
    user: &CurrentUser,
    action: &str,
    id: Uuid,
    details: Value,
) -> Result<(), ApiError> {
    let actor = user.id.to_string();
    let target = id.to_string();
    audit::record(
        db,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action,
            target: Some(("follow_up_policy", &target)),
            details,
        },
    )
    .await?;
    Ok(())
}

/// `PUT /api/follow-up/policies`: administrators of the place set its
/// steps, creating its follow-up or replacing it. 403 for a place they do
/// not manage, before any lookup. Audited as `follow_up.policy_create` or
/// `follow_up.policy_update`. Running follow-ups take the new steps at
/// their next one.
pub async fn save(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<PolicyRequest>, JsonRejection>,
) -> Result<Json<PolicyView>, ApiError> {
    require_administrator(&user)?;
    let request = body(request)?;
    let coverage = coverage_of(&user, request.customer_id, request.site_id)?;
    let steps = normalized(request.steps);
    let (customer_id, site_id) = coverage.columns();
    let mut tx = state.pool.begin().await?;
    let current: Option<(Uuid, Value)> = sqlx::query_as(
        "SELECT id, steps FROM follow_up_policies
         WHERE customer_id IS NOT DISTINCT FROM $1 AND site_id IS NOT DISTINCT FROM $2
         FOR UPDATE",
    )
    .bind(customer_id)
    .bind(site_id)
    .fetch_optional(&mut *tx)
    .await?;
    let saved: Vec<Step> = current
        .as_ref()
        .and_then(|(_, steps)| serde_json::from_value(steps.clone()).ok())
        .unwrap_or_default();
    validate(&mut tx, &user, coverage, &steps, &saved).await?;
    let value = serde_json::to_value(&steps).map_err(|e| ApiError::Internal(e.to_string()))?;
    let (id, action) = match current {
        Some((id, _)) => {
            sqlx::query(
                "UPDATE follow_up_policies SET steps = $2, updated_by = $3, updated_at = now()
                 WHERE id = $1",
            )
            .bind(id)
            .bind(&value)
            .bind(user.id)
            .execute(&mut *tx)
            .await?;
            (id, "follow_up.policy_update")
        }
        None => {
            let id = sqlx::query_scalar(
                "INSERT INTO follow_up_policies (customer_id, site_id, steps, updated_by)
                 VALUES ($1, $2, $3, $4) RETURNING id",
            )
            .bind(customer_id)
            .bind(site_id)
            .bind(&value)
            .bind(user.id)
            .fetch_one(&mut *tx)
            .await?;
            (id, "follow_up.policy_create")
        }
    };
    record(
        &mut *tx,
        &user,
        action,
        id,
        json!({ "customer_id": customer_id, "site_id": site_id, "steps": value }),
    )
    .await?;
    let view = views(&mut tx, &user, Some(id))
        .await?
        .into_iter()
        .next()
        .ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(Json(view))
}

/// `DELETE /api/follow-up/policies/{id}`: administrators of the place; the
/// default applies again. 404 for one they do not manage; audited as
/// `follow_up.policy_delete`.
pub async fn delete(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_administrator(&user)?;
    let id = parse_id(&id)?;
    let mut tx = state.pool.begin().await?;
    let found: Option<(Option<Uuid>, Option<Uuid>)> = sqlx::query_as(
        "SELECT customer_id, site_id FROM follow_up_policies WHERE id = $1 FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let (customer_id, site_id) = found
        .filter(|(c, s)| Coverage::from_columns(*c, *s).is_some_and(|c| manages(&user, c)))
        .ok_or(ApiError::NotFound)?;
    sqlx::query("DELETE FROM follow_up_policies WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    record(
        &mut *tx,
        &user,
        "follow_up.policy_delete",
        id,
        json!({ "customer_id": customer_id, "site_id": site_id }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Body of the link endpoints.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkRequest {
    pub token: String,
}

/// An incident a link acknowledges.
#[derive(Debug, Serialize)]
pub struct LinkIncident {
    pub id: Uuid,
    /// The incident in one line, as the message said it.
    pub text: String,
    /// `open` or `resolved`.
    pub state: String,
    pub acknowledged_by: Option<String>,
    pub acknowledged_at: Option<DateTime<Utc>>,
}

/// What a link acknowledges, and as whom.
#[derive(Debug, Serialize)]
pub struct LinkView {
    pub username: String,
    pub expires_at: DateTime<Utc>,
    /// When it was used; `null` before.
    pub used_at: Option<DateTime<Utc>>,
    pub incidents: Vec<LinkIncident>,
}

/// A live link: `(user, username, incidents, expires_at, used_at)`.
type Live = (
    Uuid,
    String,
    Vec<Uuid>,
    DateTime<Utc>,
    Option<DateTime<Utc>>,
);

async fn live(db: impl PgExecutor<'_>, token: &str, lock: bool) -> Result<Live, ApiError> {
    let token = OneTimeToken::parse(token).ok_or(ApiError::Gone(LINK_GONE.into()))?;
    let lock = if lock { "FOR UPDATE OF l" } else { "" };
    // SQL-safe: interpolates only one of two constants.
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT l.user_id, u.username, l.incident_ids, l.expires_at, l.used_at
         FROM acknowledge_links l JOIN users u ON u.id = l.user_id
         WHERE l.token_hash = $1 AND l.expires_at > now() AND u.disabled_at IS NULL {lock}"
    )))
    .bind(token.hash())
    .fetch_optional(db)
    .await?
    .ok_or(ApiError::Gone(LINK_GONE.into()))
}

/// Of `ids`, the incidents `user` still sees, and those they still may
/// acknowledge, by their roles now.
async fn rights(
    conn: &mut PgConnection,
    user: Uuid,
    ids: &[Uuid],
) -> Result<(Vec<Uuid>, Vec<Uuid>), ApiError> {
    let directory = Directory::load(&mut *conn).await?;
    let person = directory
        .people
        .get(&user)
        .ok_or(ApiError::Gone(LINK_GONE.into()))?;
    let sites: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT i.id, si.id FROM incidents i
         LEFT JOIN servers s ON s.id = i.server_id
         LEFT JOIN relays r ON r.id = i.relay_id
         JOIN sites si ON si.id = coalesce(s.site_id, r.site_id)
         WHERE i.id = ANY($1)",
    )
    .bind(ids)
    .fetch_all(&mut *conn)
    .await?;
    let place = |site: Uuid| Coverage::Site(site);
    let visible = sites
        .iter()
        .filter(|(_, site)| directory.sees(person, place(*site)))
        .map(|(id, _)| *id)
        .collect();
    let allowed = sites
        .iter()
        .filter(|(_, site)| directory.acknowledges(person, place(*site)))
        .map(|(id, _)| *id)
        .collect();
    Ok((visible, allowed))
}

/// What `live` acknowledges, of the incidents in `visible`: a link shows
/// nothing its person can no longer see.
async fn link_view(
    db: impl PgExecutor<'_>,
    (_, username, _, expires_at, used_at): Live,
    visible: &[Uuid],
) -> Result<LinkView, ApiError> {
    // SQL-safe: interpolates only the constant DETAILS.
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT i.id, i.state, a.username AS acknowledged_by, a.acknowledged_at,
                {DETAILS} AS details
         FROM incidents i
         CROSS JOIN LATERAL notification_place(i.server_id, i.relay_id) p
         LEFT JOIN incident_acknowledgements a ON a.incident_id = i.id
         WHERE i.id = ANY($1)
         ORDER BY i.opened_at, i.id"
    )))
    .bind(visible)
    .fetch_all(db)
    .await?;
    let mut incidents = Vec::with_capacity(rows.len());
    for row in rows {
        let state: String = row.try_get("state")?;
        let event = notify::Event {
            id: 0,
            event: if state == "open" {
                "incident.opened"
            } else {
                "incident.resolved"
            }
            .into(),
            customer_id: None,
            site_id: None,
            details: row.try_get("details")?,
            created_at: Utc::now(),
        };
        incidents.push(LinkIncident {
            id: row.try_get("id")?,
            text: event_text(&event),
            state,
            acknowledged_by: row.try_get("acknowledged_by")?,
            acknowledged_at: row.try_get("acknowledged_at")?,
        });
    }
    Ok(LinkView {
        username,
        expires_at,
        used_at,
        incidents,
    })
}

/// `POST /api/acknowledge-link`, without a session: what a link
/// acknowledges and as whom. Changes nothing, so a mail scanner that opens
/// the page acknowledges nothing. 410 once expired or its person disabled.
pub async fn inspect_link(
    State(state): State<ApiState>,
    request: Result<Json<LinkRequest>, JsonRejection>,
) -> Result<Json<LinkView>, ApiError> {
    let Json(request) = request.map_err(|_| ApiError::BadRequest)?;
    let mut conn = state.pool.acquire().await?;
    let live = live(&mut *conn, &request.token, false).await?;
    let (visible, _) = rights(&mut conn, live.0, &live.2).await?;
    Ok(Json(link_view(&mut *conn, live, &visible).await?))
}

/// `POST /api/acknowledge-link/use`, without a session: acknowledges the
/// link's incidents as its person, those still open and not acknowledged
/// that they still may acknowledge: an operator or above of the incident's
/// site. 403 when they may acknowledge none, 410 once expired or its person
/// disabled. Using it again changes nothing. Audited as
/// `incident.acknowledge` with `via: link`.
pub async fn use_link(
    State(state): State<ApiState>,
    request: Result<Json<LinkRequest>, JsonRejection>,
) -> Result<Json<LinkView>, ApiError> {
    let Json(request) = request.map_err(|_| ApiError::BadRequest)?;
    let mut tx = state.pool.begin().await?;
    let found = live(&mut *tx, &request.token, true).await?;
    let (user, username) = (found.0, found.1.clone());
    let (visible, allowed) = rights(&mut tx, user, &found.2).await?;
    if allowed.is_empty() {
        return Err(ApiError::Forbidden);
    }
    let now = Utc::now();
    let done = follow_up::acknowledge(&mut tx, &allowed, (user, &username), "link", now).await?;
    let actor = user.to_string();
    for incident in &done {
        let target = incident.to_string();
        audit::record(
            &mut *tx,
            Event {
                actor_kind: "user",
                actor_id: &actor,
                action: "incident.acknowledge",
                target: Some(("incident", &target)),
                details: json!({ "via": "link" }),
            },
        )
        .await?;
    }
    let token = OneTimeToken::parse(&request.token).ok_or(ApiError::Gone(LINK_GONE.into()))?;
    sqlx::query(
        "UPDATE acknowledge_links SET used_at = coalesce(used_at, $2) WHERE token_hash = $1",
    )
    .bind(token.hash())
    .bind(now)
    .execute(&mut *tx)
    .await?;
    let found = live(&mut *tx, &request.token, false).await?;
    let view = link_view(&mut *tx, found, &visible).await?;
    tx.commit().await?;
    Ok(Json(view))
}
