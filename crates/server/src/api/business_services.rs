//! Business services (ADR-0155): a named group of servers, devices and
//! checks of one customer with one availability.
//!
//! Users granted the customer as a whole see its services; operators and
//! above of the whole customer make, change and remove them. Members are
//! servers or devices of the customer's sites, or chosen checks of them.

use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgConnection;
use uuid::Uuid;

use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::sensors::{body, parse_id};
use super::silences::{known_checks, Place};
use super::{ApiError, ApiState};
use crate::notify::silence::CheckRef;

/// Most members of one service.
pub const MAX_MEMBERS: usize = 200;
const MAX_NAME: usize = 80;
const MAX_DESCRIPTION: usize = 500;

/// Query string of `GET /api/business-services`.
#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct ListParams {
    /// Only the services of this customer.
    pub customer_id: Option<Uuid>,
}

/// A member in a request: a whole server or device, or one check of it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, utoipa::ToSchema)]
#[schema(as = BusinessServiceMemberRequest)]
#[serde(deny_unknown_fields)]
pub struct MemberRequest {
    pub server_id: Uuid,
    /// Only this check of it; absent or `null` for the whole server.
    #[serde(default)]
    pub check: Option<CheckRef>,
}

/// Body of `POST /api/business-services` and `PUT
/// /api/business-services/{id}`; a change replaces every field.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[schema(as = BusinessServiceRequest)]
#[serde(deny_unknown_fields)]
pub struct ServiceRequest {
    /// The customer it belongs to; may be left out when there is only one.
    #[serde(default)]
    pub customer_id: Option<Uuid>,
    /// 1 to 80 characters, unique per customer, like `Email`.
    pub name: String,
    /// At most 500 characters.
    #[serde(default)]
    pub description: String,
    /// `all` (the default): down while any member is down; `at_least`: down
    /// while fewer than `at_least` members are up.
    #[serde(default = "all")]
    pub rule: String,
    /// For `at_least`: 1 to the number of members.
    #[serde(default)]
    pub at_least: Option<u32>,
    /// The SLA target, a percentage from 50 to 100 with up to three
    /// decimals, like 99.9; absent or `null` for none.
    #[serde(default)]
    pub target: Option<f64>,
    /// 1 to 200 members.
    pub members: Vec<MemberRequest>,
}

fn all() -> String {
    "all".into()
}

/// A member, by name.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = BusinessServiceMember)]
pub struct MemberView {
    pub server_id: Uuid,
    pub server_name: String,
    pub device: bool,
    /// The check; `null` for the whole server or device.
    pub check: Option<CheckRef>,
}

/// One business service.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = BusinessService)]
pub struct ServiceView {
    pub id: Uuid,
    pub customer_id: Uuid,
    pub customer_name: String,
    pub name: String,
    pub description: String,
    pub rule: String,
    pub at_least: Option<u32>,
    pub target: Option<f64>,
    pub members: Vec<MemberView>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Whether this user may change or remove it.
    pub can_change: bool,
}

/// Body of `GET /api/business-services`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = BusinessServiceList)]
pub struct ServiceList {
    /// By name.
    pub services: Vec<ServiceView>,
}

/// A request checked, ready to store.
#[derive(Debug)]
struct Valid {
    customer: Uuid,
    name: String,
    description: String,
    at_least: Option<i32>,
    target: Option<f64>,
    members: Vec<MemberRequest>,
}

/// What `request` says, or why not, before looking at the database.
fn shape(request: &ServiceRequest) -> Result<(String, Option<i32>), String> {
    let name = request.name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME {
        return Err(format!(
            "Give the service a name of 1 to {MAX_NAME} characters."
        ));
    }
    if request.description.chars().count() > MAX_DESCRIPTION {
        return Err(format!(
            "Keep the description under {MAX_DESCRIPTION} characters."
        ));
    }
    if request.members.is_empty() || request.members.len() > MAX_MEMBERS {
        return Err(format!(
            "Choose 1 to {MAX_MEMBERS} servers, devices or checks."
        ));
    }
    let at_least = match (request.rule.as_str(), request.at_least) {
        ("all", None) => None,
        ("all", Some(_)) => return Err("`at_least` goes with the rule `at_least` only.".into()),
        ("at_least", Some(n)) if n >= 1 && (n as usize) <= request.members.len() => {
            Some(i32::try_from(n).unwrap_or(1))
        }
        ("at_least", _) => {
            return Err(format!(
                "Say how many must be up: 1 to {}, the number of members.",
                request.members.len()
            ))
        }
        _ => return Err("The rule is `all` or `at_least`.".into()),
    };
    if let Some(target) = request.target {
        let thousandths = target * 1000.0;
        if !(50.0..=100.0).contains(&target) || (thousandths - thousandths.round()).abs() > 1e-6 {
            return Err("The target is a percentage from 50 to 100, like 99.9.".into());
        }
    }
    let mut seen = Vec::new();
    for member in &request.members {
        if seen.contains(&member) {
            return Err("A member is listed twice.".into());
        }
        seen.push(member);
    }
    Ok((name.to_owned(), at_least))
}

