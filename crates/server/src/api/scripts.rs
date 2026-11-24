//! The script library and its runs (ADR-0189).
//!
//! A script belongs to every customer (`customer_id` null) or to one.
//! Anyone who reads a customer sees its scripts and those of every
//! customer; administrators of everything manage the latter, the
//! customer's administrators (as a whole) the former. Running a script
//! needs its `run_role` on the site of every server it runs on; whether a
//! run waits for approval is decided by `scripts::run`. A run is listed to
//! those who read every server it runs on.

use std::collections::BTreeMap;

use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::types::Json as SqlJson;
use sqlx::PgConnection;
use uuid::Uuid;

use super::access::{authorize, Scope};
use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::roles::Role;
use super::sensors::{body, parse_id};
use super::snmp_library::readable_customers;
use super::{ApiError, ApiState};
use crate::remediation::risk::Tier;
use crate::scripts::run::{self, approver_role, NewRun, RunState, Target, TargetServer};
use crate::scripts::store::{self, Fields, Script};
use crate::scripts::{
    check_body, check_parameters, valid_script_name, values, Interpreter, Parameter,
    MAX_DESCRIPTION, MAX_NOTE,
};

/// Most runs of one listing.
pub const MAX_RUNS: i64 = 200;
/// Furthest a run is scheduled ahead.
const MAX_AHEAD: chrono::Duration = chrono::Duration::days(366);

/// Query string of `GET /api/remediation/scripts`.
#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct ListParams {
    /// Only the scripts this customer can use: its own and those of
    /// every customer.
    pub customer_id: Option<Uuid>,
}

/// A script as listed. Field names and types are part of the API
/// contract: add fields, never rename or remove them.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = ScriptView)]
pub struct ScriptView {
    pub id: Uuid,
    /// `null` for every customer.
    pub customer_id: Option<Uuid>,
    pub name: String,
    pub description: String,
    pub interpreter: Interpreter,
    /// The least role that may run it, on every target's site.
    pub run_role: Role,
    /// Whether the AI may propose it as a fix.
    pub ai_allowed: bool,
    /// The agent kills it after this many seconds.
    pub timeout_s: i32,
    /// The current version.
    pub version: i32,
    pub parameters: Vec<Parameter>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Set once deleted; its runs stay.
    pub deleted_at: Option<DateTime<Utc>>,
    /// Whether the caller may change and delete it. A UX hint: the
    /// routes check again.
    pub can_manage: bool,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = ScriptList)]
pub struct ScriptList {
    pub scripts: Vec<ScriptView>,
}

/// A version of a script.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = ScriptVersionView)]
pub struct VersionView {
    pub version: i32,
    pub body: String,
    pub parameters: Vec<Parameter>,
    /// What changed, in the author's words.
    pub note: String,
    pub created_by: Option<Uuid>,
    pub created_by_name: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// A script with its text and every version, newest first.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = ScriptDetail)]
pub struct ScriptDetail {
    pub script: ScriptView,
    /// The current version's text.
    pub body: String,
    pub versions: Vec<VersionView>,
}

/// Body of `POST /api/remediation/scripts` and `PUT
/// /api/remediation/scripts/{id}`. A change to `body` or `parameters`
/// makes a new version; the other fields change in place.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[schema(as = ScriptRequest)]
#[serde(deny_unknown_fields)]
pub struct ScriptRequest {
    /// The customer it belongs to, `null` or absent for every customer.
    /// Fixed once made.
    #[serde(default)]
    pub customer_id: Option<Uuid>,
    /// `[a-z0-9][a-z0-9_.-]{0,62}`, unique among the customer's live
    /// scripts.
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// `sh` (Linux) or `powershell` (Windows).
    pub interpreter: Interpreter,
    /// `operator`, `approver` or `administrator`.
    pub run_role: Role,
    #[serde(default)]
    pub ai_allowed: bool,
    /// 10 to 3600 seconds; 600 when absent.
    #[serde(default)]
    pub timeout_s: Option<i32>,
    /// At most 64 KiB.
    pub body: String,
    #[serde(default)]
    pub parameters: Vec<Parameter>,
    /// What this version changes, at most 500 characters.
    #[serde(default)]
    pub note: String,
}

/// Body of `POST /api/remediation/scripts/{id}/runs`.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[schema(as = ScriptRunRequest)]
#[serde(deny_unknown_fields)]
pub struct RunRequest {
    /// The servers: `{"kind": "servers", "server_ids": [...]}`, `{"kind":
    /// "site", "site_id"}` or `{"kind": "business_service",
    /// "business_service_id"}`, all of one customer.
    pub target: Target,
    /// Values by parameter name; defaults fill the rest.
    #[serde(default)]
    pub parameters: BTreeMap<String, String>,
    /// An earlier version to run; the current one when absent.
    #[serde(default)]
    pub version: Option<i32>,
    /// When to run it, at most a year ahead; now when absent or past.
    #[serde(default)]
    pub scheduled_at: Option<DateTime<Utc>>,
}

/// How a run's servers are doing.
#[derive(Debug, Default, Serialize, utoipa::ToSchema)]
#[schema(as = ScriptRunCounts)]
pub struct Counts {
    pub servers: i64,
    /// Not finished yet: waiting, queued, sent or running.
    pub pending: i64,
    pub succeeded: i64,
    /// Failed, expired, cancelled, or skipped with an error.
    pub failed: i64,
}

/// A run as listed. Field names and types are part of the API contract.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = ScriptRunView)]
pub struct RunView {
    pub id: Uuid,
    pub script_id: Uuid,
    pub script_name: String,
    pub version: i32,
    pub interpreter: Interpreter,
    pub customer_id: Uuid,
    pub parameters: BTreeMap<String, String>,
    pub target: Target,
    /// The person who asked; `null` for a rule's run.
    pub requested_by: Option<Uuid>,
    pub requested_by_name: Option<String>,
    /// The automation rule that started it, and the event.
    pub rule_id: Option<Uuid>,
    pub rule_name: Option<String>,
    pub trigger: Option<Value>,
    pub state: RunState,
    /// From the risk rules, the highest of its servers.
    pub tier: Tier,
    /// The rule that gave the tier: `{"source": "builtin" | "configured", "id"}`.
    pub rule: Value,
    pub reason: String,
    pub scheduled_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    /// Set while it waits for approval.
    pub expires_at: Option<DateTime<Utc>>,
    pub decided_by: Option<Uuid>,
    pub decided_by_name: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub decision_reason: Option<String>,
    pub dispatched_at: Option<DateTime<Utc>>,
    pub counts: Counts,
    /// UX hints; the routes check again.
    pub can_approve: bool,
    pub can_reject: bool,
    pub can_cancel: bool,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = ScriptRunList)]
