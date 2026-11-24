//! Remote terminals from a server's page (ADR-0178): the settings, who
//! may start, list, close and replay sessions, what the agent's report and
//! connection allow, the one-time token and the audit entries. Ignored
//! without `DATABASE_URL`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use std::sync::Arc;

use monitorflow_common::redact::Secret;
use monitorflow_common::tls::PeerRole;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::ca::{CaOptions, CertificateAuthority};
use monitorflow_server::grpc::{Counters, PeerInfo, Registry};
use monitorflow_server::remote::{AttachError, RemoteHub};
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

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

struct Harness {
    pool: PgPool,
    customer: Uuid,
    site: Uuid,
    server: Uuid,
    registry: Registry,
    hub: RemoteHub,
    _ca: tempfile::TempDir,
}

impl Harness {
    async fn new(pool: &PgPool) -> Self {
        let one = |sql: &'static str| sqlx::query_scalar::<_, Uuid>(sql);
        let customer = one("INSERT INTO customers (name) VALUES ('acme') RETURNING id")
            .fetch_one(pool)
            .await
            .unwrap();
        let site = one("INSERT INTO sites (name, customer_id) VALUES ('paris', $1) RETURNING id")
            .bind(customer)
            .fetch_one(pool)
            .await
            .unwrap();
        let server = one("INSERT INTO servers (peer_id, site_id, hostname, os_family)
             VALUES ('agent-1', $1, 'web-1', 'debian') RETURNING id")
        .bind(site)
        .fetch_one(pool)
        .await
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let ca = Arc::new(
            CertificateAuthority::open_or_create(&dir.path().join("ca"), CaOptions::default())
                .unwrap(),
        );
        let registry = Registry::default();
        let hub = RemoteHub::new(pool.clone(), registry.clone(), ca, None);
        Self {
            pool: pool.clone(),
            customer,
            site,
            server,
            registry,
            hub,
            _ca: dir,
        }
    }

    async fn send(
        &self,
        method: &str,
        uri: &str,
        cookie: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        self.send_with(ApiSettings::default(), method, uri, cookie, body)
            .await
    }

    async fn send_with(
        &self,
        settings: ApiSettings,
        method: &str,
        uri: &str,
        cookie: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let state = ApiState::new(self.pool.clone(), self.registry.clone(), settings)
            .with_remote(self.hub.clone());
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header(CSRF_HEADER, "1")
            .header("content-type", "application/json")
            .header("cookie", cookie)
            .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
            .unwrap();
        let response = api::router(state).oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 22)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn login(&self, name: &str, role: Role, scope: GrantScope) -> String {
        users::create_user_with(
            &self.pool,
            name,
            &Secret::new(PASSWORD),
            role,
            scope,
            "test",
        )
        .await
        .unwrap();
        let state = ApiState::new(
            self.pool.clone(),
            Registry::default(),
            ApiSettings::default(),
        );
        let body = json!({ "username": name, "password": PASSWORD }).to_string();
        let request = Request::post("/api/auth/login")
            .header(CSRF_HEADER, "1")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();
        let response = api::router(state).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned()
    }
}

impl Harness {
    async fn report(&self, kinds: Value) {
        sqlx::query("UPDATE servers SET command_kinds = $2 WHERE id = $1")
            .bind(self.server)
            .bind(kinds)
            .execute(&self.pool)
            .await
            .unwrap();
    }

    /// The agent's stream, as another instance holds it.
    fn connect(&self) {
        let now = chrono::Utc::now();
        self.registry.set_remote(vec![PeerInfo {
            id: "agent-1".into(),
            role: PeerRole::Agent,
            via_relay: None,
            relay_path: Vec::new(),
            connection: 0,
            cert_serial: None,
            agent_version: "0.12.0".into(),
            hostname: "web-1".into(),
            connected_at: now,
            last_seen: now,
            last_sequence: 1,
            counters: Counters::default(),
            sensor_revision_sent: None,
            sensor_revision_applied: None,
        }]);
    }

    async fn audit(&self, action: &str) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = $1")
            .bind(action)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }
}

