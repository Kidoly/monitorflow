//! Automatic agent updates (ADR-0053, ADR-0055) against PostgreSQL: on by
//! default, a canary per site then batches, site overrides, the pause, the
//! failure threshold that halts a rollout and its resume, one try per
//! server and version, and the API. Ignored without `DATABASE_URL`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use monitorflow_common::config::ServerConfig;
use monitorflow_common::proto::v1::SelfUpdate;
use monitorflow_common::redact::Secret;
use monitorflow_common::release::Version;
use monitorflow_server::agent_releases::{ReleaseCache, Sources};
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::auto_update::AutoUpdate;
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

/// The API's answer to `method uri` with `body`, as the session `cookie`.
async fn call(
    pool: &PgPool,
    cookie: &str,
    method: &str,
    uri: &str,
    body: Value,
) -> (StatusCode, Value) {
    call_with(pool, ApiSettings::default(), cookie, method, uri, body).await
}

async fn call_with(
    pool: &PgPool,
    settings: ApiSettings,
    cookie: &str,
    method: &str,
    uri: &str,
    body: Value,
) -> (StatusCode, Value) {
    let state = ApiState::new(pool.clone(), Registry::default(), settings);
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

/// A customer with sites `paris` and `lyon`.
async fn sites(pool: &PgPool) -> (Uuid, Uuid, Uuid) {
    let customer = insert(
        pool,
        "INSERT INTO customers (name) VALUES ('acme') RETURNING id",
        &[],
    )
    .await;
    let c = customer.to_string();
    let paris = insert(
        pool,
        "INSERT INTO sites (name, customer_id) VALUES ('paris', $1::uuid) RETURNING id",
        &[&c],
    )
    .await;
    let lyon = insert(
        pool,
        "INSERT INTO sites (name, customer_id) VALUES ('lyon', $1::uuid) RETURNING id",
        &[&c],
    )
    .await;
    (customer, paris, lyon)
}

async fn server(pool: &PgPool, peer: &str, site: Uuid, family: &str, version: &str) -> Uuid {
    insert(
        pool,
        "INSERT INTO servers (peer_id, site_id, hostname, os_family, agent_version)
         VALUES ($1, $2::uuid, $1, $3, $4) RETURNING id",
        &[peer, &site.to_string(), family, version],
    )
    .await
}

/// A release cache holding `versions`.
fn cache(dir: &TempDir, versions: &[&str]) -> Sources {
    for version in versions {
        let path = dir.path().join("agent").join(version);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("VERSION"), format!("{version}\n")).unwrap();
    }
    let mut sources = Sources::from_config(&ServerConfig::default());
    sources.cache = ReleaseCache::in_mirror_dir(dir.path());
    sources
}

fn updater(pool: &PgPool, sources: &Sources) -> AutoUpdate {
    AutoUpdate::new(pool.clone(), sources.clone())
        .with_server_version("1.0.0".parse::<Version>().unwrap())
}

async fn tick(updater: &AutoUpdate) -> Vec<String> {
    let mut peers = updater.tick().await.unwrap();
    peers.sort();
    peers
}

/// Finishes the automatic update command of `peer` as `state`.
async fn finish(pool: &PgPool, peer: &str, state: &str) {
    sqlx::query(
        "UPDATE commands SET state = $2, finished_at = now()
         WHERE server_id = (SELECT id FROM servers WHERE peer_id = $1) AND kind = 'self_update'
           AND state IN ('queued', 'sent', 'running')",
    )
    .bind(peer)
    .bind(state)
    .execute(pool)
    .await
    .unwrap();
}

async fn audited(pool: &PgPool, action: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = $1")
        .bind(action)
        .fetch_one(pool)
        .await
        .unwrap()
}

