//! `POST /api/servers/{id}/revoke` and `POST /api/relays/{id}/revoke`
//! (ADR-0029): marks the peer and its certificates revoked, puts the
//! serials in the CRL through [`CrlPublisher::revoke`](crate::crl::CrlPublisher::revoke)
//! and closes its live streams. Revoking a relay also revokes the
//! certificates obtained through it, and those renewed from them
//! (ADR-0028). This is "Remove" in the interface
//! (ADR-0062): it also resolves the peer's open incidents and drops its
//! unused tokens, and the peer leaves every list until it enrolls again.

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::Json;
use chrono::{DateTime, Utc};
use monitorflow_common::tls::PeerRole;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use super::access::authorize_admin;
use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::sensors::{body, parse_id};
use super::{ApiError, ApiState};
use crate::alerting::offline;
use crate::ca::{RevocationReason, Serial};
use crate::grpc::CloseReason;

/// A certificate of another peer revoked with a relay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IssuedThrough {
    pub serial: String,
    pub peer_role: String,
    pub peer_id: String,
}

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
    /// For a relay, the certificates of other peers among `serials`: those
    /// obtained through it (ADR-0028).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub issued_through: Vec<IssuedThrough>,
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
    // SQL-safe: `table` is one of two constants, never input.
    let row: Option<(String, Uuid)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT peer_id, site_id FROM {table} WHERE id = $1"
    )))
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
    // SQL-safe: `table` is one of two constants, never input.
    let revoked_at: DateTime<Utc> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "UPDATE {table} SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL
         RETURNING revoked_at"
    )))
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| ApiError::Conflict(format!("the {kind} is already revoked")))?;
    let mut serials = revoke_certificates(&mut tx, role, &peer_id, reason).await?;
    let issued_through = match role {
        PeerRole::Relay => revoke_issued_through(&mut tx, &peer_id, reason).await?,
        _ => Vec::new(),
    };
    serials.extend(issued_through.iter().map(|c| (c.0.clone(), c.3)));
    let shown: Vec<String> = serials.iter().map(|(s, _)| s.to_string()).collect();
    let issued_through: Vec<IssuedThrough> = issued_through
        .into_iter()
        .map(|(serial, peer_role, peer_id, _)| IssuedThrough {
            serial: serial.to_string(),
            peer_role,
            peer_id,
        })
        .collect();
    // A token from before the removal must not bring the peer back.
    sqlx::query(
        "DELETE FROM enrollment_tokens WHERE role = $1 AND peer_id = $2 AND used_at IS NULL",
    )
    .bind(role.as_str())
    .bind(&peer_id)
    .execute(&mut *tx)
    .await?;
    let incidents = offline::resolve_removed(&mut *tx).await?;
    if role != PeerRole::Relay {
        // Enrolling again starts from a clean slate.
        sqlx::query("DELETE FROM alert_states WHERE server_id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
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
                "issued_through": issued_through,
                "incidents_resolved": incidents,
            }),
        },
    )
    .await?;
    // In the CA before the commit: a failure here rolls the rows back, and
    // a failed commit after it leaves the safe side, revoked in the CRL.
    for (serial, not_after) in serials.iter().cloned() {
        crl.revoke(serial, reason, not_after)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
    }
    tx.commit().await?;
    let revoked: Vec<Serial> = serials.into_iter().map(|(s, _)| s).collect();
    let closed_streams = state.registry.close(role, &peer_id, CloseReason::Revoked)
        + state.registry.close_serials(&revoked, CloseReason::Revoked);
    tracing::info!(user = %user.id, %kind, %peer_id, ?reason, closed_streams, "revoked a peer");
    Ok(Json(Revoked {
        revoked_at,
        reason,
        serials: shown,
        issued_through,
        closed_streams,
    }))
}

/// Marks every unexpired, unrevoked certificate of the peer revoked in
/// `tx` and returns their serials and expiry, for the caller to pass to the
/// CA before committing. Expired certificates are refused anyway and stay out of the
/// CRL.
pub(super) async fn revoke_certificates(
    tx: &mut Transaction<'_, Postgres>,
    role: PeerRole,
    peer_id: &str,
    reason: RevocationReason,
) -> Result<Vec<(Serial, SystemTime)>, sqlx::Error> {
    let rows: Vec<(Vec<u8>, DateTime<Utc>)> = sqlx::query_as(
        "UPDATE certificates SET revoked_at = now(), revocation_reason = $3
         WHERE peer_role = $1 AND peer_id = $2 AND revoked_at IS NULL AND not_after > now()
         RETURNING serial, not_after",
    )
    .bind(role.as_str())
    .bind(peer_id)
    .bind(reason_name(reason))
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows
        .iter()
        .map(|(serial, not_after)| (Serial::from_bytes(serial), (*not_after).into()))
        .collect())
}

/// Marks revoked in `tx` every unexpired, unrevoked certificate of another
/// peer that the relay `peer_id` could have obtained (ADR-0028): those
/// renewed through it, anywhere on the path, and then, transitively, those
/// renewed from one of them or through a relay presenting one of them. The
/// relay chose the keys of the first ones, so none can be trusted.
/// Returns serial, role, peer ID and expiry of each.
async fn revoke_issued_through(
    tx: &mut Transaction<'_, Postgres>,
    peer_id: &str,
    reason: RevocationReason,
) -> Result<Vec<(Serial, String, String, SystemTime)>, sqlx::Error> {
    let rows: Vec<(Vec<u8>, String, String, DateTime<Utc>)> = sqlx::query_as(
        "WITH RECURSIVE tainted (serial) AS (
             SELECT serial FROM certificates
             WHERE relay_path @> ARRAY[$1::text]
                OR relay_serial IN (SELECT serial FROM certificates
                                    WHERE peer_role = 'relay' AND peer_id = $1)
             UNION
             SELECT c.serial FROM certificates c JOIN tainted t
                 ON c.renewed_from = t.serial OR c.relay_serial = t.serial
         )
         UPDATE certificates SET revoked_at = now(), revocation_reason = $2
         WHERE serial IN (SELECT serial FROM tainted)
           AND NOT (peer_role = 'relay' AND peer_id = $1)
           AND revoked_at IS NULL AND not_after > now()
         RETURNING serial, peer_role, peer_id, not_after",
    )
    .bind(peer_id)
    .bind(reason_name(reason))
    .fetch_all(&mut **tx)
    .await?;
    let mut rows: Vec<_> = rows
        .into_iter()
        .map(|(serial, role, id, not_after)| {
            (Serial::from_bytes(&serial), role, id, not_after.into())
        })
        .collect();
    rows.sort_by(|a, b| (&a.2, a.0.as_bytes()).cmp(&(&b.2, b.0.as_bytes())));
    Ok(rows)
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
