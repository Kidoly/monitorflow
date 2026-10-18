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

pub async fn record(db: impl PgExecutor<'_>, event: Event<'_>) -> Result<(), sqlx::Error> {
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
