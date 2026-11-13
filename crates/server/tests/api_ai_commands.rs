//! AI commands turned on and off from the web interface (ADR-0098): who
//! may, what the agent's report allows, the command, audit entry and
//! notification, the server page's last change and the site's switch.
//! Ignored without `DATABASE_URL`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use monitorflow_common::proto::v1::AiCommandsSwitch;
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
use prost::Message;
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
        Self {
            pool: pool.clone(),
            customer,
            site,
            server,
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
        let state = ApiState::new(self.pool.clone(), Registry::default(), settings);
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
    /// What the agent of `server` last reported, as `command_kinds`.
    async fn report(&self, server: Uuid, kinds: Value) {
        sqlx::query("UPDATE servers SET command_kinds = $2 WHERE id = $1")
            .bind(server)
            .bind(kinds)
            .execute(&self.pool)
            .await
            .unwrap();
    }

    async fn add_server(&self, name: &str) -> Uuid {
        sqlx::query_scalar(
            "INSERT INTO servers (peer_id, site_id, hostname, os_family)
             VALUES ($1, $2, $1, 'debian') RETURNING id",
        )
        .bind(name)
        .bind(self.site)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    async fn commands(&self, server: Uuid) -> Vec<(String, Vec<u8>, i64)> {
        sqlx::query_as(
            "SELECT state, payload, extract(epoch FROM expires_at - issued_at)::bigint
             FROM commands WHERE server_id = $1 AND kind = 'ai_commands' ORDER BY issued_at, id",
        )
        .bind(server)
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }
}

const CONSENT: fn() -> Value = || json!({"ai_commands": "accepted", "shell": "off"});

db_tests! {
    #[sqlx::test]
    async fn site_administrators_switch_and_others_cannot(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let h = Harness::new(&pool).await;
        h.report(h.server, CONSENT()).await;
        let other = sqlx::query_scalar::<_, Uuid>("INSERT INTO customers (name) VALUES ('other') RETURNING id")
            .fetch_one(&pool)
            .await
            .unwrap();
        let outsider = h.login("outsider", Role::Administrator, GrantScope::Customer(other)).await;
        let viewer = h.login("viewer", Role::Viewer, GrantScope::Site(h.site)).await;
        let approver = h.login("approver", Role::Approver, GrantScope::Customer(h.customer)).await;
        let site_admin = h.login("alice", Role::Administrator, GrantScope::Site(h.site)).await;
        let customer_admin = h.login("bob", Role::Administrator, GrantScope::Customer(h.customer)).await;
        let global = h.login("root", Role::Administrator, GrantScope::Global).await;
        let uri = format!("/api/servers/{}/ai-commands", h.server);
        let on = Some(json!({"on": true}));

        assert_eq!(h.send("POST", &uri, &outsider, on.clone()).await.0, StatusCode::NOT_FOUND);
        for cookie in [&viewer, &approver] {
            assert_eq!(h.send("POST", &uri, cookie, on.clone()).await.0, StatusCode::FORBIDDEN);
        }
        let missing = format!("/api/servers/{}/ai-commands", Uuid::new_v4());
        assert_eq!(h.send("POST", &missing, &global, on.clone()).await.0, StatusCode::NOT_FOUND);
        let bad = Some(json!({"on": "yes"}));
        assert!(h.send("POST", &uri, &site_admin, bad).await.0.is_client_error());
        assert!(h.commands(h.server).await.is_empty());

        for cookie in [&customer_admin, &global] {
            assert_eq!(h.send("POST", &uri, cookie, on.clone()).await.0, StatusCode::CREATED);
        }
        let (status, created) = h.send("POST", &uri, &site_admin, on).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        assert_eq!((created["kind"].as_str(), created["state"].as_str()), (Some("ai_commands"), Some("queued")));
        assert_eq!(created["parameters"], json!({"on": true}));

        // The last request wins: the two before it are cancelled.
        let commands = h.commands(h.server).await;
        let states: Vec<_> = commands.iter().map(|c| c.0.as_str()).collect();
        assert_eq!(states, ["cancelled", "cancelled", "queued"]);
        let (_, payload, ttl) = &commands[2];
        assert!(AiCommandsSwitch::decode(payload.as_slice()).unwrap().on);
        assert_eq!(*ttl, 3600);

        // Audited and told to the channels, with who asked.
        let audited: Vec<(String, Value)> = sqlx::query_as(
            "SELECT actor_id, details FROM audit_log WHERE action = 'server.ai_commands' ORDER BY id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(audited.len(), 3);
        assert_eq!(audited[2].1["on"], true);
        assert_eq!(audited[2].1["hostname"], "web-1");
        let told: Vec<(Option<Uuid>, Option<Uuid>, Value)> = sqlx::query_as(
            "SELECT customer_id, site_id, details FROM notification_outbox
             WHERE event = 'ai_commands.changed' ORDER BY id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(told.len(), 3);
        assert_eq!((told[2].0, told[2].1), (Some(h.customer), Some(h.site)));
        assert_eq!(told[2].2["ai_commands"]["requested_by"], "alice");
        assert_eq!(told[2].2["server"]["name"], "web-1");

        // The server's page names who asked, for every reader.
        let (status, detail) = h.send("GET", &format!("/api/servers/{}", h.server), &viewer, None).await;
        assert_eq!(status, StatusCode::OK);
        let change = &detail["ai_commands_change"];
        assert_eq!((change["on"].as_bool(), change["state"].as_str()), (Some(true), Some("queued")));
        assert_eq!(change["requested_by"], "alice");
    }

    #[sqlx::test]
    async fn on_needs_the_hosts_consent_and_off_does_not(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let h = Harness::new(&pool).await;
        let admin = h.login("alice", Role::Administrator, GrantScope::Site(h.site)).await;
        let uri = format!("/api/servers/{}/ai-commands", h.server);
        let (on, off) = (Some(json!({"on": true})), Some(json!({"on": false})));

        // An agent from before ADR-0098 does not know the command.
        let (status, error) = h.send("POST", &uri, &admin, off.clone()).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(error.to_string().contains("updates by itself"), "{error}");

        h.report(h.server, json!({"ai_commands": "off", "shell": "accepted"})).await;
        let (status, error) = h.send("POST", &uri, &admin, on.clone()).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(error.to_string().contains("MF_AI_COMMANDS_FROM_WEB=1"), "{error}");
        let (status, _) = h.send("POST", &uri, &admin, off).await;
        assert_eq!(status, StatusCode::CREATED);
        let commands = h.commands(h.server).await;
        assert_eq!(commands.len(), 1);
        assert!(!AiCommandsSwitch::decode(commands[0].1.as_slice()).unwrap().on);
        assert_eq!(commands[0].2, 7 * 24 * 3600);

        sqlx::query("UPDATE servers SET os_family = 'windows' WHERE id = $1")
            .bind(h.server)
            .execute(&pool)
            .await
            .unwrap();
        let (_, error) = h.send("POST", &uri, &admin, on.clone()).await;
        assert!(error.to_string().contains("ai-commands from-web on"), "{error}");

        sqlx::query("UPDATE servers SET revoked_at = now() WHERE id = $1")
            .bind(h.server)
            .execute(&pool)
            .await
            .unwrap();
        h.report(h.server, CONSENT()).await;
        assert_eq!(h.send("POST", &uri, &admin, on).await.0, StatusCode::CONFLICT);
    }

    #[sqlx::test]
    async fn a_site_switches_the_servers_that_can_and_says_why_not(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let h = Harness::new(&pool).await;
        let admin = h.login("alice", Role::Administrator, GrantScope::Site(h.site)).await;
        let operator = h.login("op", Role::Operator, GrantScope::Site(h.site)).await;
        h.report(h.server, CONSENT()).await;
        let local = h.add_server("web-2").await;
        h.report(local, json!({"ai_commands": "off", "shell": "off"})).await;
        let already = h.add_server("web-3").await;
        h.report(already, json!({"ai_commands": "accepted", "shell": "accepted"})).await;
        let old = h.add_server("web-4").await;
        h.report(old, json!({"shell": "accepted"})).await;
        let uri = format!("/api/sites/{}/ai-commands", h.site);

        assert_eq!(h.send("POST", &uri, &operator, Some(json!({"on": true}))).await.0, StatusCode::FORBIDDEN);
        let (status, change) = h.send("POST", &uri, &admin, Some(json!({"on": true}))).await;
        assert_eq!(status, StatusCode::OK, "{change}");
        assert_eq!(change["queued"], json!([h.server]));
        let skipped: Vec<_> = change["skipped"].as_array().unwrap().iter().map(|s| s["hostname"].as_str().unwrap()).collect();
        assert_eq!(skipped, ["web-2"]);

        let (_, change) = h.send("POST", &uri, &admin, Some(json!({"on": false}))).await;
        assert_eq!(change["queued"], json!([already]));
        assert_eq!(change["skipped"][0]["hostname"], "web-4");
        assert_eq!(h.commands(local).await.len(), 0);
    }
}
