//! The per-site agent auto-update policy (ADR-0053) against PostgreSQL:
//! the API turns it on, the tick queues one self_update per older Linux
//! agent of the site and version, audited, and never twice. Ignored
//! without `DATABASE_URL`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use monitorflow_common::config::ServerConfig;
use monitorflow_common::proto::v1::SelfUpdate;
use monitorflow_common::redact::Secret;
use monitorflow_server::agent_auto_update;
use monitorflow_server::agent_releases::{ReleaseCache, Sources};
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
use prost::Message;
use serde_json::{json, Value};
use sqlx::PgPool;
use tempfile::TempDir;
use tower::ServiceExt;
use uuid::Uuid;

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

const PASSWORD: &str = "correct horse battery staple";

async fn insert(pool: &PgPool, sql: &str, binds: &[&str]) -> Uuid {
    let mut query = sqlx::query_scalar::<_, Uuid>(sql);
    for bind in binds {
        query = query.bind(*bind);
    }
    query.fetch_one(pool).await.unwrap()
}

/// The API's answer to `method uri` with `body`, as `username`.
async fn call(
    pool: &PgPool,
    cookie: &str,
    method: &str,
    uri: &str,
    body: Value,
) -> (StatusCode, Value) {
    let state = ApiState::new(pool.clone(), Registry::default(), ApiSettings::default());
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header(CSRF_HEADER, "1")
        .header("content-type", "application/json")
        .header("cookie", cookie)
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = api::router(state).oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn login(pool: &PgPool, name: &str, role: Role, scope: GrantScope) -> String {
    users::create_user_with(pool, name, &Secret::new(PASSWORD), role, scope, "test")
        .await
        .unwrap();
    let state = ApiState::new(pool.clone(), Registry::default(), ApiSettings::default());
    let request = Request::post("/api/auth/login")
        .header(CSRF_HEADER, "1")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({ "username": name, "password": PASSWORD }).to_string(),
        ))
        .unwrap();
    let response = api::router(state).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response.headers()["set-cookie"].to_str().unwrap();
    cookie.split(';').next().unwrap().to_owned()
}

db_tests! {
    #[sqlx::test]
    async fn sites_that_opt_in_get_one_update_per_server_and_version(pool: PgPool) {
        let customer = insert(&pool, "INSERT INTO customers (name) VALUES ('acme') RETURNING id", &[]).await;
        let customer_s = customer.to_string();
        let site = insert(&pool, "INSERT INTO sites (name, customer_id) VALUES ('paris', $1::uuid) RETURNING id", &[&customer_s]).await;
        let other = insert(&pool, "INSERT INTO sites (name, customer_id) VALUES ('lyon', $1::uuid) RETURNING id", &[&customer_s]).await;
        let server = |peer: &'static str, site: Uuid, family: &'static str, version: &'static str| {
            let pool = pool.clone();
            async move {
                insert(
                    &pool,
                    "INSERT INTO servers (peer_id, site_id, hostname, os_family, agent_version)
                     VALUES ($1, $2::uuid, $1, $3, $4) RETURNING id",
                    &[peer, &site.to_string(), family, version],
                )
                .await
            }
        };
        let old = server("old", site, "debian", "0.2.0").await;
        server("current", site, "debian", "0.3.0").await;
        server("dev", site, "rhel", "dev").await;
        server("windows", site, "windows", "0.2.0").await;
        server("elsewhere", other, "debian", "0.2.0").await;

        let mirrors = TempDir::new().unwrap();
        let mut sources = Sources::from_config(&ServerConfig::default());
        sources.cache = ReleaseCache::in_mirror_dir(mirrors.path());
        // Off by default.
        std::fs::create_dir_all(mirrors.path().join("agent/0.3.0")).unwrap();
        std::fs::write(mirrors.path().join("agent/0.3.0/VERSION"), "0.3.0\n").unwrap();
        assert!(agent_auto_update::tick(&pool, &sources).await.unwrap().is_empty());

        let viewer = login(&pool, "viewer", Role::Viewer, GrantScope::Customer(customer)).await;
        let admin = login(&pool, "admin", Role::Administrator, GrantScope::Customer(customer)).await;
        let uri = format!("/api/sites/{site}/agent-auto-update");
        let on = json!({ "enabled": true });
        assert_eq!(call(&pool, &viewer, "PUT", &uri, on.clone()).await.0, StatusCode::FORBIDDEN);
        let (status, body) = call(&pool, &admin, "PUT", &uri, on).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["agent_auto_update"], true);

        let mut first = agent_auto_update::tick(&pool, &sources).await.unwrap();
        first.sort();
        assert_eq!(first, ["old", "windows"]);
        let (kind, payload): (String, Vec<u8>) =
            sqlx::query_as("SELECT kind, payload FROM commands WHERE server_id = $1")
                .bind(old)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(kind, "self_update");
        assert_eq!(SelfUpdate::decode(payload.as_slice()).unwrap().version, "0.3.0");
        // In flight, then failed: never queued twice for that version.
        assert!(agent_auto_update::tick(&pool, &sources).await.unwrap().is_empty());
        sqlx::query("UPDATE commands SET state = 'failed', finished_at = now()")
            .execute(&pool)
            .await
            .unwrap();
        assert!(agent_auto_update::tick(&pool, &sources).await.unwrap().is_empty());
        // A newer release is tried.
        std::fs::create_dir_all(mirrors.path().join("agent/0.4.0")).unwrap();
        std::fs::write(mirrors.path().join("agent/0.4.0/VERSION"), "0.4.0\n").unwrap();
        let mut peers = agent_auto_update::tick(&pool, &sources).await.unwrap();
        peers.sort();
        assert_eq!(peers, ["current", "old", "windows"]);

        let audited: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = $1")
            .bind("agent.auto_update")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(audited, 5);
        let toggled: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = $1")
            .bind("site.agent_auto_update")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(toggled, 1);
    }
}
