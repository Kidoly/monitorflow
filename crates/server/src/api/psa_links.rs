//! Customers matched to PSA clients, name suggestions, and the default
//! client of sites matched to nothing (ADR-0164, ADR-0191). Global
//! administrators only, like the rest of the PSA connection.

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgExecutor;
use uuid::Uuid;

use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::psa::{connected, recorded, require_global_admin, ConnectionView};
use super::sensors::{body, parse_id};
use super::{ApiError, ApiState};
use crate::psa::{self, names};

/// One MonitorFlow customer and the PSA client it is matched to.
#[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
#[schema(as = PsaCustomerLink)]
pub struct CustomerLink {
    pub customer_id: Uuid,
    pub customer_name: String,
    /// The PSA client; `null` when not matched.
    pub client_id: Option<i64>,
    pub client_name: Option<String>,
    /// When it was matched; `null` when not.
    pub linked_at: Option<DateTime<Utc>>,
    /// Sites of the customer, and how many have a match of their own.
    pub site_count: i64,
    pub matched_sites: i64,
}

/// Body of `GET /api/psa/customer-links`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = PsaCustomerLinkList)]
pub struct CustomerLinkList {
    /// Every customer, by name.
    pub customers: Vec<CustomerLink>,
}

/// A customer and the PSA client whose name reads the same.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = PsaSuggestion)]
pub struct Suggestion {
    pub customer_id: Uuid,
    pub customer_name: String,
    pub client_id: i64,
    pub client_name: String,
}

/// Body of `GET /api/psa/customer-links/suggestions`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = PsaSuggestionList)]
pub struct SuggestionList {
    /// For customers without a match, by customer name.
    pub suggestions: Vec<Suggestion>,
}

/// Body of `PUT /api/psa/customer-links/{customer_id}`.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[schema(as = PsaCustomerLinkRequest)]
#[serde(deny_unknown_fields)]
pub struct CustomerLinkRequest {
    /// The PSA client, an `id` of `GET /api/psa/clients`.
    pub client_id: i64,
}

/// Body of `PUT /api/psa/default-client`.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[schema(as = PsaDefaultClientRequest)]
#[serde(deny_unknown_fields)]
pub struct DefaultClientRequest {
    /// The PSA client; `null` opens no ticket for sites matched to
    /// nothing.
    #[serde(default)]
    pub client_id: Option<i64>,
    /// One of its sites; absent or `null` lets the PSA choose.
    #[serde(default)]
    pub psa_site_id: Option<i64>,
}

const CUSTOMERS: &str = "SELECT c.id AS customer_id, c.name AS customer_name, l.client_id,
        l.client_name, l.linked_at,
        (SELECT count(*) FROM sites s WHERE s.customer_id = c.id) AS site_count,
        (SELECT count(*) FROM sites s JOIN psa_site_links sl ON sl.site_id = s.id
         WHERE s.customer_id = c.id) AS matched_sites
    FROM customers c
    LEFT JOIN psa_customer_links l ON l.customer_id = c.id";

/// `GET /api/psa/customer-links`: every customer and its match.
pub async fn customer_links(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<CustomerLinkList>, ApiError> {
    require_global_admin(&user)?;
    // SQL-safe: interpolates only the constant CUSTOMERS.
    let customers = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{CUSTOMERS} ORDER BY lower(c.name), c.id"
    )))
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(CustomerLinkList { customers }))
}

async fn customer_link(db: impl PgExecutor<'_>, id: Uuid) -> Result<CustomerLink, ApiError> {
    // SQL-safe: interpolates only the constant CUSTOMERS.
    sqlx::query_as(sqlx::AssertSqlSafe(format!("{CUSTOMERS} WHERE c.id = $1")))
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or(ApiError::NotFound)
}

/// `GET /api/psa/customer-links/suggestions`: for customers without a
/// match, the PSA client whose name reads the same once case, accents,
/// punctuation and company suffixes are left out. Reads every client of
/// the PSA, up to 3,000.
pub async fn suggestions(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<SuggestionList>, ApiError> {
    require_global_admin(&user)?;
    let (connection, halo) = connected(&state).await?;
    let unmatched: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT c.id, c.name FROM customers c
         WHERE NOT EXISTS (SELECT 1 FROM psa_customer_links l WHERE l.customer_id = c.id)
         ORDER BY lower(c.name), c.id",
    )
    .fetch_all(&state.pool)
    .await?;
    if unmatched.is_empty() {
        return Ok(Json(SuggestionList {
            suggestions: Vec::new(),
        }));
    }
    let taken: Vec<i64> =
        sqlx::query_scalar("SELECT client_id FROM psa_customer_links WHERE connection_id = $1")
            .bind(connection.id)
            .fetch_all(&state.pool)
            .await?;
    let clients = recorded(&state, &connection, halo.all_clients().await).await?;
    let suggestions = names::suggest(&unmatched, &clients, &taken)
        .into_iter()
        .map(|(customer, client)| Suggestion {
            customer_id: customer,
            customer_name: unmatched
                .iter()
                .find(|(id, _)| *id == customer)
                .map(|(_, n)| n.clone())
                .unwrap_or_default(),
            client_id: client.id,
            client_name: client.name.clone(),
        })
        .collect();
    Ok(Json(SuggestionList { suggestions }))
}

async fn audited(
    db: impl PgExecutor<'_>,
    user: &CurrentUser,
    action: &str,
    target: (&str, &str),
    details: serde_json::Value,
) -> Result<(), ApiError> {
    let actor = user.id.to_string();
    audit::record(
        db,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action,
            target: Some(target),
            details,
        },
    )
    .await?;
    Ok(())
}

