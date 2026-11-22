//! The PSA connection (ADR-0164, ADR-0166): one per installation, HaloPSA
//! first, managed by global administrators only, since one PSA serves
//! every customer and its credentials reach all of them. The client secret
//! is sealed at rest and never returned; an edit without it keeps it only
//! for the same address. The address may be private: a global
//! administrator controls the server anyway (ADR-0163).

use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use monitorflow_common::redact::Secret;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgExecutor;

use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::notifications::is_global_admin;
use super::sensors::{body, parse_id};
use super::{ApiError, ApiState};
use crate::psa::halopsa::{HaloError, Record};
use crate::psa::{self, halopsa, Connection, Credentials, Halo, HALOPSA, MAX_ID};

/// The connection as the web interface shows it; never its secret.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[schema(as = PsaConnection)]
pub struct ConnectionView {
    /// `halopsa`, the only kind so far.
    pub kind: String,
    /// The PSA's address, like `https://halo.example.com`.
    pub base_url: String,
    /// The tenant named on the token request of hosted instances; `null`
    /// when none.
    pub tenant: Option<String>,
    /// The API application's Client ID.
    pub client_id: String,
    /// Why the stored secret does not open (ADR-0125), so it must be
    /// entered again; `null` when it opens.
    pub secret_error: Option<String>,
    /// Whether MonitorFlow uses the connection.
    pub enabled: bool,
    /// The PSA agent the application acts as, from the last success;
    /// `null` before the first.
    pub agent_name: Option<String>,
    /// The last request that worked; `null` before the first.
    pub last_success_at: Option<DateTime<Utc>>,
    /// The last failure, in the PSA's words when it gave some; `null`
    /// once a request works again.
    pub last_error: Option<String>,
    /// When `last_error` happened.
    pub last_error_at: Option<DateTime<Utc>>,
    /// When it was set up.
    pub created_at: DateTime<Utc>,
    /// When it was last changed.
    pub updated_at: DateTime<Utc>,
    /// What tickets it opens and how it closes them.
    pub tickets: psa::TicketSettings,
}

impl From<Connection> for ConnectionView {
    fn from(c: Connection) -> Self {
        Self {
            kind: c.kind,
            base_url: c.base_url,
            tenant: c.tenant,
            client_id: c.client_id,
            secret_error: c.client_secret.err(),
            enabled: c.enabled,
            agent_name: c.agent_name,
            last_success_at: c.last_success_at,
            last_error: c.last_error,
            last_error_at: c.last_error_at,
            created_at: c.created_at,
            updated_at: c.updated_at,
            tickets: c.tickets,
        }
    }
}

/// Body of `GET /api/psa/connection`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = PsaConnectionState)]
pub struct ConnectionState {
    /// The connection; `null` when none is set up.
    pub connection: Option<ConnectionView>,
}

