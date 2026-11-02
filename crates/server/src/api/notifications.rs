//! Notification channels (ADR-0060): email, webhook, and Slack, Teams or
//! Discord (ADR-0077) destinations, with a test that answers in plain
//! words. Each covers everything, one customer or one site, and an
//! administrator manages exactly the channels of what they administer
//! (ADR-0078): others do not exist for them. The SMTP password, webhook
//! signing secret or chat webhook URL is written, never read back: views
//! say `has_secret`.

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
use super::roles::Role;
use super::sensors::{body, parse_id};
use super::{ApiError, ApiState};
use crate::notify::{self, Channel, Coverage, Destination, Dispatcher};

/// Longest accepted secret.
const MAX_SECRET: usize = 1024;

/// A channel as the web interface shows it.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct ChannelView {
    pub id: Uuid,
    /// `email`, `webhook`, `slack`, `teams` or `discord`.
    pub kind: String,
    /// email: `{"host", "port", "username", "from", "to"}`; webhook:
    /// `{"url"}`; chat: `{"name", "host"}`.
    pub settings: Value,
    /// Whether a password, signing secret or chat webhook URL is stored.
    pub has_secret: bool,
    /// The customer it covers, or the customer of the site it covers;
    /// `null` for everything.
    pub customer_id: Option<Uuid>,
    pub customer_name: Option<String>,
    /// The site it covers; `null` for a whole customer or everything.
    pub site_id: Option<Uuid>,
    pub site_name: Option<String>,
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
    /// The SMTP password, webhook signing secret or chat webhook URL. On
    /// `PUT`, absent, `null` or empty keeps the stored one.
    #[serde(default)]
    pub secret: Option<String>,
    /// The customer to cover; with `site_id` also `null`, everything.
    #[serde(default)]
    pub customer_id: Option<Uuid>,
    /// The site to cover. Not together with `customer_id`.
    #[serde(default)]
    pub site_id: Option<Uuid>,
}

/// Body of `POST /api/notifications/channels/{id}/test`.
#[derive(Debug, Serialize)]
pub struct TestResult {
    pub ok: bool,
    /// Why it failed, in plain words.
    pub error: Option<String>,
}

const VIEW: &str = "SELECT n.id, n.kind, n.settings, n.secret IS NOT NULL AS has_secret,
        coalesce(n.customer_id, s.customer_id) AS customer_id, c.name AS customer_name,
        n.site_id, s.name AS site_name, n.created_at, n.last_sent_at,
        n.last_error, n.last_error_at
    FROM notification_channels n
    LEFT JOIN sites s ON s.id = n.site_id
    LEFT JOIN customers c ON c.id = coalesce(n.customer_id, s.customer_id)";

pub(super) fn is_global_admin(user: &CurrentUser) -> bool {
    user.administered_customers() == Scope::All
}

/// Whether `user` may see and change what covers `coverage`: everything
/// takes a global administrator, a customer an administrator of it, and a
/// site an administrator of it or of its customer.
pub(super) fn manages(user: &CurrentUser, coverage: Coverage) -> bool {
    match coverage {
        Coverage::Everything => is_global_admin(user),
        Coverage::Customer(id) => user.administered_customers().contains(id),
        Coverage::Site(id) => user.sites_with(Role::Administrator).contains(id),
    }
}

