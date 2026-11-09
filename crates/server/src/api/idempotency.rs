//! `Idempotency-Key` on writes of the public API (ADR-0101): the first
//! request with a key's value runs and its answer is kept 24 hours; the
//! same request again gets that answer back instead of running twice.

use axum::body::{to_bytes, Body};
use axum::extract::Request;
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Router;
use ring::digest::{Context, SHA256};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use super::api_keys::{bearer, KeySecret};
use super::{ApiError, ApiState, BODY_LIMIT};

pub const HEADER: &str = "idempotency-key";
/// Set on an answer given again rather than made anew.
pub const REPLAYED: &str = "idempotent-replayed";
/// Answers larger than this are not kept: a retry runs again.
const MAX_ANSWER: usize = 1 << 20;
const MAX_VALUE: usize = 255;

/// Routes whose answer hands out a secret once (`/api/…` paths, POST):
/// their answers are never stored.
fn hands_out_a_secret(path: &str) -> bool {
    let segments: Vec<&str> = path.trim_start_matches("/api/").split('/').collect();
    matches!(
        segments.as_slice(),
        ["tokens"]
            | ["install-keys"]
            | ["display-links"]
            | ["users"]
            | ["users", _, "link"]
            | ["integrations"]
            | ["integrations", _, "secret"]
    )
}

/// A valid header value: 1 to 255 visible ASCII characters.
fn parse(value: &HeaderValue) -> Option<String> {
    let bytes = value.as_bytes();
    let valid =
        (1..=MAX_VALUE).contains(&bytes.len()) && bytes.iter().all(|b| b.is_ascii_graphic());
    valid.then(|| String::from_utf8_lossy(bytes).into_owned())
}

/// Hands `request` to `inner`, through the idempotency store when it is a
/// write with a key and an `Idempotency-Key`.
pub(super) async fn run(state: ApiState, inner: Router, request: Request) -> Response {
    let write = matches!(
        *request.method(),
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    );
    let value = request.headers().get(HEADER);
    let secret = bearer(request.headers());
    let multipart = request
        .headers()
        .get(CONTENT_TYPE)
        .is_some_and(|v| v.as_bytes().starts_with(b"multipart/"));
    let (Some(value), Some(secret), true, false) = (value, secret, write, multipart) else {
        return call(inner, request).await;
    };
    let Some(value) = parse(value) else {
        return ApiError::Invalid(format!(
            "Idempotency-Key is 1 to {MAX_VALUE} visible ASCII characters, a UUID for example."
        ))
        .into_response();
    };
    idempotent(&state.pool, inner, request, secret, value)
        .await
        .unwrap_or_else(IntoResponse::into_response)
}

async fn call(inner: Router, request: Request) -> Response {
    match inner.oneshot(request).await {
        Ok(response) => response,
        Err(never) => match never {},
    }
}

/// What the store says about a request.
enum Claim {
    /// First time, or the first one never finished: run it.
    Run,
    Running,
    Reused,
    Replay {
        status: i16,
        content_type: Option<String>,
        body: Option<Vec<u8>>,
    },
}

async fn idempotent(
    pool: &PgPool,
    inner: Router,
    request: Request,
    secret: KeySecret,
    value: String,
) -> Result<Response, ApiError> {
    let key: Option<Uuid> = sqlx::query_scalar("SELECT id FROM api_keys WHERE token_hash = $1")
        .bind(secret.hash())
        .fetch_optional(pool)
        .await?;
    // An unknown key is refused by the handler, without storing anything.
    let Some(key) = key else {
        return Ok(call(inner, request).await);
    };
    let (parts, body) = request.into_parts();
    let body = to_bytes(body, BODY_LIMIT)
        .await
        .map_err(|_| ApiError::BadRequest)?;
    let mut hash = Context::new(&SHA256);
    for part in [
        parts.method.as_str().as_bytes(),
        b"\n",
        parts.uri.to_string().as_bytes(),
        b"\n",
        &body,
    ] {
        hash.update(part);
    }
    let fingerprint = hash.finish().as_ref().to_vec();

    match claim(pool, key, &value, &fingerprint).await? {
        Claim::Run => {}
        Claim::Running => return Err(ApiError::IdempotencyInProgress),
        Claim::Reused => return Err(ApiError::IdempotencyKeyReused),
        Claim::Replay {
            status,
            content_type,
            body,
        } => return Ok(replay(status, content_type, body)),
    }

    let secret_answer = parts.method == Method::POST && hands_out_a_secret(parts.uri.path());
    let response = call(inner, Request::from_parts(parts, Body::from(body))).await;
    let status = response.status();
    let (parts, body) = response.into_parts();
    let body = match to_bytes(body, usize::MAX).await {
        Ok(body) => body,
        Err(error) => return Err(ApiError::Internal(format!("reading an answer: {error}"))),
    };
    let keep = !status.is_server_error() && body.len() <= MAX_ANSWER;
    let content_type = parts
        .headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let stored = if keep {
        let kept = (!(secret_answer && status.is_success())).then(|| body.to_vec());
        sqlx::query(
            "UPDATE api_idempotency SET status = $3, content_type = $4, body = $5
             WHERE key_id = $1 AND value = $2",
        )
        .bind(key)
        .bind(&value)
        .bind(status.as_u16() as i16)
        .bind(&content_type)
        .bind(kept)
        .execute(pool)
        .await
    } else {
        sqlx::query("DELETE FROM api_idempotency WHERE key_id = $1 AND value = $2")
            .bind(key)
            .bind(&value)
            .execute(pool)
            .await
    };
    if let Err(error) = stored {
        tracing::warn!(%error, key = %key, "could not keep an idempotent answer");
    }
    Ok(Response::from_parts(parts, Body::from(body)))
}

