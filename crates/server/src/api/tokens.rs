//! Enrollment tokens from the web interface (ADR-0029): `POST /api/tokens`
//! returns the token once, `GET /api/tokens` lists tokens without their
//! secret, `DELETE /api/tokens/{id}` revokes an unused one. Tokens are
//! created by [`crate::enroll::token::create`], like the CLI's.

use std::time::Duration;

use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use monitorflow_common::config::ServerConfig;
use monitorflow_common::tls::{Fingerprint, PeerRole};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use super::access::{authorize_admin, Scope};
use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::sensors::{body, parse_id};
use super::servers::{decode_cursor, encode_cursor, page_limit};
use super::{ApiError, ApiState};
use crate::enroll::token::{self, Creator, TokenError, TokenRequest};

/// Page size of `GET /api/tokens` without `limit`.
pub const DEFAULT_LIMIT: u32 = 100;
/// Largest accepted `limit`.
pub const MAX_LIMIT: u32 = 500;

/// What the API needs to create tokens and the configuration snippet that
/// goes with them; from the server's CA and configuration.
#[derive(Debug, Clone)]
pub struct EnrollmentSettings {
    /// SHA-256 of the CA certificate, pinned in every token (ADR-0007).
    pub ca_fingerprint: Fingerprint,
    /// `upstream_url` for a peer that connects to this server directly.
    pub upstream_url: String,
    /// `enroll_url` of this server's enrollment listener.
    pub enroll_url: String,
}

impl EnrollmentSettings {
    /// URLs on the first of `server_names`, the name the server's
    /// certificate is issued for first, and the ports peers reach the
    /// listeners at.
    pub fn from_config(config: &ServerConfig, ca_fingerprint: Fingerprint) -> Self {
        let host = config
            .server_names
            .first()
            .map_or("localhost", String::as_str);
        let host = match host.parse::<std::net::IpAddr>() {
            Ok(std::net::IpAddr::V6(ip)) => format!("[{ip}]"),
            _ => host.to_owned(),
        };
        Self {
            ca_fingerprint,
            upstream_url: format!(
                "https://{host}:{}",
                config
                    .grpc_public_port
                    .unwrap_or(config.grpc_listen_addr.port())
            ),
            enroll_url: format!(
                "https://{host}:{}",
                config
                    .enroll_public_port
                    .unwrap_or(config.enroll_listen_addr.port())
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TokenRole {
    Agent,
    Relay,
}

impl From<TokenRole> for PeerRole {
    fn from(role: TokenRole) -> Self {
        match role {
            TokenRole::Agent => Self::Agent,
            TokenRole::Relay => Self::Relay,
        }
    }
}

/// Body of `POST /api/tokens`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateRequest {
    pub role: TokenRole,
    pub peer_id: String,
    pub site_id: Uuid,
    /// Validity in seconds, 60 to 604800; one hour by default.
    pub ttl_s: Option<u64>,
    /// For relays: DNS names and IP addresses its agents connect to.
    #[serde(default)]
    pub server_names: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TokenState {
    Pending,
    Used,
    Expired,
}

/// One token, never with its secret.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct TokenView {
    pub id: Uuid,
    pub role: String,
    pub peer_id: String,
    pub site_id: Uuid,
    pub site_name: String,
    pub customer_id: Uuid,
    pub customer_name: String,
    pub server_names: Vec<String>,
    /// `null` for tokens made with the server CLI.
    pub created_by: Option<Uuid>,
    pub created_by_username: Option<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub used_at: Option<DateTime<Utc>>,
    #[sqlx(try_from = "String")]
    pub state: TokenState,
}

impl TokenState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Used => "used",
            Self::Expired => "expired",
        }
    }
}

impl TryFrom<String> for TokenState {
    type Error = String;

    fn try_from(state: String) -> Result<Self, String> {
        [Self::Pending, Self::Used, Self::Expired]
            .into_iter()
            .find(|s| s.as_str() == state)
            .ok_or_else(|| format!("unknown token state `{state}`"))
    }
}

/// Body of `POST /api/tokens`: the stored token, its secret string and the
/// peer configuration it needs. Shown once; nothing returns it again. No
/// `Debug`, so the secret cannot end up in a log line.
#[derive(Serialize)]
pub struct CreatedToken {
    #[serde(flatten)]
    pub view: TokenView,
    pub token: String,
    pub upstream_url: String,
    pub enroll_url: String,
    /// The install sources and the default one (ADR-0054).
    pub install: super::settings::AgentInstall,
}