/// Only administrators reach channels at all; for them a channel they do
/// not manage is 404, like a site they do not read.
pub(super) fn require_administrator(user: &CurrentUser) -> Result<(), ApiError> {
    if user.is_administrator() {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}

async fn view(db: impl PgExecutor<'_>, id: Uuid) -> Result<ChannelView, ApiError> {
    // SQL-safe: interpolates only the constant VIEW.
    sqlx::query_as(sqlx::AssertSqlSafe(format!("{VIEW} WHERE n.id = $1")))
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or(ApiError::NotFound)
}

/// A valid request: what it sends to, its secret and what it covers.
struct Valid {
    destination: Destination,
    secret: Option<Secret>,
    coverage: Coverage,
}

/// Checks a request `user` may make; `current` is the channel it edits, if
/// any. Whether `user` manages what it asks to cover comes first and needs
/// no lookup, so a refusal says nothing about which customers and sites
/// exist. A chat channel's secret is its URL: a new one is checked for its
/// service, and an edit without one keeps the stored URL's host, so that
/// [`Destination::same_target`] sees the same place.
async fn validate(
    db: impl PgExecutor<'_>,
    user: &CurrentUser,
    request: ChannelRequest,
    current: Option<&Channel>,
) -> Result<Valid, ApiError> {
    let coverage = Coverage::from_columns(request.customer_id, request.site_id)
        .ok_or_else(|| ApiError::Invalid("choose a customer or a site, not both".into()))?;
    if !manages(user, coverage) {
        return Err(ApiError::Forbidden);
    }
    let (destination, secret) = checked(
        &request.kind,
        request.settings,
        request.secret,
        current.map(|c| &c.destination),
    )?;
    let unknown = match coverage {
        Coverage::Everything => None,
        Coverage::Customer(id) => Some((
            sqlx::query_scalar("SELECT id FROM customers WHERE id = $1").bind(id),
            "unknown customer",
        )),
        Coverage::Site(id) => Some((
            sqlx::query_scalar("SELECT id FROM sites WHERE id = $1").bind(id),
            "unknown site",
        )),
    };
    if let Some((query, error)) = unknown {
        let exists: Option<Uuid> = query.fetch_optional(db).await?;
        exists.ok_or_else(|| ApiError::Invalid(error.into()))?;
    }
    Ok(Valid {
        destination,
        secret: secret.map(Secret::new),
        coverage,
    })
}

/// The destination of `kind` and `settings`, trimmed and checked, with
/// the secret to store: `None` keeps the stored one. A chat destination's
/// secret is its URL: a new one is checked for its service, and without
/// one the stored chat URL's host is kept, so that
/// [`Destination::same_target`] sees the same place.
pub(super) fn checked(
    kind: &str,
    mut settings: Value,
    secret: Option<String>,
    current: Option<&Destination>,
) -> Result<(Destination, Option<String>), ApiError> {
    let Some(object) = settings.as_object_mut() else {
        return Err(ApiError::Invalid("settings must be an object".into()));
    };
    object.insert("kind".into(), kind.into());
    let destination: Destination = serde_json::from_value(settings)
        .map_err(|error| ApiError::Invalid(format!("invalid {kind} settings: {error}")))?;
    let mut destination = destination.normalize().map_err(ApiError::Invalid)?;
    let mut secret = secret.filter(|s| !s.is_empty());
    if secret.as_ref().is_some_and(|s| s.len() > MAX_SECRET) {
        return Err(ApiError::Invalid(format!(
            "the secret is longer than {MAX_SECRET} bytes"
        )));
    }
    if let Some((service, _)) = destination.chat() {
        let host = match (secret.take(), current.and_then(Destination::chat)) {
            (Some(url), _) => {
                let checked = service.check_url(&url).map_err(ApiError::Invalid)?;
                secret = Some(checked.url);
                checked.host
            }
            (None, Some((_, chat))) => chat.host.clone(),
            (None, None) => {
                return Err(ApiError::Invalid(format!(
                    "Paste the {} webhook URL.",
                    service.name()
                )))
            }
        };
        destination = destination.with_chat_host(&host);
    }
    Ok((destination, secret))
}

/// What the audit keeps of a channel: no secret, and only the host of a
/// webhook, whose path often is one.
fn audited(destination: &Destination, coverage: Coverage) -> Value {
    let target = match destination {
        Destination::Email(email) => json!({
            "host": email.host, "port": email.port, "to": email.to,
        }),
        Destination::Webhook(webhook) => json!({
            "host": reqwest::Url::parse(&webhook.url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_owned)),
        }),
        Destination::Slack(chat) | Destination::Teams(chat) | Destination::Discord(chat) => {
            json!({ "host": chat.host, "name": chat.name })
        }
    };
    let (customer_id, site_id) = coverage.columns();
    json!({
        "kind": destination.kind(),
        "target": target,
        "customer_id": customer_id,
        "site_id": site_id,
    })
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

/// `GET /api/notifications/channels`: the channels `user` manages.
pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<ChannelList>, ApiError> {
    require_administrator(&user)?;
    let (all, customers) = user.administered_customers().bind();
    let (_, sites) = user.sites_with(Role::Administrator).bind();
    // SQL-safe: interpolates only the constant VIEW.
    let channels = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{VIEW} WHERE $1 OR n.customer_id = ANY($2) OR n.site_id = ANY($3)
         ORDER BY n.created_at, n.id"
    )))
    .bind(all)
    .bind(&customers)
    .bind(&sites)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(ChannelList { channels }))
}

/// `POST /api/notifications/channels`: administrators of what it covers;
/// audited as `notification.channel_create`.
pub async fn create(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<ChannelRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<ChannelView>), ApiError> {
    require_administrator(&user)?;
    let request = body(request)?;
    let valid = validate(&state.pool, &user, request, None).await?;
    let mut tx = state.pool.begin().await?;
    let id = notify::insert(
        &mut *tx,
        &valid.destination,
        valid.secret.as_ref(),
        valid.coverage,
        !is_global_admin(&user),
    )
    .await?;
    let details = audited(&valid.destination, valid.coverage);
    record(
        &mut *tx,
        &user,
        "notification.channel_create",
        id,
        details,
        valid.secret.is_some(),
    )
    .await?;
    let view = view(&mut *tx, id).await?;
    tx.commit().await?;
    tracing::info!(user = %user.id, channel = %id, kind = valid.destination.kind(), "added a notification channel");
    Ok((StatusCode::CREATED, Json(view)))
}

