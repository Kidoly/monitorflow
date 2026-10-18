//! REST API against a real PostgreSQL. Each `#[sqlx::test]` gets a fresh
//! database with the migrations applied. Without `DATABASE_URL` at build
//! time the tests are reported as ignored (see `build.rs`).

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use chrono::{TimeZone, Utc};
use monitorflow_common::redact::Secret;
use monitorflow_common::tls::PeerRole;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::session;
use monitorflow_server::api::users::{self, CreateUserError};
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::{Counters, PeerInfo, Registry};
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
    app_with(pool, Registry::default())
}

fn app_with(pool: PgPool, registry: Registry) -> Router {
    api::router(ApiState::new(pool, registry, ApiSettings::default()))
}

async fn call(app: Router, request: Request<Body>) -> (StatusCode, serde_json::Value) {
    let (status, _, body) = call_with_cookie(app, request).await;
    (status, body)
}

/// Like [`call`], also returning the `Set-Cookie` header.
async fn call_with_cookie(
    app: Router,
    request: Request<Body>,
) -> (StatusCode, Option<String>, serde_json::Value) {
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let cookie = response
        .headers()
        .get("set-cookie")
        .map(|v| v.to_str().unwrap().to_owned());
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    let body = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, cookie, body)
}

async fn add_user(pool: &PgPool, username: &str, role: Role) -> Uuid {
    users::create_user(pool, username, &Secret::new(PASSWORD), role, "test")
        .await
        .unwrap()
}

fn login_request(username: &str, password: &str) -> Request<Body> {
    let body = json!({ "username": username, "password": password }).to_string();
    Request::post("/api/auth/login")
        .header("content-type", "application/json")
        .header(CSRF_HEADER, "1")
        .body(Body::from(body))
        .unwrap()
}

/// Logs in and returns the `name=value` part of the session cookie.
async fn login(pool: &PgPool, username: &str) -> String {
    let (status, cookie, _) =
        call_with_cookie(app(pool.clone()), login_request(username, PASSWORD)).await;
    assert_eq!(status, StatusCode::OK);
    cookie.unwrap().split(';').next().unwrap().to_owned()
}

