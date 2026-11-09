//! What an unauthenticated caller can make the server do (ADR-0106): the
//! sign-in limits, and that the setup and password links are checked
//! before a password is hashed. Ignored without `DATABASE_URL`.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::login_limit::LoginLimits;
use monitorflow_server::api::{self, password, setup, users, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
use serde_json::{json, Value};
use sqlx::PgPool;
use tokio::sync::Mutex;
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";
const WINDOW: Duration = Duration::from_secs(600);

/// [`password::runs`] counts for the whole process: the tests of this
/// file take turns.
static ONE_AT_A_TIME: Mutex<()> = Mutex::const_new(());

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

/// One server, so its limits last across requests: 3 wrong attempts per
/// source and 5 per account.
fn app(pool: &PgPool) -> Router {
    let mut state = ApiState::new(pool.clone(), Registry::default(), ApiSettings::default());
    state.login_limits = Arc::new(LoginLimits::new(3, 5, WINDOW));
    api::router(state)
}

async fn send(
    app: &Router,
    uri: &str,
    source: &str,
    cookie: Option<&str>,
    body: Value,
) -> (StatusCode, Option<String>, Value) {
    let mut request = Request::post(uri)
        .header("content-type", "application/json")
        .header(CSRF_HEADER, "1")
        // As the reverse proxy appends it; what the client sent comes first.
        .header("x-forwarded-for", format!("203.0.113.200, {source}"));
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    let request = request.body(Body::from(body.to_string())).unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let cookie = response
        .headers()
        .get("set-cookie")
        .map(|v| v.to_str().unwrap().to_owned());
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, cookie, body)
}

async fn post(app: &Router, uri: &str, source: &str, body: Value) -> (StatusCode, Value) {
    let (status, _, body) = send(app, uri, source, None, body).await;
    (status, body)
}

async fn login(app: &Router, source: &str, username: &str, password: &str) -> (StatusCode, Value) {
    let body = json!({ "username": username, "password": password });
    post(app, "/api/auth/login", source, body).await
}