/// `PUT /api/notifications/channels/{id}`: administrators of what it covers
/// now and of what it will cover; replaces the settings and coverage, and
/// the secret when one is given; audited as `notification.channel_update`.
///
/// A stored secret stays only with its target, or a customer administrator
/// could point a channel an MSP set up at their own server and receive the
/// MSP's mail password. Whoever changes where a channel sends types it
/// again.
pub async fn update(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<ChannelRequest>, JsonRejection>,
) -> Result<Json<ChannelView>, ApiError> {
    require_administrator(&user)?;
    let id = parse_id(&id)?;
    let request = body(request)?;
    let mut tx = state.pool.begin().await?;
    let current = notify::lock(&mut *tx, id)
        .await?
        .filter(|c| manages(&user, c.coverage))
        .ok_or(ApiError::NotFound)?;
    let valid = validate(&mut *tx, &user, request, Some(&current)).await?;
    // A new chat URL is a new target, though the host may stay the same.
    let new_chat_url = valid.destination.chat().is_some() && valid.secret.is_some();
    let same_target = current.destination.same_target(&valid.destination) && !new_chat_url;
    if !same_target && current.secret.is_some() && valid.secret.is_none() {
        return Err(ApiError::Invalid(
            "Enter the password, signing secret or webhook URL again: a saved one stays with the address it was saved for."
                .into(),
        ));
    }
    // A global administrator's save vouches for the destination; anyone
    // else's leaves it limited, and limits a new one.
    let public_only = !is_global_admin(&user) && (current.public_only || !same_target);
    let found = notify::update(
        &mut *tx,
        id,
        &valid.destination,
        valid.secret.as_ref().map(Some),
        valid.coverage,
        public_only,
    )
    .await?;
    if !found {
        return Err(ApiError::NotFound);
    }
    let details = audited(&valid.destination, valid.coverage);
    record(
        &mut *tx,
        &user,
        "notification.channel_update",
        id,
        details,
        valid.secret.is_some(),
    )
    .await?;
    let view = view(&mut *tx, id).await?;
    tx.commit().await?;
    Ok(Json(view))
}

/// `DELETE /api/notifications/channels/{id}`: administrators of what it
/// covers; 204; audited as `notification.channel_delete`.
pub async fn delete(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_administrator(&user)?;
    let id = parse_id(&id)?;
    let mut tx = state.pool.begin().await?;
    let channel = notify::lock(&mut *tx, id)
        .await?
        .filter(|c| manages(&user, c.coverage))
        .ok_or(ApiError::NotFound)?;
    sqlx::query("DELETE FROM notification_channels WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    record(
        &mut *tx,
        &user,
        "notification.channel_delete",
        id,
        json!({ "kind": channel.destination.kind() }),
        false,
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/notifications/channels/{id}/test`: administrators of what it
/// covers; sends a test now and answers whether it went through, with the
/// error in plain words. The outcome is also the channel's last status.
pub async fn test(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<TestResult>, ApiError> {
    require_administrator(&user)?;
    let id = parse_id(&id)?;
    let channel = notify::load(&state.pool, id)
        .await?
        .filter(|c| manages(&user, c.coverage))
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
            error: Some(error.message),
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::{Chat, Email, Webhook};

    #[test]
    fn the_audit_keeps_no_secret_and_only_the_webhook_host() {
        let hook = Destination::Webhook(Webhook {
            url: "https://hooks.slack.com/services/T0/B0/XXXXSECRET".into(),
        });
        let details = audited(&hook, Coverage::Everything).to_string();
        assert!(details.contains("hooks.slack.com"));
        assert!(!details.contains("XXXXSECRET"));
        let mail = Destination::Email(Email {
            host: "smtp.x.io".into(),
            port: 587,
            username: "ops".into(),
            from: "ops@x.io".into(),
            to: vec!["a@x.io".into()],
        });
        let details = audited(&mail, Coverage::Site(Uuid::nil()));
        assert_eq!(details["target"]["to"][0], "a@x.io");
        assert_eq!(details["site_id"], Uuid::nil().to_string());
        assert_eq!(details["customer_id"], Value::Null);
        let chat = Destination::Slack(Chat {
            name: "#ops".into(),
            host: "hooks.slack.com".into(),
        });
        let details = audited(&chat, Coverage::Everything);
        assert_eq!(details["target"]["host"], "hooks.slack.com");
        assert_eq!(details["kind"], "slack");
    }
}
