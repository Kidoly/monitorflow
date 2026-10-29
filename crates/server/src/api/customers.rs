//! Customers (ADR-0026): `GET /api/customers`, creation and renaming of
//! customers and creation of sites under a customer by administrators,
//! from the API or the server CLI.

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgExecutor, PgPool};
use uuid::Uuid;

use super::access::Scope;
use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::sensors::{body, parse_id, UNIQUE_VIOLATION};
use super::sites::{self, SiteView};
use super::{ApiError, ApiState};

/// Longest customer or site name, in characters.
pub const MAX_NAME_LEN: usize = 64;

/// One customer.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct CustomerView {
    pub id: Uuid,
    pub name: String,
    /// Sites of the customer the user reads, not all of them.
    pub site_count: i64,
}

/// Body of `GET /api/customers`, sorted by name.
#[derive(Debug, Serialize)]
pub struct CustomerList {
    pub customers: Vec<CustomerView>,
}

/// Body of `POST /api/customers` and `POST /api/customers/{id}/sites`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NameRequest {
    pub name: String,
}

#[derive(Debug, thiserror::Error)]
pub enum TenancyError {
    #[error(
        "names must be 1 to {MAX_NAME_LEN} characters, without control characters or surrounding spaces"
    )]
    InvalidName,
    #[error("a {0} with this name already exists")]
    AlreadyExists(&'static str),
    #[error("site `{0}` belongs to another customer")]
    OtherCustomer(String),
    #[error("database query failed: {0}")]
    Database(#[from] sqlx::Error),
}

impl From<TenancyError> for ApiError {
    fn from(error: TenancyError) -> Self {
        match error {
            TenancyError::InvalidName => Self::Invalid(error.to_string()),
            TenancyError::AlreadyExists(_) | TenancyError::OtherCustomer(_) => {
                Self::Conflict(error.to_string())
            }
            TenancyError::Database(error) => Self::Database(error),
        }
    }
}

/// Customer and site names: they show in the interface and, for sites, in
/// the `site` label of every series (ADR-0013).
pub fn validate_name(name: &str) -> Result<(), TenancyError> {
    let len = name.chars().count();
    let valid = (1..=MAX_NAME_LEN).contains(&len)
        && name.trim() == name
        && !name.chars().any(char::is_control);
    valid.then_some(()).ok_or(TenancyError::InvalidName)
}

fn unique(error: sqlx::Error, what: &'static str) -> TenancyError {
    let code = error.as_database_error().and_then(|e| e.code());
    if code.is_some_and(|c| c == UNIQUE_VIOLATION) {
        TenancyError::AlreadyExists(what)
    } else {
        TenancyError::Database(error)
    }
}

/// Who creates: `("user", id)` from the API, `("system", "cli")` from the
/// CLI.
pub type Actor<'a> = (&'a str, &'a str);

/// Creates the customer `name` and audits it in one transaction.
pub async fn create_customer(
    pool: &PgPool,
    name: &str,
    actor: Actor<'_>,
) -> Result<Uuid, TenancyError> {
    validate_name(name)?;
    let mut tx = pool.begin().await?;
    let id: Uuid = sqlx::query_scalar("INSERT INTO customers (name) VALUES ($1) RETURNING id")
        .bind(name)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| unique(e, "customer"))?;
    let target = id.to_string();
    audit::record(
        &mut *tx,
        Event {
            actor_kind: actor.0,
            actor_id: actor.1,
            action: "customer.create",
            target: Some(("customer", &target)),
            details: json!({ "name": name }),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(id)
}

/// Renames `customer` to `name` and audits it; `false` when it does not
/// exist. Only the name changes: grants and sites refer to the ID.
pub async fn rename_customer(
    pool: &PgPool,
    customer: Uuid,
    name: &str,
    actor: Actor<'_>,
) -> Result<bool, TenancyError> {
    validate_name(name)?;
    let mut tx = pool.begin().await?;
    let old: Option<String> = sqlx::query_scalar(
        "UPDATE customers c SET name = $2 FROM customers o
         WHERE c.id = $1 AND o.id = c.id RETURNING o.name",
    )
    .bind(customer)
    .bind(name)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| unique(e, "customer"))?;
    let Some(old) = old else {
        return Ok(false);
    };
    if old != name {
        let target = customer.to_string();
        audit::record(
            &mut *tx,
            Event {
                actor_kind: actor.0,
                actor_id: actor.1,
                action: "customer.rename",
                target: Some(("customer", &target)),
                details: json!({ "from": old, "to": name }),
            },
        )
        .await?;
    }
    tx.commit().await?;
    Ok(true)
}

/// Creates the site `name` under `customer` and audits it.
pub async fn create_site(
    pool: &PgPool,
    customer: Uuid,
    name: &str,
    actor: Actor<'_>,
) -> Result<Uuid, TenancyError> {
    validate_name(name)?;
    let mut tx = pool.begin().await?;
    let id: Uuid =
        sqlx::query_scalar("INSERT INTO sites (name, customer_id) VALUES ($1, $2) RETURNING id")
            .bind(name)
            .bind(customer)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| unique(e, "site"))?;
    let target = id.to_string();
    audit::record(
        &mut *tx,
        Event {
            actor_kind: actor.0,
            actor_id: actor.1,
            action: "site.create",
            target: Some(("site", &target)),
            details: json!({ "name": name, "customer_id": customer }),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(id)
}

/// The site `name` if it belongs to `customer`, created when missing and
/// `create_missing`. A site of another customer is an error.
pub async fn site_of_customer(
    pool: &PgPool,
    name: &str,
    customer: Uuid,
    create_missing: bool,
) -> Result<Option<Uuid>, TenancyError> {
    let existing: Option<(Uuid, Uuid)> =
        sqlx::query_as("SELECT id, customer_id FROM sites WHERE name = $1")
            .bind(name)
            .fetch_optional(pool)
            .await?;
    match existing {
        Some((id, owner)) if owner == customer => Ok(Some(id)),
        Some(_) => Err(TenancyError::OtherCustomer(name.to_owned())),
        None if !create_missing => Ok(None),
        None => match create_site(pool, customer, name, ("system", "cli")).await {
            Ok(id) => Ok(Some(id)),
            // Created concurrently: check whose it is.
            Err(TenancyError::AlreadyExists(_)) => {
                Box::pin(site_of_customer(pool, name, customer, false)).await
            }
            Err(other) => Err(other),
        },
    }
}

/// ID of the customer `name`.
pub async fn customer_id<'e>(
    db: impl PgExecutor<'e>,
    name: &str,
) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar("SELECT id FROM customers WHERE name = $1")
        .bind(name)
        .fetch_optional(db)
        .await
}

