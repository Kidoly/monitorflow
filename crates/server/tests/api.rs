//! REST API against a real PostgreSQL. Each `#[sqlx::test]` gets a fresh
//! database with the migrations applied. Without `DATABASE_URL` at build
//! time the tests are reported as ignored (see `build.rs`).

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use monitorflow_common::redact::Secret;
use monitorflow_server::api::session;
use monitorflow_server::api::users::{self, CreateUserError};
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
use serde_json::json;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery";

/// Marks every test in the block as ignored when `DATABASE_URL` is unset.
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

fn app(pool: PgPool) -> Router {
    api::router(ApiState::new(
        pool,
        Registry::default(),
        ApiSettings::default(),
    ))
}

async fn call(app: Router, request: Request<Body>) -> (StatusCode, serde_json::Value) {
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    let body = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, body)
}

async fn add_user(pool: &PgPool, username: &str, role: Role) -> Uuid {
    users::create_user(pool, username, &Secret::new(PASSWORD), role, "test")
        .await
        .unwrap()
}

async fn audit_actions(pool: &PgPool) -> Vec<(String, String, serde_json::Value)> {
    sqlx::query_as("SELECT action, actor_id, details FROM audit_log ORDER BY id")
        .fetch_all(pool)
        .await
        .unwrap()
}

db_tests! {
    #[sqlx::test]
    async fn readiness_is_ok_with_database(pool: PgPool) {
        let request = Request::get("/api/health/ready").body(Body::empty()).unwrap();
        let (status, body) = call(app(pool), request).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, serde_json::json!({ "status": "ready" }));
    }

    #[sqlx::test]
    async fn sessions_resolve_until_revoked_idle_or_expired(pool: PgPool) {
        let settings = ApiSettings::default();
        let id = add_user(&pool, "alice", Role::Viewer).await;
        let token = session::create(&pool, id, &settings).await.unwrap();
        assert_eq!(session::resolve(&pool, &token, &settings).await.unwrap(), Some(id));
        assert!(session::revoke(&pool, &token).await.unwrap());
        assert_eq!(session::resolve(&pool, &token, &settings).await.unwrap(), None);
        assert!(!session::revoke(&pool, &token).await.unwrap());

        let idle = session::create(&pool, id, &settings).await.unwrap();
        sqlx::query("UPDATE sessions SET last_seen_at = now() - interval '31 minutes'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(session::resolve(&pool, &idle, &settings).await.unwrap(), None);
        assert_eq!(session::prune(&pool, &settings).await.unwrap(), 1);

        let short = ApiSettings {
            session_lifetime: std::time::Duration::from_millis(50),
            ..ApiSettings::default()
        };
        let expired = session::create(&pool, id, &short).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(session::resolve(&pool, &expired, &short).await.unwrap(), None);
    }

    #[sqlx::test]
    async fn create_user_validates_and_audits(pool: PgPool) {
        let id = add_user(&pool, "Alice", Role::Administrator).await;
        let hash: String = sqlx::query_scalar("SELECT password_hash FROM users WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(hash.starts_with("$argon2id$"), "{hash}");

        let err = users::create_user(&pool, "ALICE", &Secret::new(PASSWORD), Role::Viewer, "test")
            .await
            .unwrap_err();
        assert!(matches!(err, CreateUserError::AlreadyExists), "{err}");
        let err = users::create_user(&pool, "bob", &Secret::new("short"), Role::Viewer, "test")
            .await
            .unwrap_err();
        assert!(matches!(err, CreateUserError::PasswordTooShort), "{err}");

        let audit = audit_actions(&pool).await;
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].0, "user.create");
        assert_eq!(audit[0].2, json!({ "username": "Alice", "role": "administrator" }));
    }
}