/// The customer a request is for: the one it names, or the only one there
/// is. The user must change it as a whole.
async fn customer_of(
    db: &mut PgConnection,
    user: &CurrentUser,
    named: Option<Uuid>,
) -> Result<Uuid, ApiError> {
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    let customer = match named {
        Some(id) => sqlx::query_scalar::<_, Uuid>("SELECT id FROM customers WHERE id = $1")
            .bind(id)
            .fetch_optional(&mut *db)
            .await?
            .filter(|id| user.visible_customers().contains(*id))
            .ok_or(ApiError::NotFound)?,
        None => {
            let ids: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM customers ORDER BY name")
                .fetch_all(&mut *db)
                .await?;
            match ids.as_slice() {
                [only] => *only,
                _ => return Err(ApiError::Invalid("Choose the customer.".into())),
            }
        }
    };
    if !user.modifiable_customers().contains(customer) {
        return Err(ApiError::Forbidden);
    }
    Ok(customer)
}

async fn validate(
    state: &ApiState,
    user: &CurrentUser,
    request: &ServiceRequest,
) -> Result<Valid, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let customer = customer_of(&mut conn, user, request.customer_id).await?;
    let (name, at_least) = shape(request).map_err(ApiError::Invalid)?;
    let servers: Vec<Uuid> = request.members.iter().map(|m| m.server_id).collect();
    let found: Vec<Uuid> = sqlx::query_scalar(
        "SELECT s.id FROM servers s JOIN sites si ON si.id = s.site_id
         WHERE s.id = ANY($1) AND si.customer_id = $2 AND s.revoked_at IS NULL",
    )
    .bind(&servers)
    .bind(customer)
    .fetch_all(&mut *conn)
    .await?;
    if let Some(missing) = servers.iter().find(|id| !found.contains(id)) {
        return Err(ApiError::Invalid(format!(
            "{missing} is not a server or device of this customer."
        )));
    }
    for member in &request.members {
        if let Some(check) = &member.check {
            known_checks(
                &state.pool,
                std::slice::from_ref(check),
                Place::Server(member.server_id),
            )
            .await?;
        }
    }
    Ok(Valid {
        customer,
        name,
        description: request.description.trim().to_owned(),
        at_least,
        target: request.target,
        members: request.members.clone(),
    })
}

type Row = (
    Uuid,
    Uuid,
    String,
    String,
    String,
    String,
    Option<i32>,
    Option<f64>,
    DateTime<Utc>,
    DateTime<Utc>,
);

const SELECT: &str = "SELECT b.id, b.customer_id, c.name, b.name, b.description, b.rule,
        b.at_least, b.target, b.created_at, b.updated_at
    FROM business_services b JOIN customers c ON c.id = b.customer_id";

async fn views(
    db: &mut PgConnection,
    user: &CurrentUser,
    rows: Vec<Row>,
) -> Result<Vec<ServiceView>, ApiError> {
    let ids: Vec<Uuid> = rows.iter().map(|r| r.0).collect();
    type Member = (Uuid, Uuid, String, String, Option<String>, String);
    let members: Vec<Member> = sqlx::query_as(
        "SELECT m.service_id, m.server_id, s.hostname, s.kind, m.check_name, m.check_instance
         FROM business_service_members m JOIN servers s ON s.id = m.server_id
         WHERE m.service_id = ANY($1) ORDER BY m.position, s.hostname, m.check_name",
    )
    .bind(&ids)
    .fetch_all(&mut *db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| ServiceView {
            members: members
                .iter()
                .filter(|m| m.0 == r.0)
                .map(|m| MemberView {
                    server_id: m.1,
                    server_name: m.2.clone(),
                    device: m.3 == "device",
                    check: m.4.clone().map(|name| CheckRef {
                        name,
                        instance: m.5.clone(),
                    }),
                })
                .collect(),
            can_change: user.modifiable_customers().contains(r.1),
            id: r.0,
            customer_id: r.1,
            customer_name: r.2,
            name: r.3,
            description: r.4,
            rule: r.5,
            at_least: r.6.and_then(|n| u32::try_from(n).ok()),
            target: r.7,
            created_at: r.8,
            updated_at: r.9,
        })
        .collect())
}

