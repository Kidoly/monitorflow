//! The signed-in person's phones (ADR-0186): the web app installed on a
//! phone, its long session, and its push notifications.
//!
//! Only for a person in a browser: a phone is the browser it is added
//! from, and its push subscription comes from that browser's Push API. v1
//! leaves `/api/me/` out ([`super::v1::left_out`]) and an API key gets 403.
//! Every change is audited.

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::header::SET_COOKIE;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{PgExecutor, Row};
use uuid::Uuid;

use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::sensors::{body, parse_id};
use super::{session, ApiError, ApiState};
use crate::notify::push::{self, Notice, Outcome, Subscription};
use crate::notify::Dispatcher;

/// Most phones one person may add.
pub const MAX_PHONES: i64 = 10;
/// Longest name of a phone.
pub const MAX_NAME: usize = 80;

/// One phone, as its owner sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PhoneView {
    pub id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
    /// When the app was last used on it.
    pub last_seen_at: DateTime<Utc>,
    /// Whether notifications are on.
    pub push: bool,
    /// The push service its browser uses, like `web.push.apple.com`.
    pub push_host: Option<String>,
    /// When the last notification was taken by the push service.
    pub push_sent_at: Option<DateTime<Utc>>,
    /// Why the last one was not, in a sentence.
    pub push_error: Option<String>,
    /// Whether it is the phone of the request's own session.
    pub this_phone: bool,
}

/// `GET /api/me/phones`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Phones {
    pub phones: Vec<PhoneView>,
    /// The key a browser subscribes with (`applicationServerKey`).
    pub push_key: String,
    /// How long a phone's session lasts, in days, and how long it lasts
    /// unused.
    pub session_days: u64,
    pub idle_days: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddPhone {
    pub name: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rename {
    pub name: String,
}

/// A browser's `PushSubscription.toJSON()`.
#[derive(Debug, Deserialize)]
pub struct PushRequest {
    pub endpoint: String,
    pub keys: PushKeys,
    /// Sent by some browsers; not used.
    #[serde(default, rename = "expirationTime")]
    pub _expiration_time: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct PushKeys {
    pub p256dh: String,
    pub auth: String,
}

/// `POST /api/me/phones/{id}/test`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TestResult {
    pub ok: bool,
    pub error: Option<String>,
}

fn name(text: &str) -> Result<String, ApiError> {
    let name = text.trim();
    if name.is_empty() {
        return Err(ApiError::Invalid("Give the phone a name.".into()));
    }
    if name.chars().count() > MAX_NAME {
        return Err(ApiError::Invalid(format!(
            "A phone's name has at most {MAX_NAME} characters."
        )));
    }
    if name.chars().any(char::is_control) {
        return Err(ApiError::Invalid(
            "A phone's name is one line of text.".into(),
        ));
    }
    Ok(name.to_owned())
}

fn view(row: &sqlx::postgres::PgRow, this: Option<Uuid>) -> Result<PhoneView, sqlx::Error> {
    let id: Uuid = row.try_get("id")?;
    let endpoint: Option<String> = row.try_get("push_endpoint")?;
    let push_host = endpoint.as_deref().and_then(|e| {
        reqwest::Url::parse(e)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
    });
    Ok(PhoneView {
        id,
        name: row.try_get("name")?,
        created_at: row.try_get("created_at")?,
        last_seen_at: row.try_get("last_seen_at")?,
        push: endpoint.is_some(),
        push_host,
        push_sent_at: row.try_get("push_sent_at")?,
        push_error: row.try_get("push_error")?,
        this_phone: this == Some(id),
    })
}

const COLUMNS: &str = "id, name, created_at, last_seen_at, push_endpoint, push_sent_at, push_error";

async fn one(
    db: impl PgExecutor<'_>,
    user: Uuid,
    id: Uuid,
    this: Option<Uuid>,
) -> Result<PhoneView, ApiError> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM phones WHERE id = $1 AND user_id = $2"
    )))
    .bind(id)
    .bind(user)
    .fetch_optional(db)
    .await?
    .ok_or(ApiError::NotFound)?;
    Ok(view(&row, this)?)
}

async fn this_phone(state: &ApiState, user: &CurrentUser) -> Result<Option<Uuid>, ApiError> {
    match user.session() {
        Some(token) => Ok(session::phone_of(&state.pool, token).await?),
        None => Ok(None),
    }
}

async fn record(
    db: impl PgExecutor<'_>,
    user: &CurrentUser,
    action: &str,
    phone: Uuid,
    details: Value,
) -> Result<(), ApiError> {
    let actor = user.id.to_string();
    let id = phone.to_string();
    audit::record(
        db,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action,
            target: Some(("phone", &id)),
            details,
        },
    )
    .await?;
    Ok(())
}