/// Every customer with all its sites counted, for the CLI.
pub async fn list_all(pool: &PgPool) -> Result<Vec<CustomerView>, sqlx::Error> {
    visible(pool, &Scope::All, true, &[], None).await
}

/// Customers the user sees: those of a global grant, granted as a whole,
/// or with a readable site. Only `customer` when set.
async fn visible(
    pool: &PgPool,
    customers: &Scope,
    all_sites: bool,
    sites: &[Uuid],
    customer: Option<Uuid>,
) -> Result<Vec<CustomerView>, sqlx::Error> {
    let (all_customers, customers) = customers.bind();
    sqlx::query_as(
        "SELECT c.id, c.name,
                count(si.id) FILTER (WHERE $1 OR si.id = ANY($2)) AS site_count
         FROM customers c LEFT JOIN sites si ON si.customer_id = c.id
         WHERE $5::uuid IS NULL OR c.id = $5
         GROUP BY c.id
         HAVING $3 OR c.id = ANY($4) OR count(si.id) FILTER (WHERE si.id = ANY($2)) > 0
         ORDER BY c.name",
    )
    .bind(all_sites)
    .bind(sites)
    .bind(all_customers)
    .bind(&customers)
    .bind(customer)
    .fetch_all(pool)
    .await
}

async fn visible_to(
    state: &ApiState,
    user: &CurrentUser,
    customer: Option<Uuid>,
) -> Result<Vec<CustomerView>, ApiError> {
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    let (all_sites, sites) = user.readable_sites().bind();
    let customers = user.visible_customers();
    Ok(visible(&state.pool, &customers, all_sites, &sites, customer).await?)
}

/// `GET /api/customers`. Needs at least one role.
pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<CustomerList>, ApiError> {
    let customers = visible_to(&state, &user, None).await?;
    Ok(Json(CustomerList { customers }))
}

/// `POST /api/customers`: global administrators only.
pub async fn create(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<NameRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<CustomerView>), ApiError> {
    if user.administered_customers() != Scope::All {
        return Err(ApiError::Forbidden);
    }
    let name = body(request)?.name;
    let actor = user.id.to_string();
    let id = create_customer(&state.pool, &name, ("user", &actor)).await?;
    tracing::info!(user = %user.id, customer = %id, "customer created");
    let customer = CustomerView {
        id,
        name,
        site_count: 0,
    };
    Ok((StatusCode::CREATED, Json(customer)))
}

/// `PUT /api/customers/{id}`: renames a customer; global administrators
/// only, like creation.
pub async fn rename(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<NameRequest>, JsonRejection>,
) -> Result<Json<CustomerView>, ApiError> {
    if user.administered_customers() != Scope::All {
        return Err(ApiError::Forbidden);
    }
    let customer = parse_id(&id)?;
    let name = body(request)?.name;
    let actor = user.id.to_string();
    if !rename_customer(&state.pool, customer, &name, ("user", &actor)).await? {
        return Err(ApiError::NotFound);
    }
    tracing::info!(user = %user.id, %customer, "customer renamed");
    visible_to(&state, &user, Some(customer))
        .await?
        .pop()
        .map(Json)
        .ok_or(ApiError::NotFound)
}

/// `POST /api/customers/{id}/sites`: administrators of the customer. A
/// customer the user does not see is 404, one it sees without that right
/// 403.
pub async fn create_customer_site(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<NameRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<SiteView>), ApiError> {
    let customer = parse_id(&id)?;
    if visible_to(&state, &user, Some(customer)).await?.is_empty() {
        return Err(ApiError::NotFound);
    }
    if !user.administered_customers().contains(customer) {
        return Err(ApiError::Forbidden);
    }
    let name = body(request)?.name;
    let actor = user.id.to_string();
    let site = create_site(&state.pool, customer, &name, ("user", &actor)).await?;
    tracing::info!(user = %user.id, %customer, %site, "site created");
    let view = sites::fetch(&state, site)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok((StatusCode::CREATED, Json(view)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_validated() {
        for ok in ["acme", "Acme Corp", "é", &"a".repeat(64)] {
            assert!(validate_name(ok).is_ok(), "{ok}");
        }
        for bad in ["", " acme", "acme ", "a\tb", "a\u{7}", &"a".repeat(65)] {
            assert!(validate_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn errors_map_to_statuses() {
        let status = |e: TenancyError| ApiError::from(e).status();
        assert_eq!(status(TenancyError::InvalidName), StatusCode::BAD_REQUEST);
        assert_eq!(
            status(TenancyError::AlreadyExists("site")),
            StatusCode::CONFLICT
        );
        assert_eq!(
            status(TenancyError::OtherCustomer("x".into())),
            StatusCode::CONFLICT
        );
    }
}
