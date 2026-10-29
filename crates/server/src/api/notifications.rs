//! Notification channels (ADR-0058): email and webhook destinations set
//! up by global administrators, each optionally limited to one customer,
//! with a test that answers in plain words. The SMTP password or webhook
//! signing secret is written, never read back: views say `has_secret`.

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use monitorflow_common::redact::Secret;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgExecutor;
use uuid::Uuid;

use super::access::Scope;
use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::sensors::{body, parse_id};
use super::{ApiError, ApiState};
use crate::notify::{self, Destination, Dispatcher};

/// Longest accepted secret.
const MAX_SECRET: usize = 1024;

/// A channel as the web interface shows it.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct ChannelView {
    pub id: Uuid,
    /// `email` or `webhook`.
    pub kind: String,
    /// email: `{"host", "port", "username", "from", "to"}`; webhook:
    /// `{"url"}`.
    pub settings: Value,
    /// Whether a password or signing secret is stored.
    pub has_secret: bool,
    /// `null` for every customer.
    pub customer_id: Option<Uuid>,
    pub customer_name: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_sent_at: Option<DateTime<Utc>>,
    /// The last delivery's or test's error, `null` once one works.
    pub last_error: Option<String>,
    pub last_error_at: Option<DateTime<Utc>>,
}

/// Body of `GET /api/notifications/channels`.
#[derive(Debug, Serialize)]
pub struct ChannelList {
    pub channels: Vec<ChannelView>,
}

/// Body of `POST /api/notifications/channels` and of `PUT` on one.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelRequest {
    pub kind: String,
    pub settings: Value,
    /// The SMTP password or webhook signing secret. On `PUT`, absent,
    /// `null` or empty keeps the stored one.
    #[serde(default)]
    pub secret: Option<String>,
    /// `null` for every customer.
    #[serde(default)]
    pub customer_id: Option<Uuid>,
}

/// Body of `POST /api/notifications/channels/{id}/test`.
#[derive(Debug, Serialize)]
pub struct TestResult {
    pub ok: bool,
    /// Why it failed, in plain words.
    pub error: Option<String>,
}

const VIEW: &str = "SELECT n.id, n.kind, n.settings, n.secret IS NOT NULL AS has_secret,
        n.customer_id, c.name AS customer_name, n.created_at, n.last_sent_at,
        n.last_error, n.last_error_at
    FROM notification_channels n LEFT JOIN customers c ON c.id = n.customer_id";

fn require_global_admin(user: &CurrentUser) -> Result<(), ApiError> {
    if user.administered_customers() == Scope::All {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}

async fn view(db: impl PgExecutor<'_>, id: Uuid) -> Result<ChannelView, ApiError> {
    sqlx::query_as(&format!("{VIEW} WHERE n.id = $1"))
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or(ApiError::NotFound)
}

/// The destination, the secret and the customer of a valid request.
async fn validate(
    db: impl PgExecutor<'_>,
    request: ChannelRequest,
) -> Result<(Destination, Option<Secret>, Option<Uuid>), ApiError> {
    let mut settings = request.settings;
    let Some(object) = settings.as_object_mut() else {
        return Err(ApiError::Invalid("settings must be an object".into()));
    };
    object.insert("kind".into(), request.kind.clone().into());
    let destination: Destination = serde_json::from_value(settings).map_err(|error| {
        ApiError::Invalid(format!("invalid {} settings: {error}", request.kind))
    })?;
    let destination = destination.normalize().map_err(ApiError::Invalid)?;
    let secret = request.secret.filter(|s| !s.is_empty());
    if secret.as_ref().is_some_and(|s| s.len() > MAX_SECRET) {
        return Err(ApiError::Invalid(format!(
            "the secret is longer than {MAX_SECRET} bytes"
        )));
    }
    if let Some(customer) = request.customer_id {
        let exists: Option<Uuid> = sqlx::query_scalar("SELECT id FROM customers WHERE id = $1")
            .bind(customer)
            .fetch_optional(db)
            .await?;
        exists.ok_or_else(|| ApiError::Invalid("unknown customer".into()))?;
    }
    Ok((destination, secret.map(Secret::new), request.customer_id))
}

/// What the audit keeps of a channel: no secret, and only the host of a
/// webhook, whose path often is one (Slack, Teams).
fn audited(destination: &Destination, customer: Option<Uuid>) -> Value {
    let target = match destination {
        Destination::Email(email) => json!({
            "host": email.host, "port": email.port, "to": email.to,
        }),
        Destination::Webhook(webhook) => json!({
            "host": reqwest::Url::parse(&webhook.url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_owned)),
        }),
    };
    json!({ "kind": destination.kind(), "target": target, "customer_id": customer })
}

