//! The PSA connection (ADR-0164, ADR-0166): one per installation, HaloPSA
//! first, managed by global administrators only, since one PSA serves
//! every customer and its credentials reach all of them. The client secret
//! is sealed at rest and never returned; an edit without it keeps it only
//! for the same address. The address may be private: a global
//! administrator controls the server anyway (ADR-0163).

use axum::extract::rejection::JsonRejection;
use axum::extract::State;
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
use super::sensors::body;
use super::{ApiError, ApiState};
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