/// `GET /api/business-services`: the services of the customers the user
/// is granted as a whole.
pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
    params: Result<Query<ListParams>, QueryRejection>,
) -> Result<Json<ServiceList>, ApiError> {
    let Query(params) = params.map_err(|r| ApiError::Invalid(r.body_text()))?;
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    let customers = user.visible_customers();
    if let Some(customer) = params.customer_id {
        if !customers.contains(customer) {
            return Err(ApiError::NotFound);
        }
    }
    let (all, ids) = customers.bind();
    let mut conn = state.pool.acquire().await?;
    // SQL-safe: interpolates only the constant SELECT.
    let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{SELECT} WHERE ($1 OR b.customer_id = ANY($2))
           AND ($3::uuid IS NULL OR b.customer_id = $3)
         ORDER BY lower(b.name), b.id"
    )))
    .bind(all)
    .bind(&ids)
    .bind(params.customer_id)
    .fetch_all(&mut *conn)
    .await?;
    let services = views(&mut conn, &user, rows).await?;
    Ok(Json(ServiceList { services }))
}

async fn one(db: &mut PgConnection, user: &CurrentUser, id: Uuid) -> Result<ServiceView, ApiError> {
    // SQL-safe: interpolates only the constant SELECT.
    let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!("{SELECT} WHERE b.id = $1")))
        .bind(id)
        .fetch_all(&mut *db)
        .await?;
    views(db, user, rows).await?.pop().ok_or(ApiError::NotFound)
}

async fn store(db: &mut PgConnection, id: Uuid, valid: &Valid) -> Result<(), ApiError> {
    let result = sqlx::query(
        "UPDATE business_services SET customer_id = $2, name = $3, description = $4,
                rule = $5, at_least = $6, target = $7, updated_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(valid.customer)
    .bind(&valid.name)
    .bind(&valid.description)
    .bind(if valid.at_least.is_some() {
        "at_least"
    } else {
        "all"
    })
    .bind(valid.at_least)
    .bind(valid.target)
    .execute(&mut *db)
    .await;
    if let Err(sqlx::Error::Database(e)) = &result {
        if e.constraint() == Some("business_services_name_key") {
            return Err(ApiError::Conflict(format!(
                "There is already a service named {}.",
                valid.name
            )));
        }
    }
    result?;
    sqlx::query("DELETE FROM business_service_members WHERE service_id = $1")
        .bind(id)
        .execute(&mut *db)
        .await?;
    for (position, member) in valid.members.iter().enumerate() {
        sqlx::query(
            "INSERT INTO business_service_members
                 (service_id, server_id, check_name, check_instance, position)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(id)
        .bind(member.server_id)
        .bind(member.check.as_ref().map(|c| &c.name))
        .bind(member.check.as_ref().map_or("", |c| c.instance.as_str()))
        .bind(i32::try_from(position).unwrap_or(0))
        .execute(&mut *db)
        .await?;
    }
    Ok(())
}

fn audited(valid: &Valid) -> serde_json::Value {
    json!({
        "customer_id": valid.customer,
        "name": valid.name,
        "rule": if valid.at_least.is_some() { "at_least" } else { "all" },
        "at_least": valid.at_least,
        "target": valid.target,
        "members": valid.members,
    })
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
            target: Some(("business_service", &id.to_string())),
            details,
        },
    )
    .await
}

