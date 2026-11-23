//! Automation rules (ADR-0179): run a library script on a server when an
//! incident opens there, one of its checks fails, or it joins.
//!
//! A rule covers everything, one customer or one site. Those who read
//! that place see it; its administrators make, change and remove it
//! (administrators of everything for a rule of everything, of the
//! customer as a whole, or of the site). The script must be usable there:
//! one of every customer, or the place's customer's own.

use std::collections::BTreeMap;

use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::types::Json as SqlJson;
use sqlx::PgConnection;
use uuid::Uuid;

use super::access::Scope;
use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::roles::Role;
use super::sensors::{body, parse_id};
use super::snmp_library::readable_customers;
use super::{ApiError, ApiState};
use crate::scripts::automation::{self, valid_metric, EventKind, Firing};
use crate::scripts::store;

/// Most firings shown with a rule.
const FIRINGS: i64 = 50;
/// Longest name, in characters.
const MAX_NAME: usize = 80;

/// Query string of `GET /api/remediation/automation-rules`.
#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct ListParams {
    /// Rules of this customer: of the customer as a whole and of its
    /// sites.
    pub customer_id: Option<Uuid>,
    /// Rules running this script.
    pub script_id: Option<Uuid>,
}

/// Body of `POST /api/remediation/automation-rules` and `PUT
/// /api/remediation/automation-rules/{id}`; a change replaces every
/// field.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[schema(as = AutomationRuleRequest)]
#[serde(deny_unknown_fields)]
pub struct RuleRequest {
    /// At most 80 characters.
    pub name: String,
    /// The customer it covers; with neither this nor `site_id`, it covers
    /// everything.
    #[serde(default)]
    pub customer_id: Option<Uuid>,
    /// The one site it covers.
    #[serde(default)]
    pub site_id: Option<Uuid>,
    pub event: EventKind,
    /// The incident's metric: required for `check_failed`, optional for
    /// `incident_opened` (any but `offline` when absent), none for
    /// `server_joined`.
    #[serde(default)]
    pub metric: Option<String>,
    pub script_id: Uuid,
    /// Values by parameter name; defaults fill the rest.
    #[serde(default)]
    pub parameters: BTreeMap<String, String>,
    /// On when absent.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// At most one run per server in this many minutes, 1 to 10080; 60
    /// when absent.
    #[serde(default)]
    pub cooldown_minutes: Option<i32>,
}

fn yes() -> bool {
    true
}

/// A rule. Field names and types are part of the API contract.
#[derive(Debug, Serialize, utoipa::ToSchema, sqlx::FromRow)]
#[schema(as = AutomationRuleView)]
pub struct RuleView {
    pub id: Uuid,
    pub name: String,
    pub customer_id: Option<Uuid>,
    pub site_id: Option<Uuid>,
    #[sqlx(try_from = "String")]
    #[schema(value_type = EventKind)]
    pub event: EventName,
    pub metric: Option<String>,
    pub script_id: Uuid,
    pub script_name: String,
    #[schema(value_type = BTreeMap<String, String>)]
    pub parameters: SqlJson<BTreeMap<String, String>>,
    pub enabled: bool,
    pub cooldown_minutes: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Whether the caller may change and delete it. A UX hint.
    #[sqlx(skip)]
    pub can_manage: bool,
}

/// An event name read from the table.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(transparent)]
pub struct EventName(EventKind);

impl TryFrom<String> for EventName {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        EventKind::parse(&value)
            .map(EventName)
            .ok_or_else(|| format!("unknown event {value}"))
    }
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = AutomationRuleList)]
pub struct RuleList {
    pub rules: Vec<RuleView>,
}

/// A rule with the events it took lately, newest first.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = AutomationRuleDetail)]
pub struct RuleDetail {
    pub rule: RuleView,
    pub firings: Vec<Firing>,
}

/// Where a rule applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    Everything,
    Customer(Uuid),
    Site { site: Uuid, customer: Uuid },
}

impl Place {
    fn customer(self) -> Option<Uuid> {
        match self {
            Self::Everything => None,
            Self::Customer(c) | Self::Site { customer: c, .. } => Some(c),
        }
    }
}

/// The place of `customer` and `site`, if the user reads it.
async fn place(
    db: &mut PgConnection,
    user: &CurrentUser,
    customers: &Scope,
    customer: Option<Uuid>,
    site: Option<Uuid>,
) -> Result<Place, ApiError> {
    match (customer, site) {
        (None, None) => Ok(Place::Everything),
        (Some(c), None) if customers.contains(c) => Ok(Place::Customer(c)),
        (None, Some(s)) if user.readable_sites().contains(s) => {
            let customer: Uuid = sqlx::query_scalar("SELECT customer_id FROM sites WHERE id = $1")
                .bind(s)
                .fetch_optional(&mut *db)
                .await?
                .ok_or(ApiError::NotFound)?;
            Ok(Place::Site { site: s, customer })
        }
        (Some(_), Some(_)) => Err(ApiError::Invalid(
            "A rule covers a customer or one of its sites, not both.".into(),
        )),
        _ => Err(ApiError::NotFound),
    }
}