db_tests! {
    #[sqlx::test]
    async fn sites_update_a_canary_then_batches_once_per_version(pool: PgPool) {
        let (_, paris, lyon) = sites(&pool).await;
        for n in 1..=8 {
            server(&pool, &format!("paris-{n}"), paris, "debian", "0.0.1").await;
        }
        server(&pool, "paris-current", paris, "debian", "0.0.3").await;
        server(&pool, "paris-dev", paris, "rhel", "dev").await;
        let old = server(&pool, "lyon-1", lyon, "windows", "0.0.1").await;

        let dir = TempDir::new().unwrap();
        let sources = cache(&dir, &["0.0.2", "0.0.3", "0.0.4-rc.1"]);
        let updater = updater(&pool, &sources);
        // On by default: one canary per site, never the pre-release.
        assert_eq!(tick(&updater).await, ["lyon-1", "paris-1"]);
        let (kind, payload): (String, Vec<u8>) =
            sqlx::query_as("SELECT kind, payload FROM commands WHERE server_id = $1")
                .bind(old)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(kind, "self_update");
        assert_eq!(SelfUpdate::decode(payload.as_slice()).unwrap().version, "0.0.3");
        assert_eq!(audited(&pool, "auto_update.rollout_started").await, 1);
        // The canaries are in flight.
        assert!(tick(&updater).await.is_empty());

        // Paris' canary succeeded: a batch of 25% of its 10 servers.
        finish(&pool, "paris-1", "succeeded").await;
        assert_eq!(tick(&updater).await, ["paris-2", "paris-3", "paris-4"]);
        assert!(tick(&updater).await.is_empty());
        for n in 2..=4 {
            finish(&pool, &format!("paris-{n}"), "succeeded").await;
        }
        assert_eq!(tick(&updater).await, ["paris-5", "paris-6", "paris-7"]);
        // One expires unsent: it never ran, so it is tried again.
        finish(&pool, "paris-5", "expired").await;
        assert_eq!(tick(&updater).await, ["paris-5"]);

        // A failure is one try: never queued again for this version.
        finish(&pool, "paris-6", "failed").await;
        finish(&pool, "paris-7", "succeeded").await;
        finish(&pool, "paris-5", "succeeded").await;
        assert_eq!(tick(&updater).await, ["paris-8"]);
        finish(&pool, "paris-8", "succeeded").await;
        assert!(tick(&updater).await.is_empty());
        let tries: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM commands c JOIN servers s ON s.id = c.server_id
             WHERE s.peer_id = 'paris-6'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(tries, 1);
        assert_eq!(audited(&pool, "agent.auto_update").await, 10);
    }

    #[sqlx::test]
    async fn overrides_pause_and_the_server_version_hold_updates_back(pool: PgPool) {
        let (customer, paris, lyon) = sites(&pool).await;
        server(&pool, "paris-1", paris, "debian", "0.0.1").await;
        server(&pool, "lyon-1", lyon, "debian", "0.0.1").await;
        let dir = TempDir::new().unwrap();
        let sources = cache(&dir, &["0.0.2", "2.0.0"]);

        let admin = login(&pool, "admin", Role::Administrator, GrantScope::Customer(customer)).await;
        let viewer = login(&pool, "viewer", Role::Viewer, GrantScope::Customer(customer)).await;
        let global = login(&pool, "root", Role::Administrator, GrantScope::Global).await;
        let site_uri = format!("/api/sites/{paris}/agent-auto-update");
        let off = json!({ "enabled": false });
        assert_eq!(call(&pool, &viewer, "PUT", &site_uri, off.clone()).await.0, StatusCode::FORBIDDEN);
        let (status, body) = call(&pool, &admin, "PUT", &site_uri, off).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["agent_auto_update"], false);
        // 2.0.0 is newer than the server (1.0.0): 0.0.2 is the target.
        let updater = updater(&pool, &sources);
        assert_eq!(tick(&updater).await, ["lyon-1"]);

        // Server-wide off; Paris turned on explicitly, Lyon inherits.
        let (status, body) = call(&pool, &admin, "PUT", "/api/settings/auto-update", json!({ "enabled": false })).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "global administrators only: {body}");
        let (status, body) = call(&pool, &global, "PUT", "/api/settings/auto-update", json!({ "enabled": false })).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["enabled"], false);
        let (status, _) = call(&pool, &admin, "PUT", &site_uri, json!({ "enabled": true })).await;
        assert_eq!(status, StatusCode::OK);
        // Paused, nothing moves.
        let (status, body) = call(&pool, &global, "PUT", "/api/settings/auto-update", json!({ "paused": true })).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["paused"], true);
        assert!(tick(&updater).await.is_empty());
        call(&pool, &global, "PUT", "/api/settings/auto-update", json!({ "paused": false })).await;
        assert_eq!(tick(&updater).await, ["paris-1"]);
        // Back to inheriting, which is off now.
        let (status, body) = call(&pool, &admin, "PUT", &site_uri, json!({ "enabled": null })).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["agent_auto_update"], Value::Null);
        assert_eq!(body["agent_auto_update_effective"], false);
        assert_eq!(audited(&pool, "site.agent_auto_update").await, 3);
        assert_eq!(audited(&pool, "settings.auto_update").await, 3);

        // The configuration locks `enabled`.
        let locked = ApiSettings {
            auto_update_enabled: Some(true),
            ..ApiSettings::default()
        };
        let (status, _) = call_with(&pool, locked.clone(), &global, "PUT", "/api/settings/auto-update", json!({ "enabled": false })).await;
        assert_eq!(status, StatusCode::CONFLICT);
        let (status, body) = call_with(&pool, locked, &global, "GET", "/api/auto-update", Value::Null).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["settings"]["enabled"], true);
        assert_eq!(body["settings"]["enabled_locked"], true);
        let (status, _) = call(&pool, &global, "PUT", "/api/settings/auto-update", json!({ "failure_threshold": 0 })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[sqlx::test]
    async fn failures_halt_the_rollout_until_resumed(pool: PgPool) {
        let (_, paris, lyon) = sites(&pool).await;
        server(&pool, "paris-1", paris, "debian", "0.0.1").await;
        server(&pool, "paris-2", paris, "debian", "0.0.1").await;
        server(&pool, "lyon-1", lyon, "debian", "0.0.1").await;
        server(&pool, "lyon-2", lyon, "debian", "0.0.1").await;
        let dir = TempDir::new().unwrap();
        let sources = cache(&dir, &["0.0.2"]);
        let updater = updater(&pool, &sources);
        assert_eq!(tick(&updater).await, ["lyon-1", "paris-1"]);
        // One failed canary blocks its site only.
        finish(&pool, "paris-1", "failed").await;
        assert!(tick(&updater).await.is_empty());
        // The second failure reaches the default threshold of 2.
        finish(&pool, "lyon-1", "failed").await;
        assert!(tick(&updater).await.is_empty());
        assert_eq!(audited(&pool, "auto_update.halted").await, 1);
        let state: String = sqlx::query_scalar("SELECT state FROM auto_rollouts WHERE version = '0.0.2'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(state, "halted");

        let global = login(&pool, "root", Role::Administrator, GrantScope::Global).await;
        let settings = ApiSettings {
            agent_releases: sources.clone(),
            ..ApiSettings::default()
        };
        let (status, body) = call_with(&pool, settings, &global, "GET", "/api/auto-update", Value::Null).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["agents"]["target"], "0.0.2");
        assert_eq!(body["agents"]["rollout"]["state"], "halted");
        assert_eq!(body["agents"]["totals"]["failed"], 2);
        assert_eq!(body["agents"]["totals"]["waiting"], 2);
        assert_eq!(body["agents"]["sites"][0]["phase"], "blocked");

        let uri = "/api/auto-update/rollouts/agent/0.0.2/resume";
        let (status, body) = call(&pool, &global, "POST", uri, Value::Null).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["state"], "rolling");
        assert_eq!(call(&pool, &global, "POST", uri, Value::Null).await.0, StatusCode::CONFLICT);
        assert_eq!(audited(&pool, "auto_update.resume").await, 1);
        // Each site gets a new canary; the failed servers are not retried.
        assert_eq!(tick(&updater).await, ["lyon-2", "paris-2"]);
    }

    #[sqlx::test]
    async fn relays_update_first_and_their_agents_follow(pool: PgPool) {
        let (customer, paris, _) = sites(&pool).await;
        let relay = insert(
            &pool,
            "INSERT INTO relays (peer_id, site_id, name, relay_version)
             VALUES ('relay-1', $1::uuid, 'relay-1', '0.0.1') RETURNING id",
            &[&paris.to_string()],
        )
        .await;
        let behind = server(&pool, "behind", paris, "debian", "0.0.1").await;
        sqlx::query("UPDATE servers SET relay_id = $1 WHERE id = $2")
            .bind(relay)
            .bind(behind)
            .execute(&pool)
            .await
            .unwrap();
        let dir = TempDir::new().unwrap();
        let sources = cache(&dir, &["0.0.2"]);
        let updater = updater(&pool, &sources);

        // The agent waits for its relay, which is older than the target.
        assert!(tick(&updater).await.is_empty());
        let mut relays = updater.tick_relays().await.unwrap();
        relays.sort();
        assert_eq!(relays, ["relay-1"]);
        let (version, payload): (String, Vec<u8>) =
            sqlx::query_as("SELECT version, payload FROM relay_commands WHERE relay_id = $1")
                .bind(relay)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(version, "0.0.2");
        assert_eq!(SelfUpdate::decode(payload.as_slice()).unwrap().version, "0.0.2");
        assert!(updater.tick_relays().await.unwrap().is_empty(), "once per version");
        assert_eq!(audited(&pool, "relay.auto_update").await, 1);

        // Updated and reconnected with the new version: the agent follows.
        sqlx::query("UPDATE relay_commands SET state = 'succeeded', finished_at = now()")
            .execute(&pool)
            .await
            .unwrap();
        assert!(tick(&updater).await.is_empty(), "the relay still reports 0.0.1");
        sqlx::query("UPDATE relays SET relay_version = '0.0.2'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(tick(&updater).await, ["behind"]);

        // The API shows both kinds.
        let global = login(&pool, "root", Role::Administrator, GrantScope::Global).await;
        let settings = ApiSettings {
            agent_releases: sources.clone(),
            ..ApiSettings::default()
        };
        let (status, body) = call_with(&pool, settings, &global, "GET", "/api/auto-update", Value::Null).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["relays"]["totals"]["up_to_date"], 1);
        assert_eq!(body["relays"]["rollout"]["kind"], "relay");
        assert_eq!(body["agents"]["totals"]["pending"], 1);

        // A relay waits while an agent behind it is being updated.
        sqlx::query("UPDATE relays SET relay_version = '0.0.1'")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM relay_auto_updates").execute(&pool).await.unwrap();
        assert!(updater.tick_relays().await.unwrap().is_empty());
        let _ = customer;
    }

    #[sqlx::test]
    async fn administrators_update_a_relay_by_hand(pool: PgPool) {
        let (customer, paris, _) = sites(&pool).await;
        let relay = insert(
            &pool,
            "INSERT INTO relays (peer_id, site_id, name) VALUES ('relay-1', $1::uuid, 'relay-1') RETURNING id",
            &[&paris.to_string()],
        )
        .await;
        let admin = login(&pool, "admin", Role::Administrator, GrantScope::Customer(customer)).await;
        let viewer = login(&pool, "viewer", Role::Viewer, GrantScope::Customer(customer)).await;
        let uri = format!("/api/relays/{relay}/update");
        let body = json!({ "version": "0.3.0" });
        assert_eq!(call(&pool, &viewer, "POST", &uri, body.clone()).await.0, StatusCode::FORBIDDEN);
        let (status, command) = call(&pool, &admin, "POST", &uri, body.clone()).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{command}");
        assert_eq!(command["state"], "queued");
        assert_eq!(command["version"], "0.3.0");
        assert_eq!(call(&pool, &admin, "POST", &uri, body).await.0, StatusCode::CONFLICT);
        let (status, _) = call(&pool, &admin, "POST", &uri, json!({ "version": "nope" })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(audited(&pool, "relay.update").await, 1);
        let (status, detail) = call(&pool, &admin, "GET", &format!("/api/relays/{relay}"), Value::Null).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(detail["last_update"]["version"], "0.3.0");
        assert_eq!(detail["updates"].as_array().unwrap().len(), 1);
        assert_eq!(detail["relay_version"], Value::Null);
    }
}
