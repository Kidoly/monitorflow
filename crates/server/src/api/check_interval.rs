//! `PUT /api/sites/{id}/check-interval`: how often the built-in checks of a
//! site run (ADR-0097). `null` goes back to the built-in interval.
//! Operators of the site may; audited. The schema's trigger bumps the
//! site's sensor revision, so connected agents get it at the next poll.

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use super::access::authorize;
use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::sensors::{body, check_interval, parse_id};
use super::{ApiError, ApiState};

/// Body of the request and of the answer.
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CheckInterval {
    /// `null`: the built-in interval.
    pub interval_ms: Option<u64>,
}

/// The column's value for `request`, or why it is refused.
fn column(request: CheckInterval) -> Result<Option<i32>, String> {
    let Some(ms) = request.interval_ms else {
        return Ok(None);
    };
    check_interval(ms)?;
    // 24 h fits: check_interval bounds it.
    Ok(Some(i32::try_from(ms).map_err(|e| e.to_string())?))
}

pub async fn site(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<CheckInterval>, JsonRejection>,
) -> Result<Json<CheckInterval>, ApiError> {
    let id = parse_id(&id)?;
    let mut tx = state.pool.begin().await?;
    let before: Option<(Uuid, Option<i32>)> =
        sqlx::query_as("SELECT id, check_interval_ms FROM sites WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    authorize(&user, before.map(|s| s.0), true)?;
    let request = body(request)?;
    let value = column(request).map_err(ApiError::Invalid)?;
    sqlx::query("UPDATE sites SET check_interval_ms = $2 WHERE id = $1")
        .bind(id)
        .bind(value)
        .execute(&mut *tx)
        .await?;
    let actor = user.id.to_string();
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action: "site.check_interval",
            target: Some(("site", &id.to_string())),
            details: json!({
                "before": before.and_then(|s| s.1),
                "after": value,
            }),
        },
    )
    .await?;
    tx.commit().await?;
    tracing::info!(user = %user.id, site = %id, interval_ms = ?value, "check interval changed");
    Ok(Json(request))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(value: serde_json::Value) -> serde_json::Result<CheckInterval> {
        serde_json::from_value(value)
    }

    #[test]
    fn intervals_are_bounded_and_null_is_the_default() {
        let column = |v| column(parse(v).unwrap());
        assert_eq!(column(json!({"interval_ms": null})), Ok(None));
        assert_eq!(column(json!({})), Ok(None));
        assert_eq!(column(json!({"interval_ms": 10_000})), Ok(Some(10_000)));
        assert_eq!(
            column(json!({"interval_ms": 86_400_000})),
            Ok(Some(86_400_000))
        );
        for ms in [0, 5_000, 86_400_001u64] {
            let error = column(json!({ "interval_ms": ms })).unwrap_err();
            assert_eq!(error, "interval must be between 10 s and 24 h");
        }
        assert!(parse(json!({"interval_ms": -1})).is_err());
        assert!(parse(json!({"interval_ms": 60_000, "site": 1})).is_err());
    }
}
