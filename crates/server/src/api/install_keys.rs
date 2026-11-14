//! Reusable install keys from the web interface (ADR-0073): `POST
//! /api/install-keys` returns the key once with what the install commands
//! need, `GET /api/install-keys` lists keys and the hosts waiting for
//! approval, `DELETE /api/install-keys/{id}` revokes a key, and
//! `/api/pending-hosts/{peer_id}/approve` or `/reject` decides on a host.

use std::time::Duration;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use monitorflow_common::tls::{validate_id, PeerRole};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use super::access::{authorize_admin, Scope};
use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::revoke::revoke_certificates;
use super::sensors::{body, parse_id};
use super::{ApiError, ApiState};
use crate::ca::RevocationReason;
use crate::enroll::install_key::{self, KeyRequest};
use crate::enroll::token::Creator;
use crate::grpc::CloseReason;

/// Keys listed, newest first; older ones stay in the audit log.
const LIST_LIMIT: i64 = 100;

/// Body of `POST /api/install-keys`.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[schema(as = InstallKeyRequest)]
#[serde(deny_unknown_fields)]
pub struct CreateRequest {
    /// `agent` by default. A `relay` key enrolls one relay, whose names an
    /// administrator confirms (ADR-0066), so it takes no host limit or
    /// approval.
    #[serde(default)]
    pub role: KeyRole,
    /// The site the hosts join.
    pub site_id: Uuid,
    /// Validity in seconds, 3600 (one hour) to 31536000 (one year);
    /// 2592000 (30 days) by default, 86400 (one day) for a relay.
    #[schema(minimum = 3600, maximum = 31536000)]
    pub ttl_s: Option<u64>,
    /// Hosts the key may enroll, 1 to 100000; no limit when absent or
    /// `null`. Ignored for a relay key, which enrolls one host.
    #[schema(minimum = 1, maximum = 100000)]
    pub max_hosts: Option<u32>,
    /// New hosts wait for an administrator before they appear (see
    /// `pending_hosts`). `false` by default; ignored for a relay key.
    #[serde(default)]
    pub require_approval: bool,
}

/// What the key enrolls.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, utoipa::ToSchema)]
#[schema(as = InstallKeyRole)]
#[serde(rename_all = "snake_case")]
pub enum KeyRole {
    #[default]
    Agent,
    Relay,
}

/// `revoked` first, then `expired`, then `used_up` once `host_count`
/// reaches `max_hosts`; `active` otherwise. Only `active` keys enroll.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[schema(as = InstallKeyState)]
#[serde(rename_all = "snake_case")]
pub enum KeyState {
    Active,
    Expired,
    UsedUp,
    Revoked,
}

impl TryFrom<String> for KeyState {
    type Error = String;

    fn try_from(state: String) -> Result<Self, String> {
        Ok(match state.as_str() {
            "active" => Self::Active,
            "expired" => Self::Expired,
            "used_up" => Self::UsedUp,
            "revoked" => Self::Revoked,
            _ => return Err(format!("unknown key state `{state}`")),
        })
    }
}

/// One key, never with its secret.
#[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
#[schema(as = InstallKey)]
pub struct KeyView {
    pub id: Uuid,
    /// `agent` or `relay`.
    pub role: String,
    pub site_id: Uuid,
    pub site_name: String,
    pub customer_id: Uuid,
    pub customer_name: String,
    /// `null` for keys made with the server CLI, or when that user was
    /// deleted.
    pub created_by_username: Option<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    /// Hosts it may enroll; `null` for no limit. 1 for a relay key.
    pub max_hosts: Option<i32>,
    /// Hosts it enrolled so far.
    pub host_count: i32,
    pub require_approval: bool,
    /// `null` unless revoked.
    pub revoked_at: Option<DateTime<Utc>>,
    #[sqlx(try_from = "String")]
    pub state: KeyState,
}

/// A host enrolled by a key that asks for approval.
#[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct PendingHost {
    /// The ID it was given, which approving or rejecting it takes.
    pub peer_id: String,
    /// The host name it reported.
    pub hostname: String,
    pub site_id: Uuid,
    pub site_name: String,
    pub customer_name: String,
    /// The key it enrolled with.
    pub install_key_id: Uuid,
    /// The IP address it enrolled from; `null` when unknown.
    pub source: Option<String>,
    /// When it enrolled.
    pub created_at: DateTime<Utc>,
}

/// Body of `GET /api/install-keys`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = InstallKeyList)]
pub struct KeyList {
    /// The 100 newest keys, revoked and expired ones included, newest
    /// first.
    pub keys: Vec<KeyView>,
    /// Every host waiting for approval, oldest first.
    pub pending_hosts: Vec<PendingHost>,
}