/// Body of `PUT /api/psa/connection` and of
/// `POST /api/psa/connection/test`.
#[derive(Debug, Default, Deserialize, utoipa::ToSchema)]
#[schema(as = PsaConnectionRequest)]
#[serde(deny_unknown_fields)]
pub struct ConnectionRequest {
    /// `halopsa`; absent means `halopsa`.
    #[serde(default)]
    pub kind: Option<String>,
    /// The PSA's address, like `https://halo.example.com`; a trailing
    /// `/api` is taken off. On the test, absent means the stored one.
    #[serde(default)]
    pub base_url: Option<String>,
    /// The tenant of a hosted instance; absent, `null` or empty for none.
    #[serde(default)]
    pub tenant: Option<String>,
    /// The API application's Client ID. On the test, absent means the
    /// stored one.
    #[serde(default)]
    pub client_id: Option<String>,
    /// The application's Client Secret. Write-only. Absent or empty keeps
    /// the stored one, but only for the same address.
    #[serde(default)]
    pub client_secret: Option<String>,
    /// Whether MonitorFlow uses it; absent means `true`. Ignored by the
    /// test.
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// Body of `POST /api/psa/connection/test`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = PsaConnectionTest)]
pub struct TestResult {
    /// Whether MonitorFlow signed in and read the application's agent.
    pub ok: bool,
    /// The PSA agent the application acts as, when it worked.
    pub agent_name: Option<String>,
    /// Why it failed, in the PSA's words when it gave some; `null` when
    /// it worked.
    pub error: Option<String>,
}

fn require_global_admin(user: &CurrentUser) -> Result<(), ApiError> {
    if is_global_admin(user) {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}

/// `text` trimmed, `None` when empty.
fn given(text: Option<String>) -> Option<String> {
    text.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty())
}

/// A short identifier: at most [`MAX_ID`] characters, no control ones.
fn identifier(text: String, what: &str) -> Result<String, ApiError> {
    if text.chars().count() > MAX_ID || text.chars().any(char::is_control) {
        return Err(ApiError::Invalid(format!(
            "The {what} is at most {MAX_ID} characters, on one line."
        )));
    }
    Ok(text)
}

/// The credentials `request` asks for, completed from `current` where it
/// leaves something out. The secret comes from `current` only for the
/// same address, or a changed address could send it elsewhere.
fn credentials(
    request: ConnectionRequest,
    current: Option<&Connection>,
    for_test: bool,
) -> Result<Credentials, ApiError> {
    if request.kind.as_deref().is_some_and(|k| k != HALOPSA) {
        return Err(ApiError::Invalid("`kind` is `halopsa`.".into()));
    }
    let base_url = match (given(request.base_url), current) {
        (Some(url), _) => halopsa::normalize_base_url(&url).map_err(ApiError::Invalid)?,
        (None, Some(current)) if for_test => current.base_url.clone(),
        (None, _) => halopsa::normalize_base_url("").map_err(ApiError::Invalid)?,
    };
    let client_id = match (given(request.client_id), current) {
        (Some(id), _) => identifier(id, "Client ID")?,
        (None, Some(current)) if for_test => current.client_id.clone(),
        (None, _) => return Err(ApiError::Invalid("Enter the Client ID.".into())),
    };
    let tenant = given(request.tenant)
        .map(|t| identifier(t, "tenant"))
        .transpose()?;
    let tenant = if for_test && tenant.is_none() {
        current.and_then(|c| c.tenant.clone())
    } else {
        tenant
    };
    let client_secret = match given(request.client_secret) {
        Some(secret) => Secret::new(secret),
        None => match current.filter(|c| c.base_url == base_url) {
            Some(current) => current
                .client_secret
                .clone()
                .map_err(|_| ApiError::Invalid("Enter the Client Secret again.".into()))?,
            None => return Err(ApiError::Invalid("Enter the Client Secret.".into())),
        },
    };
    Ok(Credentials {
        base_url,
        tenant,
        client_id,
        client_secret,
    })
}

/// What the audit keeps: never the secret.
fn audited(credentials: &Credentials, enabled: bool, secret_changed: bool) -> Value {
    json!({
        "kind": HALOPSA,
        "base_url": credentials.base_url,
        "tenant": credentials.tenant,
        "client_id": credentials.client_id,
        "enabled": enabled,
        "secret_changed": secret_changed,
    })
}

async fn record(
    db: impl PgExecutor<'_>,
    user: &CurrentUser,
    action: &str,
    details: Value,
) -> Result<(), ApiError> {
    let actor = user.id.to_string();
    audit::record(
        db,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action,
            target: Some(("psa_connection", HALOPSA)),
            details,
        },
    )
    .await?;
    Ok(())
}

