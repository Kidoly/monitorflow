//! `POST /api/servers/{id}/revoke` and `POST /api/relays/{id}/revoke`
//! (ADR-0029): marks the peer and its certificates revoked, puts the
//! serials in the CRL through [`CrlPublisher::revoke`](crate::crl::CrlPublisher::revoke)
//! and closes its live streams.

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::Json;
use chrono::{DateTime, Utc};
use monitorflow_common::tls::PeerRole;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use super::access::authorize_admin;
use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::sensors::{body, parse_id};
use super::{ApiError, ApiState};
use crate::ca::{RevocationReason, Serial};
use crate::grpc::CloseReason;

/// Body of both revoke routes.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevokeRequest {
    /// `unspecified` when absent.
    pub reason: Option<RevocationReason>,
}

/// Answer of both revoke routes.
#[derive(Debug, Serialize)]
pub struct Revoked {
    pub revoked_at: DateTime<Utc>,
    pub reason: RevocationReason,
    /// Certificates put in the CRL, lowercase hexadecimal.
    pub serials: Vec<String>,
    /// Live streams closed: the peer's and, for a relay, those it forwards.
    pub closed_streams: usize,
}

/// `POST /api/servers/{id}/revoke`: administrators of the site's customer.
pub async fn server(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<RevokeRequest>, JsonRejection>,
) -> Result<Json<Revoked>, ApiError> {
    revoke(&state, &user, PeerRole::Agent, &id, request).await
}

/// `POST /api/relays/{id}/revoke`: administrators of the site's customer.
pub async fn relay(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<RevokeRequest>, JsonRejection>,
) -> Result<Json<Revoked>, ApiError> {
    revoke(&state, &user, PeerRole::Relay, &id, request).await
}

async fn revoke(
    state: &ApiState,
    user: &CurrentUser,
    role: PeerRole,
    id: &str,
    request: Result<Json<RevokeRequest>, JsonRejection>,
) -> Result<Json<Revoked>, ApiError> {
    let id = parse_id(id)?;
    // The table name is one of two constants, never input.
    let (table, kind) = match role {
        PeerRole::Relay => ("relays", "relay"),
        _ => ("servers", "server"),
    };
    let row: Option<(String, Uuid)> = sqlx::query_as(&format!(
        "SELECT peer_id, site_id FROM {table} WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(&state.pool)
    .await?;
    authorize_admin(user, row.as_ref().map(|r| r.1))?;
    let (peer_id, site_id) = row.ok_or(ApiError::NotFound)?;
    let reason = body(request)?
        .reason
        .unwrap_or(RevocationReason::Unspecified);
    let crl = state
        .crl
        .as_deref()
        .ok_or_else(|| ApiError::Internal("revocation is not configured".into()))?;

    let mut tx = state.pool.begin().await?;
    let revoked_at: DateTime<Utc> = sqlx::query_scalar(&format!(
        "UPDATE {table} SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL
         RETURNING revoked_at"
    ))
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| ApiError::Conflict(format!("the {kind} is already revoked")))?;
    // Expired certificates are refused anyway and stay out of the CRL.
    let serials: Vec<Vec<u8>> = sqlx::query_scalar(
        "UPDATE certificates SET revoked_at = now(), revocation_reason = $3
         WHERE peer_role = $1 AND peer_id = $2 AND revoked_at IS NULL AND not_after > now()
         RETURNING serial",
    )
    .bind(role.as_str())
    .bind(&peer_id)
    .bind(reason_name(reason))
    .fetch_all(&mut *tx)
    .await?;
    let serials: Vec<Serial> = serials.iter().map(|s| Serial::from_bytes(s)).collect();
    let shown: Vec<String> = serials.iter().map(Serial::to_string).collect();
    let actor = user.id.to_string();
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action: if role == PeerRole::Relay {
                "relay.revoke"
            } else {
                "server.revoke"
            },
            target: Some((kind, &id.to_string())),
            details: json!({
                "peer_id": peer_id,
                "site_id": site_id,
                "reason": reason,
                "serials": shown,
            }),
        },
    )
    .await?;
    // In the CA before the commit: a failure here rolls the rows back, and
    // a failed commit after it leaves the safe side, revoked in the CRL.
    for serial in serials {
        crl.revoke(serial, reason)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
    }
    tx.commit().await?;
    let closed_streams = state.registry.close(&peer_id, CloseReason::Revoked);
    tracing::info!(user = %user.id, %kind, %peer_id, ?reason, closed_streams, "revoked a peer");
    Ok(Json(Revoked {
        revoked_at,
        reason,
        serials: shown,
        closed_streams,
    }))
}

/// `certificates.revocation_reason`, as the API spells it.
fn reason_name(reason: RevocationReason) -> String {
    json!(reason).as_str().unwrap_or("unspecified").to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasons_parse_and_default() {
        let request: RevokeRequest = serde_json::from_value(json!({})).unwrap();
        assert_eq!(request.reason, None);
        let request: RevokeRequest =
            serde_json::from_value(json!({ "reason": "key_compromise" })).unwrap();
        assert_eq!(request.reason, Some(RevocationReason::KeyCompromise));
        assert_eq!(
            reason_name(RevocationReason::KeyCompromise),
            "key_compromise"
        );
        for bad in [json!({ "reason": "boredom" }), json!({ "comment": "x" })] {
            assert!(serde_json::from_value::<RevokeRequest>(bad).is_err());
        }
    }
}
