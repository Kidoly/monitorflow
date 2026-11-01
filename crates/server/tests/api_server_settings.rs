//! The Settings page's API (ADR-0069): who reads and changes it, the audit,
//! the configuration's locks, sessions following a change at once, and
//! retention requests to the updater. Ignored without `DATABASE_URL`.

use std::path::Path;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Utc;
use monitorflow_common::config::ServerConfig;
use monitorflow_common::redact::Secret;
use monitorflow_common::server_update::retention::{
    Kept, Retention, RetentionRequest, RETENTION_REQUEST_FILE,
};
use monitorflow_common::server_update::{Status, STATUS_FILE};
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::server_update::ServerUpdateSettings;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
use serde_json::{json, Value};
use sqlx::PgPool;
use tempfile::TempDir;
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";
const SERVER: &str = "/api/settings/server";
const RETENTION: &str = "/api/settings/retention";

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

async fn call(
    pool: &PgPool,
    settings: ApiSettings,
    method: &str,
    uri: &str,
    cookie: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value, Option<String>) {
    let state = ApiState::new(pool.clone(), Registry::default(), settings);
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header(CSRF_HEADER, "1")
        .header("content-type", "application/json");
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    let request = request
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let response = api::router(state).oneshot(request).await.unwrap();
    let status = response.status();
    let cookie = response
        .headers()
        .get("set-cookie")
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned());
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body, cookie)
}

