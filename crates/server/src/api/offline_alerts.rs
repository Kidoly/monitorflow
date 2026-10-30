//! `PUT /api/{sites,servers,relays}/{id}/offline-alerts`: turns offline
//! incidents (ADR-0057) off or back on for a site, a server or a relay.
//! Operators of the site may; audited. Open incidents of a muted peer
//! resolve at the next check.

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use super::access::authorize;
use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::sensors::{body, parse_id};
use super::{ApiError, ApiState};

/// Body of the request and of the answer.
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OfflineAlerts {
    pub enabled: bool,
}

#[derive(Debug, Clone, Copy)]
enum Owner {
    Site,
    Server,
    Relay,
}

impl Owner {
    fn kind(self) -> &'static str {
        match self {
            Self::Site => "site",
            Self::Server => "server",
            Self::Relay => "relay",
        }
    }

    /// The owner's site, or its ID for a site.
    fn site_query(self) -> &'static str {
        match self {
            Self::Site => "SELECT id FROM sites WHERE id = $1",
            Self::Server => "SELECT site_id FROM servers WHERE id = $1",
            Self::Relay => "SELECT site_id FROM relays WHERE id = $1",
        }
    }

    fn update(self) -> &'static str {
        match self {
            Self::Site => "UPDATE sites SET offline_alerts = $2 WHERE id = $1",
            Self::Server => "UPDATE servers SET offline_alerts = $2 WHERE id = $1",
            Self::Relay => "UPDATE relays SET offline_alerts = $2 WHERE id = $1",
        }
    }
}

pub async fn site(
    state: State<ApiState>,
    user: CurrentUser,
    id: Path<String>,
    request: Result<Json<OfflineAlerts>, JsonRejection>,
) -> Result<Json<OfflineAlerts>, ApiError> {
    set(Owner::Site, state, user, id, request).await
}

pub async fn server(
    state: State<ApiState>,
    user: CurrentUser,
    id: Path<String>,
    request: Result<Json<OfflineAlerts>, JsonRejection>,
) -> Result<Json<OfflineAlerts>, ApiError> {
    set(Owner::Server, state, user, id, request).await
}

pub async fn relay(
    state: State<ApiState>,
    user: CurrentUser,
    id: Path<String>,
    request: Result<Json<OfflineAlerts>, JsonRejection>,
) -> Result<Json<OfflineAlerts>, ApiError> {
    set(Owner::Relay, state, user, id, request).await
}

async fn set(
    owner: Owner,
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<OfflineAlerts>, JsonRejection>,
) -> Result<Json<OfflineAlerts>, ApiError> {
    let id = parse_id(&id)?;
    let site: Option<Uuid> = sqlx::query_scalar(owner.site_query())
        .bind(id)
        .fetch_optional(&state.pool)
        .await?;
    authorize(&user, site, true)?;
    let request = body(request)?;
    let mut tx = state.pool.begin().await?;
    sqlx::query(owner.update())
        .bind(id)
        .bind(request.enabled)
        .execute(&mut *tx)
        .await?;
    let actor = user.id.to_string();
    let action = format!("{}.offline_alerts", owner.kind());
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action: &action,
            target: Some((owner.kind(), &id.to_string())),
            details: json!({ "enabled": request.enabled }),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(Json(request))
}