async fn record(
    db: impl PgExecutor<'_>,
    user: &CurrentUser,
    action: &str,
    id: Uuid,
    mut details: Value,
    secret_set: bool,
) -> Result<(), ApiError> {
    details["secret_set"] = secret_set.into();
    let actor = user.id.to_string();
    let target = id.to_string();
    audit::record(
        db,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action,
            target: Some(("notification_channel", &target)),
            details,
        },
    )
    .await?;
    Ok(())
}

/// `GET /api/notifications/channels`: global administrators.
pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<ChannelList>, ApiError> {
    require_global_admin(&user)?;
    let channels = sqlx::query_as(&format!("{VIEW} ORDER BY n.created_at, n.id"))
        .fetch_all(&state.pool)
        .await?;
    Ok(Json(ChannelList { channels }))
}

/// `POST /api/notifications/channels`: global administrators; audited as
/// `notification.channel_create`.
pub async fn create(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<ChannelRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<ChannelView>), ApiError> {
    require_global_admin(&user)?;
    let request = body(request)?;
    let (destination, secret, customer) = validate(&state.pool, request).await?;
    let mut tx = state.pool.begin().await?;
    let id = notify::insert(&mut *tx, &destination, secret.as_ref(), customer).await?;
    let details = audited(&destination, customer);
    record(
        &mut *tx,
        &user,
        "notification.channel_create",
        id,
        details,
        secret.is_some(),
    )
    .await?;
    let view = view(&mut *tx, id).await?;
    tx.commit().await?;
    tracing::info!(user = %user.id, channel = %id, kind = destination.kind(), "added a notification channel");
    Ok((StatusCode::CREATED, Json(view)))
}

/// `PUT /api/notifications/channels/{id}`: global administrators; replaces
/// the settings and customer, and the secret when one is given; audited as
/// `notification.channel_update`.
pub async fn update(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<ChannelRequest>, JsonRejection>,
) -> Result<Json<ChannelView>, ApiError> {
    require_global_admin(&user)?;
    let id = parse_id(&id)?;
    let request = body(request)?;
    let (destination, secret, customer) = validate(&state.pool, request).await?;
    let mut tx = state.pool.begin().await?;
    let found = notify::update(
        &mut *tx,
        id,
        &destination,
        secret.as_ref().map(Some),
        customer,
    )
    .await?;
    if !found {
        return Err(ApiError::NotFound);
    }
    let details = audited(&destination, customer);
    record(
        &mut *tx,
        &user,
        "notification.channel_update",
        id,
        details,
        secret.is_some(),
    )
    .await?;
    let view = view(&mut *tx, id).await?;
    tx.commit().await?;
    Ok(Json(view))
}

/// `DELETE /api/notifications/channels/{id}`: global administrators; 204;
/// audited as `notification.channel_delete`.
pub async fn delete(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_global_admin(&user)?;
    let id = parse_id(&id)?;
    let mut tx = state.pool.begin().await?;
    let kind: Option<String> =
        sqlx::query_scalar("DELETE FROM notification_channels WHERE id = $1 RETURNING kind")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let kind = kind.ok_or(ApiError::NotFound)?;
    record(
        &mut *tx,
        &user,
        "notification.channel_delete",
        id,
        json!({ "kind": kind }),
        false,
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/notifications/channels/{id}/test`: global administrators;
/// sends a test now and answers whether it went through, with the error
/// in plain words. The outcome is also the channel's last status.
pub async fn test(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<TestResult>, ApiError> {
    require_global_admin(&user)?;
    let id = parse_id(&id)?;
    let channel = notify::load(&state.pool, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let dispatcher = Dispatcher::new(state.pool.clone(), state.settings.web_url.clone());
    let result = dispatcher.test(&channel).await?;
    tracing::info!(user = %user.id, channel = %id, ok = result.is_ok(), "tested a notification channel");
    Ok(Json(match result {
        Ok(()) => TestResult {
            ok: true,
            error: None,
        },
        Err(error) => TestResult {
            ok: false,
            error: Some(error.0),
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::{Email, Webhook};

    #[test]
    fn the_audit_keeps_no_secret_and_only_the_webhook_host() {
        let hook = Destination::Webhook(Webhook {
            url: "https://hooks.slack.com/services/T0/B0/XXXXSECRET".into(),
        });
        let details = audited(&hook, None).to_string();
        assert!(details.contains("hooks.slack.com"));
        assert!(!details.contains("XXXXSECRET"));
        let mail = Destination::Email(Email {
            host: "smtp.x.io".into(),
            port: 587,
            username: "ops".into(),
            from: "ops@x.io".into(),
            to: vec!["a@x.io".into()],
        });
        let details = audited(&mail, Some(Uuid::nil()));
        assert_eq!(details["target"]["to"][0], "a@x.io");
        assert_eq!(details["customer_id"], Uuid::nil().to_string());
    }
}
