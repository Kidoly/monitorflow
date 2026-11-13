//! API keys and the public API (ADR-0101) against a real PostgreSQL: who
//! makes and turns off keys, what a key reaches through `/api/v1`, and
//! only there, its access and areas, expiry, the maker losing rights,
//! error codes, rate limits and audit. Ignored without `DATABASE_URL`.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use axum::Router;
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{v1, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
use serde_json::{json, Value};
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

struct World {
    app: Router,
    customer_a: Uuid,
    customer_b: Uuid,
    site_a: Uuid,
    server_a: Uuid,
    server_b: Uuid,
    /// Session cookies by username.
    root: String,
    admin_a: String,
    viewer_a: String,
}

async fn world_with(pool: &PgPool, limits: v1::Limits) -> World {
    monitorflow_server::secrets::install_for_tests();
    let mut state = ApiState::new(pool.clone(), Registry::default(), ApiSettings::default());
    state.api_key_limits = Arc::new(limits);
    let app = v1::app(state);
    let one = |sql: &'static str| sqlx::query_scalar::<_, Uuid>(sql);
    let customer = |name: &'static str| {
        one("INSERT INTO customers (name) VALUES ($1) RETURNING id")
            .bind(name)
            .fetch_one(pool)
    };
    let (customer_a, customer_b) = (customer("a").await.unwrap(), customer("b").await.unwrap());
    let site = |name: &'static str, customer: Uuid| {
        one("INSERT INTO sites (name, customer_id) VALUES ($1, $2) RETURNING id")
            .bind(name)
            .bind(customer)
            .fetch_one(pool)
    };
    let site_a = site("a1", customer_a).await.unwrap();
    let site_b = site("b1", customer_b).await.unwrap();
    let server = |peer: &'static str, site: Uuid| {
        one("INSERT INTO servers (peer_id, site_id, hostname, os_family)
             VALUES ($1, $2, $1, 'debian') RETURNING id")
        .bind(peer)
        .bind(site)
        .fetch_one(pool)
    };
    let server_a = server("agent-a", site_a).await.unwrap();
    let server_b = server("agent-b", site_b).await.unwrap();
    for (name, role, scope) in [
        ("root", Role::Administrator, GrantScope::Global),
        (
            "admin_a",
            Role::Administrator,
            GrantScope::Customer(customer_a),
        ),
        ("viewer_a", Role::Viewer, GrantScope::Customer(customer_a)),
    ] {
        users::create_user_with(pool, name, &Secret::new(PASSWORD), role, scope, "test")
            .await
            .unwrap();
    }
    let mut world = World {
        app,
        customer_a,
        customer_b,
        site_a,
        server_a,
        server_b,
        root: String::new(),
        admin_a: String::new(),
        viewer_a: String::new(),
    };
    world.root = world.login("root").await;
    world.admin_a = world.login("admin_a").await;
    world.viewer_a = world.login("viewer_a").await;
    world
}

async fn world(pool: &PgPool) -> World {
    world_with(pool, v1::Limits::default()).await
}

