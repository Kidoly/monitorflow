//! A signed-in person changes their own password (ADR-0138): current
//! password required, same length rule as elsewhere, sign-in limits,
//! audited, other sessions ended and this one kept, never through an API
//! key or without the CSRF header. Ignored without `DATABASE_URL`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::own_password::{UNCHANGED, WRONG_CURRENT};
use monitorflow_server::api::{users, v1, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery";
const NEW: &str = "a brand new long password";
const URI: &str = "/api/me/password";

macro_rules! db_tests {
    ($($item:item)*) => {
        $(
            #[cfg_attr(
                not(db_tests),
                ignore = "DATABASE_URL is not set; run `docker compose -f deploy/docker-compose.dev.yml up -d postgres` and export DATABASE_URL from .env.example"
            )]
            $item
        )*
    };
}

/// One app per test: the sign-in limits live in its state.
fn app(pool: &PgPool) -> Router {
    monitorflow_server::secrets::install_for_tests();
    v1::app(ApiState::new(
        pool.clone(),
        Registry::default(),
        ApiSettings::default(),
    ))
}

#[derive(Default)]
struct Call<'a> {
    cookie: Option<&'a str>,
    key: Option<&'a str>,
    no_csrf: bool,
}

/// `(status, set-cookie, body)` of `method uri` with `body`.
async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    call: Call<'_>,
) -> (StatusCode, Option<String>, Value) {
    let mut request = Request::builder().method(method).uri(uri);
    if !call.no_csrf {
        request = request.header(CSRF_HEADER, "1");
    }
    if let Some(cookie) = call.cookie {
        request = request.header("cookie", cookie);
    }
    if let Some(key) = call.key {
        request = request.header("authorization", format!("Bearer {key}"));
    }
    let request = match body {
        Some(body) => request
            .header("content-type", "application/json")
            .body(Body::from(body.to_string())),
        None => request.body(Body::empty()),
    }
    .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let cookie = response
        .headers()
        .get("set-cookie")
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned());
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        cookie,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn login(app: &Router, name: &str, password: &str) -> (StatusCode, Option<String>) {
    let body = json!({ "username": name, "password": password });
    let (status, cookie, _) =
        send(app, "POST", "/api/auth/login", Some(body), Call::default()).await;
    (status, cookie)
}

/// A user `name` with `role`, signed in: `(id, cookie)`.
async fn person(app: &Router, pool: &PgPool, name: &str, role: Role) -> (Uuid, String) {
    let id = users::create_user(pool, name, &Secret::new(PASSWORD), role, "test")
        .await
        .unwrap();
    let (status, cookie) = login(app, name, PASSWORD).await;
    assert_eq!(status, StatusCode::OK);
    (id, cookie.unwrap())
}

async fn change(app: &Router, cookie: &str, current: &str, new: &str) -> (StatusCode, Value) {
    let body = json!({ "current_password": current, "new_password": new });
    let call = Call {
        cookie: Some(cookie),
        ..Call::default()
    };
    let (status, _, body) = send(app, "POST", URI, Some(body), call).await;
    (status, body)
}

async fn signed_in(app: &Router, cookie: &str) -> bool {
    let call = Call {
        cookie: Some(cookie),
        ..Call::default()
    };
    send(app, "GET", "/api/auth/me", None, call).await.0 == StatusCode::OK
}

async fn audited(pool: &PgPool, user: Uuid, action: &str) -> Vec<Value> {
    sqlx::query_scalar(
        "SELECT details FROM audit_log WHERE actor_id = $1 AND action = $2 ORDER BY id",
    )
    .bind(user.to_string())
    .bind(action)
    .fetch_all(pool)
    .await
    .unwrap()
}