fn with_cookie(method: &str, uri: &str, cookie: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("cookie", cookie)
        .header(CSRF_HEADER, "1")
        .body(Body::empty())
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
    async fn login_sets_a_session_cookie_and_me_returns_roles(pool: PgPool) {
        let id = add_user(&pool, "Admin", Role::Administrator).await;
        // Usernames match regardless of case.
        let (status, cookie, body) =
            call_with_cookie(app(pool.clone()), login_request("admin", PASSWORD)).await;
        assert_eq!(status, StatusCode::OK);
        let expected = json!({
            "id": id,
            "username": "Admin",
            "roles": [{ "role": "administrator", "site_id": null }],
        });
        assert_eq!(body, expected);
        let cookie = cookie.unwrap();
        for attr in ["__Host-mf_session=", "HttpOnly", "SameSite=Strict", "Secure", "Path=/"] {
            assert!(cookie.contains(attr), "{cookie}");
        }

        // Only the hash of the token is stored.
        let token = cookie.split(';').next().unwrap().split_once('=').unwrap().1;
        let stored: Vec<u8> = sqlx::query_scalar("SELECT token_hash FROM sessions")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(stored.len(), 32);
        assert_ne!(stored, token.as_bytes());

        let session = cookie.split(';').next().unwrap();
        let (status, body) = call(app(pool.clone()), with_cookie("GET", "/api/auth/me", session)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, expected);

        let audit = audit_actions(&pool).await;
        assert_eq!(audit.last().unwrap().0, "auth.login");
        assert_eq!(audit.last().unwrap().1, id.to_string());
    }

    #[sqlx::test]
    async fn failed_logins_are_generic_and_audited(pool: PgPool) {
        let id = add_user(&pool, "alice", Role::Viewer).await;
        let disabled = add_user(&pool, "bob", Role::Viewer).await;
        sqlx::query("UPDATE users SET disabled_at = now() WHERE id = $1")
            .bind(disabled)
            .execute(&pool)
            .await
            .unwrap();

        for (username, password) in [("alice", "wrong password"), ("nobody", PASSWORD), ("bob", PASSWORD)] {
            let (status, cookie, body) =
                call_with_cookie(app(pool.clone()), login_request(username, password)).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{username}");
            assert_eq!(cookie, None);
            assert_eq!(body, json!({ "error": "invalid credentials" }));
        }

        let failures: Vec<_> = audit_actions(&pool)
            .await
            .into_iter()
            .filter(|(action, _, _)| action == "auth.login_failed")
            .map(|(_, actor, details)| (actor, details["reason"].as_str().unwrap().to_owned()))
            .collect();
        assert_eq!(
            failures,
            [
                (id.to_string(), "bad_password".to_owned()),
                ("unknown".to_owned(), "unknown_user".to_owned()),
                (disabled.to_string(), "disabled".to_owned()),
            ]
        );
        // Neither the password nor the submitted username is recorded.
        let all: String = sqlx::query_scalar("SELECT string_agg(details::text, ' ') FROM audit_log")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(!all.contains("wrong password") && !all.contains("nobody"), "{all}");
    }

    #[sqlx::test]
    async fn sessions_expire_after_idle_timeout_or_lifetime(pool: PgPool) {
        add_user(&pool, "alice", Role::Viewer).await;
        let me = |cookie: String| {
            let pool = pool.clone();
            async move { call(app(pool), with_cookie("GET", "/api/auth/me", &cookie)).await.0 }
        };

        let idle = login(&pool, "alice").await;
        assert_eq!(me(idle.clone()).await, StatusCode::OK);
        sqlx::query("UPDATE sessions SET last_seen_at = now() - interval '31 minutes'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(me(idle).await, StatusCode::UNAUTHORIZED);

        let old = login(&pool, "alice").await;
        sqlx::query(
            "UPDATE sessions SET created_at = now() - interval '13 hours',
                                 expires_at = now() - interval '1 hour'
             WHERE last_seen_at > now() - interval '1 minute'",
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(me(old).await, StatusCode::UNAUTHORIZED);

        // The next login prunes both dead sessions.
        login(&pool, "alice").await;
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    #[sqlx::test]
    async fn logout_invalidates_the_session(pool: PgPool) {
        let id = add_user(&pool, "alice", Role::Viewer).await;
        let session = login(&pool, "alice").await;
        let other = login(&pool, "alice").await;

        let (status, cookie, _) =
            call_with_cookie(app(pool.clone()), with_cookie("POST", "/api/auth/logout", &session)).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(cookie.unwrap().contains("Max-Age=0"));

        let (status, _) = call(app(pool.clone()), with_cookie("GET", "/api/auth/me", &session)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _) = call(app(pool.clone()), with_cookie("POST", "/api/auth/logout", &session)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        // Other sessions of the same user are untouched.
        let (status, _) = call(app(pool.clone()), with_cookie("GET", "/api/auth/me", &other)).await;
        assert_eq!(status, StatusCode::OK);

        let last = audit_actions(&pool).await.pop().unwrap();
        assert_eq!((last.0.as_str(), last.1), ("auth.logout", id.to_string()));
    }

    #[sqlx::test]
    async fn disabling_a_user_ends_their_sessions(pool: PgPool) {
        let id = add_user(&pool, "alice", Role::Viewer).await;
        let session = login(&pool, "alice").await;
        sqlx::query("UPDATE users SET disabled_at = now() WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        let (status, _) = call(app(pool.clone()), with_cookie("GET", "/api/auth/me", &session)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
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

    #[sqlx::test]
    async fn servers_require_a_session_and_a_role(pool: PgPool) {
        let (status, _) = call(app(pool.clone()), with_cookie("GET", "/api/servers", "x=y")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        let id = add_user(&pool, "norole", Role::Viewer).await;
        sqlx::query("DELETE FROM user_roles WHERE user_id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        let session = login(&pool, "norole").await;
        let (status, body) = call(app(pool.clone()), with_cookie("GET", "/api/servers", &session)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body, json!({ "error": "forbidden" }));
    }

    #[sqlx::test]
    async fn servers_join_live_registry_status(pool: PgPool) {
        let paris: Uuid = sqlx::query_scalar("INSERT INTO sites (name) VALUES ('paris') RETURNING id")
            .fetch_one(&pool)
            .await
            .unwrap();
        let seen = Utc.with_ymd_and_hms(2026, 10, 1, 8, 0, 0).unwrap();
        for (peer, host) in [("agent-b", "web-2"), ("agent-a", "web-1")] {
            sqlx::query(
                "INSERT INTO servers (peer_id, site_id, hostname, os_family, agent_version, last_seen_at)
                 VALUES ($1, $2, $3, 'debian', '0.0.9', $4)",
            )
            .bind(peer)
            .bind(paris)
            .bind(host)
            .bind(seen)
            .execute(&pool)
            .await
            .unwrap();
        }

        let registry = Registry::default();
        let connected = Utc.with_ymd_and_hms(2026, 10, 18, 12, 0, 0).unwrap();
        let heartbeat = Utc.with_ymd_and_hms(2026, 10, 18, 12, 5, 0).unwrap();
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let _registration = registry
            .register(
                PeerInfo {
                    id: "agent-a".into(),
                    role: PeerRole::Agent,
                    via_relay: Some("relay-1".into()),
                    connection: 0,
                    agent_version: "0.1.0".into(),
                    hostname: "web-1".into(),
                    connected_at: connected,
                    last_seen: heartbeat,
                    last_sequence: 1,
                    counters: Counters::default(),
                },
                tx,
            )
            .unwrap();

        add_user(&pool, "alice", Role::Viewer).await;
        let session = login(&pool, "alice").await;
        let (status, body) =
            call(app_with(pool.clone(), registry), with_cookie("GET", "/api/servers", &session)).await;
        assert_eq!(status, StatusCode::OK);
        let servers = body["servers"].as_array().unwrap();
        assert_eq!(servers.len(), 2);

        let online = &servers[0];
        assert_eq!(online["peer_id"], "agent-a");
        assert_eq!(online["hostname"], "web-1");
        assert_eq!(online["site_name"], "paris");
        assert_eq!(online["site_id"], json!(paris));
        assert_eq!(online["status"], "online");
        assert_eq!(online["connected_at"], "2026-10-18T12:00:00Z");
        assert_eq!(online["last_heartbeat"], "2026-10-18T12:05:00Z");
        assert_eq!(online["via_relay"], "relay-1");
        assert_eq!(online["agent_version"], "0.1.0");

        let offline = &servers[1];
        assert_eq!(offline["peer_id"], "agent-b");
        assert_eq!(offline["status"], "offline");
        assert_eq!(offline["connected_at"], serde_json::Value::Null);
        assert_eq!(offline["last_heartbeat"], "2026-10-01T08:00:00Z");
        assert_eq!(offline["agent_version"], "0.0.9");
        assert_eq!(offline["os_family"], "debian");
        assert_eq!(offline["revoked_at"], serde_json::Value::Null);
    }

    #[sqlx::test]
    async fn site_scoped_roles_see_only_their_sites(pool: PgPool) {
        let mut sites = Vec::new();
        for name in ["paris", "lyon"] {
            let site: Uuid = sqlx::query_scalar("INSERT INTO sites (name) VALUES ($1) RETURNING id")
                .bind(name)
                .fetch_one(&pool)
                .await
                .unwrap();
            sqlx::query(
                "INSERT INTO servers (peer_id, site_id, hostname, os_family) VALUES ($1, $2, $1, 'rhel')",
            )
            .bind(format!("agent-{name}"))
            .bind(site)
            .execute(&pool)
            .await
            .unwrap();
            sites.push(site);
        }
        let id = add_user(&pool, "paris-viewer", Role::Viewer).await;
        sqlx::query("UPDATE user_roles SET site_id = $2 WHERE user_id = $1")
            .bind(id)
            .bind(sites[0])
            .execute(&pool)
            .await
            .unwrap();
        let session = login(&pool, "paris-viewer").await;
        let (status, body) = call(app(pool.clone()), with_cookie("GET", "/api/servers", &session)).await;
        assert_eq!(status, StatusCode::OK);
        let peers: Vec<_> = body["servers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["peer_id"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(peers, ["agent-paris"]);
    }
}