/// Query string of `GET /api/tokens`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListParams {
    pub state: Option<TokenState>,
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

/// Body of `GET /api/tokens`, newest first.
#[derive(Debug, Serialize)]
pub struct TokenList {
    pub tokens: Vec<TokenView>,
    pub next_cursor: Option<String>,
}

const SELECT_TOKENS: &str = "SELECT t.id, t.role, t.peer_id, t.site_id, si.name AS site_name,
            si.customer_id, c.name AS customer_name, t.server_names, t.created_by,
            u.username AS created_by_username, t.created_at, t.expires_at, t.used_at,
            CASE WHEN t.used_at IS NOT NULL THEN 'used'
                 WHEN t.expires_at <= now() THEN 'expired'
                 ELSE 'pending' END AS state
     FROM enrollment_tokens t
     JOIN sites si ON si.id = t.site_id
     JOIN customers c ON c.id = si.customer_id
     LEFT JOIN users u ON u.id = t.created_by";

impl From<TokenError> for ApiError {
    fn from(error: TokenError) -> Self {
        match error {
            TokenError::Invalid(message) => Self::Invalid(message),
            TokenError::Db(error) => Self::Database(error),
            TokenError::Random => Self::Internal(error.to_string()),
        }
    }
}

async fn site_of_token(state: &ApiState, id: Uuid) -> Result<Option<Uuid>, ApiError> {
    Ok(
        sqlx::query_scalar("SELECT site_id FROM enrollment_tokens WHERE id = $1")
            .bind(id)
            .fetch_optional(&state.pool)
            .await?,
    )
}

async fn fetch(state: &ApiState, id: Uuid) -> Result<TokenView, ApiError> {
    sqlx::query_as(&format!("{SELECT_TOKENS} WHERE t.id = $1"))
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or(ApiError::NotFound)
}

/// `POST /api/tokens`: administrators of the site's customer. A site the
/// user does not read is 404.
pub async fn create(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<CreateRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<CreatedToken>), ApiError> {
    let request = body(request)?;
    let exists: Option<Uuid> = sqlx::query_scalar("SELECT id FROM sites WHERE id = $1")
        .bind(request.site_id)
        .fetch_optional(&state.pool)
        .await?;
    let site_id = authorize_admin(&user, exists)?;
    let settings = state
        .enrollment
        .as_deref()
        .ok_or_else(|| ApiError::Internal("enrollment is not configured".into()))?;
    // Before the token exists, so a failure here does not lose one.
    let install = super::settings::agent_install(&state).await?;
    let ttl = request
        .ttl_s
        .map_or(token::DEFAULT_TTL, Duration::from_secs);
    let created = token::create(
        &state.pool,
        settings.ca_fingerprint,
        &TokenRequest {
            role: request.role.into(),
            peer_id: request.peer_id,
            site_id,
            ttl,
            creator: Creator::User(user.id),
            server_names: request.server_names,
            parent_relay: None,
        },
    )
    .await?;
    let view = fetch(&state, created.id).await?;
    Ok((
        StatusCode::CREATED,
        Json(CreatedToken {
            view,
            token: created.token.to_secret_string().expose().to_owned(),
            upstream_url: settings.upstream_url.clone(),
            enroll_url: settings.enroll_url.clone(),
            install,
        }),
    ))
}

