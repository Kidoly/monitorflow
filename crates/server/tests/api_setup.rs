//! First-run setup (ADR-0057): the setup link creates the first
//! administrator and the default site once, and dies with the first user.
//! Ignored without `DATABASE_URL`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::{self, setup, users, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";

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

fn app(pool: &PgPool) -> Router {
    api::router(ApiState::new(
        pool.clone(),
        Registry::default(),
        ApiSettings::default(),
    ))
}

async fn call(pool: &PgPool, request: Request<Body>) -> (StatusCode, Option<String>, Value) {
    let response = app(pool).oneshot(request).await.unwrap();
    let status = response.status();
    let cookie = response
        .headers()
        .get("set-cookie")
        .map(|v| v.to_str().unwrap().to_owned());
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, cookie, body)
}

async fn pending(pool: &PgPool) -> bool {
    let request = Request::get("/api/setup").body(Body::empty()).unwrap();
    let (status, _, body) = call(pool, request).await;
    assert_eq!(status, StatusCode::OK);
    body["pending"].as_bool().unwrap()
}

/// A new setup link's secret, from its fragment.
async fn issue(pool: &PgPool) -> String {
    let token = setup::issue(pool).await.unwrap().expect("no user yet");
    let url = token.url("https://mon.example.net");
    url.split_once('#').unwrap().1.to_owned()
}

async fn complete(
    pool: &PgPool,
    token: &str,
    username: &str,
    password: &str,
) -> (StatusCode, Option<String>, Value) {
    let body = json!({ "token": token, "username": username, "password": password });
    let request = Request::post("/api/setup")
        .header("content-type", "application/json")
        .header(CSRF_HEADER, "1")
        .body(Body::from(body.to_string()))
        .unwrap();
    call(pool, request).await
}

async fn audit_actions(pool: &PgPool) -> Vec<String> {
    sqlx::query_scalar("SELECT action FROM audit_log ORDER BY id")
        .fetch_all(pool)
        .await
        .unwrap()
}

db_tests! {
    #[sqlx::test]
    async fn the_link_creates_the_administrator_and_the_default_site_once(pool: PgPool) {
        assert!(pending(&pool).await);
        let token = issue(&pool).await;

        let (status, cookie, body) = complete(&pool, &token, "admin", PASSWORD).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["username"], "admin");
        assert_eq!(
            body["roles"],
            json!([{ "role": "administrator", "customer_id": null, "site_id": null }])
        );
        // Signed in at once: the cookie opens a session.
        let cookie = cookie.unwrap();
        let cookie = cookie.split(';').next().unwrap();
        let me = Request::get("/api/auth/me")
            .header("cookie", cookie)
            .body(Body::empty())
            .unwrap();
        let (status, _, me) = call(&pool, me).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(me["username"], "admin");

        let sites: Vec<(String, String)> = sqlx::query_as(
            "SELECT s.name, c.name FROM sites s JOIN customers c ON c.id = s.customer_id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(sites, [("main".to_owned(), "default".to_owned())]);
        assert!(!pending(&pool).await);
        assert_eq!(
            audit_actions(&pool).await,
            ["setup.link_issued", "user.create", "site.create", "auth.login"]
        );

        // Single use: the same link now says setup is done.
        let (status, _, _) = complete(&pool, &token, "eve", PASSWORD).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(setup::issue(&pool).await.unwrap().is_none());
    }

    #[sqlx::test]
    async fn a_wrong_replaced_or_expired_link_is_refused(pool: PgPool) {
        let old = issue(&pool).await;
        let token = issue(&pool).await;
        for bad in [old.as_str(), "not a token", &"0".repeat(64)] {
            let (status, _, body) = complete(&pool, bad, "admin", PASSWORD).await;
            assert_eq!(status, StatusCode::GONE, "{bad}");
            assert!(body["error"].as_str().unwrap().contains("setup-link"), "{body}");
        }
        // A bad form leaves the link alive.
        let (status, _, body) = complete(&pool, &token, "admin", "short").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"].as_str().unwrap().contains("12"), "{body}");
        let (status, _, _) = complete(&pool, &token, "a b", PASSWORD).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        sqlx::query("UPDATE setup_tokens SET created_at = now() - interval '2 days', expires_at = now() - interval '1 day'")
            .execute(&pool)
            .await
            .unwrap();
        let (status, _, _) = complete(&pool, &token, "admin", PASSWORD).await;
        assert_eq!(status, StatusCode::GONE);
        assert!(pending(&pool).await);
        let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(users, 0);
    }

    #[sqlx::test]
    async fn a_user_created_on_the_command_line_kills_the_link(pool: PgPool) {
        let token = issue(&pool).await;
        users::create_user(&pool, "root", &Secret::new(PASSWORD), Role::Administrator, "cli")
            .await
            .unwrap();
        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM setup_tokens")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 0);
        let (status, _, _) = complete(&pool, &token, "admin", PASSWORD).await;
        assert_eq!(status, StatusCode::CONFLICT);
    }

    #[sqlx::test]
    async fn an_existing_site_is_kept_instead_of_main(pool: PgPool) {
        sqlx::query("INSERT INTO sites (name) VALUES ('paris')")
            .execute(&pool)
            .await
            .unwrap();
        let token = issue(&pool).await;
        let (status, _, _) = complete(&pool, &token, "admin", PASSWORD).await;
        assert_eq!(status, StatusCode::OK);
        let sites: Vec<String> = sqlx::query_scalar("SELECT name FROM sites")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(sites, ["paris"]);
    }

    #[sqlx::test]
    async fn setup_needs_the_csrf_header(pool: PgPool) {
        let token = issue(&pool).await;
        let body = json!({ "token": token, "username": "admin", "password": PASSWORD });
        let request = Request::post("/api/setup")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let (status, _, _) = call(&pool, request).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(pending(&pool).await);
    }
}