/// The person's own phone: 404 for anyone else's.
async fn own(state: &ApiState, user: &CurrentUser, id: &str) -> Result<Uuid, ApiError> {
    let id = parse_id(id)?;
    let found: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM phones WHERE id = $1 AND user_id = $2")
            .bind(id)
            .bind(user.id)
            .fetch_optional(&state.pool)
            .await?;
    found.ok_or(ApiError::NotFound)
}

/// `GET /api/me/phones`: the person's phones, newest first.
pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<Phones>, ApiError> {
    user.in_person()?;
    let this = this_phone(&state, &user).await?;
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM phones WHERE user_id = $1 ORDER BY created_at DESC, id"
    )))
    .bind(user.id)
    .fetch_all(&state.pool)
    .await?;
    let phones = rows
        .iter()
        .map(|row| view(row, this))
        .collect::<Result<_, _>>()?;
    let vapid = push::vapid(&state.pool)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(Phones {
        phones,
        push_key: vapid.public_base64(),
        session_days: session::phone_lifetime(&state.settings).as_secs() / 86_400,
        idle_days: session::phone_idle(&state.settings).as_secs() / 86_400,
    }))
}

/// `POST /api/me/phones`: makes the browser of this session a phone of the
/// person. Its session lasts as long as a phone's from now, and ends when
/// the phone is removed. A session that is a phone already keeps it,
/// renamed.
pub async fn add(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<AddPhone>, JsonRejection>,
) -> Result<Response, ApiError> {
    user.in_person()?;
    let token = user.session().ok_or(ApiError::Forbidden)?.clone();
    let name = name(&body(request)?.name)?;
    let mut tx = state.pool.begin().await?;
    // One at a time per person, so the limit holds.
    sqlx::query("SELECT 1 FROM users WHERE id = $1 FOR UPDATE")
        .bind(user.id)
        .execute(&mut *tx)
        .await?;
    let id = match session::phone_of(&mut *tx, &token).await? {
        Some(id) => {
            sqlx::query("UPDATE phones SET name = $2 WHERE id = $1")
                .bind(id)
                .bind(&name)
                .execute(&mut *tx)
                .await?;
            id
        }
        None => {
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM phones WHERE user_id = $1")
                .bind(user.id)
                .fetch_one(&mut *tx)
                .await?;
            if count >= MAX_PHONES {
                return Err(ApiError::Conflict(format!(
                    "You have {MAX_PHONES} phones already. Remove one you no longer use first."
                )));
            }
            sqlx::query_scalar("INSERT INTO phones (user_id, name) VALUES ($1, $2) RETURNING id")
                .bind(user.id)
                .bind(&name)
                .fetch_one(&mut *tx)
                .await?
        }
    };
    if !session::attach_phone(&mut *tx, &token, id, &state.settings).await? {
        return Err(ApiError::Unauthorized);
    }
    record(
        &mut *tx,
        &user,
        "user.phone_add",
        id,
        json!({ "name": name }),
    )
    .await?;
    let view = one(&mut *tx, user.id, id, Some(id)).await?;
    tx.commit().await?;
    let mut headers = HeaderMap::new();
    headers.insert(
        SET_COOKIE,
        session::set_phone_cookie(&state.settings, &token),
    );
    Ok((StatusCode::CREATED, headers, Json(view)).into_response())
}

/// `PUT /api/me/phones/{id}`: renames a phone.
pub async fn rename(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<Rename>, JsonRejection>,
) -> Result<Json<PhoneView>, ApiError> {
    user.in_person()?;
    let id = own(&state, &user, &id).await?;
    let name = name(&body(request)?.name)?;
    let this = this_phone(&state, &user).await?;
    let mut tx = state.pool.begin().await?;
    sqlx::query("UPDATE phones SET name = $2 WHERE id = $1")
        .bind(id)
        .bind(&name)
        .execute(&mut *tx)
        .await?;
    record(
        &mut *tx,
        &user,
        "user.phone_rename",
        id,
        json!({ "name": name }),
    )
    .await?;
    let view = one(&mut *tx, user.id, id, this).await?;
    tx.commit().await?;
    Ok(Json(view))
}