/// Body of `POST /api/install-keys`: the stored key, its secret string,
/// named `token` as installers take it, and what the commands need. Shown
/// once; no `Debug`, so the secret cannot end up in a log line.
#[derive(Serialize, utoipa::ToSchema)]
#[schema(as = CreatedInstallKey)]
pub struct CreatedKey {
    #[serde(flatten)]
    pub view: KeyView,
    /// The secret, in the same `mfe1.…` form as an enrollment token. Shown
    /// in this answer only; MonitorFlow keeps only its hash.
    pub token: String,
    /// Where enrolled hosts connect, `https://host:port`.
    pub upstream_url: String,
    /// Where installers enroll, `https://host:port`.
    pub enroll_url: String,
    /// The install sources and the default one.
    pub install: super::settings::AgentInstall,
    /// The address in `upstream_url` and `enroll_url`.
    pub address: super::public_address::PublicAddress,
}

const SELECT_KEYS: &str = "SELECT k.id, k.role, k.site_id, si.name AS site_name, si.customer_id,
            c.name AS customer_name, u.username AS created_by_username, k.created_at,
            k.expires_at, k.max_hosts, k.host_count, k.require_approval, k.revoked_at,
            CASE WHEN k.revoked_at IS NOT NULL THEN 'revoked'
                 WHEN k.expires_at <= now() THEN 'expired'
                 WHEN k.host_count >= k.max_hosts THEN 'used_up'
                 ELSE 'active' END AS state
     FROM install_keys k
     JOIN sites si ON si.id = k.site_id
     JOIN customers c ON c.id = si.customer_id
     LEFT JOIN users u ON u.id = k.created_by";

/// `POST /api/install-keys`: administrators of the site's customer. A
/// site the user does not read is 404.
pub async fn create(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<CreateRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<CreatedKey>), ApiError> {
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
    // Before the key exists, so a failure here does not lose one.
    let install = super::settings::agent_install(&state).await?;
    let address = super::public_address::current(&state.pool, settings).await?;
    let key = match request.role {
        KeyRole::Agent => KeyRequest {
            role: PeerRole::Agent,
            site_id,
            ttl: request
                .ttl_s
                .map_or(install_key::DEFAULT_TTL, Duration::from_secs),
            max_hosts: request.max_hosts,
            require_approval: request.require_approval,
            creator: Creator::User(user.id),
        },
        KeyRole::Relay => KeyRequest {
            role: PeerRole::Relay,
            site_id,
            ttl: request
                .ttl_s
                .map_or(install_key::RELAY_TTL, Duration::from_secs),
            max_hosts: Some(1),
            require_approval: false,
            creator: Creator::User(user.id),
        },
    };
    let created = install_key::create(&state.pool, state.ca_pin(settings), &key).await?;
    // SQL-safe: interpolates only the constant SELECT_KEYS.
    let view = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{SELECT_KEYS} WHERE k.id = $1"
    )))
    .bind(created.id)
    .fetch_one(&state.pool)
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(CreatedKey {
            view,
            token: created.key.to_secret_string().expose().to_owned(),
            upstream_url: address.upstream_url.clone(),
            enroll_url: address.enroll_url.clone(),
            install,
            address,
        }),
    ))
}

/// `GET /api/install-keys`: keys and pending hosts of the sites whose
/// customer the user administers. 403 without such a grant.
pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<KeyList>, ApiError> {
    let scope = user.administered_sites();
    if scope == Scope::Only(Vec::new()) {
        return Err(ApiError::Forbidden);
    }
    let (all_sites, sites) = scope.bind();
    // SQL-safe: interpolates only the constant SELECT_KEYS.
    let keys = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{SELECT_KEYS} WHERE $1 OR k.site_id = ANY($2)
         ORDER BY k.created_at DESC, k.id DESC LIMIT $3"
    )))
    .bind(all_sites)
    .bind(&sites)
    .bind(LIST_LIMIT)
    .fetch_all(&state.pool)
    .await?;
    let pending_hosts = sqlx::query_as(
        "SELECT p.peer_id, p.hostname, p.site_id, si.name AS site_name,
                c.name AS customer_name, p.install_key_id, p.source, p.created_at
         FROM pending_hosts p
         JOIN sites si ON si.id = p.site_id
         JOIN customers c ON c.id = si.customer_id
         WHERE $1 OR p.site_id = ANY($2)
         ORDER BY p.created_at, p.peer_id",
    )
    .bind(all_sites)
    .bind(&sites)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(KeyList {
        keys,
        pending_hosts,
    }))
}