/// `GET /api/psa/connection`: global administrators.
pub async fn get_connection(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<ConnectionState>, ApiError> {
    require_global_admin(&user)?;
    let connection = psa::load(&state.pool).await?.map(ConnectionView::from);
    Ok(Json(ConnectionState { connection }))
}

/// `PUT /api/psa/connection`: sets up or changes the connection; global
/// administrators; audited as `psa.connection.create` or
/// `psa.connection.update`. Saving tests nothing: the test is its own
/// request.
pub async fn put_connection(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<ConnectionRequest>, JsonRejection>,
) -> Result<Json<ConnectionView>, ApiError> {
    require_global_admin(&user)?;
    let request = body(request)?;
    let enabled = request.enabled.unwrap_or(true);
    let secret_given = given(request.client_secret.clone()).is_some();
    let mut tx = state.pool.begin().await?;
    let current = psa::lock(&mut *tx).await?;
    let credentials = credentials(request, current.as_ref(), false)?;
    let sealed = psa::sealed(&credentials.client_secret)?;
    // A new address or ID makes the last state meaningless.
    let same = current.as_ref().is_some_and(|c| {
        c.base_url == credentials.base_url && c.client_id == credentials.client_id
    });
    sqlx::query(
        "INSERT INTO psa_connections (kind, base_url, tenant, client_id, client_secret, enabled)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (single) DO UPDATE SET kind = $1, base_url = $2, tenant = $3,
             client_id = $4, client_secret = $5, enabled = $6, updated_at = now(),
             agent_name = CASE WHEN $7 THEN psa_connections.agent_name END,
             last_success_at = CASE WHEN $7 THEN psa_connections.last_success_at END,
             last_error = CASE WHEN $7 THEN psa_connections.last_error END,
             last_error_at = CASE WHEN $7 THEN psa_connections.last_error_at END",
    )
    .bind(HALOPSA)
    .bind(&credentials.base_url)
    .bind(&credentials.tenant)
    .bind(&credentials.client_id)
    .bind(sealed)
    .bind(enabled)
    .bind(same)
    .execute(&mut *tx)
    .await?;
    // Ids of another instance mean nothing here.
    if current
        .as_ref()
        .is_some_and(|c| c.base_url != credentials.base_url)
    {
        sqlx::query("DELETE FROM psa_site_links")
            .execute(&mut *tx)
            .await?;
    }
    let action = if current.is_some() {
        "psa.connection.update"
    } else {
        "psa.connection.create"
    };
    record(
        &mut *tx,
        &user,
        action,
        audited(&credentials, enabled, secret_given),
    )
    .await?;
    let saved = psa::load(&mut *tx).await?.ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    tracing::info!(user = %user.id, "saved the PSA connection");
    Ok(Json(saved.into()))
}

/// `DELETE /api/psa/connection`: global administrators; 204, or 404 when
/// there is none; audited as `psa.connection.delete`.
pub async fn delete_connection(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<StatusCode, ApiError> {
    require_global_admin(&user)?;
    let mut tx = state.pool.begin().await?;
    let current = psa::lock(&mut *tx).await?.ok_or(ApiError::NotFound)?;
    sqlx::query("DELETE FROM psa_connections WHERE id = $1")
        .bind(current.id)
        .execute(&mut *tx)
        .await?;
    record(
        &mut *tx,
        &user,
        "psa.connection.delete",
        json!({ "kind": current.kind, "base_url": current.base_url }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/psa/connection/test`: signs in and reads the application's
/// agent, read-only, with the body's values over the stored ones, so a
/// form can be tested before it is saved. Testing the stored connection
/// as it is records the outcome on it.
pub async fn test_connection(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<ConnectionRequest>, JsonRejection>,
) -> Result<Json<TestResult>, ApiError> {
    require_global_admin(&user)?;
    let request = body(request)?;
    let current = psa::load(&state.pool).await?;
    let credentials = credentials(request, current.as_ref(), true)?;
    let stored = current.as_ref().filter(|c| {
        c.base_url == credentials.base_url
            && c.client_id == credentials.client_id
            && c.tenant == credentials.tenant
            && c.client_secret.as_ref().ok() == Some(&credentials.client_secret)
    });
    let outcome = Halo::new(credentials).me().await;
    if let Some(stored) = stored {
        match &outcome {
            Ok(agent) => psa::record_success(&state.pool, stored.id, Some(&agent.name)).await?,
            Err(error) => psa::record_failure(&state.pool, stored.id, &error.message).await?,
        }
    }
    Ok(Json(match outcome {
        Ok(agent) => TestResult {
            ok: true,
            agent_name: Some(agent.name),
            error: None,
        },
        Err(error) => TestResult {
            ok: false,
            agent_name: None,
            error: Some(error.message),
        },
    }))
}

/// Query string of `GET /api/psa/clients`.
#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ClientSearch {
    /// Part of the client's name; absent or empty lists the first ones.
    pub search: Option<String>,
}

/// Body of `GET /api/psa/clients` and `GET /api/psa/clients/{id}/sites`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = PsaRecordList)]
pub struct RecordList {
    /// By name; clients at most 50 per search.
    pub records: Vec<Record>,
}

/// One MonitorFlow site and the PSA client and site it is matched to.
#[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
#[schema(as = PsaSiteLink)]
pub struct SiteLink {
    /// The MonitorFlow site.
    pub site_id: uuid::Uuid,
    pub site_name: String,
    /// Its customer.
    pub customer_id: uuid::Uuid,
    pub customer_name: String,
    /// The PSA client its tickets go to; `null` when not matched.
    pub client_id: Option<i64>,
    /// The client's name when matched; `null` otherwise.
    pub client_name: Option<String>,
    /// The PSA site within that client; `null` for none.
    pub psa_site_id: Option<i64>,
    pub psa_site_name: Option<String>,
    /// When it was matched; `null` when not.
    pub linked_at: Option<DateTime<Utc>>,
}

/// Body of `GET /api/psa/site-links`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = PsaSiteLinkList)]
pub struct SiteLinkList {
    /// Every MonitorFlow site, by customer and name.
    pub sites: Vec<SiteLink>,
}

/// Body of `PUT /api/psa/site-links/{site_id}`.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[schema(as = PsaSiteLinkRequest)]
#[serde(deny_unknown_fields)]
pub struct SiteLinkRequest {
    /// The PSA client, an `id` of `GET /api/psa/clients`.
    pub client_id: i64,
    /// One of that client's sites, from `GET /api/psa/clients/{id}/sites`;
    /// absent or `null` lets the PSA choose.
    #[serde(default)]
    pub psa_site_id: Option<i64>,
}

/// The connection and a client for it; 409 when there is none.
async fn connected(state: &ApiState) -> Result<(Connection, Halo), ApiError> {
    let connection = psa::load(&state.pool)
        .await?
        .ok_or_else(|| ApiError::Conflict("Set up the HaloPSA connection first.".into()))?;
    let credentials = connection
        .credentials()
        .map_err(|e| ApiError::PsaUnavailable(e.message))?;
    Ok((connection, Halo::new(credentials)))
}

/// `outcome` with the PSA's failure recorded on the connection and turned
/// into a 502.
async fn recorded<T>(
    state: &ApiState,
    connection: &Connection,
    outcome: Result<T, HaloError>,
) -> Result<T, ApiError> {
    match outcome {
        Ok(value) => {
            psa::record_success(&state.pool, connection.id, None).await?;
            Ok(value)
        }
        Err(error) => {
            psa::record_failure(&state.pool, connection.id, &error.message).await?;
            Err(ApiError::PsaUnavailable(error.message))
        }
    }
}

/// `GET /api/psa/clients`: the PSA's active clients, read through the
/// connection; global administrators.
pub async fn clients(
    State(state): State<ApiState>,
    user: CurrentUser,
    query: Result<Query<ClientSearch>, QueryRejection>,
) -> Result<Json<RecordList>, ApiError> {
    require_global_admin(&user)?;
    let Query(query) = query.map_err(|e| ApiError::Invalid(e.body_text()))?;
    let (connection, halo) = connected(&state).await?;
    let search = query.search.unwrap_or_default();
    let records = recorded(&state, &connection, halo.clients(&search).await).await?;
    Ok(Json(RecordList { records }))
}

/// `GET /api/psa/clients/{id}/sites`: the sites of one PSA client.
pub async fn client_sites(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<RecordList>, ApiError> {
    require_global_admin(&user)?;
    let (connection, halo) = connected(&state).await?;
    let records = recorded(&state, &connection, halo.sites(id).await).await?;
    Ok(Json(RecordList { records }))
}

const LINKS: &str = "SELECT s.id AS site_id, s.name AS site_name, c.id AS customer_id,
        c.name AS customer_name, l.client_id, l.client_name, l.halo_site_id AS psa_site_id,
        l.halo_site_name AS psa_site_name, l.linked_at
    FROM sites s
    JOIN customers c ON c.id = s.customer_id
    LEFT JOIN psa_site_links l ON l.site_id = s.id";

/// `GET /api/psa/site-links`: every site with its match; global
/// administrators.
pub async fn site_links(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<SiteLinkList>, ApiError> {
    require_global_admin(&user)?;
    // SQL-safe: interpolates only the constant LINKS.
    let sites = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{LINKS} ORDER BY lower(c.name), lower(s.name), s.id"
    )))
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(SiteLinkList { sites }))
}

async fn site_link(db: impl PgExecutor<'_>, site: uuid::Uuid) -> Result<SiteLink, ApiError> {
    // SQL-safe: interpolates only the constant LINKS.
    sqlx::query_as(sqlx::AssertSqlSafe(format!("{LINKS} WHERE s.id = $1")))
        .bind(site)
        .fetch_optional(db)
        .await?
        .ok_or(ApiError::NotFound)
}

/// `PUT /api/psa/site-links/{site_id}`: matches a site to a PSA client and
/// optionally one of its sites, checked with the PSA, which also gives the
/// names kept; audited as `psa.link.update`.
pub async fn put_site_link(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(site): Path<String>,
    request: Result<Json<SiteLinkRequest>, JsonRejection>,
) -> Result<Json<SiteLink>, ApiError> {
    require_global_admin(&user)?;
    let site = parse_id(&site)?;
    let request = body(request)?;
    site_link(&state.pool, site).await?;
    let (connection, halo) = connected(&state).await?;
    let client = recorded(&state, &connection, halo.client(request.client_id).await)
        .await?
        .ok_or_else(|| ApiError::Invalid("HaloPSA has no such client.".into()))?;
    let psa_site = match request.psa_site_id {
        None => None,
        Some(id) => {
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
    };
    let mut tx = state.pool.begin().await?;
    let taken: Option<String> = match &psa_site {
        Some(found) => {
            sqlx::query_scalar(
                "SELECT s.name FROM psa_site_links l JOIN sites s ON s.id = l.site_id
             WHERE l.connection_id = $1 AND l.halo_site_id = $2 AND l.site_id <> $3",
            )
            .bind(connection.id)
            .bind(found.id)
            .bind(site)
            .fetch_optional(&mut *tx)
            .await?
        }
        None => None,
    };
    if let Some(other) = taken {
        return Err(ApiError::Conflict(format!(
            "That HaloPSA site is already matched to {other}."
        )));
    }
    sqlx::query(
        "INSERT INTO psa_site_links (site_id, connection_id, client_id, client_name,
                                     halo_site_id, halo_site_name)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (site_id) DO UPDATE SET connection_id = $2, client_id = $3,
             client_name = $4, halo_site_id = $5, halo_site_name = $6, linked_at = now()",
    )
    .bind(site)
    .bind(connection.id)
    .bind(client.id)
    .bind(&client.name)
    .bind(psa_site.as_ref().map(|s| s.id))
    .bind(psa_site.as_ref().map(|s| s.name.as_str()))
    .execute(&mut *tx)
    .await?;
    let actor = user.id.to_string();
    let target = site.to_string();
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action: "psa.link.update",
            target: Some(("site", &target)),
            details: json!({
                "client_id": client.id,
                "psa_site_id": psa_site.as_ref().map(|s| s.id),
            }),
        },
    )
    .await?;
    let link = site_link(&mut *tx, site).await?;
    tx.commit().await?;
    Ok(Json(link))
}

/// `DELETE /api/psa/site-links/{site_id}`: the site's incidents open no
/// more tickets; 204, 404 when it was not matched; audited as
/// `psa.link.delete`.
pub async fn delete_site_link(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(site): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_global_admin(&user)?;
    let site = parse_id(&site)?;
    let mut tx = state.pool.begin().await?;
    let removed: Option<i64> =
        sqlx::query_scalar("DELETE FROM psa_site_links WHERE site_id = $1 RETURNING client_id")
            .bind(site)
            .fetch_optional(&mut *tx)
            .await?;
    let client = removed.ok_or(ApiError::NotFound)?;
    let actor = user.id.to_string();
    let target = site.to_string();
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action: "psa.link.delete",
            target: Some(("site", &target)),
            details: json!({ "client_id": client }),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Body of `GET /api/psa/categories`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = PsaCategoryList)]
pub struct CategoryList {
    /// First-level categories as tickets take them, like `Hardware>Server`.
    pub categories: Vec<String>,
}

/// Body of `PUT /api/psa/ticket-settings`.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[schema(as = PsaTicketSettingsRequest)]
#[serde(deny_unknown_fields)]
pub struct TicketSettingsRequest {
    /// A ticket type of `GET /api/psa/ticket-types`; `null` for the
    /// PSA's default.
    #[serde(default)]
    pub ticket_type_id: Option<i64>,
    /// A category of `GET /api/psa/categories`; `null` or empty for none.
    #[serde(default)]
    pub category: Option<String>,
    /// `critical` or `warning`; absent keeps the current one.
    #[serde(default)]
    pub min_severity: Option<String>,
    /// A status of `GET /api/psa/statuses` that resolved incidents move
    /// their ticket to; `null` adds a note only.
    #[serde(default)]
    pub resolved_status_id: Option<i64>,
}

/// `GET /api/psa/ticket-types`: the PSA's ticket types for tickets.
pub async fn ticket_types(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<RecordList>, ApiError> {
    require_global_admin(&user)?;
    let (connection, halo) = connected(&state).await?;
    let records = recorded(&state, &connection, halo.ticket_types().await).await?;
    Ok(Json(RecordList { records }))
}

/// `GET /api/psa/categories`: the PSA's first-level categories.
pub async fn categories(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<CategoryList>, ApiError> {
    require_global_admin(&user)?;
    let (connection, halo) = connected(&state).await?;
    let categories = recorded(&state, &connection, halo.categories().await).await?;
    Ok(Json(CategoryList { categories }))
}

/// `GET /api/psa/statuses`: the PSA's ticket statuses.
pub async fn statuses(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<RecordList>, ApiError> {
    require_global_admin(&user)?;
    let (connection, halo) = connected(&state).await?;
    let records = recorded(&state, &connection, halo.statuses().await).await?;
    Ok(Json(RecordList { records }))
}

/// `PUT /api/psa/ticket-settings`: the ticket type, category, least
/// severity and resolved status, each checked against the PSA's lists;
/// audited as `psa.connection.tickets`.
pub async fn put_ticket_settings(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<TicketSettingsRequest>, JsonRejection>,
) -> Result<Json<ConnectionView>, ApiError> {
    require_global_admin(&user)?;
    let request = body(request)?;
    let (connection, halo) = connected(&state).await?;
    let min_severity = request
        .min_severity
        .unwrap_or_else(|| connection.tickets.min_severity.clone());
    if !["warning", "critical"].contains(&min_severity.as_str()) {
        return Err(ApiError::Invalid(
            "`min_severity` is `warning` or `critical`.".into(),
        ));
    }
    let ticket_type = match request.ticket_type_id {
        None => None,
        Some(id) => Some(
            recorded(&state, &connection, halo.ticket_types().await)
                .await?
                .into_iter()
                .find(|t| t.id == id)
                .ok_or_else(|| ApiError::Invalid("HaloPSA has no such ticket type.".into()))?,
        ),
    };
    let category = given(request.category);
    if let Some(category) = &category {
        let known = recorded(&state, &connection, halo.categories().await).await?;
        if !known.contains(category) {
            return Err(ApiError::Invalid("HaloPSA has no such category.".into()));
        }
    }
    let status = match request.resolved_status_id {
        None => None,
        Some(id) => Some(
            recorded(&state, &connection, halo.statuses().await)
                .await?
                .into_iter()
                .find(|s| s.id == id)
                .ok_or_else(|| ApiError::Invalid("HaloPSA has no such status.".into()))?,
        ),
    };
    let mut tx = state.pool.begin().await?;
    sqlx::query(
        "UPDATE psa_connections SET ticket_type_id = $2, ticket_type_name = $3, category = $4,
             min_severity = $5, resolved_status_id = $6, resolved_status_name = $7,
             updated_at = now()
         WHERE id = $1",
    )
    .bind(connection.id)
    .bind(ticket_type.as_ref().map(|t| t.id))
    .bind(ticket_type.as_ref().map(|t| t.name.as_str()))
    .bind(&category)
    .bind(&min_severity)
    .bind(status.as_ref().map(|s| s.id))
    .bind(status.as_ref().map(|s| s.name.as_str()))
    .execute(&mut *tx)
    .await?;
    record(
        &mut *tx,
        &user,
        "psa.connection.tickets",
        json!({
            "ticket_type_id": ticket_type.as_ref().map(|t| t.id),
            "category": category,
            "min_severity": min_severity,
            "resolved_status_id": status.as_ref().map(|s| s.id),
        }),
    )
    .await?;
    let saved = psa::load(&mut *tx).await?.ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(Json(saved.into()))
}
