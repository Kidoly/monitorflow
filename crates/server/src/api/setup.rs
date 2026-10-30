//! First-run setup in the browser (ADR-0057). While no user exists, the
//! server issues a one-time setup link, `<web_url>/setup/#<secret>`, and
//! logs it. `POST /api/setup` with the secret creates the first global
//! administrator, the default site `main` when there is no site yet, and
//! signs the new administrator in.
//!
//! One link is live at a time: issuing one replaces the previous one. The
//! link is single use, expires after [`TTL`], and dies as soon as any user
//! exists, including one created with `monitorflow-server user add`.

use std::time::Duration;

use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::http::header::SET_COOKIE;
use axum::response::{IntoResponse, Response};
use axum::Json;
use monitorflow_common::redact::Secret;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::audit::{self, Event};
use super::auth::Me;
use super::one_time::OneTimeToken;
use super::users::{self, CreateUserError, GrantScope};
use super::{session, ApiError, ApiState, Role};

/// How long a setup link stays valid.
pub const TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Name of the site created with the first administrator.
pub const DEFAULT_SITE: &str = "main";

/// Path of the setup page in the web interface.
pub const PAGE_PATH: &str = "/setup/";

/// Audit actor of what the setup link does before a user exists.
const ACTOR: (&str, &str) = ("system", "setup");

/// The secret of a setup link.
pub type SetupToken = OneTimeToken;

impl SetupToken {
    /// The link to give the person installing the server.
    pub fn setup_url(&self, web_base_url: &str) -> String {
        self.url(web_base_url, PAGE_PATH)
    }
}

/// Whether the server has no user yet, so setup is still to do.
pub async fn pending(db: &mut PgConnection) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT NOT EXISTS (SELECT 1 FROM users)")
        .fetch_one(db)
        .await
}

/// Issues a new setup link, replacing any previous one, or `None` when a
/// user already exists.
pub async fn issue(pool: &PgPool) -> Result<Option<SetupToken>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    // Serializes with user creation: a user created meanwhile either is
    // seen here or deletes the row after.
    sqlx::query("LOCK TABLE setup_tokens IN EXCLUSIVE MODE")
        .execute(&mut *tx)
        .await?;
    if !pending(&mut tx).await? {
        sqlx::query("DELETE FROM setup_tokens")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        return Ok(None);
    }
    let token = SetupToken::generate();
    sqlx::query(
        "INSERT INTO setup_tokens (token_hash, expires_at)
         VALUES ($1, now() + make_interval(secs => $2))
         ON CONFLICT (singleton) DO UPDATE
         SET token_hash = EXCLUDED.token_hash, created_at = now(),
             expires_at = EXCLUDED.expires_at",
    )
    .bind(token.hash())
    .bind(TTL.as_secs_f64())
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut *tx,
        Event {
            actor_kind: ACTOR.0,
            actor_id: ACTOR.1,
            action: "setup.link_issued",
            target: None,
            details: json!({ "ttl_seconds": TTL.as_secs() }),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(Some(token))
}

/// Creates the site [`DEFAULT_SITE`] under the default customer (the
/// column's default) when no site exists, audited as done by `actor`.
/// Returns its ID if created.
pub async fn ensure_default_site(
    tx: &mut PgConnection,
    actor: (&str, &str),
) -> Result<Option<Uuid>, sqlx::Error> {
    let created: Option<(Uuid, Uuid)> = sqlx::query_as(
        "INSERT INTO sites (name)
         SELECT $1 WHERE NOT EXISTS (SELECT 1 FROM sites)
         ON CONFLICT (name) DO NOTHING
         RETURNING id, customer_id",
    )
    .bind(DEFAULT_SITE)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some((id, customer_id)) = created {
        let target = id.to_string();
        audit::record(
            &mut *tx,
            Event {
                actor_kind: actor.0,
                actor_id: actor.1,
                action: "site.create",
                target: Some(("site", &target)),
                details: json!({ "name": DEFAULT_SITE, "customer_id": customer_id }),
            },
        )
        .await?;
    }
    Ok(created.map(|(id, _)| id))
}

/// Body of `GET /api/setup`.
#[derive(Debug, Serialize)]
pub struct Status {
    /// No user exists yet: sign-in cannot work, the setup link is needed.
    pub pending: bool,
}

/// `GET /api/setup`, without authentication: whether setup is pending.
pub async fn status(State(state): State<ApiState>) -> Result<Json<Status>, ApiError> {
    let mut db = state.pool.acquire().await?;
    Ok(Json(Status {
        pending: pending(&mut db).await?,
    }))
}

#[derive(Deserialize)]
pub struct SetupRequest {
    pub token: String,
    pub username: String,
    pub password: Secret,
}

/// `POST /api/setup`: consumes the setup link, creates the first
/// administrator and signs it in.
pub async fn complete(
    State(state): State<ApiState>,
    request: Result<Json<SetupRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = request.map_err(|_| ApiError::BadRequest)?;
    // Checked before hashing, so a bad form keeps the link alive.
    let hash = users::hash_new_password(&request.username, &request.password)
        .await
        .map_err(|e| match e {
            CreateUserError::InvalidUsername | CreateUserError::PasswordTooShort => {
                ApiError::Invalid(e.to_string())
            }
            other => ApiError::Internal(other.to_string()),
        })?;
    let token = SetupToken::parse(&request.token);

    let mut tx = state.pool.begin().await?;
    // The DELETE takes the row: of two requests with the same link, the
    // second finds nothing once the first commits.
    let live: Option<bool> = match &token {
        Some(token) => {
            sqlx::query_scalar(
                "DELETE FROM setup_tokens WHERE token_hash = $1 RETURNING expires_at > now()",
            )
            .bind(token.hash())
            .fetch_optional(&mut *tx)
            .await?
        }
        None => None,
    };
    if live != Some(true) {
        // An expired link is deleted all the same.
        tx.commit().await?;
        if !pending(&mut *state.pool.acquire().await?).await? {
            return Err(ApiError::Conflict("setup is already done; sign in".into()));
        }
        tracing::info!(expired = live.is_some(), "setup link refused");
        return Err(ApiError::Gone(
            "this setup link is no longer valid; print a new one with `monitorflow-server setup-link`"
                .into(),
        ));
    }
    if !pending(&mut tx).await? {
        tx.commit().await?;
        return Err(ApiError::Conflict("setup is already done; sign in".into()));
    }
    let id = users::insert_user(
        &mut tx,
        &request.username,
        Some(&hash),
        Role::Administrator,
        GrantScope::Global,
        ACTOR,
    )
    .await
    .map_err(|e| match e {
        CreateUserError::Database(e) => ApiError::Database(e),
        other => ApiError::Internal(other.to_string()),
    })?;
    let site = ensure_default_site(&mut tx, ACTOR).await?;
    let token = session::create(&mut *tx, id, &state.settings).await?;
    let actor_id = id.to_string();
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "user",
            actor_id: &actor_id,
            action: "auth.login",
            target: None,
            details: json!({ "via": "setup" }),
        },
    )
    .await?;
    tx.commit().await?;
    tracing::info!(user = %id, default_site = ?site, "first administrator created from the setup link");

    let cookie = session::set_cookie(&state.settings, &token);
    let me = Me {
        id,
        username: request.username,
        roles: vec![super::RoleGrant {
            role: Role::Administrator,
            customer_id: None,
            site_id: None,
        }],
    };
    Ok(([(SET_COOKIE, cookie)], Json(me)).into_response())
}