pub struct RunList {
    pub runs: Vec<RunView>,
}

/// One server of a run, with its output.
#[derive(Debug, Serialize, utoipa::ToSchema, sqlx::FromRow)]
#[schema(as = ScriptRunServer)]
pub struct RunServer {
    pub server_id: Uuid,
    pub hostname: String,
    pub site_id: Uuid,
    /// `waiting` (not dispatched), `skipped` (`error` says why), or the
    /// command's state: `queued`, `sent`, `running`, `succeeded`,
    /// `failed`, `expired`, `cancelled`.
    pub status: String,
    pub command_id: Option<Uuid>,
    pub exit_code: Option<i32>,
    /// The last 64 KiB of each.
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub error: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

/// A run with the text it runs and each server's output.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = ScriptRunDetail)]
pub struct RunDetail {
    pub run: RunView,
    pub body: String,
    pub servers: Vec<RunServer>,
}

/// Query string of `GET /api/remediation/script-runs`.
#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct RunParams {
    /// Runs of this script.
    pub script_id: Option<Uuid>,
    /// Runs on this server.
    pub server_id: Option<Uuid>,
    /// Runs of this automation rule.
    pub rule_id: Option<Uuid>,
    /// `awaiting_approval`, `scheduled`, `dispatched`, `rejected`,
    /// `expired`, `cancelled` or `refused`.
    pub state: Option<String>,
    /// At most 200, the default.
    pub limit: Option<i64>,
}

/// Body of `POST /api/remediation/script-runs/{id}/reject`.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[schema(as = ScriptRunRejectRequest)]
#[serde(deny_unknown_fields)]
pub struct RejectRequest {
    /// Why, at most 1000 characters.
    pub reason: String,
}

// Who may do what.

/// Whether `user` manages scripts of `customer`.
fn manages(user: &CurrentUser, customer: Option<Uuid>) -> bool {
    match customer {
        None => user.administered_customers() == Scope::All,
        Some(c) => user.administered_customers().contains(c),
    }
}

/// Whether `user` reads `script`.
fn reads(customers: &Scope, script: &Script) -> bool {
    script.customer_id.is_none_or(|c| customers.contains(c))
}

fn view(user: &CurrentUser, s: &Script) -> ScriptView {
    ScriptView {
        id: s.id,
        customer_id: s.customer_id,
        name: s.name.clone(),
        description: s.description.clone(),
        interpreter: s.interpreter,
        run_role: s.run_role,
        ai_allowed: s.ai_allowed,
        timeout_s: s.timeout_s,
        version: s.version,
        parameters: s.parameters.clone(),
        created_at: s.created_at,
        updated_at: s.updated_at,
        deleted_at: s.deleted_at,
        can_manage: s.deleted_at.is_none() && manages(user, s.customer_id),
    }
}