/// Whether `user` manages rules of `place`.
fn manages(user: &CurrentUser, place: Place) -> bool {
    match place {
        Place::Everything => user.administered_customers() == Scope::All,
        Place::Customer(c) => user.administered_customers().contains(c),
        Place::Site { site, .. } => user.sites_with(Role::Administrator).contains(site),
    }
}

/// Checks `request` and the script it runs; returns the place.
async fn checked(
    db: &mut PgConnection,
    user: &CurrentUser,
    request: &RuleRequest,
) -> Result<Place, ApiError> {
    let name = request.name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME {
        return Err(ApiError::Invalid(format!(
            "Name the rule, in at most {MAX_NAME} characters."
        )));
    }
    let customers = readable_customers(db, user).await?;
    let place = place(db, user, &customers, request.customer_id, request.site_id).await?;
    if !manages(user, place) {
        return Err(ApiError::Forbidden);
    }
    match (request.event, request.metric.as_deref()) {
        (EventKind::CheckFailed, None) => {
            return Err(ApiError::Invalid("Name the check that fails.".into()))
        }
        (EventKind::ServerJoined, Some(_)) => {
            return Err(ApiError::Invalid("A server joining has no metric.".into()))
        }
        (_, Some(metric)) if !valid_metric(metric) => {
            return Err(ApiError::Invalid(format!(
                "`{}` is not a metric name.",
                metric.escape_debug()
            )))
        }
        _ => {}
    }
    let cooldown = request.cooldown_minutes.unwrap_or(60);
    if !(1..=10080).contains(&cooldown) {
        return Err(ApiError::Invalid(
            "The cooldown is 1 minute to 7 days.".into(),
        ));
    }
    let script = store::get(&mut *db, request.script_id)
        .await?
        .filter(|s| s.deleted_at.is_none())
        .filter(|s| s.customer_id.is_none_or(|c| customers.contains(c)))
        .ok_or(ApiError::NotFound)?;
    if script.customer_id.is_some() && script.customer_id != place.customer() {
        return Err(ApiError::Invalid(
            "This script belongs to one customer: a rule running it covers that customer or \
             one of its sites."
                .into(),
        ));
    }
    crate::scripts::values(&script.parameters, &request.parameters).map_err(ApiError::Invalid)?;
    Ok(place)
}

const SELECT: &str = "SELECT r.id, r.name, r.customer_id, r.site_id, r.event, r.metric,
        r.script_id, s.name AS script_name, r.parameters, r.enabled, r.cooldown_minutes,
        r.created_at, r.updated_at
    FROM automation_rules r JOIN scripts s ON s.id = r.script_id";

/// The rule `id` if the user reads its place.
async fn readable(
    db: &mut PgConnection,
    user: &CurrentUser,
    id: &str,
) -> Result<(RuleView, Place), ApiError> {
    let id = parse_id(id)?;
    let customers = readable_customers(db, user).await?;
    // SQL-safe: interpolates only the constant SELECT.
    let row: RuleView = sqlx::query_as(sqlx::AssertSqlSafe(format!("{SELECT} WHERE r.id = $1")))
        .bind(id)
        .fetch_optional(&mut *db)
        .await?
        .ok_or(ApiError::NotFound)?;
    let place = place(db, user, &customers, row.customer_id, row.site_id).await?;
    Ok((with_manage(user, row, place), place))
}

fn with_manage(user: &CurrentUser, mut row: RuleView, place: Place) -> RuleView {
    row.can_manage = manages(user, place);
    row
}

async fn record(
    db: &mut PgConnection,
    user: &CurrentUser,
    action: &str,
    id: Uuid,
    details: serde_json::Value,
) -> Result<(), sqlx::Error> {
    let actor = user.id.to_string();
    audit::record(
        &mut *db,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action,
            target: Some(("automation_rule", &id.to_string())),
            details,
        },
    )
    .await
}

