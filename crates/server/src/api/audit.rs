//! Writes to the append-only `audit_log`. Only INSERT, never UPDATE or
//! DELETE. Details must not contain secrets.

use serde_json::Value;
use sqlx::PgExecutor;

/// One audit row. `actor_kind` is one of the schema's values: `user`,
/// `system`, `ai` or `peer`.
#[derive(Debug, Clone)]
pub struct Event<'a> {
    pub actor_kind: &'a str,
    pub actor_id: &'a str,
    pub action: &'a str,
    pub target: Option<(&'a str, &'a str)>,
    pub details: Value,
}

/// Writes `event`. Within a request made with an API key, `details` gains
/// `api_key`, the key's ID, so the log tells a person from their script
/// (ADR-0101).
pub async fn record(db: impl PgExecutor<'_>, mut event: Event<'_>) -> Result<(), sqlx::Error> {
    if let (Some(key), Some(details)) = (super::v1::current_key(), event.details.as_object_mut()) {
        details.insert("api_key".into(), Value::String(key.to_string()));
    }
    let (target_kind, target_id) = event.target.unzip();
    sqlx::query(
        "INSERT INTO audit_log (actor_kind, actor_id, action, target_kind, target_id, details)
         VALUES ($1, $2, $3, $4, $5, $6::jsonb)",
    )
    .bind(event.actor_kind)
    .bind(event.actor_id)
    .bind(event.action)
    .bind(target_kind)
    .bind(target_id)
    .bind(event.details.to_string())
    .execute(db)
    .await?;
    Ok(())
}