/// The script `id` if the user reads it.
async fn readable(
    db: &mut PgConnection,
    user: &CurrentUser,
    id: &str,
) -> Result<(Script, Scope), ApiError> {
    let id = parse_id(id)?;
    let customers = readable_customers(db, user).await?;
    let script = store::get(&mut *db, id)
        .await?
        .filter(|s| reads(&customers, s))
        .ok_or(ApiError::NotFound)?;
    Ok((script, customers))
}

async fn detail_of(
    db: &mut PgConnection,
    user: &CurrentUser,
    script: &Script,
) -> Result<ScriptDetail, ApiError> {
    let versions = store::versions(&mut *db, script.id).await?;
    Ok(ScriptDetail {
        script: view(user, script),
        body: script.body.clone(),
        versions: versions
            .into_iter()
            .map(|v| VersionView {
                version: v.version,
                body: v.body,
                parameters: v.parameters.0,
                note: v.note,
                created_by: v.created_by,
                created_by_name: v.created_by_name,
                created_at: v.created_at,
            })
            .collect(),
    })
}

/// The checked fields of `request`.
fn checked(request: &ScriptRequest) -> Result<Fields<'_>, ApiError> {
    let invalid = |m: String| ApiError::Invalid(m);
    if !valid_script_name(&request.name) {
        return Err(invalid(
            "The name is lower case letters, digits, `.`, `_` and `-`, starting with a letter or \
             digit, at most 63."
                .into(),
        ));
    }
    if request.description.chars().count() > MAX_DESCRIPTION {
        return Err(invalid(format!(
            "The description is at most {MAX_DESCRIPTION} characters."
        )));
    }
    if request.note.chars().count() > MAX_NOTE || request.note.contains(['\n', '\r']) {
        return Err(invalid(format!(
            "The note is one line of at most {MAX_NOTE} characters."
        )));
    }
    if request.run_role == Role::Viewer {
        return Err(invalid(
            "Viewers never run scripts: choose operator, approver or administrator.".into(),
        ));
    }
    let timeout_s = request.timeout_s.unwrap_or(600);
    if !(10..=3600).contains(&timeout_s) {
        return Err(invalid("The timeout is 10 to 3600 seconds.".into()));
    }
    check_body(&request.body).map_err(invalid)?;
    check_parameters(&request.parameters).map_err(invalid)?;
    Ok(Fields {
        name: &request.name,
        description: &request.description,
        interpreter: request.interpreter,
        run_role: request.run_role,
        ai_allowed: request.ai_allowed,
        timeout_s,
    })
}

fn taken(error: sqlx::Error, name: &str) -> ApiError {
    if store::name_taken(&error) {
        ApiError::Conflict(format!("A script named `{name}` already exists."))
    } else {
        error.into()
    }
}

async fn record(
    db: &mut PgConnection,
    user: &CurrentUser,
    action: &str,
    target: (&str, Uuid),
    details: Value,
) -> Result<(), sqlx::Error> {
    let actor = user.id.to_string();
    audit::record(
        &mut *db,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action,
            target: Some((target.0, &target.1.to_string())),
            details,
        },
    )
    .await
}

// Scripts.

/// `GET /api/remediation/scripts`.
pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
    params: Result<Query<ListParams>, QueryRejection>,
) -> Result<Json<ScriptList>, ApiError> {
    let Query(params) = params.map_err(|e| ApiError::Invalid(e.body_text()))?;
    let mut db = state.pool.acquire().await?;
    let customers = readable_customers(&mut db, &user).await?;
    if params.customer_id.is_some_and(|c| !customers.contains(c)) {
        return Err(ApiError::NotFound);
    }
    let (all, ids) = match params.customer_id {
        Some(c) => (false, vec![c]),
        None => customers.bind(),
    };
    let scripts = store::list(&mut *db, all, &ids).await?;
    Ok(Json(ScriptList {
        scripts: scripts.iter().map(|s| view(&user, s)).collect(),
    }))
}