/// `GET /api/remediation/automation-rules`.
pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
    params: Result<Query<ListParams>, QueryRejection>,
) -> Result<Json<RuleList>, ApiError> {
    let Query(params) = params.map_err(|e| ApiError::Invalid(e.body_text()))?;
    let mut db = state.pool.acquire().await?;
    let customers = readable_customers(&mut db, &user).await?;
    if params.customer_id.is_some_and(|c| !customers.contains(c)) {
        return Err(ApiError::NotFound);
    }
    // SQL-safe: interpolates only the constant SELECT.
    let rows: Vec<RuleView> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{SELECT} WHERE ($1::uuid IS NULL OR r.script_id = $1)
           AND ($2::uuid IS NULL OR r.customer_id = $2
                OR r.site_id IN (SELECT id FROM sites WHERE customer_id = $2))
         ORDER BY r.name, r.id"
    )))
    .bind(params.script_id)
    .bind(params.customer_id)
    .fetch_all(&mut *db)
    .await?;
    let mut rules = Vec::new();
    for row in rows {
        match place(&mut db, &user, &customers, row.customer_id, row.site_id).await {
            Ok(place) => rules.push(with_manage(&user, row, place)),
            Err(ApiError::NotFound) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(Json(RuleList { rules }))
}

/// `GET /api/remediation/automation-rules/{id}`.
pub async fn get(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<RuleDetail>, ApiError> {
    let mut db = state.pool.acquire().await?;
    let (rule, _) = readable(&mut db, &user, &id).await?;
    let firings = automation::firings(&state.pool, rule.id, FIRINGS).await?;
    Ok(Json(RuleDetail { rule, firings }))
}

async fn write(
    tx: &mut PgConnection,
    id: Option<Uuid>,
    request: &RuleRequest,
    user: Uuid,
) -> Result<Uuid, sqlx::Error> {
    let cooldown = request.cooldown_minutes.unwrap_or(60);
    // A new rule gets a new ID; $1 is the row to change otherwise.
    sqlx::query_scalar(
        "INSERT INTO automation_rules (id, name, customer_id, site_id, event, metric, script_id,
                                       parameters, enabled, cooldown_minutes, created_by)
         VALUES (coalesce($1, gen_random_uuid()), $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
         ON CONFLICT (id) DO UPDATE SET name = $2, customer_id = $3, site_id = $4, event = $5,
                metric = $6, script_id = $7, parameters = $8, enabled = $9,
                cooldown_minutes = $10, updated_at = now()
         RETURNING id",
    )
    .bind(id)
    .bind(request.name.trim())
    .bind(request.customer_id)
    .bind(request.site_id)
    .bind(request.event.as_str())
    .bind(request.metric.as_deref())
    .bind(request.script_id)
    .bind(SqlJson(&request.parameters))
    .bind(request.enabled)
    .bind(cooldown)
    .bind(user)
    .fetch_one(&mut *tx)
    .await
}

fn details(request: &RuleRequest) -> serde_json::Value {
    json!({
        "name": request.name.trim(), "customer_id": request.customer_id,
        "site_id": request.site_id, "event": request.event, "metric": request.metric,
        "script_id": request.script_id, "enabled": request.enabled,
        "cooldown_minutes": request.cooldown_minutes.unwrap_or(60),
    })
}

/// `POST /api/remediation/automation-rules`.
pub async fn create(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<RuleRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<RuleDetail>), ApiError> {
    let request = body(request)?;
    let mut tx = state.pool.begin().await?;
    checked(&mut tx, &user, &request).await?;
    let id = write(&mut tx, None, &request, user.id).await?;
    record(
        &mut tx,
        &user,
        "automation_rule.create",
        id,
        details(&request),
    )
    .await?;
    tx.commit().await?;
    let mut db = state.pool.acquire().await?;
    let (rule, _) = readable(&mut db, &user, &id.to_string()).await?;
    Ok((
        StatusCode::CREATED,
        Json(RuleDetail {
            rule,
            firings: Vec::new(),
        }),
    ))
}

/// `PUT /api/remediation/automation-rules/{id}`.
pub async fn update(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<RuleRequest>, JsonRejection>,
) -> Result<Json<RuleDetail>, ApiError> {
    let request = body(request)?;
    let mut tx = state.pool.begin().await?;
    let (before, place) = readable(&mut tx, &user, &id).await?;
    if !manages(&user, place) {
        return Err(ApiError::Forbidden);
    }
    checked(&mut tx, &user, &request).await?;
    write(&mut tx, Some(before.id), &request, user.id).await?;
    record(
        &mut tx,
        &user,
        "automation_rule.update",
        before.id,
        details(&request),
    )
    .await?;
    tx.commit().await?;
    let mut db = state.pool.acquire().await?;
    let (rule, _) = readable(&mut db, &user, &before.id.to_string()).await?;
    let firings = automation::firings(&state.pool, rule.id, FIRINGS).await?;
    Ok(Json(RuleDetail { rule, firings }))
}

/// `DELETE /api/remediation/automation-rules/{id}`.
pub async fn delete(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let mut tx = state.pool.begin().await?;
    let (rule, place) = readable(&mut tx, &user, &id).await?;
    if !manages(&user, place) {
        return Err(ApiError::Forbidden);
    }
    sqlx::query("DELETE FROM automation_rules WHERE id = $1")
        .bind(rule.id)
        .execute(&mut *tx)
        .await?;
    record(
        &mut tx,
        &user,
        "automation_rule.delete",
        rule.id,
        json!({ "name": rule.name, "script_id": rule.script_id }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_read_as_documented() {
        let request: RuleRequest = serde_json::from_value(json!({
            "name": "Clear the cache when /var fills", "event": "incident_opened",
            "metric": "monitorflow_disk_used_ratio", "script_id": Uuid::nil(),
        }))
        .unwrap();
        assert!(request.enabled);
        assert_eq!(request.cooldown_minutes, None);
        assert!(serde_json::from_value::<RuleRequest>(json!({
            "name": "x", "event": "incident.opened", "script_id": Uuid::nil(),
        }))
        .is_err());
        assert_eq!(Place::Customer(Uuid::nil()).customer(), Some(Uuid::nil()));
        assert_eq!(Place::Everything.customer(), None);
    }
}