/// `PUT /api/psa/customer-links/{customer_id}`: matches a customer to a
/// PSA client, checked with the PSA; audited as `psa.link.update`.
pub async fn put_customer_link(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(customer): Path<String>,
    request: Result<Json<CustomerLinkRequest>, JsonRejection>,
) -> Result<Json<CustomerLink>, ApiError> {
    require_global_admin(&user)?;
    let customer = parse_id(&customer)?;
    let request = body(request)?;
    let current = customer_link(&state.pool, customer).await?;
    let (connection, halo) = connected(&state).await?;
    let client = recorded(&state, &connection, halo.client(request.client_id).await)
        .await?
        .ok_or_else(|| ApiError::Invalid("HaloPSA has no such client.".into()))?;
    let mut tx = state.pool.begin().await?;
    let taken: Option<String> = sqlx::query_scalar(
        "SELECT c.name FROM psa_customer_links l JOIN customers c ON c.id = l.customer_id
         WHERE l.connection_id = $1 AND l.client_id = $2 AND l.customer_id <> $3",
    )
    .bind(connection.id)
    .bind(client.id)
    .bind(customer)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(other) = taken {
        return Err(ApiError::Conflict(format!(
            "That HaloPSA client is already matched to {other}."
        )));
    }
    let elsewhere: Option<String> = sqlx::query_scalar(
        "SELECT s.name FROM psa_site_links l JOIN sites s ON s.id = l.site_id
         WHERE s.customer_id = $1 AND l.client_id <> $2
         ORDER BY lower(s.name) LIMIT 1",
    )
    .bind(customer)
    .bind(client.id)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(site) = elsewhere {
        return Err(ApiError::Conflict(format!(
            "The site {site} of {} is matched to another HaloPSA client: change or remove its match first.",
            current.customer_name
        )));
    }
    sqlx::query(
        "INSERT INTO psa_customer_links (customer_id, connection_id, client_id, client_name)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (customer_id) DO UPDATE SET connection_id = $2, client_id = $3,
             client_name = $4, linked_at = now()",
    )
    .bind(customer)
    .bind(connection.id)
    .bind(client.id)
    .bind(&client.name)
    .execute(&mut *tx)
    .await?;
    let target = customer.to_string();
    audited(
        &mut *tx,
        &user,
        "psa.link.update",
        ("customer", &target),
        json!({ "client_id": client.id }),
    )
    .await?;
    let link = customer_link(&mut *tx, customer).await?;
    tx.commit().await?;
    Ok(Json(link))
}

/// `DELETE /api/psa/customer-links/{customer_id}`: 204, 404 when it was
/// not matched; audited as `psa.link.delete`.
pub async fn delete_customer_link(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(customer): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_global_admin(&user)?;
    let customer = parse_id(&customer)?;
    let mut tx = state.pool.begin().await?;
    let removed: Option<i64> = sqlx::query_scalar(
        "DELETE FROM psa_customer_links WHERE customer_id = $1 RETURNING client_id",
    )
    .bind(customer)
    .fetch_optional(&mut *tx)
    .await?;
    let client = removed.ok_or(ApiError::NotFound)?;
    let target = customer.to_string();
    audited(
        &mut *tx,
        &user,
        "psa.link.delete",
        ("customer", &target),
        json!({ "client_id": client }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `PUT /api/psa/default-client`: the PSA client, and optionally one of
/// its sites, that incidents of sites matched to nothing open tickets
/// for; checked with the PSA; audited as `psa.connection.default`.
pub async fn put_default_client(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<DefaultClientRequest>, JsonRejection>,
) -> Result<Json<ConnectionView>, ApiError> {
    require_global_admin(&user)?;
    let request = body(request)?;
    let (connection, halo) = connected(&state).await?;
    let client = match request.client_id {
        None => None,
        Some(id) => Some(
            recorded(&state, &connection, halo.client(id).await)
                .await?
                .ok_or_else(|| ApiError::Invalid("HaloPSA has no such client.".into()))?,
        ),
    };
    let site = match (&client, request.psa_site_id) {
        (None, Some(_)) => {
            return Err(ApiError::Invalid(
                "Choose the client before one of its sites.".into(),
            ))
        }
        (Some(client), Some(id)) => {
            let (found, owner) = recorded(&state, &connection, halo.site(id).await)
                .await?
                .ok_or_else(|| ApiError::Invalid("HaloPSA has no such site.".into()))?;
            if owner.is_some_and(|owner| owner != client.id) {
                return Err(ApiError::Invalid(format!(
                    "That HaloPSA site belongs to another client than {}.",
                    client.name
                )));
            }
            Some(found)
        }
        _ => None,
    };
    let mut tx = state.pool.begin().await?;
    sqlx::query(
        "UPDATE psa_connections SET default_client_id = $2, default_client_name = $3,
             default_site_id = $4, default_site_name = $5, updated_at = now()
         WHERE id = $1",
    )
    .bind(connection.id)
    .bind(client.as_ref().map(|c| c.id))
    .bind(client.as_ref().map(|c| c.name.as_str()))
    .bind(site.as_ref().map(|s| s.id))
    .bind(site.as_ref().map(|s| s.name.as_str()))
    .execute(&mut *tx)
    .await?;
    audited(
        &mut *tx,
        &user,
        "psa.connection.default",
        ("psa_connection", psa::HALOPSA),
        json!({
            "client_id": client.as_ref().map(|c| c.id),
            "psa_site_id": site.as_ref().map(|s| s.id),
        }),
    )
    .await?;
    let saved = psa::load(&mut *tx).await?.ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(Json(saved.into()))
}