/// `GET /api/remediation/scripts/{id}`.
pub async fn get(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<ScriptDetail>, ApiError> {
    let mut db = state.pool.acquire().await?;
    let (script, _) = readable(&mut db, &user, &id).await?;
    Ok(Json(detail_of(&mut db, &user, &script).await?))
}

/// `POST /api/remediation/scripts`.
pub async fn create(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<ScriptRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<ScriptDetail>), ApiError> {
    let request = body(request)?;
    let fields = checked(&request)?;
    let mut tx = state.pool.begin().await?;
    if let Some(customer) = request.customer_id {
        if !readable_customers(&mut tx, &user).await?.contains(customer) {
            return Err(ApiError::NotFound);
        }
    }
    if !manages(&user, request.customer_id) {
        return Err(ApiError::Forbidden);
    }
    let id = store::insert(
        &mut tx,
        request.customer_id,
        &fields,
        &request.body,
        &request.parameters,
        &request.note,
        user.id,
    )
    .await
    .map_err(|e| taken(e, &request.name))?;
    record(
        &mut tx,
        &user,
        "script.create",
        ("script", id),
        json!({ "name": request.name, "customer_id": request.customer_id, "version": 1,
                "run_role": request.run_role, "ai_allowed": request.ai_allowed }),
    )
    .await?;
    let script = store::get(&mut *tx, id).await?.ok_or(ApiError::NotFound)?;
    let detail = detail_of(&mut tx, &user, &script).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(detail)))
}