db_tests! {
    #[sqlx::test]
    async fn changing_ends_the_other_sessions_and_keeps_this_one(pool: PgPool) {
        let app = app(&pool);
        let (id, here) = person(&app, &pool, "ana", Role::Viewer).await;
        let (_, elsewhere) = login(&app, "ana", PASSWORD).await;
        let elsewhere = elsewhere.unwrap();
        let (_, bob) = person(&app, &pool, "bob", Role::Viewer).await;

        let (status, body) = change(&app, &here, PASSWORD, NEW).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        assert!(signed_in(&app, &here).await);
        assert!(!signed_in(&app, &elsewhere).await);
        // Someone else's session is not touched.
        assert!(signed_in(&app, &bob).await);

        assert_eq!(login(&app, "ana", PASSWORD).await.0, StatusCode::UNAUTHORIZED);
        assert_eq!(login(&app, "ana", NEW).await.0, StatusCode::OK);
        assert_eq!(
            audited(&pool, id, "user.password_set").await,
            vec![json!({ "via": "self", "sessions_ended": 1 })]
        );
    }

    #[sqlx::test]
    async fn a_wrong_current_password_changes_nothing_and_is_audited(pool: PgPool) {
        let app = app(&pool);
        let (id, here) = person(&app, &pool, "ana", Role::Administrator).await;
        let (_, elsewhere) = login(&app, "ana", PASSWORD).await;

        let (status, body) = change(&app, &here, "not the password", NEW).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], WRONG_CURRENT);
        assert!(signed_in(&app, &elsewhere.unwrap()).await);
        assert_eq!(login(&app, "ana", PASSWORD).await.0, StatusCode::OK);
        assert_eq!(
            audited(&pool, id, "user.password_change_failed").await,
            vec![json!({ "reason": "bad_password" })]
        );
        assert!(audited(&pool, id, "user.password_set").await.is_empty());
    }

    #[sqlx::test]
    async fn a_weak_or_unchanged_password_is_refused_before_any_check(pool: PgPool) {
        let app = app(&pool);
        let (id, here) = person(&app, &pool, "ana", Role::Viewer).await;
        let (status, body) = change(&app, &here, PASSWORD, "short").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"].as_str().unwrap().contains("at least 12"), "{body}");
        let (status, body) = change(&app, &here, PASSWORD, PASSWORD).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], UNCHANGED);
        // Neither counted as a wrong password.
        assert!(audited(&pool, id, "user.password_change_failed").await.is_empty());
        let (status, body) = change(&app, &here, "", NEW).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], WRONG_CURRENT);
    }

    #[sqlx::test]
    async fn wrong_current_passwords_share_the_sign_in_limits(pool: PgPool) {
        let app = app(&pool);
        let (id, here) = person(&app, &pool, "ana", Role::Viewer).await;
        // Ten wrong attempts from one source, as for the login page (ADR-0106).
        for _ in 0..10 {
            let (status, _) = change(&app, &here, "not the password", NEW).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
        }
        let (status, body) = change(&app, &here, PASSWORD, NEW).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
        assert_eq!(body["code"], "rate_limited");
        // The login page is held to the same limit.
        assert_eq!(login(&app, "ana", PASSWORD).await.0, StatusCode::TOO_MANY_REQUESTS);
        let failed = audited(&pool, id, "user.password_change_failed").await;
        assert_eq!(failed.len(), 10);
        assert_eq!(failed[9]["limit_reached"], "source");
    }

    #[sqlx::test]
    async fn the_session_csrf_and_api_key_rules_apply(pool: PgPool) {
        let app = app(&pool);
        let (_, cookie) = person(&app, &pool, "ana", Role::Administrator).await;
        let body = json!({ "current_password": PASSWORD, "new_password": NEW });

        let (status, _, _) = send(&app, "POST", URI, Some(body.clone()), Call::default()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let call = Call { cookie: Some(&cookie), no_csrf: true, ..Call::default() };
        let (status, _, _) = send(&app, "POST", URI, Some(body.clone()), call).await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        let call = Call { cookie: Some(&cookie), ..Call::default() };
        let (status, _, created) =
            send(&app, "POST", "/api/api-keys", Some(json!({ "name": "Script" })), call).await;
        assert_eq!(status, StatusCode::CREATED, "{}", created["error"]);
        let secret = created["secret"].as_str().unwrap();
        // The public API leaves it out, and a key elsewhere is no session.
        let call = Call { key: Some(secret), ..Call::default() };
        let (status, _, _) = send(&app, "POST", "/api/v1/me/password", Some(body.clone()), call).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let call = Call { key: Some(secret), ..Call::default() };
        let (status, _, _) = send(&app, "POST", URI, Some(body), call).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(login(&app, "ana", PASSWORD).await.0, StatusCode::OK);
    }
}