/// `DELETE /api/install-keys/{id}`: revokes a key, so it enrolls no more
/// hosts. Hosts it enrolled keep working. 409 if already revoked.
pub async fn revoke(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let id = parse_id(&id)?;
    let site: Option<Uuid> = sqlx::query_scalar("SELECT site_id FROM install_keys WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?;
    let site_id = authorize_admin(&user, site)?;
    let mut tx = state.pool.begin().await?;
    // Waits for an enrollment holding the key's row lock.
    let revoked: Option<i32> = sqlx::query_scalar(
        "UPDATE install_keys SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL
         RETURNING host_count",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(host_count) = revoked else {
        return Err(ApiError::Conflict("the key is already revoked".into()));
    };
    let actor = user.id.to_string();
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action: "install_key.revoke",
            target: Some(("install_key", &id.to_string())),
            details: json!({ "site_id": site_id, "host_count": host_count }),
        },
    )
    .await?;
    tx.commit().await?;
    tracing::info!(user = %user.id, key_id = %id, "revoked an install key");
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/pending-hosts/{peer_id}/approve`: the host's next connection
/// is accepted and it appears in the server list; a removed server is back
/// from now (ADR-0062).
pub async fn approve(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(peer_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let (peer_id, site_id) = pending(&state, &user, &peer_id).await?;
    let mut tx = state.pool.begin().await?;
    take(&mut tx, &peer_id).await?;
    sqlx::query(
        "UPDATE servers SET revoked_at = NULL, last_seen_at = now()
         WHERE peer_id = $1 AND revoked_at IS NOT NULL",
    )
    .bind(&peer_id)
    .execute(&mut *tx)
    .await?;
    record(
        &mut tx,
        &user,
        "pending_host.approve",
        &peer_id,
        json!({ "site_id": site_id }),
    )
    .await?;
    tx.commit().await?;
    tracing::info!(user = %user.id, %peer_id, "approved a new host");
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/pending-hosts/{peer_id}/reject`: revokes the host's
/// certificates, so it can never connect under this ID.
pub async fn reject(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(peer_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let (peer_id, site_id) = pending(&state, &user, &peer_id).await?;
    let crl = state
        .crl
        .as_deref()
        .ok_or_else(|| ApiError::Internal("revocation is not configured".into()))?;
    let mut tx = state.pool.begin().await?;
    take(&mut tx, &peer_id).await?;
    let reason = RevocationReason::Unspecified;
    let serials = revoke_certificates(&mut tx, PeerRole::Agent, &peer_id, reason).await?;
    let shown: Vec<String> = serials.iter().map(|(s, _)| s.to_string()).collect();
    let details = json!({ "site_id": site_id, "serials": shown });
    record(&mut tx, &user, "pending_host.reject", &peer_id, details).await?;
    // In the CA before the commit, as for revocation (ADR-0029).
    for (serial, not_after) in serials {
        crl.revoke(serial, reason, not_after)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
    }
    tx.commit().await?;
    state
        .registry
        .close(PeerRole::Agent, &peer_id, CloseReason::Revoked);
    tracing::info!(user = %user.id, %peer_id, "rejected a new host");
    Ok(StatusCode::NO_CONTENT)
}

/// The pending host `peer_id` and its site, if the user administers it.
async fn pending(
    state: &ApiState,
    user: &CurrentUser,
    peer_id: &str,
) -> Result<(String, Uuid), ApiError> {
    validate_id(peer_id).map_err(|_| ApiError::NotFound)?;
    let site: Option<Uuid> =
        sqlx::query_scalar("SELECT site_id FROM pending_hosts WHERE peer_id = $1")
            .bind(peer_id)
            .fetch_optional(&state.pool)
            .await?;
    Ok((peer_id.to_owned(), authorize_admin(user, site)?))
}

/// Removes the pending row; 409 when another administrator decided first.
async fn take(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    peer_id: &str,
) -> Result<(), ApiError> {
    let deleted = sqlx::query("DELETE FROM pending_hosts WHERE peer_id = $1")
        .bind(peer_id)
        .execute(&mut **tx)
        .await?;
    if deleted.rows_affected() == 0 {
        return Err(ApiError::Conflict(
            "the host was already approved or rejected".into(),
        ));
    }
    Ok(())
}

async fn record(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user: &CurrentUser,
    action: &str,
    peer_id: &str,
    details: serde_json::Value,
) -> Result<(), ApiError> {
    let actor = user.id.to_string();
    audit::record(
        &mut **tx,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action,
            target: Some(("agent", peer_id)),
            details,
        },
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_default_to_unlimited_hosts_without_approval() {
        let request: CreateRequest =
            serde_json::from_value(json!({ "site_id": Uuid::nil() })).unwrap();
        assert_eq!(request.ttl_s, None);
        assert_eq!(request.max_hosts, None);
        assert!(!request.require_approval);
        let bad = json!({ "site_id": Uuid::nil(), "token": "x" });
        assert!(serde_json::from_value::<CreateRequest>(bad).is_err());
        assert_eq!(
            KeyState::try_from("used_up".to_owned()),
            Ok(KeyState::UsedUp)
        );
        assert!(KeyState::try_from("pending".to_owned()).is_err());
    }
}