async fn login(pool: &PgPool, name: &str, role: Role, scope: GrantScope) -> String {
    users::create_user_with(pool, name, &Secret::new(PASSWORD), role, scope, "test")
        .await
        .unwrap();
    let body = json!({ "username": name, "password": PASSWORD });
    let (status, _, cookie) = call(
        pool,
        ApiSettings::default(),
        "POST",
        "/api/auth/login",
        None,
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    cookie.unwrap()
}

async fn customer_admin(pool: &PgPool) -> String {
    let customer = sqlx::query_scalar("INSERT INTO customers (name) VALUES ('acme') RETURNING id")
        .fetch_one(pool)
        .await
        .unwrap();
    login(
        pool,
        "bob",
        Role::Administrator,
        GrantScope::Customer(customer),
    )
    .await
}

async fn audits(pool: &PgPool, action: &str) -> Vec<Value> {
    sqlx::query_scalar("SELECT details FROM audit_log WHERE action = $1 ORDER BY id")
        .bind(action)
        .fetch_all(pool)
        .await
        .unwrap()
}

/// A volume shared with an updater that reports `.env`'s retention.
fn updater(dir: &Path, age_s: i64, logs_locked: bool) -> ApiSettings {
    std::fs::create_dir_all(dir.join("inbox")).unwrap();
    let status = Status {
        updater_version: "0.2.0".into(),
        heartbeat_at: Utc::now() - chrono::Duration::seconds(age_s),
        running: None,
        update: None,
        backups: Default::default(),
        retention: Some(Retention {
            metrics: Kept {
                value: "90d".into(),
                locked: false,
            },
            logs: Kept {
                value: "1y".into(),
                locked: logs_locked,
            },
            applying: false,
            error: None,
        }),
    };
    std::fs::write(dir.join(STATUS_FILE), serde_json::to_vec(&status).unwrap()).unwrap();
    ApiSettings {
        server_update: ServerUpdateSettings {
            dir: Some(dir.to_owned()),
            ..ServerUpdateSettings::from_config(&ServerConfig::default())
        },
        ..ApiSettings::default()
    }
}

db_tests! {
    #[sqlx::test]
    async fn only_global_administrators_reach_the_settings(pool: PgPool) {
        let admin = customer_admin(&pool).await;
        let settings = ApiSettings::default;
        let cookie = Some(admin.as_str());
        let (status, ..) = call(&pool, settings(), "GET", SERVER, cookie, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, ..) = call(&pool, settings(), "PUT", SERVER, cookie, Some(json!({}))).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, ..) = call(&pool, settings(), "PUT", RETENTION, cookie, Some(json!({}))).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[sqlx::test]
    async fn changes_apply_at_once_and_are_audited(pool: PgPool) {
        let cookie = login(&pool, "alice", Role::Administrator, GrantScope::Global).await;
        let settings = ApiSettings::from_config(&ServerConfig {
            mirror_default_url: Some("http://mf.example:7480".into()),
            ..ServerConfig::default()
        });
        let live = settings.live.clone();
        let (status, view, _) = call(&pool, settings.clone(), "GET", SERVER, Some(&cookie), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(view["session_lifetime"], json!({ "value": 43200, "default": 43200, "locked": false }));
        assert_eq!(view["session_idle_timeout"]["value"], 1800);
        assert_eq!(view["mirror_public_url"]["value"], "http://mf.example:7480");
        assert_eq!(view["retention"], Value::Null);

        // An old session: a shorter lifetime ends it, the new one survives.
        sqlx::query("UPDATE sessions SET created_at = now() - interval '3 hours'")
            .execute(&pool)
            .await
            .unwrap();
        let fresh = login(&pool, "carol", Role::Administrator, GrantScope::Global).await;
        let body = json!({
            "session_lifetime": 7200,
            "session_idle_timeout": 600,
            "mirror_public_url": "http://mirror.example:7480/",
        });
        let (status, view, _) =
            call(&pool, settings.clone(), "PUT", SERVER, Some(&fresh), Some(body)).await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!(view["session_lifetime"]["value"], 7200);
        assert_eq!(view["mirror_public_url"]["value"], "http://mirror.example:7480");
        assert_eq!(live.session_idle_timeout(), Duration::from_secs(600));
        assert_eq!(live.mirror_public_url().as_deref(), Some("http://mirror.example:7480"));
        let (status, ..) = call(&pool, settings.clone(), "GET", SERVER, Some(&cookie), None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, ..) = call(&pool, settings.clone(), "GET", SERVER, Some(&fresh), None).await;
        assert_eq!(status, StatusCode::OK);

        let audit = audits(&pool, "settings.server").await;
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0]["old"]["session_lifetime"], Value::Null);
        assert_eq!(audit[0]["new"]["session_lifetime"], 7200);

        // A failed change leaves everything.
        let (status, ..) = call(
            &pool, settings.clone(), "PUT", SERVER, Some(&fresh),
            Some(json!({ "session_lifetime": 60, "session_idle_timeout": null, "mirror_public_url": null })),
        ).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(audits(&pool, "settings.server").await.len(), 1);

        // Back to the defaults: NULL again.
        let (status, view, _) = call(
            &pool, settings.clone(), "PUT", SERVER, Some(&fresh),
            Some(json!({ "session_lifetime": null, "session_idle_timeout": null, "mirror_public_url": null })),
        ).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(view["session_lifetime"]["value"], 43200);
        let stored: Option<i32> = sqlx::query_scalar("SELECT session_lifetime FROM server_settings")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(stored, None);
    }

    #[sqlx::test]
    async fn the_configuration_locks_values(pool: PgPool) {
        let cookie = login(&pool, "alice", Role::Administrator, GrantScope::Global).await;
        let settings = ApiSettings::from_config(&ServerConfig {
            api_session_lifetime: Some(3600),
            mirror_public_url: Some("http://config.example".into()),
            ..ServerConfig::default()
        });
        let (_, view, _) = call(&pool, settings.clone(), "GET", SERVER, Some(&cookie), None).await;
        assert_eq!(view["session_lifetime"], json!({ "value": 3600, "default": 43200, "locked": true }));
        assert_eq!(view["mirror_public_url"]["locked"], true);
        assert_eq!(view["session_idle_timeout"]["locked"], false);
        let (status, ..) = call(
            &pool, settings.clone(), "PUT", SERVER, Some(&cookie),
            Some(json!({ "session_lifetime": 7200, "session_idle_timeout": null, "mirror_public_url": null })),
        ).await;
        assert_eq!(status, StatusCode::CONFLICT);
        let (status, ..) = call(
            &pool, settings, "PUT", SERVER, Some(&cookie),
            Some(json!({ "session_lifetime": null, "session_idle_timeout": 900, "mirror_public_url": null })),
        ).await;
        assert_eq!(status, StatusCode::OK);
    }

    #[sqlx::test]
    async fn retention_goes_through_the_updater(pool: PgPool) {
        let cookie = login(&pool, "alice", Role::Administrator, GrantScope::Global).await;
        let body = json!({ "metrics_days": 60, "logs_days": null });
        let (status, ..) = call(&pool, ApiSettings::default(), "PUT", RETENTION, Some(&cookie), Some(body.clone())).await;
        assert_eq!(status, StatusCode::CONFLICT, "no updater");

        let dir = TempDir::new().unwrap();
        let gone = updater(dir.path(), 3600, true);
        let (status, ..) = call(&pool, gone, "PUT", RETENTION, Some(&cookie), Some(body.clone())).await;
        assert_eq!(status, StatusCode::CONFLICT, "not running");

        let settings = updater(dir.path(), 5, true);
        let (_, view, _) = call(&pool, settings.clone(), "GET", SERVER, Some(&cookie), None).await;
        assert_eq!(view["retention"]["metrics"], json!({ "value": "90d", "locked": false }));
        assert_eq!(view["retention"]["logs"]["locked"], true);
        assert_eq!(view["retention"]["default_metrics"], "90d");

        let locked = json!({ "metrics_days": null, "logs_days": 7 });
        let (status, ..) = call(&pool, settings.clone(), "PUT", RETENTION, Some(&cookie), Some(locked)).await;
        assert_eq!(status, StatusCode::CONFLICT, "locked");
        let (status, ..) = call(
            &pool, settings.clone(), "PUT", RETENTION, Some(&cookie),
            Some(json!({ "metrics_days": 0, "logs_days": null })),
        ).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (status, view, _) = call(&pool, settings.clone(), "PUT", RETENTION, Some(&cookie), Some(body.clone())).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(view["retention"]["applying"], true);
        let bytes = std::fs::read(dir.path().join("inbox").join(RETENTION_REQUEST_FILE)).unwrap();
        let request = RetentionRequest::parse(&bytes).unwrap();
        assert_eq!((request.metrics_days, request.logs_days), (Some(60), None));
        assert_eq!(request.requested_by, "alice");
        let audit = audits(&pool, "settings.retention").await;
        assert_eq!(audit[0]["old"]["metrics"], "90d");
        assert_eq!(audit[0]["new"]["metrics_days"], 60);

        let (status, ..) = call(&pool, settings, "PUT", RETENTION, Some(&cookie), Some(body)).await;
        assert_eq!(status, StatusCode::CONFLICT, "one at a time");
    }
}