async fn failed_logins(pool: &PgPool) -> Vec<Value> {
    sqlx::query_scalar(
        "SELECT details FROM audit_log WHERE action = 'auth.login_failed' ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn root(pool: &PgPool) {
    users::create_user(
        pool,
        "root",
        &Secret::new(PASSWORD),
        Role::Administrator,
        "test",
    )
    .await
    .unwrap();
}

db_tests! {
    #[sqlx::test]
    async fn a_source_is_refused_after_its_wrong_attempts_at_no_cost(pool: PgPool) {
        let _turn = ONE_AT_A_TIME.lock().await;
        root(&pool).await;
        let app = app(&pool);
        // Unknown and wrong-password accounts answer alike, then the
        // source is refused, the right password included.
        for username in ["ghost", "root", "ghost"] {
            let (status, body) = login(&app, "192.0.2.1", username, "not the password").await;
            assert_eq!((status, body["code"].as_str()), (StatusCode::UNAUTHORIZED, Some("invalid_credentials")), "{body}");
        }
        let (runs, rows) = (password::runs(), failed_logins(&pool).await);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1], json!({ "reason": "bad_password" }));
        assert_eq!(
            rows[2],
            json!({ "reason": "unknown_user", "limit_reached": "source", "source": "192.0.2.1" })
        );
        for (username, password) in [("root", PASSWORD), ("ghost", "x"), ("root", "x")] {
            let (status, body) = login(&app, "192.0.2.1", username, password).await;
            assert_eq!((status, body["code"].as_str()), (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited")), "{body}");
        }
        // Refused attempts verified nothing and wrote nothing.
        assert_eq!(password::runs(), runs);
        assert_eq!(failed_logins(&pool).await.len(), 3);

        // Someone else signs in meanwhile, and a right password is not an
        // attempt that counts.
        for _ in 0..5 {
            let (status, body) = login(&app, "192.0.2.2", "ROOT", PASSWORD).await;
            assert_eq!((status, body["username"].as_str()), (StatusCode::OK, Some("root")), "{body}");
        }
    }

    #[sqlx::test]
    async fn an_account_is_refused_across_sources_whether_it_exists_or_not(pool: PgPool) {
        let _turn = ONE_AT_A_TIME.lock().await;
        root(&pool).await;
        let app = app(&pool);
        // The administrator's usual place.
        let (status, _) = login(&app, "192.0.2.99", "root", PASSWORD).await;
        assert_eq!(status, StatusCode::OK);

        // Five wrong attempts on each name, from sources that stay under
        // their own limit: the same answers for the real and the made-up one.
        let mut answers = Vec::new();
        for (username, net) in [("root", 10), ("ghost", 20)] {
            let mut seen = Vec::new();
            for n in 0..7 {
                let source = format!("198.51.{net}.{}", n / 2);
                seen.push(login(&app, &source, username, "not the password").await.0.as_u16());
            }
            answers.push(seen);
        }
        assert_eq!(answers[0], [401, 401, 401, 401, 401, 429, 429]);
        assert_eq!(answers[0], answers[1]);
        let rows = failed_logins(&pool).await;
        assert_eq!(rows.len(), 10);
        assert_eq!(rows[4]["limit_reached"], "account");
        assert_eq!(rows[3].get("limit_reached"), None);

        // The right password from a new place waits for the window; from
        // the usual place it signs in, so the attack locks nobody out there.
        let runs = password::runs();
        let (status, _) = login(&app, "198.51.100.77", "root", PASSWORD).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(password::runs(), runs);
        let (status, body) = login(&app, "192.0.2.99", "root", PASSWORD).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    #[sqlx::test]
    async fn setup_hashes_only_for_the_live_link(pool: PgPool) {
        let _turn = ONE_AT_A_TIME.lock().await;
        let app = app(&pool);
        let token = setup::issue(&pool).await.unwrap().expect("no user yet");
        let complete = |token: String, password: &'static str| {
            let body = json!({ "token": token, "username": "admin", "password": password });
            let app = app.clone();
            async move { post(&app, "/api/setup", "192.0.2.1", body).await.0 }
        };
        let runs = password::runs();
        // No token, a malformed one, a well-formed wrong one: 410 each
        // without hashing; a short password is refused before either.
        for wrong in [String::new(), "zz".repeat(32), "0".repeat(64)] {
            assert_eq!(complete(wrong, PASSWORD).await, StatusCode::GONE);
        }
        assert_eq!(complete("0".repeat(64), "short").await, StatusCode::BAD_REQUEST);
        assert_eq!(complete(token.expose().to_owned(), "short").await, StatusCode::BAD_REQUEST);
        assert_eq!(password::runs(), runs);

        // An expired link is refused unhashed and deleted.
        sqlx::query("UPDATE setup_tokens SET created_at = now() - interval '2 days', expires_at = now() - interval '1 day'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(complete(token.expose().to_owned(), PASSWORD).await, StatusCode::GONE);
        assert_eq!(password::runs(), runs);
        let left: i64 = sqlx::query_scalar("SELECT count(*) FROM setup_tokens")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(left, 0);

        // The live link still works after all that, once: two requests
        // at the same time both hash, and one creates the administrator.
        let token = setup::issue(&pool).await.unwrap().unwrap().expose().to_owned();
        let (first, second) = tokio::join!(
            complete(token.clone(), PASSWORD),
            complete(token.clone(), PASSWORD)
        );
        let mut both = [first.as_u16(), second.as_u16()];
        both.sort_unstable();
        assert!(both == [200, 409] || both == [200, 410], "{both:?}");
        let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(users, 1);
        // Once done, a link answers 409 and hashes nothing.
        let runs = password::runs();
        assert_eq!(complete(token, PASSWORD).await, StatusCode::CONFLICT);
        assert_eq!(password::runs(), runs);
    }

    #[sqlx::test]
    async fn a_password_link_hashes_only_while_it_is_live(pool: PgPool) {
        let _turn = ONE_AT_A_TIME.lock().await;
        root(&pool).await;
        let app = app(&pool);
        let credentials = json!({ "username": "root", "password": PASSWORD });
        let (_, cookie, me) = send(&app, "/api/auth/login", "192.0.2.1", None, credentials).await;
        let uri = format!("/api/users/{}/link", me["id"].as_str().unwrap());
        let (status, _, link) = send(&app, &uri, "192.0.2.1", cookie.as_deref(), json!({})).await;
        assert_eq!(status, StatusCode::CREATED, "{link}");
        let link = link["token"].as_str().unwrap().to_owned();
        let set = |token: String, password: &'static str| {
            let body = json!({ "token": token, "password": password });
            let app = app.clone();
            async move { post(&app, "/api/password-link/use", "192.0.2.1", body).await.0 }
        };
        let runs = password::runs();
        for wrong in [String::new(), "zz".repeat(32), "0".repeat(64)] {
            assert_eq!(set(wrong, "a new long password").await, StatusCode::GONE);
        }
        assert_eq!(set(link.clone(), "short").await, StatusCode::BAD_REQUEST);
        assert_eq!(password::runs(), runs);

        // Two requests with the live link: one sets the password.
        let (first, second) = tokio::join!(
            set(link.clone(), "a new long password"),
            set(link.clone(), "another long password")
        );
        let mut both = [first.as_u16(), second.as_u16()];
        both.sort_unstable();
        assert_eq!(both, [200, 410]);
        // Used: refused without hashing.
        let runs = password::runs();
        assert_eq!(set(link, "a third long password").await, StatusCode::GONE);
        assert_eq!(password::runs(), runs);
    }
}