async fn json_of(response: Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

impl World {
    async fn login(&self, username: &str) -> String {
        let body = json!({ "username": username, "password": PASSWORD }).to_string();
        let request = Request::post("/api/auth/login")
            .header("content-type", "application/json")
            .header(CSRF_HEADER, "1")
            .body(Body::from(body))
            .unwrap();
        let response = self.app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned()
    }

    /// A request with the session `cookie`, as the web interface sends it.
    async fn session(
        &self,
        method: &str,
        uri: &str,
        cookie: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header(CSRF_HEADER, "1")
            .header("cookie", cookie)
            .header("content-type", "application/json")
            .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
            .unwrap();
        let response = self.app.clone().oneshot(request).await.unwrap();
        (response.status(), json_of(response).await)
    }

    /// A request with `key`, as a script sends it: no cookie, no CSRF
    /// header.
    async fn with_key(&self, method: &str, uri: &str, key: &str, body: Option<Value>) -> Response {
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", format!("Bearer {key}"));
        if body.is_some() {
            request = request.header("content-type", "application/json");
        }
        let request = request
            .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
            .unwrap();
        self.app.clone().oneshot(request).await.unwrap()
    }

    async fn key(
        &self,
        method: &str,
        uri: &str,
        key: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let response = self.with_key(method, uri, key, body).await;
        (response.status(), json_of(response).await)
    }

    /// A write with `key` and `Idempotency-Key: value`.
    async fn once(&self, uri: &str, key: &str, value: &str, body: Value) -> Response {
        let request = Request::post(uri)
            .header("authorization", format!("Bearer {key}"))
            .header("idempotency-key", value)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        self.app.clone().oneshot(request).await.unwrap()
    }

    /// Makes a key as `cookie`'s user; returns its ID and secret.
    async fn make(&self, cookie: &str, body: Value) -> (Uuid, String) {
        let (status, created) = self
            .session("POST", "/api/api-keys", cookie, Some(body))
            .await;
        // The answer holds the secret: say only why it failed.
        assert_eq!(status, StatusCode::CREATED, "{}", created["error"]);
        let id = created["key"]["id"].as_str().unwrap().parse().unwrap();
        (id, created["secret"].as_str().unwrap().to_owned())
    }

    async fn hostnames(&self, key: &str) -> Vec<String> {
        let (status, body) = self.key("GET", "/api/v1/servers", key, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let mut names: Vec<String> = body["servers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["hostname"].as_str().unwrap().to_owned())
            .collect();
        names.sort();
        names
    }
}

db_tests! {
    #[sqlx::test]
    async fn administrators_make_list_and_turn_off_keys_of_what_they_administer(pool: PgPool) {
        let w = world(&pool).await;
        // Everything needs a global administrator.
        let body = json!({ "name": "HaloPSA" });
        let (status, error) = w.session("POST", "/api/api-keys", &w.admin_a, Some(body.clone())).await;
        assert_eq!((status, error["code"].as_str()), (StatusCode::FORBIDDEN, Some("forbidden")));
        let (root_key, _) = w.make(&w.root, body).await;
        // A customer's administrator makes keys of that customer or its sites,
        // never of another one.
        let (a_key, secret) = w.make(&w.admin_a, json!({ "name": "Ansible", "customer_id": w.customer_a,
            "role": "operator", "areas": ["alerts", "servers", "alerts"], "expires": "30d" })).await;
        assert!(secret.starts_with("mfk_") && secret.len() == 47);
        w.make(&w.admin_a, json!({ "name": "Site", "site_id": w.site_a })).await;
        let (status, _) = w.session("POST", "/api/api-keys", &w.admin_a,
            Some(json!({ "name": "B", "customer_id": w.customer_b }))).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        // A viewer makes none.
        let (status, _) = w.session("POST", "/api/api-keys", &w.viewer_a,
            Some(json!({ "name": "V", "site_id": w.site_a }))).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        for bad in [
            json!({ "name": " " }),
            json!({ "name": "x", "areas": [] }),
            json!({ "name": "x", "areas": ["everything"] }),
            json!({ "name": "x", "customer_id": w.customer_a, "site_id": w.site_a }),
            json!({ "name": "x", "role": "owner" }),
            json!({ "name": "x", "expires": "2y" }),
            json!({ "name": "x", "secret": "mine" }),
        ] {
            let (status, error) = w.session("POST", "/api/api-keys", &w.root, Some(bad.clone())).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {error}");
            assert_eq!(error["code"], "invalid", "{bad}");
        }

        // Listings show only what the user could have made, never a secret.
        let (_, list) = w.session("GET", "/api/api-keys", &w.admin_a, None).await;
        let names: Vec<&str> = list["keys"].as_array().unwrap().iter().map(|k| k["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["Site", "Ansible"]);
        let ansible = &list["keys"][1];
        assert_eq!(ansible["areas"], json!(["servers", "alerts"]));
        assert_eq!((ansible["role"].as_str(), ansible["customer_name"].as_str()), (Some("operator"), Some("a")));
        // Compared without printing the secret on failure.
        assert!(ansible["prefix"].as_str() == Some(&secret[4..12]));
        assert!(!list.to_string().contains(&secret));
        let (_, list) = w.session("GET", "/api/api-keys", &w.root, None).await;
        assert_eq!(list["keys"].as_array().unwrap().len(), 3);
        let (status, _) = w.session("GET", "/api/api-keys", &w.viewer_a, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        // Turning off: another's reach is 404, then the key stops.
        let uri = format!("/api/api-keys/{root_key}");
        assert_eq!(w.session("DELETE", &uri, &w.admin_a, None).await.0, StatusCode::NOT_FOUND);
        assert_eq!(w.hostnames(&secret).await, ["agent-a"]);
        let uri = format!("/api/api-keys/{a_key}");
        assert_eq!(w.session("DELETE", &uri, &w.admin_a, None).await.0, StatusCode::NO_CONTENT);
        let (status, error) = w.key("GET", "/api/v1/servers", &secret, None).await;
        assert_eq!((status, error["code"].as_str()), (StatusCode::UNAUTHORIZED, Some("unauthenticated")));

        // Audited, without the secret.
        let audit: Vec<(String, Value)> = sqlx::query_as(
            "SELECT action, details FROM audit_log WHERE action LIKE 'api_key.%' ORDER BY id")
            .fetch_all(&pool).await.unwrap();
        let actions: Vec<&str> = audit.iter().map(|(a, _)| a.as_str()).collect();
        assert_eq!(actions, ["api_key.create", "api_key.create", "api_key.create", "api_key.revoke"]);
        assert_eq!(audit[1].1["areas"], json!(["servers", "alerts"]));
        // No part of the secret, not even the prefix shown in the list.
        let audit = format!("{audit:?}");
        assert!(!audit.contains(&secret[4..12]) && !audit.contains("prefix"));
    }

    #[sqlx::test]
    async fn a_key_works_under_v1_only_and_within_its_maker_and_reach(pool: PgPool) {
        let w = world(&pool).await;
        let (_, everything) = w.make(&w.root, json!({ "name": "All" })).await;
        let (_, customer_a) = w.make(&w.root, json!({ "name": "A", "customer_id": w.customer_a })).await;
        let (_, site_a) = w.make(&w.admin_a, json!({ "name": "S", "site_id": w.site_a, "role": "administrator" })).await;
        assert_eq!(w.hostnames(&everything).await, ["agent-a", "agent-b"]);
        assert_eq!(w.hostnames(&customer_a).await, ["agent-a"]);
        assert_eq!(w.hostnames(&site_a).await, ["agent-a"]);
        let uri = format!("/api/v1/servers/{}", w.server_b);
        assert_eq!(w.key("GET", &uri, &customer_a, None).await.0, StatusCode::NOT_FOUND);
        let (status, customers) = w.key("GET", "/api/v1/customers", &customer_a, None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(!customers.to_string().contains(&w.customer_b.to_string()));

        // Not on the web interface's paths, and not with a bad header.
        assert_eq!(w.key("GET", "/api/servers", &everything, None).await.0, StatusCode::UNAUTHORIZED);
        // Each case named by a fixed label: a failure never prints the key.
        let other_scheme = format!("Token {everything}");
        for (what, header) in [("unknown key", "Bearer nope"), ("basic", "Basic abc"),
                               ("other scheme", other_scheme.as_str())] {
            let request = Request::get("/api/v1/servers").header("authorization", header)
                .header("cookie", &w.root).body(Body::empty()).unwrap();
            let response = w.app.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{what}");
        }
        // Routes for people in a browser are not in v1.
        for (method, uri) in [("GET", "/api/v1/auth/me"), ("POST", "/api/v1/auth/logout"),
                              ("GET", "/api/v1/setup"), ("GET", "/api/v1/api-keys"),
                              ("POST", "/api/v1/api-keys"), ("GET", "/api/v1/display"),
                              ("POST", "/api/v1/password-link/use")] {
            let (status, error) = w.key(method, uri, &everything, Some(json!({}))).await;
            assert_eq!((status, error["code"].as_str()), (StatusCode::NOT_FOUND, Some("not_found")), "{method} {uri}");
        }
        // A session works under v1 as under /api, CSRF header included.
        let (status, _) = w.session("GET", "/api/v1/servers", &w.viewer_a, None).await;
        assert_eq!(status, StatusCode::OK);
        let request = Request::post("/api/v1/silences").header("cookie", &w.root)
            .header("content-type", "application/json").body(Body::from("{}")).unwrap();
        let response = w.app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(json_of(response).await["code"], "cross_site");

        // The maker's rights of now: a key of A's administrator follows
        // their grants, and a disabled maker's keys are refused.
        let silence = json!({ "scope": "server", "target_id": w.server_a,
                              "ends_at": chrono::Utc::now() + chrono::Duration::days(1),
                              "reason": "deploy" });
        let (status, body) = w.key("POST", "/api/v1/silences", &site_a, Some(silence.clone())).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        sqlx::query("UPDATE user_roles SET role = 'viewer' WHERE user_id = (SELECT id FROM users WHERE username = 'admin_a')")
            .execute(&pool).await.unwrap();
        let (status, _) = w.key("POST", "/api/v1/silences", &site_a, Some(silence.clone())).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        sqlx::query("UPDATE users SET disabled_at = now() WHERE username = 'admin_a'")
            .execute(&pool).await.unwrap();
        assert_eq!(w.key("GET", "/api/v1/servers", &site_a, None).await.0, StatusCode::UNAUTHORIZED);
    }

    #[sqlx::test]
    async fn access_areas_and_expiry_limit_a_key(pool: PgPool) {
        let w = world(&pool).await;
        let (read_only, reader) = w.make(&w.root, json!({ "name": "R" })).await;
        let (_, alerts) = w.make(&w.root, json!({ "name": "PSA", "role": "operator", "areas": ["alerts"] })).await;
        let incident: Uuid = sqlx::query_scalar(
            "INSERT INTO incidents (server_id, metric, instance, severity, opened_at, last_value,
                                    last_value_at, peak_value, threshold)
             VALUES ($1, 'monitorflow_cpu_usage_ratio', 'total', 'warning', now(), 0.95, now(),
                     0.95, '{}') RETURNING id")
            .bind(w.server_a).fetch_one(&pool).await.unwrap();
        let ack = json!({ "incident_ids": [incident] });

        // Read only: reads, never writes.
        assert_eq!(w.key("GET", "/api/v1/incidents", &reader, None).await.0, StatusCode::OK);
        let (status, error) = w.key("POST", "/api/v1/incidents/acknowledge", &reader, Some(ack.clone())).await;
        assert_eq!((status, error["code"].as_str()), (StatusCode::FORBIDDEN, Some("forbidden")), "{error}");

        // An "alerts" key acknowledges, and lists servers, but nothing else.
        let response = w.with_key("POST", "/api/v1/incidents/acknowledge", &alerts, Some(ack)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(w.hostnames(&alerts).await, ["agent-a", "agent-b"]);
        for (method, uri) in [("GET", format!("/api/v1/servers/{}", w.server_a)),
                              ("GET", format!("/api/v1/servers/{}/commands", w.server_a)),
                              ("GET", "/api/v1/dashboards".into()),
                              ("GET", "/api/v1/settings/server".into()),
                              ("GET", "/api/v1/remediation/approvals".into())] {
            let (status, error) = w.key(method, &uri, &alerts, None).await;
            assert_eq!((status, error["code"].as_str()), (StatusCode::FORBIDDEN, Some("api_key_scope")), "{uri}");
        }
        // What the key did is the maker's action, with the key's ID.
        let details: Value = sqlx::query_scalar(
            "SELECT details FROM audit_log WHERE action = 'incident.acknowledge'")
            .fetch_one(&pool).await.unwrap();
        let alerts_id: Uuid = sqlx::query_scalar("SELECT id FROM api_keys WHERE name = 'PSA'")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(details["api_key"], json!(alerts_id));

        // Use is recorded; an expired key is refused.
        let used: bool = sqlx::query_scalar("SELECT last_used_at IS NOT NULL FROM api_keys WHERE id = $1")
            .bind(read_only).fetch_one(&pool).await.unwrap();
        assert!(used);
        sqlx::query("UPDATE api_keys SET expires_at = now() - interval '1 second' WHERE id = $1")
            .bind(read_only).execute(&pool).await.unwrap();
        assert_eq!(w.key("GET", "/api/v1/servers", &reader, None).await.0, StatusCode::UNAUTHORIZED);
    }

    #[sqlx::test]
    async fn keys_are_rate_limited_with_standard_headers(pool: PgPool) {
        let w = world_with(&pool, v1::Limits::new(2)).await;
        let (_, key) = w.make(&w.root, json!({ "name": "Loop" })).await;
        let (_, other) = w.make(&w.root, json!({ "name": "Other" })).await;
        let header = |r: &Response, name: &str| r.headers().get(name).map(|v| v.to_str().unwrap().to_owned());
        let first = w.with_key("GET", "/api/v1/sites", &key, None).await;
        assert_eq!(first.status(), StatusCode::OK);
        assert_eq!(header(&first, "ratelimit-limit").as_deref(), Some("2"));
        assert_eq!(header(&first, "ratelimit-remaining").as_deref(), Some("1"));
        let reset: u64 = header(&first, "ratelimit-reset").unwrap().parse().unwrap();
        assert!((1..=60).contains(&reset));
        assert_eq!(w.with_key("GET", "/api/v1/sites", &key, None).await.status(), StatusCode::OK);
        let over = w.with_key("GET", "/api/v1/sites", &key, None).await;
        assert_eq!(over.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(header(&over, "retry-after").is_some());
        assert_eq!(header(&over, "ratelimit-remaining").as_deref(), Some("0"));
        assert_eq!(json_of(over).await["code"], "rate_limited");
        // Each key counts its own; sessions are not limited.
        assert_eq!(w.with_key("GET", "/api/v1/sites", &other, None).await.status(), StatusCode::OK);
        for _ in 0..3 {
            assert_eq!(w.session("GET", "/api/v1/sites", &w.root, None).await.0, StatusCode::OK);
        }
    }

    #[sqlx::test]
    async fn writes_with_an_idempotency_key_run_once(pool: PgPool) {
        let w = world(&pool).await;
        let (_, key) = w.make(&w.root, json!({ "name": "Deploy", "role": "administrator" })).await;
        let (_, other) = w.make(&w.root, json!({ "name": "Other", "role": "administrator" })).await;
        let silence = json!({ "scope": "server", "target_id": w.server_a,
                              "ends_at": chrono::Utc::now() + chrono::Duration::days(1),
                              "reason": "deploy" });
        let replayed = |r: &Response| r.headers().get("idempotent-replayed").is_some();

        let first = w.once("/api/v1/silences", &key, "run-1", silence.clone()).await;
        assert_eq!(first.status(), StatusCode::CREATED);
        assert!(!replayed(&first));
        let first = json_of(first).await;
        let again = w.once("/api/v1/silences", &key, "run-1", silence.clone()).await;
        assert_eq!(again.status(), StatusCode::CREATED);
        assert!(replayed(&again));
        assert_eq!(again.headers()["content-type"], "application/json");
        assert_eq!(json_of(again).await, first);
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM silences").fetch_one(&pool).await.unwrap();
        assert_eq!(count, 1);

        // Another key's value is its own; another request with the value is refused.
        let theirs = w.once("/api/v1/silences", &other, "run-1", silence.clone()).await;
        assert!(theirs.status() == StatusCode::CREATED && !replayed(&theirs));
        let mut changed = silence.clone();
        changed["reason"] = json!("other");
        let reused = w.once("/api/v1/silences", &key, "run-1", changed).await;
        assert_eq!(reused.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(json_of(reused).await["code"], "idempotency_key_reused");
        let bad = w.once("/api/v1/silences", &key, "has space", silence.clone()).await;
        assert_eq!(bad.status(), StatusCode::BAD_REQUEST);

        // Errors are kept too, except server errors.
        let invalid = json!({ "scope": "server", "target_id": w.server_a, "reason": "x" });
        let first = w.once("/api/v1/silences", &key, "bad-1", invalid.clone()).await;
        assert_eq!(first.status(), StatusCode::BAD_REQUEST);
        let again = w.once("/api/v1/silences", &key, "bad-1", invalid).await;
        assert!(again.status() == StatusCode::BAD_REQUEST && replayed(&again));

        // A first request still running.
        sqlx::query("UPDATE api_idempotency SET status = NULL, body = NULL WHERE value = 'run-1'
                     AND key_id = (SELECT id FROM api_keys WHERE name = 'Deploy')")
            .execute(&pool).await.unwrap();
        let running = w.once("/api/v1/silences", &key, "run-1", silence).await;
        assert_eq!(running.status(), StatusCode::CONFLICT);
        assert_eq!(json_of(running).await["code"], "idempotency_in_progress");

        // A secret is never stored: the replay says it was done.
        let endpoint = json!({ "name": "PSA", "url": "https://psa.example.com/hook" });
        let first = w.once("/api/v1/integrations", &key, "hook-1", endpoint.clone()).await;
        assert_eq!(first.status(), StatusCode::CREATED);
        let secret = json_of(first).await["secret"].as_str().unwrap().to_owned();
        let again = w.once("/api/v1/integrations", &key, "hook-1", endpoint).await;
        assert!(again.status() == StatusCode::CONFLICT && replayed(&again));
        let stored: Vec<Option<Vec<u8>>> = sqlx::query_scalar("SELECT body FROM api_idempotency WHERE value = 'hook-1'")
            .fetch_all(&pool).await.unwrap();
        assert_eq!(stored, [None]);
        assert!(!json_of(again).await.to_string().contains(&secret));
    }

    #[sqlx::test]
    async fn a_dead_key_gets_no_stored_answer_and_replays_count(pool: PgPool) {
        let w = world_with(&pool, v1::Limits::new(4)).await;
        let (id, key) = w.make(&w.admin_a, json!({ "name": "Deploy", "role": "operator", "site_id": w.site_a })).await;
        let silence = json!({ "scope": "server", "target_id": w.server_a,
                              "ends_at": chrono::Utc::now() + chrono::Duration::days(1),
                              "reason": "deploy" });
        let first = w.once("/api/v1/silences", &key, "run-1", silence.clone()).await;
        assert_eq!(first.status(), StatusCode::CREATED);
        let again = w.once("/api/v1/silences", &key, "run-1", silence.clone()).await;
        assert_eq!(again.status(), StatusCode::CREATED);
        // The replay counted against the key's limit as the first did.
        assert_eq!(again.headers()["ratelimit-remaining"], "2");

        // Its maker disabled, then the key expired: 401, not the answer.
        let disable = |on: bool| sqlx::query("UPDATE users SET disabled_at = CASE WHEN $1 THEN now() END WHERE username = 'admin_a'")
            .bind(on).execute(&pool);
        disable(true).await.unwrap();
        let dead = w.once("/api/v1/silences", &key, "run-1", silence.clone()).await;
        assert_eq!(dead.status(), StatusCode::UNAUTHORIZED);
        assert!(dead.headers().get("idempotent-replayed").is_none());
        disable(false).await.unwrap();
        sqlx::query("UPDATE api_keys SET expires_at = now() - interval '1 second' WHERE id = $1")
            .bind(id).execute(&pool).await.unwrap();
        let dead = w.once("/api/v1/silences", &key, "run-1", silence.clone()).await;
        assert_eq!(dead.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(json_of(dead).await["code"], "unauthenticated");

        // Alive again, replays stop at the limit like any request.
        sqlx::query("UPDATE api_keys SET expires_at = NULL WHERE id = $1")
            .bind(id).execute(&pool).await.unwrap();
        for expected in [StatusCode::CREATED, StatusCode::CREATED, StatusCode::TOO_MANY_REQUESTS] {
            let again = w.once("/api/v1/silences", &key, "run-1", silence.clone()).await;
            assert_eq!(again.status(), expected);
        }
    }

    #[sqlx::test]
    async fn a_key_narrower_than_its_maker_makes_no_display_link(pool: PgPool) {
        let w = world(&pool).await;
        // root sees everything; the key administers one site.
        let (_, key) = w.make(&w.root, json!({ "name": "Site", "role": "administrator", "site_id": w.site_a })).await;
        let dashboard = json!({
            "name": "Everything", "shared_with": { "kind": "personal" },
            "spec": { "version": 1, "range": "6h", "panels": [
                { "id": "c", "title": "CPU", "kind": "chart", "metric": "monitorflow_cpu_usage_ratio",
                  "combine": "avg", "servers": { "kind": "all" } }
            ] }
        });
        let (status, made) = w.key("POST", "/api/v1/dashboards", &key, Some(dashboard)).await;
        assert_eq!(status, StatusCode::CREATED, "{made}");
        let id = made["id"].as_str().unwrap();
        let link = json!({ "name": "TV", "dashboard_ids": [id], "seconds_per_page": 30, "expires": "1y" });
        let (status, body) = w.key("POST", "/api/v1/display-links", &key, Some(link.clone())).await;
        assert_eq!((status, body["code"].as_str()), (StatusCode::NOT_FOUND, Some("not_found")));
        let links: i64 = sqlx::query_scalar("SELECT count(*) FROM display_links").fetch_one(&pool).await.unwrap();
        assert_eq!(links, 0);

        // Its maker's links are neither listed nor turned off with it.
        let (status, made) = w.session("POST", "/api/display-links", &w.root, Some(link)).await;
        assert_eq!(status, StatusCode::CREATED, "{}", made["error"]);
        let list = format!("/api/v1/dashboards/{id}/display-links");
        assert_eq!(w.key("GET", &list, &key, None).await.0, StatusCode::NOT_FOUND);
        let one = format!("/api/v1/display-links/{}", made["id"].as_str().unwrap());
        assert_eq!(w.key("DELETE", &one, &key, None).await.0, StatusCode::NOT_FOUND);
        // Nor with the session under v1; the web interface's path works.
        assert_eq!(w.session("GET", &list, &w.root, None).await.0, StatusCode::NOT_FOUND);
        let (status, body) = w.session("GET", &list.replace("/v1", ""), &w.root, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["links"].as_array().unwrap().len(), 1);
    }

    #[sqlx::test]
    async fn a_key_leaves_its_maker_s_contact_alone(pool: PgPool) {
        let w = world(&pool).await;
        let contact = json!({ "email": "admin@a.example" });
        let (status, _) = w.session("PUT", "/api/on-call/contact", &w.admin_a, Some(contact.clone())).await;
        assert_eq!(status, StatusCode::OK);
        // A viewer key on one site, alerts included.
        let (_, key) = w.make(&w.admin_a, json!({ "name": "Read", "site_id": w.site_a })).await;
        let theirs = json!({ "email": "someone@else.example", "kind": "webhook",
                             "settings": { "url": "https://else.example/hook" } });
        for (method, uri, body) in [
            ("GET", "/api/v1/on-call/contact", None),
            ("PUT", "/api/v1/on-call/contact", Some(theirs)),
            ("POST", "/api/v1/on-call/contact/test", None),
        ] {
            let (status, body) = w.key(method, uri, &key, body).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}");
            assert!(!body.to_string().contains("a.example"), "{body}");
        }
        let (status, body) = w.session("GET", "/api/on-call/contact", &w.admin_a, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!((&body["email"], &body["kind"]), (&contact["email"], &Value::Null));
        // The rest of on-call stays reachable.
        assert_eq!(w.key("GET", "/api/v1/on-call/schedules", &key, None).await.0, StatusCode::OK);
    }

    #[sqlx::test]
    async fn a_key_hands_out_no_password_link(pool: PgPool) {
        let w = world(&pool).await;
        // The widest key there is: all of a global administrator's reach.
        let (_, key) = w.make(&w.root, json!({ "name": "Admin", "role": "administrator" })).await;
        let invite = json!({ "username": "newcomer", "role": "administrator" });
        let (status, body) = w.key("POST", "/api/v1/users", &key, Some(invite)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        let (status, body) = w.key("GET", "/api/v1/users", &key, None).await;
        assert_eq!(status, StatusCode::OK);
        let users = body["users"].as_array().unwrap();
        assert_eq!(users.len(), 3, "{body}");
        let admin = users.iter().find(|u| u["username"] == "admin_a").unwrap();
        let link = format!("/api/v1/users/{}/link", admin["id"].as_str().unwrap());
        let (status, body) = w.key("POST", &link, &key, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        let links: i64 = sqlx::query_scalar("SELECT count(*) FROM password_links").fetch_one(&pool).await.unwrap();
        assert_eq!(links, 0);
        // A person still does both.
        let (status, _) = w.session("POST", &link.replace("/v1", ""), &w.root, None).await;
        assert_eq!(status, StatusCode::CREATED);
    }

    #[sqlx::test]
    async fn the_document_is_served_without_a_key(pool: PgPool) {
        let w = world(&pool).await;
        let request = Request::get("/api/v1/openapi.json").body(Body::empty()).unwrap();
        let response = w.app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let doc = json_of(response).await;
        assert_eq!(doc["info"]["version"], "1");
        assert!(doc["paths"]["/api/v1/incidents/acknowledge"]["post"].is_object());
    }
}