/// `POST /api/business-services`.
pub async fn create(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<ServiceRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<ServiceView>), ApiError> {
    let request = body(request)?;
    let valid = validate(&state, &user, &request).await?;
    let mut tx = state.pool.begin().await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO business_services (customer_id, name, created_by)
         VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(valid.customer)
    .bind(format!("new {}", Uuid::new_v4()))
    .bind(user.id)
    .fetch_one(&mut *tx)
    .await?;
    store(&mut tx, id, &valid).await?;
    record(
        &mut tx,
        &user,
        "business_service.create",
        id,
        audited(&valid),
    )
    .await?;
    let view = one(&mut tx, &user, id).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(view)))
}

/// The customer of the service `id` if the user sees it, or 404.
async fn visible(
    db: &mut PgConnection,
    user: &CurrentUser,
    id: &str,
) -> Result<(Uuid, Uuid), ApiError> {
    let id = parse_id(id)?;
    let customer: Option<Uuid> =
        sqlx::query_scalar("SELECT customer_id FROM business_services WHERE id = $1")
            .bind(id)
            .fetch_optional(&mut *db)
            .await?;
    match customer {
        Some(c) if user.visible_customers().contains(c) => {
            if user.modifiable_customers().contains(c) {
                Ok((id, c))
            } else {
                Err(ApiError::Forbidden)
            }
        }
        _ if !user.can_read() => Err(ApiError::Forbidden),
        _ => Err(ApiError::NotFound),
    }
}

/// `PUT /api/business-services/{id}`.
pub async fn update(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<ServiceRequest>, JsonRejection>,
) -> Result<Json<ServiceView>, ApiError> {
    let request = body(request)?;
    let mut conn = state.pool.acquire().await?;
    let (id, customer) = visible(&mut conn, &user, &id).await?;
    drop(conn);
    let request = ServiceRequest {
        customer_id: request.customer_id.or(Some(customer)),
        ..request
    };
    let valid = validate(&state, &user, &request).await?;
    let mut tx = state.pool.begin().await?;
    store(&mut tx, id, &valid).await?;
    record(
        &mut tx,
        &user,
        "business_service.update",
        id,
        audited(&valid),
    )
    .await?;
    let view = one(&mut tx, &user, id).await?;
    tx.commit().await?;
    Ok(Json(view))
}

/// `DELETE /api/business-services/{id}`: its past is gone from reports too.
pub async fn remove(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let mut tx = state.pool.begin().await?;
    let (id, customer) = visible(&mut tx, &user, &id).await?;
    let name: String =
        sqlx::query_scalar("DELETE FROM business_services WHERE id = $1 RETURNING name")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    record(
        &mut tx,
        &user,
        "business_service.remove",
        id,
        json!({"customer_id": customer, "name": name}),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> ServiceRequest {
        ServiceRequest {
            customer_id: None,
            name: " Email ".into(),
            description: String::new(),
            rule: "all".into(),
            at_least: None,
            target: Some(99.9),
            members: vec![
                MemberRequest {
                    server_id: Uuid::from_u128(1),
                    check: None,
                },
                MemberRequest {
                    server_id: Uuid::from_u128(2),
                    check: Some(CheckRef {
                        name: "service_postfix".into(),
                        instance: String::new(),
                    }),
                },
            ],
        }
    }

    #[test]
    fn a_sensible_request() {
        assert_eq!(shape(&request()), Ok(("Email".to_owned(), None)));
        let two = ServiceRequest {
            rule: "at_least".into(),
            at_least: Some(2),
            target: Some(99.995),
            ..request()
        };
        assert_eq!(shape(&two), Ok(("Email".to_owned(), Some(2))));
    }

    #[test]
    fn requests_that_say_nothing_sensible() {
        for bad in [
            ServiceRequest {
                name: "  ".into(),
                ..request()
            },
            ServiceRequest {
                members: vec![],
                ..request()
            },
            ServiceRequest {
                rule: "most".into(),
                ..request()
            },
            ServiceRequest {
                at_least: Some(1),
                ..request()
            },
            ServiceRequest {
                rule: "at_least".into(),
                at_least: Some(3),
                ..request()
            },
            ServiceRequest {
                target: Some(101.0),
                ..request()
            },
            ServiceRequest {
                target: Some(99.9999),
                ..request()
            },
            ServiceRequest {
                members: vec![request().members[0].clone(), request().members[0].clone()],
                ..request()
            },
        ] {
            assert!(shape(&bad).is_err(), "{bad:?}");
        }
    }
}