db_tests! {
    #[sqlx::test]
    async fn global_administrators_choose_who_may_open_terminals(pool: PgPool) {
        let h = Harness::new(&pool).await;
        let global = h.login("root", Role::Administrator, GrantScope::Global).await;
        let customer_admin = h.login("bob", Role::Administrator, GrantScope::Customer(h.customer)).await;
        let uri = "/api/settings/remote-access";
        let (status, settings) = h.send("GET", uri, &global, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(settings, json!({"min_role": "administrator", "record": true,
                                    "idle_timeout_minutes": 15, "retention_days": 90}));
        assert_eq!(h.send("GET", uri, &customer_admin, None).await.0, StatusCode::FORBIDDEN);
        let wider = json!({"min_role": "operator", "record": false, "idle_timeout_minutes": 30,
                           "retention_days": 7});
        assert_eq!(h.send("PUT", uri, &customer_admin, Some(wider.clone())).await.0, StatusCode::FORBIDDEN);
        assert_eq!(h.send("PUT", uri, &global, Some(wider.clone())).await, (StatusCode::OK, wider.clone()));
        assert_eq!(h.send("GET", uri, &global, None).await.1, wider);
        for bad in [
            json!({"min_role": "viewer", "record": true, "idle_timeout_minutes": 15, "retention_days": 90}),
            json!({"min_role": null, "record": true, "idle_timeout_minutes": 0, "retention_days": 90}),
            json!({"min_role": null, "record": true, "idle_timeout_minutes": 15, "retention_days": 4000}),
        ] {
            assert_eq!(h.send("PUT", uri, &global, Some(bad)).await.0, StatusCode::BAD_REQUEST);
        }
        assert_eq!(h.audit("settings.remote_access").await, 1);
    }

    #[sqlx::test]
    async fn a_session_starts_only_where_the_role_the_host_and_its_agent_allow(pool: PgPool) {
        let h = Harness::new(&pool).await;
        let operator = h.login("olga", Role::Operator, GrantScope::Site(h.site)).await;
        let admin = h.login("alice", Role::Administrator, GrantScope::Site(h.site)).await;
        let uri = format!("/api/servers/{}/remote-sessions", h.server);
        let start = Some(json!({"kind": "terminal"}));

        // The server's page says what each person may do.
        let detail = format!("/api/servers/{}", h.server);
        let (_, page) = h.send("GET", &detail, &operator, None).await;
        assert_eq!(page["remote_terminal"], json!({"can_open": false, "can_review": false,
                                                   "recorded": true, "idle_timeout_minutes": 15}));
        assert_eq!(h.send("GET", &detail, &admin, None).await.1["remote_terminal"]["can_open"], true);

        // Administrators only, by default.
        assert_eq!(h.send("POST", &uri, &operator, start.clone()).await.0, StatusCode::FORBIDDEN);
        assert_eq!(h.send("GET", &uri, &operator, None).await.0, StatusCode::FORBIDDEN);
        // An agent too old, one whose host did not allow it, one not connected.
        let (status, error) = h.send("POST", &uri, &admin, start.clone()).await;
        assert_eq!(status, StatusCode::CONFLICT, "{error}");
        assert!(error.to_string().contains("updates by itself"), "{error}");
        h.report(json!({"remote_session": "off"})).await;
        let (status, error) = h.send("POST", &uri, &admin, start.clone()).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(error.to_string().contains("MF_REMOTE_TERMINAL=1"), "{error}");
        h.report(json!({"remote_session": "accepted"})).await;
        let (status, error) = h.send("POST", &uri, &admin, start.clone()).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(error.to_string().contains("not connected"), "{error}");
        assert_eq!(
            h.send("POST", &uri, &admin, Some(json!({"kind": "desktop"}))).await.0,
            StatusCode::BAD_REQUEST
        );

        h.connect();
        let (status, started) = h.send("POST", &uri, &admin, start.clone()).await;
        assert_eq!(status, StatusCode::CREATED, "{started}");
        let id: Uuid = started["session"]["id"].as_str().unwrap().parse().unwrap();
        assert_eq!(started["session"]["requested_by"], "alice");
        assert_eq!(started["session"]["recorded"], true);
        assert_eq!(started["session"]["closed_at"], Value::Null);
        assert_eq!(started["socket"], format!("/api/v1/remote-sessions/{id}/socket"));
        let token = started["token"].as_str().unwrap().to_owned();
        assert_eq!(h.audit("remote_session.start").await, 1);
        let stored: Vec<u8> = sqlx::query_scalar("SELECT token_sha256 FROM remote_sessions WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_ne!(stored, token.as_bytes(), "only its hash is kept");

        // The token works once, for that session, at a valid size.
        assert_eq!(h.hub.attach(id, "wrong", 80, 24).await.unwrap_err(), AttachError::Refused);
        assert_eq!(h.hub.attach(id, &token, 0, 24).await.unwrap_err(), AttachError::BadSize);
        let attached = h.hub.attach(id, &token, 120, 40).await.unwrap();
        assert_eq!((attached.peer_id.as_str(), attached.username.as_str()), ("agent-1", "alice"));
        assert_eq!(attached.idle_timeout_seconds, 900);
        assert_eq!(h.hub.attach(id, &token, 120, 40).await.unwrap_err(), AttachError::Refused);

        // Wider roles once the settings allow them.
        sqlx::query("UPDATE remote_access_settings SET min_role = 'operator', record = false")
            .execute(&pool)
            .await
            .unwrap();
        let (status, started) = h.send("POST", &uri, &operator, start.clone()).await;
        assert_eq!(status, StatusCode::CREATED, "{started}");
        assert_eq!(started["session"]["recorded"], false);
        let (status, list) = h.send("GET", &uri, &operator, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(list.as_array().unwrap().len(), 2);
    }

    #[sqlx::test]
    async fn sessions_close_and_recordings_are_for_site_administrators(pool: PgPool) {
        let h = Harness::new(&pool).await;
        h.report(json!({"remote_session": "accepted"})).await;
        h.connect();
        sqlx::query("UPDATE remote_access_settings SET min_role = 'operator'")
            .execute(&pool)
            .await
            .unwrap();
        let operator = h.login("olga", Role::Operator, GrantScope::Site(h.site)).await;
        let other = h.login("oscar", Role::Operator, GrantScope::Site(h.site)).await;
        let admin = h.login("alice", Role::Administrator, GrantScope::Site(h.site)).await;
        let uri = format!("/api/servers/{}/remote-sessions", h.server);
        let (_, started) = h.send("POST", &uri, &operator, Some(json!({}))).await;
        let id = started["session"]["id"].as_str().unwrap().to_owned();

        // Someone else's session: seen, not closed.
        let close = format!("/api/remote-sessions/{id}/close");
        assert_eq!(h.send("GET", &format!("/api/remote-sessions/{id}"), &other, None).await.0, StatusCode::OK);
        assert_eq!(h.send("POST", &close, &other, None).await.0, StatusCode::FORBIDDEN);
        // Closed before its browser came: at once, and its token stops working.
        assert_eq!(h.send("POST", &close, &operator, None).await.0, StatusCode::NO_CONTENT);
        let (_, session) = h.send("GET", &format!("/api/remote-sessions/{id}"), &operator, None).await;
        assert_eq!(session["close_reason"], "closed by olga before it opened");
        assert_eq!(h.send("POST", &close, &admin, None).await.0, StatusCode::CONFLICT);
        let token = started["token"].as_str().unwrap();
        let uuid: Uuid = id.parse().unwrap();
        assert_eq!(h.hub.attach(uuid, token, 80, 24).await.unwrap_err(), AttachError::Refused);

        // The recording: administrators of the site only.
        sqlx::query("INSERT INTO remote_session_recordings (session_id, seq, at_ms, output)
                     VALUES ($1, 0, 120, 'uptime\r\n'::bytea), ($1, 1, 900, ' 10:00 up'::bytea)")
            .bind(uuid)
            .execute(&pool)
            .await
            .unwrap();
        let recording = format!("/api/remote-sessions/{id}/recording");
        assert_eq!(h.send("GET", &recording, &operator, None).await.0, StatusCode::FORBIDDEN);
        let (status, replay) = h.send("GET", &recording, &admin, None).await;
        assert_eq!(status, StatusCode::OK, "{replay}");
        assert_eq!(replay["chunks"], json!([{"at_ms": 120, "data": "dXB0aW1lDQo="},
                                            {"at_ms": 900, "data": "IDEwOjAwIHVw"}]));
        assert_eq!(h.audit("remote_session.recording_read").await, 1);
    }

    #[sqlx::test]
    async fn sessions_nobody_runs_are_closed_and_old_recordings_deleted(pool: PgPool) {
        let h = Harness::new(&pool).await;
        let insert = |id: Uuid, connected: bool, seen: &'static str, created: &'static str| {
            let pool = pool.clone();
            let server = h.server;
            async move {
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "INSERT INTO remote_sessions (id, server_id, kind, username, token_sha256,
                         token_expires_at, recorded, idle_timeout_seconds, connected_at, seen_at,
                         created_at)
                     VALUES ($1, $2, 'terminal', 'alice', sha256('t'::bytea),
                             now() - interval '1 minute', true, 900,
                             CASE WHEN $3 THEN now() END, now() - interval '{seen}',
                             now() - interval '{created}')"
                )))
                .bind(id)
                .bind(server)
                .bind(connected)
                .execute(&pool)
                .await
                .unwrap();
            }
        };
        let (never, stale, live, old) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        insert(never, false, "0 seconds", "2 minutes").await;
        insert(stale, true, "5 minutes", "10 minutes").await;
        insert(live, true, "5 seconds", "10 minutes").await;
        insert(old, true, "100 days", "100 days").await;
        sqlx::query("INSERT INTO remote_session_recordings (session_id, seq, at_ms, output)
                     VALUES ($1, 0, 0, 'x'), ($2, 0, 0, 'y')")
            .bind(old)
            .bind(live)
            .execute(&pool)
            .await
            .unwrap();
        h.hub.tidy().await.unwrap();
        let reason = |id: Uuid| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, Option<String>>("SELECT close_reason FROM remote_sessions WHERE id = $1")
                    .bind(id)
                    .fetch_one(&pool)
                    .await
                    .unwrap()
            }
        };
        assert_eq!(reason(never).await.as_deref(), Some("the browser never connected"));
        assert_eq!(reason(stale).await.as_deref(), Some("the central server instance running it stopped"));
        assert_eq!(reason(live).await, None);
        let kept: Vec<Uuid> = sqlx::query_scalar("SELECT session_id FROM remote_session_recordings")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(kept, [live]);
        assert_eq!(h.audit("remote_session.close").await, 3);
    }
}