/// `GET /api/tokens`: tokens of the sites whose customer the user
/// administers, newest first. 403 without such a grant.
pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
    params: Result<Query<ListParams>, QueryRejection>,
) -> Result<Json<TokenList>, ApiError> {
    let Query(params) = params.map_err(|r| ApiError::Invalid(r.body_text()))?;
    let scope = user.administered_sites();
    if scope == Scope::Only(Vec::new()) {
        return Err(ApiError::Forbidden);
    }
    let (all_sites, sites) = scope.bind();
    let limit = page_limit(params.limit, DEFAULT_LIMIT, MAX_LIMIT)?;
    let after: Option<(DateTime<Utc>, Uuid)> =
        params.cursor.as_deref().map(decode_cursor).transpose()?;
    let (after_time, after_id) = after.unzip();
    let mut tokens: Vec<TokenView> = sqlx::query_as(&format!(
        "SELECT * FROM ({SELECT_TOKENS}
                        WHERE ($1 OR t.site_id = ANY($2))
                          AND ($3::timestamptz IS NULL OR (t.created_at, t.id) < ($3, $4))) x
         WHERE $5::text IS NULL OR x.state = $5
         ORDER BY x.created_at DESC, x.id DESC
         LIMIT $6"
    ))
    .bind(all_sites)
    .bind(&sites)
    .bind(after_time)
    .bind(after_id)
    .bind(params.state.map(TokenState::as_str))
    .bind(i64::from(limit) + 1)
    .fetch_all(&state.pool)
    .await?;
    let next_cursor = if tokens.len() > limit as usize {
        tokens.truncate(limit as usize);
        tokens.last().map(|t| encode_cursor(&(t.created_at, t.id)))
    } else {
        None
    };
    Ok(Json(TokenList {
        tokens,
        next_cursor,
    }))
}

/// `DELETE /api/tokens/{id}`: revokes a token that was not used, expired
/// or not, by deleting it; the audit log keeps its record. A used token is
/// 409.
pub async fn delete(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let id = parse_id(&id)?;
    authorize_admin(&user, site_of_token(&state, id).await?)?;
    let mut tx = state.pool.begin().await?;
    // Waits for an enrollment consuming it, then sees `used_at`.
    let deleted: Option<(String, String, Uuid)> = sqlx::query_as(
        "DELETE FROM enrollment_tokens WHERE id = $1 AND used_at IS NULL
         RETURNING role, peer_id, site_id",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((role, peer_id, site_id)) = deleted else {
        return Err(ApiError::Conflict("the token was already used".into()));
    };
    let actor = user.id.to_string();
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action: "enrollment_token.revoke",
            target: Some((&role, &peer_id)),
            details: json!({ "token_id": id, "site_id": site_id }),
        },
    )
    .await?;
    tx.commit().await?;
    tracing::info!(user = %user.id, token_id = %id, %role, %peer_id, "revoked an enrollment token");
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_and_states_parse() {
        let request: CreateRequest = serde_json::from_value(json!({
            "role": "relay", "peer_id": "r1", "site_id": Uuid::nil(),
            "server_names": ["relay.example"]
        }))
        .unwrap();
        assert_eq!(request.role, TokenRole::Relay);
        assert_eq!(request.ttl_s, None);
        assert_eq!(PeerRole::from(TokenRole::Agent), PeerRole::Agent);
        for bad in [
            json!({ "role": "server", "peer_id": "s", "site_id": Uuid::nil() }),
            json!({ "role": "agent", "peer_id": "a", "site_id": Uuid::nil(), "token": "x" }),
        ] {
            assert!(serde_json::from_value::<CreateRequest>(bad).is_err());
        }
        assert_eq!(
            TokenState::try_from("used".to_owned()),
            Ok(TokenState::Used)
        );
        assert!(TokenState::try_from("revoked".to_owned()).is_err());
    }

    #[test]
    fn urls_come_from_the_first_server_name() {
        let mut config = ServerConfig::default();
        let settings = EnrollmentSettings::from_config(&config, [0; 32]);
        assert_eq!(settings.upstream_url, "https://localhost:7443");
        assert_eq!(settings.enroll_url, "https://localhost:7445");
        // Behind published ports, as the compose file sets them.
        let published = ServerConfig {
            grpc_public_port: Some(27443),
            enroll_public_port: Some(27445),
            ..ServerConfig::default()
        };
        let settings = EnrollmentSettings::from_config(&published, settings.ca_fingerprint);
        assert_eq!(settings.upstream_url, "https://localhost:27443");
        assert_eq!(settings.enroll_url, "https://localhost:27445");
        config.server_names = vec!["::1".into()];
        let settings = EnrollmentSettings::from_config(&config, [0; 32]);
        assert_eq!(settings.enroll_url, "https://[::1]:7445");
    }

    #[test]
    fn token_errors_map_to_statuses() {
        let status = |e: TokenError| ApiError::from(e).status();
        assert_eq!(
            status(TokenError::Invalid("bad".into())),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            status(TokenError::Random),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }
}