/// `PUT /api/me/phones/{id}/push`: turns notifications on with the
/// browser's subscription. Only from the phone itself: the subscription
/// belongs to the browser that made it.
pub async fn set_push(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<PushRequest>, JsonRejection>,
) -> Result<Json<PhoneView>, ApiError> {
    user.in_person()?;
    let id = own(&state, &user, &id).await?;
    let this = this_phone(&state, &user).await?;
    if this != Some(id) {
        return Err(ApiError::Invalid(
            "Turn notifications on from the phone itself.".into(),
        ));
    }
    let request = body(request)?;
    let subscription =
        Subscription::parse(&request.endpoint, &request.keys.p256dh, &request.keys.auth)
            .map_err(ApiError::Invalid)?;
    let mut tx = state.pool.begin().await?;
    // A browser that was another phone of anyone before gives up that
    // subscription: it reaches this phone now.
    sqlx::query(
        "UPDATE phones SET push_endpoint = NULL, push_p256dh = NULL, push_auth = NULL
         WHERE push_endpoint = $1 AND id <> $2",
    )
    .bind(&subscription.endpoint)
    .bind(id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE phones SET push_endpoint = $2, push_p256dh = $3, push_auth = $4,
                push_error = NULL
         WHERE id = $1",
    )
    .bind(id)
    .bind(&subscription.endpoint)
    .bind(&subscription.p256dh)
    .bind(&subscription.auth)
    .execute(&mut *tx)
    .await?;
    record(
        &mut *tx,
        &user,
        "user.phone_push_on",
        id,
        json!({ "host": subscription.host() }),
    )
    .await?;
    let view = one(&mut *tx, user.id, id, this).await?;
    tx.commit().await?;
    Ok(Json(view))
}

/// `DELETE /api/me/phones/{id}/push`: turns notifications off.
pub async fn delete_push(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<PhoneView>, ApiError> {
    user.in_person()?;
    let id = own(&state, &user, &id).await?;
    let this = this_phone(&state, &user).await?;
    let mut tx = state.pool.begin().await?;
    sqlx::query(
        "UPDATE phones SET push_endpoint = NULL, push_p256dh = NULL, push_auth = NULL,
                push_error = NULL
         WHERE id = $1",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    record(&mut *tx, &user, "user.phone_push_off", id, json!({})).await?;
    let view = one(&mut *tx, user.id, id, this).await?;
    tx.commit().await?;
    Ok(Json(view))
}

/// `DELETE /api/me/phones/{id}`: removes a phone, which signs it out and
/// stops its notifications. Removing the phone of this session signs this
/// session out too.
pub async fn remove(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    user.in_person()?;
    let id = own(&state, &user, &id).await?;
    let this = this_phone(&state, &user).await?;
    let mut tx = state.pool.begin().await?;
    let name: String = sqlx::query_scalar("DELETE FROM phones WHERE id = $1 RETURNING name")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    record(
        &mut *tx,
        &user,
        "user.phone_remove",
        id,
        json!({ "name": name }),
    )
    .await?;
    tx.commit().await?;
    let mut headers = HeaderMap::new();
    if this == Some(id) {
        headers.insert(SET_COOKIE, session::clear_cookie(&state.settings));
    }
    Ok((StatusCode::NO_CONTENT, headers).into_response())
}

/// `POST /api/me/phones/{id}/test`: sends a test notification to a phone.
/// Counted with the tests of the person's contact.
pub async fn test(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<TestResult>, ApiError> {
    user.in_person()?;
    let id = own(&state, &user, &id).await?;
    if !state.contact_tests.attempt(&user.id) {
        return Err(ApiError::TooManyRequests(
            "Too many tests. Wait a minute and try again.".into(),
        ));
    }
    let phone = push::phones(&state.pool, user.id)
        .await?
        .into_iter()
        .find(|p| p.id == id)
        .ok_or_else(|| ApiError::Invalid("Turn notifications on for this phone first.".into()))?;
    let vapid = push::vapid(&state.pool)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let (client, subject) =
        Dispatcher::new(state.pool.clone(), state.settings.web_url.clone()).push_sender();
    let outcome = push::send(
        &client,
        &vapid,
        &subject,
        &phone.subscription,
        &Notice::test(),
    )
    .await;
    push::record(&state.pool, id, &outcome).await?;
    tracing::info!(user = %user.id, phone = %id, "tested a phone");
    Ok(Json(match outcome {
        Outcome::Sent => TestResult {
            ok: true,
            error: None,
        },
        Outcome::Gone => TestResult {
            ok: false,
            error: Some(push::GONE.into()),
        },
        Outcome::Failed(error) => TestResult {
            ok: false,
            error: Some(error.message),
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_one_short_line() {
        assert_eq!(name("  Alice's phone ").unwrap(), "Alice's phone");
        assert!(name("   ").is_err());
        assert!(name("a\nb").is_err());
        assert!(name(&"x".repeat(MAX_NAME + 1)).is_err());
        assert_eq!(
            name(&"é".repeat(MAX_NAME)).unwrap().chars().count(),
            MAX_NAME
        );
    }
}