/// `PUT /api/remediation/scripts/{id}`.
pub async fn update(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<ScriptRequest>, JsonRejection>,
) -> Result<Json<ScriptDetail>, ApiError> {
    let request = body(request)?;
    let mut tx = state.pool.begin().await?;
    let (script, _) = readable(&mut tx, &user, &id).await?;
    if !manages(&user, script.customer_id) {
        return Err(ApiError::Forbidden);
    }
    if script.deleted_at.is_some() {
        return Err(ApiError::Conflict("The script was deleted.".into()));
    }
    if request.customer_id != script.customer_id {
        return Err(ApiError::Invalid(
            "A script stays with its customer; make a new one instead.".into(),
        ));
    }
    let fields = checked(&request)?;
    sqlx::query("SELECT 1 FROM scripts WHERE id = $1 FOR UPDATE")
        .bind(script.id)
        .execute(&mut *tx)
        .await?;
    let version = store::update(
        &mut tx,
        &script,
        &fields,
        &request.body,
        &request.parameters,
        &request.note,
        user.id,
    )
    .await
    .map_err(|e| taken(e, &request.name))?;
    record(
        &mut tx,
        &user,
        "script.update",
        ("script", script.id),
        json!({ "name": request.name, "version": version,
                "new_version": version != script.version,
                "run_role": request.run_role, "ai_allowed": request.ai_allowed }),
    )
    .await?;
    let script = store::get(&mut *tx, script.id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let detail = detail_of(&mut tx, &user, &script).await?;
    tx.commit().await?;
    Ok(Json(detail))
}

/// `DELETE /api/remediation/scripts/{id}`.
pub async fn delete(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let mut tx = state.pool.begin().await?;
    let (script, _) = readable(&mut tx, &user, &id).await?;
    if !manages(&user, script.customer_id) {
        return Err(ApiError::Forbidden);
    }
    if script.deleted_at.is_some() {
        return Ok(StatusCode::NO_CONTENT);
    }
    let rules = store::rules_using(&mut *tx, script.id).await?;
    if rules > 0 {
        return Err(ApiError::Conflict(format!(
            "{rules} automation rule(s) run this script; change or delete them first."
        )));
    }
    store::delete(&mut *tx, script.id).await?;
    record(
        &mut tx,
        &user,
        "script.delete",
        ("script", script.id),
        json!({ "name": script.name }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

// Runs.

/// The servers of `target`, checked against what the user reads.
async fn target_servers(
    db: &mut PgConnection,
    user: &CurrentUser,
    target: &Target,
) -> Result<Vec<TargetServer>, ApiError> {
    match target {
        Target::Servers { server_ids } => {
            if server_ids.is_empty() || server_ids.len() > run::MAX_SERVERS {
                return Err(ApiError::Invalid(format!(
                    "Choose 1 to {} servers.",
                    run::MAX_SERVERS
                )));
            }
        }
        Target::Site { site_id } => {
            authorize(user, Some(*site_id), false)?;
        }
        Target::BusinessService {
            business_service_id,
        } => {
            let customer: Option<Uuid> =
                sqlx::query_scalar("SELECT customer_id FROM business_services WHERE id = $1")
                    .bind(business_service_id)
                    .fetch_optional(&mut *db)
                    .await?;
            let customers = readable_customers(db, user).await?;
            if !customer.is_some_and(|c| customers.contains(c)) {
                return Err(ApiError::NotFound);
            }
        }
    }
    let servers = run::servers_of(&mut *db, target).await?;
    let readable = user.readable_sites();
    if let Target::Servers { server_ids } = target {
        let mut wanted = server_ids.clone();
        wanted.sort();
        wanted.dedup();
        // A server missing, removed or outside the user's sites: 404.
        if wanted.len() != servers.len() || servers.iter().any(|s| !readable.contains(s.site_id)) {
            return Err(ApiError::NotFound);
        }
    }
    let servers: Vec<TargetServer> = servers
        .into_iter()
        .filter(|s| readable.contains(s.site_id))
        .collect();
    if servers.is_empty() {
        return Err(ApiError::Invalid("There is no server to run it on.".into()));
    }
    if servers.len() > run::MAX_SERVERS {
        return Err(ApiError::Invalid(format!(
            "A run has at most {} servers.",
            run::MAX_SERVERS
        )));
    }
    if servers
        .iter()
        .any(|s| s.customer_id != servers[0].customer_id)
    {
        return Err(ApiError::Invalid(
            "A run's servers belong to one customer.".into(),
        ));
    }
    Ok(servers)
}

fn on_every_site(scope: &Scope, servers: &[TargetServer]) -> bool {
    servers.iter().all(|s| scope.contains(s.site_id))
}

/// `POST /api/remediation/scripts/{id}/runs`.
pub async fn start(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<RunRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<RunDetail>), ApiError> {
    let request = body(request)?;
    let mut db = state.pool.acquire().await?;
    let (script, _) = readable(&mut db, &user, &id).await?;
    if script.deleted_at.is_some() {
        return Err(ApiError::Conflict("The script was deleted.".into()));
    }
    let servers = target_servers(&mut db, &user, &request.target).await?;
    let customer = servers[0].customer_id;
    if script.customer_id.is_some_and(|own| own != customer) {
        return Err(ApiError::Invalid(
            "This script belongs to another customer.".into(),
        ));
    }
    if !on_every_site(&user.sites_with(script.run_role), &servers) {
        return Err(ApiError::Forbidden);
    }
    let version_number = request.version.unwrap_or(script.version);
    let version = store::version(&mut *db, script.id, version_number)
        .await?
        .ok_or_else(|| ApiError::Invalid(format!("The script has no version {version_number}.")))?;
    let values = values(&version.parameters.0, &request.parameters).map_err(ApiError::Invalid)?;
    let now = Utc::now();
    let scheduled_at = request.scheduled_at.unwrap_or(now).max(now);
    if scheduled_at > now + MAX_AHEAD {
        return Err(ApiError::Invalid(
            "Schedule a run at most a year ahead.".into(),
        ));
    }
    drop(db);
    let decision = run::classify_on(
        &state.pool,
        script.interpreter,
        &version.body,
        &values,
        &servers,
    )
    .await?;
    if decision.tier == Tier::Forbidden {
        return Err(ApiError::Invalid(format!(
            "The risk rules forbid this script: {}",
            decision.reason
        )));
    }
    let approves = on_every_site(&user.sites_with(approver_role(decision.tier)), &servers);
    let run_state = if approves {
        RunState::Scheduled
    } else {
        RunState::AwaitingApproval
    };
    let mut tx = state.pool.begin().await?;
    let run_id = run::insert(
        &mut tx,
        &NewRun {
            script: &script,
            version: version_number,
            customer,
            values: &values,
            target: &request.target,
            servers: &servers,
            requested_by: Some(user.id),
            rule: None,
            trigger: None,
            decision: &decision,
            state: run_state,
            scheduled_at,
        },
    )
    .await?;
    record(
        &mut tx,
        &user,
        "script.run",
        ("script_run", run_id),
        json!({
            "script_id": script.id, "script": script.name, "version": version_number,
            "target": request.target, "server_ids": servers.iter().map(|s| s.id).collect::<Vec<_>>(),
            "parameters": values.iter().map(|(k, _)| k).collect::<Vec<_>>(),
            "tier": decision.tier, "state": run_state.as_str(), "scheduled_at": scheduled_at,
        }),
    )
    .await?;
    tx.commit().await?;
    if run_state == RunState::Scheduled {
        run::dispatch(&state.pool, state.commands.as_ref(), run_id).await?;
    }
    let detail = run_detail(&state, &user, run_id).await?;
    Ok((StatusCode::CREATED, Json(detail)))
}

#[derive(sqlx::FromRow)]
struct RunRow {
    id: Uuid,
    script_id: Uuid,
    script_name: String,
    interpreter: String,
    version: i32,
    customer_id: Uuid,
    parameters: SqlJson<BTreeMap<String, String>>,
    target: SqlJson<Target>,
    requested_by: Option<Uuid>,
    requested_by_name: Option<String>,
    rule_id: Option<Uuid>,
    rule_name: Option<String>,
    trigger: Option<Value>,
    state: String,
    tier: String,
    rule: Value,
    reason: String,
    scheduled_at: DateTime<Utc>,
    created_at: DateTime<Utc>,
    decided_by: Option<Uuid>,
    decided_by_name: Option<String>,
    decided_at: Option<DateTime<Utc>>,
    decision_reason: Option<String>,
    dispatched_at: Option<DateTime<Utc>>,
    servers: i64,
    pending: i64,
    succeeded: i64,
    failed: i64,
    /// The sites of its servers.
    sites: Vec<Uuid>,
}

/// Runs whose every server the caller reads: `$1` all sites, `$2` the
/// sites, `$3` one run or any.
const RUNS: &str = "SELECT r.id, r.script_id, sc.name AS script_name, sc.interpreter, r.version,
        r.customer_id, r.parameters, r.target, r.requested_by, u.username AS requested_by_name,
        r.rule_id, ar.name AS rule_name, r.trigger, r.state, r.tier, r.rule, r.reason,
        r.scheduled_at, r.created_at, r.decided_by, d.username AS decided_by_name, r.decided_at,
        r.decision_reason, r.dispatched_at,
        (SELECT count(*) FROM script_run_targets t WHERE t.run_id = r.id) AS servers,
        (SELECT count(*) FROM script_run_targets t LEFT JOIN commands c ON c.id = t.command_id
         WHERE t.run_id = r.id AND t.error IS NULL
           AND (c.id IS NULL OR c.state IN ('queued', 'sent', 'running'))
           AND r.state IN ('awaiting_approval', 'scheduled', 'dispatched')) AS pending,
        (SELECT count(*) FROM script_run_targets t JOIN commands c ON c.id = t.command_id
         WHERE t.run_id = r.id AND c.state = 'succeeded') AS succeeded,
        (SELECT count(*) FROM script_run_targets t LEFT JOIN commands c ON c.id = t.command_id
         WHERE t.run_id = r.id
           AND (t.error IS NOT NULL OR c.state IN ('failed', 'expired', 'cancelled'))) AS failed,
        ARRAY(SELECT DISTINCT s.site_id FROM script_run_targets t
              JOIN servers s ON s.id = t.server_id WHERE t.run_id = r.id) AS sites
    FROM script_runs r
    JOIN scripts sc ON sc.id = r.script_id
    LEFT JOIN users u ON u.id = r.requested_by
    LEFT JOIN users d ON d.id = r.decided_by
    LEFT JOIN automation_rules ar ON ar.id = r.rule_id
    WHERE NOT EXISTS (SELECT 1 FROM script_run_targets t JOIN servers s ON s.id = t.server_id
                      WHERE t.run_id = r.id AND NOT ($1 OR s.site_id = ANY($2)))";

fn run_view(user: &CurrentUser, r: RunRow) -> Result<RunView, ApiError> {
    let tier = Tier::parse(&r.tier).ok_or(ApiError::NotFound)?;
    let state = RunState::parse(&r.state).ok_or(ApiError::NotFound)?;
    let open = matches!(state, RunState::AwaitingApproval | RunState::Scheduled);
    let waiting = state == RunState::AwaitingApproval;
    let all = |scope: Scope| r.sites.iter().all(|s| scope.contains(*s));
    let approver = all(user.sites_with(Role::Approver));
    Ok(RunView {
        id: r.id,
        script_id: r.script_id,
        script_name: r.script_name,
        version: r.version,
        interpreter: Interpreter::parse(&r.interpreter).unwrap_or(Interpreter::Sh),
        customer_id: r.customer_id,
        parameters: r.parameters.0,
        target: r.target.0,
        requested_by: r.requested_by,
        requested_by_name: r.requested_by_name,
        rule_id: r.rule_id,
        rule_name: r.rule_name,
        trigger: r.trigger,
        state,
        tier,
        rule: r.rule,
        reason: r.reason,
        scheduled_at: r.scheduled_at,
        created_at: r.created_at,
        expires_at: waiting.then_some(r.created_at + run::APPROVAL_WINDOW),
        decided_by: r.decided_by,
        decided_by_name: r.decided_by_name,
        decided_at: r.decided_at,
        decision_reason: r.decision_reason,
        dispatched_at: r.dispatched_at,
        counts: Counts {
            servers: r.servers,
            pending: r.pending,
            succeeded: r.succeeded,
            failed: r.failed,
        },
        can_approve: waiting && all(user.sites_with(approver_role(tier))),
        can_reject: waiting && approver,
        can_cancel: open && (r.requested_by == Some(user.id) || approver),
    })
}

/// The run `id` if the caller reads every server of it.
async fn run_row(state: &ApiState, user: &CurrentUser, id: Uuid) -> Result<RunRow, ApiError> {
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    let (all, sites) = user.readable_sites().bind();
    // SQL-safe: interpolates only the constant RUNS.
    sqlx::query_as(sqlx::AssertSqlSafe(format!("{RUNS} AND r.id = $3")))
        .bind(all)
        .bind(&sites)
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or(ApiError::NotFound)
}

async fn run_detail(state: &ApiState, user: &CurrentUser, id: Uuid) -> Result<RunDetail, ApiError> {
    let row = run_row(state, user, id).await?;
    let body: String = sqlx::query_scalar(
        "SELECT body FROM script_versions WHERE script_id = $1 AND version = $2",
    )
    .bind(row.script_id)
    .bind(row.version)
    .fetch_one(&state.pool)
    .await?;
    let servers: Vec<RunServer> = sqlx::query_as(
        "SELECT t.server_id, s.hostname, s.site_id,
                CASE WHEN t.error IS NOT NULL THEN 'skipped'
                     WHEN c.id IS NULL THEN 'waiting' ELSE c.state END AS status,
                t.command_id, c.exit_code, c.stdout, c.stderr, coalesce(t.error, c.error) AS error,
                c.started_at, c.finished_at
         FROM script_run_targets t JOIN servers s ON s.id = t.server_id
         LEFT JOIN commands c ON c.id = t.command_id
         WHERE t.run_id = $1 ORDER BY s.hostname, s.id",
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await?;
    Ok(RunDetail {
        run: run_view(user, row)?,
        body,
        servers,
    })
}

/// `GET /api/remediation/script-runs`.
pub async fn runs(
    State(state): State<ApiState>,
    user: CurrentUser,
    params: Result<Query<RunParams>, QueryRejection>,
) -> Result<Json<RunList>, ApiError> {
    let Query(params) = params.map_err(|e| ApiError::Invalid(e.body_text()))?;
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    let run_state = match params.state.as_deref() {
        None => None,
        Some(s) => Some(
            RunState::parse(s)
                .ok_or_else(|| ApiError::Invalid(format!("`{s}` is not a run state.")))?,
        ),
    };
    let limit = params.limit.unwrap_or(MAX_RUNS).clamp(1, MAX_RUNS);
    if let Some(server) = params.server_id {
        let site: Option<Uuid> = sqlx::query_scalar("SELECT site_id FROM servers WHERE id = $1")
            .bind(server)
            .fetch_optional(&state.pool)
            .await?;
        authorize(&user, site, false)?;
    }
    let (all, sites) = user.readable_sites().bind();
    // SQL-safe: interpolates only the constant RUNS.
    let rows: Vec<RunRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{RUNS}
           AND ($3::uuid IS NULL OR r.script_id = $3)
           AND ($4::uuid IS NULL OR r.rule_id = $4)
           AND ($5::text IS NULL OR r.state = $5)
           AND ($6::uuid IS NULL OR EXISTS (SELECT 1 FROM script_run_targets t
                                            WHERE t.run_id = r.id AND t.server_id = $6))
         ORDER BY r.created_at DESC, r.id LIMIT $7"
    )))
    .bind(all)
    .bind(&sites)
    .bind(params.script_id)
    .bind(params.rule_id)
    .bind(run_state.map(RunState::as_str))
    .bind(params.server_id)
    .bind(limit)
    .fetch_all(&state.pool)
    .await?;
    let runs = rows
        .into_iter()
        .map(|r| run_view(&user, r))
        .collect::<Result<_, _>>()?;
    Ok(Json(RunList { runs }))
}

/// `GET /api/remediation/script-runs/{id}`.
pub async fn run(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<RunDetail>, ApiError> {
    let id = parse_id(&id)?;
    Ok(Json(run_detail(&state, &user, id).await?))
}

/// Locks the run `id` for a decision and checks it waits for one.
async fn locked(tx: &mut PgConnection, id: Uuid, states: &[RunState]) -> Result<(), ApiError> {
    let current: String =
        sqlx::query_scalar("SELECT state FROM script_runs WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    if !states.iter().any(|s| s.as_str() == current) {
        return Err(ApiError::Conflict(format!(
            "The run is {}.",
            current.replace('_', " ")
        )));
    }
    Ok(())
}

/// `POST /api/remediation/script-runs/{id}/approve`.
pub async fn approve(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<RunDetail>, ApiError> {
    let id = parse_id(&id)?;
    let row = run_row(&state, &user, id).await?;
    // Classified again: the rules may have changed since.
    let servers: Vec<TargetServer> = sqlx::query_as(
        "SELECT s.id, s.site_id, st.customer_id, s.os_family FROM script_run_targets t
         JOIN servers s ON s.id = t.server_id JOIN sites st ON st.id = s.site_id
         WHERE t.run_id = $1",
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await?;
    let version = store::version(&state.pool, row.script_id, row.version)
        .await?
        .ok_or(ApiError::NotFound)?;
    let values: Vec<(String, String)> = row.parameters.0.clone().into_iter().collect();
    let interpreter = Interpreter::parse(&row.interpreter).unwrap_or(Interpreter::Sh);
    let decision =
        run::classify_on(&state.pool, interpreter, &version.body, &values, &servers).await?;
    if decision.tier == Tier::Forbidden {
        return Err(ApiError::Conflict(format!(
            "The risk rules now forbid this script: {}",
            decision.reason
        )));
    }
    let stored = Tier::parse(&row.tier).unwrap_or(Tier::HighImpact);
    let tier = stored.max(decision.tier);
    if !on_every_site(&user.sites_with(approver_role(tier)), &servers) {
        return Err(ApiError::Forbidden);
    }
    let mut tx = state.pool.begin().await?;
    locked(&mut tx, id, &[RunState::AwaitingApproval]).await?;
    sqlx::query(
        "UPDATE script_runs SET state = 'scheduled', decided_by = $2, decided_at = now(),
                scheduled_at = greatest(scheduled_at, now()) WHERE id = $1",
    )
    .bind(id)
    .bind(user.id)
    .execute(&mut *tx)
    .await?;
    record(
        &mut tx,
        &user,
        "script_run.approve",
        ("script_run", id),
        json!({ "script_id": row.script_id, "script": row.script_name, "tier": tier }),
    )
    .await?;
    tx.commit().await?;
    run::dispatch(&state.pool, state.commands.as_ref(), id).await?;
    Ok(Json(run_detail(&state, &user, id).await?))
}

/// `POST /api/remediation/script-runs/{id}/reject`.
pub async fn reject(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<RejectRequest>, JsonRejection>,
) -> Result<Json<RunDetail>, ApiError> {
    let id = parse_id(&id)?;
    let request = body(request)?;
    let reason = request.reason.trim();
    if reason.is_empty() || reason.chars().count() > 1000 {
        return Err(ApiError::Invalid(
            "Say why, in at most 1000 characters.".into(),
        ));
    }
    let row = run_row(&state, &user, id).await?;
    if !r_all(&row.sites, &user.sites_with(Role::Approver)) {
        return Err(ApiError::Forbidden);
    }
    let mut tx = state.pool.begin().await?;
    locked(&mut tx, id, &[RunState::AwaitingApproval]).await?;
    sqlx::query(
        "UPDATE script_runs SET state = 'rejected', decided_by = $2, decided_at = now(),
                decision_reason = $3 WHERE id = $1",
    )
    .bind(id)
    .bind(user.id)
    .bind(reason)
    .execute(&mut *tx)
    .await?;
    record(
        &mut tx,
        &user,
        "script_run.reject",
        ("script_run", id),
        json!({ "script_id": row.script_id, "script": row.script_name, "reason": reason }),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(run_detail(&state, &user, id).await?))
}

fn r_all(sites: &[Uuid], scope: &Scope) -> bool {
    sites.iter().all(|s| scope.contains(*s))
}

/// `POST /api/remediation/script-runs/{id}/cancel`.
pub async fn cancel(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<RunDetail>, ApiError> {
    let id = parse_id(&id)?;
    let row = run_row(&state, &user, id).await?;
    let approver = r_all(&row.sites, &user.sites_with(Role::Approver));
    if row.requested_by != Some(user.id) && !approver {
        return Err(ApiError::Forbidden);
    }
    let mut tx = state.pool.begin().await?;
    locked(
        &mut tx,
        id,
        &[RunState::AwaitingApproval, RunState::Scheduled],
    )
    .await?;
    sqlx::query(
        "UPDATE script_runs SET state = 'cancelled', decided_by = $2, decided_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(user.id)
    .execute(&mut *tx)
    .await?;
    record(
        &mut tx,
        &user,
        "script_run.cancel",
        ("script_run", id),
        json!({ "script_id": row.script_id, "script": row.script_name }),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(run_detail(&state, &user, id).await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> ScriptRequest {
        serde_json::from_value(json!({
            "name": "clear-cache", "interpreter": "sh", "run_role": "operator",
            "body": "df -h",
        }))
        .unwrap()
    }

    #[test]
    fn requests_are_checked() {
        assert_eq!(checked(&request()).unwrap().timeout_s, 600);
        let mut bad = request();
        bad.name = "Clear cache".into();
        assert!(checked(&bad).is_err());
        let mut viewer = request();
        viewer.run_role = Role::Viewer;
        assert!(checked(&viewer).is_err());
        let mut slow = request();
        slow.timeout_s = Some(5);
        assert!(checked(&slow).is_err());
        let mut empty = request();
        empty.body = " \n".into();
        assert!(checked(&empty).is_err());
        let unknown = serde_json::from_value::<ScriptRequest>(json!({
            "name": "a", "interpreter": "bash", "run_role": "operator", "body": "x",
        }));
        assert!(unknown.is_err());
    }
}