async fn claim(
    pool: &PgPool,
    key: Uuid,
    value: &str,
    fingerprint: &[u8],
) -> Result<Claim, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM api_idempotency WHERE created_at < now() - interval '24 hours'")
        .execute(&mut *tx)
        .await?;
    // A first request that has not finished after five minutes died with
    // its instance: the same request may run again.
    let claimed: Option<bool> = sqlx::query_scalar(
        "INSERT INTO api_idempotency (key_id, value, fingerprint) VALUES ($1, $2, $3)
         ON CONFLICT (key_id, value) DO UPDATE SET created_at = now()
         WHERE api_idempotency.status IS NULL
           AND api_idempotency.created_at < now() - interval '5 minutes'
           AND api_idempotency.fingerprint = EXCLUDED.fingerprint
         RETURNING true",
    )
    .bind(key)
    .bind(value)
    .bind(fingerprint)
    .fetch_optional(&mut *tx)
    .await?;
    let claim = if claimed.is_some() {
        Claim::Run
    } else {
        let (stored, status, content_type, body): (
            Vec<u8>,
            Option<i16>,
            Option<String>,
            Option<Vec<u8>>,
        ) = sqlx::query_as(
            "SELECT fingerprint, status, content_type, body FROM api_idempotency
                 WHERE key_id = $1 AND value = $2",
        )
        .bind(key)
        .bind(value)
        .fetch_one(&mut *tx)
        .await?;
        match status {
            _ if stored != fingerprint => Claim::Reused,
            None => Claim::Running,
            Some(status) => Claim::Replay {
                status,
                content_type,
                body,
            },
        }
    };
    tx.commit().await?;
    Ok(claim)
}

fn replay(status: i16, content_type: Option<String>, body: Option<Vec<u8>>) -> Response {
    let mut response = match body {
        Some(body) => {
            let status = u16::try_from(status)
                .ok()
                .and_then(|s| StatusCode::from_u16(s).ok())
                .unwrap_or(StatusCode::OK);
            let mut response = (status, body).into_response();
            if let Some(value) = content_type.and_then(|c| HeaderValue::from_str(&c).ok()) {
                response.headers_mut().insert(CONTENT_TYPE, value);
            }
            response
        }
        None => ApiError::Conflict(
            "This request was already done. Its answer held a secret, which is shown only once."
                .into(),
        )
        .into_response(),
    };
    response
        .headers_mut()
        .insert(REPLAYED, HeaderValue::from_static("true"));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_visible_ascii() {
        assert_eq!(
            parse(&HeaderValue::from_static("abc-1")).as_deref(),
            Some("abc-1")
        );
        let long = "x".repeat(MAX_VALUE);
        assert!(parse(&HeaderValue::from_str(&long).unwrap()).is_some());
        for bad in ["", "a b", &"x".repeat(MAX_VALUE + 1)] {
            assert_eq!(parse(&HeaderValue::from_str(bad).unwrap()), None, "{bad:?}");
        }
    }

    #[test]
    fn answers_with_secrets_are_known() {
        for path in [
            "/api/tokens",
            "/api/install-keys",
            "/api/display-links",
            "/api/users",
            "/api/users/x/link",
            "/api/integrations",
            "/api/integrations/x/secret",
        ] {
            assert!(hands_out_a_secret(path), "{path}");
        }
        for path in [
            "/api/silences",
            "/api/users/x/roles",
            "/api/tokens/x",
            "/api/integrations/x/test",
        ] {
            assert!(!hands_out_a_secret(path), "{path}");
        }
    }
}
