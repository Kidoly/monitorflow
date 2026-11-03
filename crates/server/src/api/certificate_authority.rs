//! The certificate authority in the web interface (ADR-0088): when it
//! expires, the change under way and who has moved, and the buttons that
//! start a change or take its next step. Global administrators only.

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::auth::CurrentUser;
use super::server_update::require_global_admin;
use super::{ApiError, ApiState};
use crate::ca::{CaError, CaInfo, Stage};
use crate::ca_rotation::{self, Actor, PeerState, RotationError, Rotator};
use crate::renew::hex;

/// Peers listed by name; the rest are only counted.
const MAX_LISTED: usize = 50;

#[derive(Debug, Serialize)]
pub struct CaView {
    pub fingerprint: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    /// Less than a year left: time to change it.
    pub expires_soon: bool,
    /// When a planned change starts by itself if nobody starts one.
    pub auto_change_at: DateTime<Utc>,
    pub rotation: Option<RotationView>,
    /// Set when a restore brought back a CA this server had retired.
    pub restored_retired_at: Option<DateTime<Utc>>,
    /// Servers and relays whose certificate comes from a CA no longer
    /// trusted: they missed an emergency change and must be installed
    /// again. Up to 50.
    pub cut_off: Vec<PeerState>,
}

#[derive(Debug, Serialize)]
pub struct RotationView {
    /// `introduced` or `switched`.
    pub stage: Stage,
    pub emergency: bool,
    pub started_at: DateTime<Utc>,
    pub switched_at: Option<DateTime<Utc>>,
    /// Emergency: when the old CA stops being trusted.
    pub distrust_at: Option<DateTime<Utc>>,
    /// The new CA while introduced, the old one once switched.
    pub other_fingerprint: Option<String>,
    /// Servers and relays with a valid certificate.
    pub total: usize,
    /// Those done with the current step.
    pub done: usize,
    /// Those not done, by name, up to 50, last seen first.
    pub waiting: Vec<PeerState>,
}

#[derive(Debug, Deserialize)]
pub struct StartRequest {
    #[serde(default)]
    pub emergency: bool,
}

fn rotator(state: &ApiState) -> Result<&Rotator, ApiError> {
    state
        .ca_rotation
        .as_ref()
        .ok_or_else(|| ApiError::Conflict("this server has no certificate authority".into()))
}

fn time(t: std::time::SystemTime) -> DateTime<Utc> {
    t.into()
}

fn unix(t: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(t, 0).unwrap_or_default()
}

/// `GET /api/certificate-authority`.
pub async fn get(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<CaView>, ApiError> {
    require_global_admin(&user)?;
    let rotator = rotator(&state)?;
    view(rotator).await.map(Json)
}

async fn view(rotator: &Rotator) -> Result<CaView, ApiError> {
    let status = rotator.ca().status();
    let CaInfo {
        fingerprint,
        not_before,
        not_after,
        ..
    } = status.active;
    let fleet = ca_rotation::fleet(rotator.pool(), rotator.ca()).await?;
    let mut cut_off: Vec<_> = fleet.iter().filter(|p| p.cut_off).cloned().collect();
    cut_off.truncate(MAX_LISTED);
    let rotation = status.rotation.map(|rotation| {
        let total = fleet.iter().filter(|p| !p.cut_off).count();
        let mut waiting: Vec<_> = fleet
            .into_iter()
            .filter(|p| ca_rotation::is_waiting(rotation.stage, p))
            .collect();
        let done = total - waiting.len();
        waiting.sort_by(|a, b| b.last_seen_at.cmp(&a.last_seen_at));
        waiting.truncate(MAX_LISTED);
        RotationView {
            stage: rotation.stage,
            emergency: rotation.emergency,
            started_at: unix(rotation.started_at),
            switched_at: rotation.switched_at.map(unix),
            distrust_at: rotation.distrust_at.map(unix),
            other_fingerprint: status.other.map(|o| hex(&o.fingerprint)),
            total,
            done,
            waiting,
        }
    });
    let now = std::time::SystemTime::now();
    Ok(CaView {
        fingerprint: hex(&fingerprint),
        created_at: time(not_before),
        expires_at: time(not_after),
        expires_soon: not_after < now + ca_rotation::WARN_BEFORE,
        auto_change_at: time(not_after - ca_rotation::AUTO_START_BEFORE),
        rotation,
        restored_retired_at: status.restored_retired.map(|r| unix(r.retired_at)),
        cut_off,
    })
}

/// `POST /api/certificate-authority/rotation`: starts a change, planned or
/// after a suspected compromise.
pub async fn start(
    State(state): State<ApiState>,
    user: CurrentUser,
    Json(request): Json<StartRequest>,
) -> Result<(StatusCode, Json<CaView>), ApiError> {
    require_global_admin(&user)?;
    let rotator = rotator(&state)?;
    let id = user.id.to_string();
    rotator
        .start(request.emergency, Actor::User(&id))
        .await
        .map_err(api_error)?;
    tracing::info!(by = %user.username, emergency = request.emergency, "change of certificate authority started");
    Ok((StatusCode::ACCEPTED, Json(view(rotator).await?)))
}

/// `POST /api/certificate-authority/rotation/next`: takes the next step
/// now, without waiting for the servers and relays that have not moved.
pub async fn next(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<CaView>, ApiError> {
    require_global_admin(&user)?;
    let rotator = rotator(&state)?;
    let id = user.id.to_string();
    rotator
        .finish_now(Actor::User(&id))
        .await
        .map_err(api_error)?;
    tracing::info!(by = %user.username, "next step of the change of certificate authority taken");
    Ok(Json(view(rotator).await?))
}

fn api_error(error: RotationError) -> ApiError {
    match error {
        RotationError::Ca(CaError::InvalidRequest(why)) => ApiError::Conflict(why),
        other => ApiError::Internal(other.to_string()),
    }
}
