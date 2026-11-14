//! Check recommendations of a server (ADR-0134): what its agent saw
//! running, as checks it does not have yet. Readers see them; operators
//! of the site dismiss one for the server, or bring it back. Adding one
//! goes through the routes of checks and alerts, like adding by hand.

use std::collections::BTreeMap;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use super::access::authorize;
use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::sensors::{body, parse_id, site_of_server};
use super::{ApiError, ApiState};
use crate::discovery;
use crate::recommend::{self, Covered, Recommendation};

#[derive(Debug, Serialize)]
pub struct RecommendationView {
    #[serde(flatten)]
    pub recommendation: Recommendation,
    /// Who dismissed it and when; null while it stands.
    pub dismissed_by: Option<String>,
    pub dismissed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
pub struct RecommendationList {
    /// When the agent last reported what runs; null before its first
    /// report, or for an agent that does not report (Windows, older).
    pub reported_at: Option<DateTime<Utc>>,
    pub recommendations: Vec<RecommendationView>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyRequest {
    pub key: String,
}

/// The server's enabled checks, its own rows over its site's by name and
/// instance (ADR-0016).
async fn covered(pool: &PgPool, server: Uuid, site: Uuid) -> Result<Covered, sqlx::Error> {
    let rows: Vec<(bool, String, String, String, Value, bool)> = sqlx::query_as(
        "SELECT server_id IS NOT NULL, kind, name, instance, params, enabled
         FROM sensor_configs WHERE site_id = $1 OR server_id = $2",
    )
    .bind(site)
    .bind(server)
    .fetch_all(pool)
    .await?;
    let mut effective: BTreeMap<(String, String), (bool, String, Value, bool)> = BTreeMap::new();
    for (own, kind, name, instance, params, enabled) in rows {
        let key = (name, instance);
        if !own && effective.contains_key(&key) {
            continue;
        }
        effective.insert(key, (own, kind, params, enabled));
    }
    let mut covered = Covered::default();
    for (_, kind, params, enabled) in effective.into_values() {
        if enabled {
            covered.add(&kind, |key| {
                params.get(key).map(|v| match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
            });
        }
    }
    Ok(covered)
}

/// The recommendations of `server` and when its agent reported.
async fn compute(
    pool: &PgPool,
    server: Uuid,
    site: Uuid,
) -> Result<(Option<DateTime<Utc>>, Vec<Recommendation>), sqlx::Error> {
    let Some((found, reported_at)) = discovery::load(pool, server).await? else {
        return Ok((None, Vec::new()));
    };
    let covered = covered(pool, server, site).await?;
    Ok((Some(reported_at), recommend::recommend(&found, &covered)))
}

/// `GET /api/servers/{id}/recommendations`.
pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<RecommendationList>, ApiError> {
    let server = parse_id(&id)?;
    let site = authorize(&user, site_of_server(&state.pool, server).await?, false)?;
    let (reported_at, found) = compute(&state.pool, server, site).await?;
    let dismissed: Vec<(String, Option<String>, DateTime<Utc>)> = sqlx::query_as(
        "SELECT d.key, u.username, d.dismissed_at
         FROM check_recommendation_dismissals d LEFT JOIN users u ON u.id = d.dismissed_by
         WHERE d.server_id = $1",
    )
    .bind(server)
    .fetch_all(&state.pool)
    .await?;
    let recommendations = found
        .into_iter()
        .map(|recommendation| {
            let by = dismissed
                .iter()
                .find(|(key, ..)| *key == recommendation.key);
            RecommendationView {
                dismissed_by: by.and_then(|(_, user, _)| user.clone()),
                dismissed_at: by.map(|(.., at)| *at),
                recommendation,
            }
        })
        .collect();
    Ok(Json(RecommendationList {
        reported_at,
        recommendations,
    }))
}

/// `POST /api/servers/{id}/recommendations/dismiss`: hides a current
/// recommendation of the server for good.
pub async fn dismiss(
    state: State<ApiState>,
    user: CurrentUser,
    id: Path<String>,
    request: Result<Json<KeyRequest>, JsonRejection>,
) -> Result<StatusCode, ApiError> {
    change(true, state, user, id, request).await
}

/// `POST /api/servers/{id}/recommendations/restore`: shows a dismissed
/// recommendation again.
pub async fn restore(
    state: State<ApiState>,
    user: CurrentUser,
    id: Path<String>,
    request: Result<Json<KeyRequest>, JsonRejection>,
) -> Result<StatusCode, ApiError> {
    change(false, state, user, id, request).await
}

async fn change(
    dismiss: bool,
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<KeyRequest>, JsonRejection>,
) -> Result<StatusCode, ApiError> {
    let server = parse_id(&id)?;
    let site = authorize(&user, site_of_server(&state.pool, server).await?, true)?;
    let KeyRequest { key } = body(request)?;
    let mut tx = state.pool.begin().await?;
    let changed = if dismiss {
        // Only what is recommended now can be dismissed, so the table
        // holds keys the server made, never text from a request.
        let (_, current) = compute(&state.pool, server, site).await?;
        if !current.iter().any(|r| r.key == key) {
            return Err(ApiError::NotFound);
        }
        sqlx::query(
            "INSERT INTO check_recommendation_dismissals (server_id, key, dismissed_by)
             VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
        )
        .bind(server)
        .bind(&key)
        .bind(user.id)
        .execute(&mut *tx)
        .await?
        .rows_affected()
    } else {
        sqlx::query("DELETE FROM check_recommendation_dismissals WHERE server_id = $1 AND key = $2")
            .bind(server)
            .bind(&key)
            .execute(&mut *tx)
            .await?
            .rows_affected()
    };
    if changed > 0 {
        let actor = user.id.to_string();
        let action = if dismiss {
            "check_recommendation.dismiss"
        } else {
            "check_recommendation.restore"
        };
        audit::record(
            &mut *tx,
            Event {
                actor_kind: "user",
                actor_id: &actor,
                action,
                target: Some(("server", &server.to_string())),
                details: json!({ "key": key }),
            },
        )
        .await?;
    } else if !dismiss {
        return Err(ApiError::NotFound);
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
