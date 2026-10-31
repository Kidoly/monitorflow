//! Relay and enrollment token administration endpoints (ADR-0029) against
//! a real PostgreSQL. Ignored without `DATABASE_URL`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use chrono::Utc;
use monitorflow_common::redact::Secret;
use monitorflow_common::tls::{EnrollmentToken, PeerRole};
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{self, ApiSettings, ApiState, EnrollmentSettings, Role};
use monitorflow_server::enroll::token;
use monitorflow_server::grpc::{Counters, PeerInfo, Registry};
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery";
const CA_PIN: [u8; 32] = [7; 32];

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
    registry: Registry,
    enrollment: bool,
}

impl Harness {
    fn new(pool: &PgPool) -> Self {
        Self {
            pool: pool.clone(),
            registry: Registry::default(),
            enrollment: true,
        }
    }

    fn app(&self) -> Router {
        let state = ApiState::new(
            self.pool.clone(),
            self.registry.clone(),
            ApiSettings::default(),
        );
        let state = if self.enrollment {
            state.with_enrollment(EnrollmentSettings {
                ca_fingerprint: CA_PIN,
                server_names: vec!["mf.example".into()],
                grpc_port: 7443,
                enroll_port: 7445,
            })
        } else {
            state
        };
        api::router(state)
    }

    async fn call(
        &self,
        method: &str,
        uri: &str,
        cookie: Option<&str>,
        body: Option<Value>,
    ) -> (StatusCode, Value, Option<String>) {
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
        let response = self.app().oneshot(request).await.unwrap();
        let status = response.status();
        let cookie = response
            .headers()
            .get("set-cookie")
            .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned());
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            cookie,
        )
    }

    /// A user with one grant, logged in; returns the session cookie.
    async fn user(&self, name: &str, role: Role, scope: GrantScope) -> String {
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
        let body = json!({ "username": name, "password": PASSWORD });
        let (status, _, cookie) = self.call("POST", "/api/auth/login", None, Some(body)).await;
        assert_eq!(status, StatusCode::OK);
        cookie.unwrap()
    }
}

async fn customer_site(pool: &PgPool, name: &str) -> (Uuid, Uuid) {
    let customer: Uuid =
        sqlx::query_scalar("INSERT INTO customers (name) VALUES ($1) RETURNING id")
            .bind(name)
            .fetch_one(pool)
            .await
            .unwrap();
    let site =
        sqlx::query_scalar("INSERT INTO sites (name, customer_id) VALUES ($1, $2) RETURNING id")
            .bind(format!("site-{name}"))
            .bind(customer)
            .fetch_one(pool)
            .await
            .unwrap();
    (customer, site)
}

fn live(id: &str, role: PeerRole, via_relay: Option<&str>) -> PeerInfo {
    let now = Utc::now();
    PeerInfo {
        id: id.into(),
        role,
        via_relay: via_relay.map(Into::into),
        connection: 0,
        cert_serial: None,
        agent_version: String::new(),
        hostname: id.into(),
        connected_at: now,
        last_seen: now,
        last_sequence: 0,
        counters: Counters::default(),
        sensor_revision_sent: None,
        sensor_revision_applied: None,
    }
}

async fn audit(pool: &PgPool, action: &str) -> Vec<(String, String, Value)> {
    sqlx::query_as(
        "SELECT actor_id, coalesce(target_id, ''), details FROM audit_log
         WHERE action = $1 ORDER BY id",
    )
    .bind(action)
    .fetch_all(pool)
    .await
    .unwrap()
}

db_tests! {
    #[sqlx::test]
    async fn relays_show_status_certificate_names_and_agents(pool: PgPool) {
        let h = Harness::new(&pool);
        let (_, site) = customer_site(&pool, "acme").await;
        let mut relays = Vec::new();
        for peer in ["relay-1", "relay-2"] {
            let id: Uuid = sqlx::query_scalar(
                "INSERT INTO relays (peer_id, site_id, name) VALUES ($1, $2, $1) RETURNING id",
            )
            .bind(peer)
            .bind(site)
            .fetch_one(&pool)
            .await
            .unwrap();
            relays.push(id);
        }
        // relay-1 enrolled with names, has a certificate and two agents.
        sqlx::query(
            "INSERT INTO enrollment_tokens (token_hash, role, peer_id, site_id, expires_at,
                                            server_names, used_at, used_by_peer_id)
             VALUES ($1, 'relay', 'relay-1', $2, now() + interval '1 hour',
                     '{relay1.acme.example}', now(), 'relay-1')",
        )
        .bind([1u8; 32].as_slice())
        .bind(site)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO certificates (serial, peer_role, peer_id, not_before, not_after)
             VALUES ('\\x01', 'relay', 'relay-1', now(), now() + interval '10 days')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let mut servers = Vec::new();
        for agent in ["web-2", "web-1"] {
            let id: Uuid = sqlx::query_scalar(
                "INSERT INTO servers (peer_id, site_id, relay_id, hostname, os_family)
                 VALUES ($1, $2, $3, $1, 'debian') RETURNING id",
            )
            .bind(agent)
            .bind(site)
            .bind(relays[0])
            .fetch_one(&pool)
            .await
            .unwrap();
            servers.push(id);
        }
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let _relay = h.registry.register(live("relay-1", PeerRole::Relay, None), tx.clone()).unwrap();
        let _agent = h.registry.register(live("web-1", PeerRole::Agent, Some("relay-1")), tx).unwrap();

        let cookie = h.user("viewer", Role::Viewer, GrantScope::Site(site)).await;
        let (status, body, _) = h.call("GET", "/api/relays?limit=1", Some(&cookie), None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let first = &body["relays"][0];
        assert_eq!(first["peer_id"], "relay-1");
        assert_eq!(first["status"], "online");
        assert_eq!(first["server_names"], json!(["relay1.acme.example"]));
        assert_eq!(first["server_count"], 2);
        assert_eq!(first["connected_agents"], 1);
        assert_eq!(first["site_name"], "site-acme");
        assert!(first["cert_not_after"].is_string());
        let cursor = body["next_cursor"].as_str().unwrap().to_owned();
        let (_, body, _) = h
            .call("GET", &format!("/api/relays?limit=1&cursor={cursor}"), Some(&cookie), None)
            .await;
        let second = &body["relays"][0];
        assert_eq!(second["peer_id"], "relay-2");
        assert_eq!(second["status"], "offline");
        assert_eq!(second["server_names"], json!([]));
        assert_eq!(second["cert_not_after"], Value::Null);
        assert_eq!(body["next_cursor"], Value::Null);

        let (status, body, _) = h
            .call("GET", &format!("/api/relays/{}", relays[0]), Some(&cookie), None)
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["certificate"]["serial"], "01");
        assert_eq!(
            body["servers"],
            json!([
                { "id": servers[1], "peer_id": "web-1", "hostname": "web-1", "status": "online" },
                { "id": servers[0], "peer_id": "web-2", "hostname": "web-2", "status": "offline" },
            ])
        );
        let (status, _, _) = h.call("GET", "/api/relays?limit=0", Some(&cookie), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _, _) = h.call("GET", "/api/relays/nope", Some(&cookie), None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (_, body, _) = h.call("GET", "/api/sites", Some(&cookie), None).await;
        assert_eq!(body["sites"][0]["relay_count"], 2);
        assert_eq!(body["sites"][0]["server_count"], 2);
    }

    #[sqlx::test]
    async fn a_relay_key_works_once_for_a_day(pool: PgPool) {
        let h = Harness::new(&pool);
        let (customer, site) = customer_site(&pool, "acme").await;
        let cookie = h.user("boss", Role::Administrator, GrantScope::Customer(customer)).await;
        let request = json!({ "site_id": site, "role": "relay" });
        let (status, created, _) = h.call("POST", "/api/install-keys", Some(&cookie), Some(request)).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        assert_eq!(created["role"], "relay");
        assert_eq!(created["max_hosts"], 1);
        assert_eq!(created["require_approval"], false);
        let ttl = chrono::DateTime::parse_from_rfc3339(created["expires_at"].as_str().unwrap()).unwrap()
            - chrono::DateTime::parse_from_rfc3339(created["created_at"].as_str().unwrap()).unwrap();
        assert_eq!(ttl.num_hours(), 24);
        let (_, list, _) = h.call("GET", "/api/install-keys", Some(&cookie), None).await;
        assert_eq!(list["keys"][0]["role"], "relay");
    }

    #[sqlx::test]
    async fn an_administrator_confirms_what_a_relay_reported(pool: PgPool) {
        let h = Harness::new(&pool);
        let (customer, site) = customer_site(&pool, "acme").await;
        let (_, other_site) = customer_site(&pool, "other").await;
        let cookie = h.user("boss", Role::Administrator, GrantScope::Customer(customer)).await;
        let relay: Uuid = sqlx::query_scalar(
            "INSERT INTO relays (peer_id, site_id, name, pending_names, pending_source)
             VALUES ('relay1', $1, 'relay1', '{relay1.example,10.0.0.5}', '203.0.113.9') RETURNING id",
        )
        .bind(site)
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO relays (peer_id, site_id, name, confirmed_names)
             VALUES ('theirs', $1, 'theirs', '{theirs.example,10.0.0.5}')",
        )
        .bind(other_site)
        .execute(&pool)
        .await
        .unwrap();

        let (status, listed, _) = h.call("GET", "/api/relays", Some(&cookie), None).await;
        assert_eq!(status, StatusCode::OK, "{listed}");
        assert_eq!(listed["relays"][0]["pending_names"], json!(["relay1.example", "10.0.0.5"]));
        assert_eq!(listed["relays"][0]["pending_source"], "203.0.113.9");
        assert_eq!(listed["relays"][0]["server_names"], json!([]));

        let uri = format!("/api/relays/{relay}/names");
        let confirm = |names: Value| h.call("POST", &uri, Some(&cookie), Some(json!({ "names": names })));
        for (names, status) in [
            (json!([]), StatusCode::BAD_REQUEST),
            (json!(["*.example"]), StatusCode::BAD_REQUEST),
            (json!(["localhost"]), StatusCode::BAD_REQUEST),
            // The server's own name, and the name of another site's relay.
            (json!(["MF.example"]), StatusCode::CONFLICT),
            (json!(["relay1.example", "theirs.example"]), StatusCode::CONFLICT),
        ] {
            let (got, body, _) = confirm(names.clone()).await;
            assert_eq!(got, status, "{names}: {body}");
        }

        // A private address is another host on another network; a name added by hand is fine.
        let (status, body, _) = confirm(json!(["Relay1.example", "10.0.0.5", "added.example"])).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        let (_, detail, _) = h.call("GET", &format!("/api/relays/{relay}"), Some(&cookie), None).await;
        assert_eq!(detail["server_names"], json!(["relay1.example", "10.0.0.5", "added.example"]));
        assert_eq!(detail["pending_names"], Value::Null);
        assert_eq!(detail["pending_source"], Value::Null);

        let (_, _, details) = audit(&pool, "relay.confirm_names").await.remove(0);
        assert_eq!(details["peer_id"], "relay1");
        assert_eq!(details["reported"], json!(["relay1.example", "10.0.0.5"]));

        // Once.
        let (status, body, _) = confirm(json!(["relay1.example"])).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
    }

    #[sqlx::test]
    async fn only_administrators_confirm_names(pool: PgPool) {
        let h = Harness::new(&pool);
        let (customer, site) = customer_site(&pool, "acme").await;
        let relay: Uuid = sqlx::query_scalar(
            "INSERT INTO relays (peer_id, site_id, name, pending_names)
             VALUES ('relay1', $1, 'relay1', '{}') RETURNING id",
        )
        .bind(site)
        .fetch_one(&pool)
        .await
        .unwrap();
        let cookie = h.user("op", Role::Operator, GrantScope::Customer(customer)).await;
        let (status, body, _) = h
            .call("POST", &format!("/api/relays/{relay}/names"), Some(&cookie), Some(json!({ "names": ["relay1.example"] })))
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    }

    #[sqlx::test]
    async fn an_install_key_is_shown_once_listed_revoked_and_approves_hosts(pool: PgPool) {
        let h = Harness::new(&pool);
        let (customer, site) = customer_site(&pool, "acme").await;
        let cookie = h.user("boss", Role::Administrator, GrantScope::Customer(customer)).await;
        let request = json!({ "site_id": site, "require_approval": true });
        let (status, created, _) = h.call("POST", "/api/install-keys", Some(&cookie), Some(request)).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let secret = created["token"].as_str().unwrap().to_owned();
        assert_eq!(EnrollmentToken::parse(&secret).unwrap().ca_fingerprint(), &CA_PIN);
        assert_eq!(created["state"], "active");
        assert_eq!(created["max_hosts"], Value::Null);
        assert_eq!(created["host_count"], 0);
        assert_eq!(created["enroll_url"], "https://mf.example:7445");
        assert_eq!(created["install"]["source"], "github");
        let ttl = chrono::DateTime::parse_from_rfc3339(created["expires_at"].as_str().unwrap()).unwrap()
            - chrono::DateTime::parse_from_rfc3339(created["created_at"].as_str().unwrap()).unwrap();
        assert_eq!(ttl.num_days(), 30);
        let (status, body, _) = h
            .call("POST", "/api/install-keys", Some(&cookie), Some(json!({ "site_id": site, "max_hosts": 0 })))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

        // A host enrolled through it waits; its record is not a token.
        let key_id: Uuid = created["id"].as_str().unwrap().parse().unwrap();
        sqlx::query(
            "INSERT INTO enrollment_tokens (token_hash, role, peer_id, site_id, expires_at, used_at,
                                            used_by_peer_id, install_key_id)
             VALUES (decode(repeat('cd', 32), 'hex'), 'agent', 'web-01', $1, now() + interval '1 second',
                     now(), 'web-01', $2)",
        )
        .bind(site)
        .bind(key_id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO pending_hosts (peer_id, site_id, install_key_id, hostname, source)
                     VALUES ('web-01', $1, $2, 'web-01', '192.0.2.7')")
            .bind(site)
            .bind(key_id)
            .execute(&pool)
            .await
            .unwrap();
        let (status, list, _) = h.call("GET", "/api/install-keys", Some(&cookie), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(list["keys"].as_array().unwrap().len(), 1);
        assert!(list["keys"][0].get("token").is_none());
        let secret_part = secret.split('.').nth(1).unwrap();
        assert!(!list.to_string().contains(secret_part));
        assert_eq!(list["pending_hosts"][0]["peer_id"], "web-01");
        assert_eq!(list["pending_hosts"][0]["source"], "192.0.2.7");
        let (_, tokens, _) = h.call("GET", "/api/tokens", Some(&cookie), None).await;
        assert_eq!(tokens["tokens"], json!([]));

        let (status, _, _) = h.call("POST", "/api/pending-hosts/web-01/approve", Some(&cookie), None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _, _) = h.call("POST", "/api/pending-hosts/web-01/approve", Some(&cookie), None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(audit(&pool, "pending_host.approve").await[0].1, "web-01");

        let uri = format!("/api/install-keys/{key_id}");
        let (status, _, _) = h.call("DELETE", &uri, Some(&cookie), None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _, _) = h.call("DELETE", &uri, Some(&cookie), None).await;
        assert_eq!(status, StatusCode::CONFLICT);
        let (_, list, _) = h.call("GET", "/api/install-keys", Some(&cookie), None).await;
        assert_eq!(list["keys"][0]["state"], "revoked");
        assert_eq!(list["pending_hosts"], json!([]));
        assert_eq!(audit(&pool, "install_key.create").await.len(), 1);
        assert_eq!(audit(&pool, "install_key.revoke").await.len(), 1);
        assert!(!audit(&pool, "install_key.create").await[0].2.to_string().contains(secret_part));

        // A site administrator cannot create keys, as with tokens.
        let site_admin = h.user("local", Role::Administrator, GrantScope::Site(site)).await;
        let (status, _, _) = h
            .call("POST", "/api/install-keys", Some(&site_admin), Some(json!({ "site_id": site })))
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[sqlx::test]
    async fn a_token_is_shown_once_and_listed_without_its_secret(pool: PgPool) {
        let h = Harness::new(&pool);
        let (customer, site) = customer_site(&pool, "acme").await;
        let cookie = h.user("boss", Role::Administrator, GrantScope::Customer(customer)).await;
        let request = json!({ "role": "agent", "peer_id": "web-01", "site_id": site, "ttl_s": 600 });
        let (status, created, _) = h.call("POST", "/api/tokens", Some(&cookie), Some(request)).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let secret = created["token"].as_str().unwrap().to_owned();
        let parsed = EnrollmentToken::parse(&secret).unwrap();
        assert_eq!(parsed.ca_fingerprint(), &CA_PIN);
        assert_eq!(created["state"], "pending");
        assert_eq!(created["peer_id"], "web-01");
        assert_eq!(created["created_by_username"], "boss");
        assert_eq!(created["enroll_url"], "https://mf.example:7445");
        assert_eq!(created["upstream_url"], "https://mf.example:7443");
        assert_eq!(created["install"]["source"], "github");
        let ttl = chrono::DateTime::parse_from_rfc3339(created["expires_at"].as_str().unwrap()).unwrap()
            - chrono::DateTime::parse_from_rfc3339(created["created_at"].as_str().unwrap()).unwrap();
        assert!((599..=601).contains(&ttl.num_seconds()), "{ttl}");

        // Only the hash is stored, and enrollment finds the token by it.
        let stored: Vec<u8> = sqlx::query_scalar("SELECT token_hash FROM enrollment_tokens")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(stored, token::hash(&parsed));

        let (status, list, _) = h.call("GET", "/api/tokens", Some(&cookie), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(list["tokens"].as_array().unwrap().len(), 1);
        assert_eq!(list["tokens"][0]["id"], created["id"]);
        assert!(list["tokens"][0].get("token").is_none());
        let text = list.to_string();
        let secret_part = secret.split('.').nth(1).unwrap();
        assert!(!text.contains(secret_part), "{text}");

        let audit = audit(&pool, "enrollment_token.create").await;
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].1, "web-01");
        assert!(!audit[0].2.to_string().contains(secret_part));
    }

    #[sqlx::test]
    async fn tokens_need_an_administrator_of_the_customer(pool: PgPool) {
        let h = Harness::new(&pool);
        let (customer, site) = customer_site(&pool, "acme").await;
        let (other, _) = customer_site(&pool, "other").await;
        let request = || Some(json!({ "role": "agent", "peer_id": "web-01", "site_id": site }));
        let cases = [
            ("viewer", Role::Viewer, GrantScope::Global, StatusCode::FORBIDDEN),
            ("operator", Role::Operator, GrantScope::Customer(customer), StatusCode::FORBIDDEN),
            ("siteadmin", Role::Administrator, GrantScope::Site(site), StatusCode::FORBIDDEN),
            ("otheradmin", Role::Administrator, GrantScope::Customer(other), StatusCode::NOT_FOUND),
            ("global", Role::Administrator, GrantScope::Global, StatusCode::CREATED),
        ];
        for (name, role, scope, expected) in cases {
            let cookie = h.user(name, role, scope).await;
            let (status, body, _) = h.call("POST", "/api/tokens", Some(&cookie), request()).await;
            assert_eq!(status, expected, "{name}: {body}");
            let (status, _, _) = h.call("GET", "/api/tokens", Some(&cookie), None).await;
            let listed = if expected == StatusCode::CREATED { StatusCode::OK } else { StatusCode::FORBIDDEN };
            // The other customer's administrator lists its own (empty) tokens.
            let listed = if name == "otheradmin" { StatusCode::OK } else { listed };
            assert_eq!(status, listed, "{name}");
        }
        let (status, _, _) = h.call("POST", "/api/tokens", None, request()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[sqlx::test]
    async fn a_token_for_an_id_removed_elsewhere_says_so(pool: PgPool) {
        let h = Harness::new(&pool);
        let (_, paris) = customer_site(&pool, "acme").await;
        let (_, lyon) = customer_site(&pool, "other").await;
        sqlx::query(
            "INSERT INTO servers (peer_id, site_id, hostname, os_family, revoked_at)
             VALUES ('web-01', $1, 'web-01', 'debian', now())",
        )
        .bind(paris)
        .execute(&pool)
        .await
        .unwrap();
        let cookie = h.user("admin", Role::Administrator, GrantScope::Global).await;
        let request = |site: Uuid| json!({ "role": "agent", "peer_id": "web-01", "site_id": site });
        let (status, body, _) = h.call("POST", "/api/tokens", Some(&cookie), Some(request(lyon))).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body["error"].as_str().unwrap().contains("site-acme"), "{body}");
        let (status, body, _) = h.call("POST", "/api/tokens", Some(&cookie), Some(request(paris))).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }

    #[sqlx::test]
    async fn approving_a_removed_host_brings_it_back(pool: PgPool) {
        let h = Harness::new(&pool);
        let (_, site) = customer_site(&pool, "acme").await;
        let cookie = h.user("admin", Role::Administrator, GrantScope::Global).await;
        let (status, key, _) = h
            .call("POST", "/api/install-keys", Some(&cookie), Some(json!({ "site_id": site, "require_approval": true })))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{key}");
        sqlx::query(
            "WITH removed AS (INSERT INTO servers (peer_id, site_id, hostname, os_family, revoked_at)
                              VALUES ('web-01', $1, 'web-01', 'debian', now()))
             INSERT INTO pending_hosts (peer_id, site_id, install_key_id, hostname)
             VALUES ('web-01', $1, $2, 'web-01')",
        )
        .bind(site)
        .bind(Uuid::parse_str(key["id"].as_str().unwrap()).unwrap())
        .execute(&pool)
        .await
        .unwrap();
        let (status, _, _) = h.call("POST", "/api/pending-hosts/web-01/approve", Some(&cookie), None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let live: i64 = sqlx::query_scalar("SELECT count(*) FROM servers WHERE revoked_at IS NULL")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(live, 1);
    }

    #[sqlx::test]
    async fn token_requests_are_validated(pool: PgPool) {
        let mut h = Harness::new(&pool);
        let (_, site) = customer_site(&pool, "acme").await;
        let cookie = h.user("admin", Role::Administrator, GrantScope::Global).await;
        let relay = json!({ "role": "relay", "peer_id": "relay-1", "site_id": site,
                            "server_names": ["relay1.example", "10.0.0.1"] });
        let (status, body, _) = h.call("POST", "/api/tokens", Some(&cookie), Some(relay.clone())).await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["server_names"], json!(["relay1.example", "10.0.0.1"]));
        for bad in [
            json!({ "role": "agent", "peer_id": "web-01", "site_id": site, "server_names": ["x.example"] }),
            json!({ "role": "agent", "peer_id": "web/01", "site_id": site }),
            json!({ "role": "agent", "peer_id": "web-01", "site_id": site, "ttl_s": 30 }),
            json!({ "role": "agent", "peer_id": "web-01", "site_id": site, "ttl_s": 604801 }),
            json!({ "role": "relay", "peer_id": "r", "site_id": site, "server_names": ["not a name"] }),
            json!({ "role": "server", "peer_id": "s", "site_id": site }),
        ] {
            let (status, body, _) = h.call("POST", "/api/tokens", Some(&cookie), Some(bad.clone())).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
        }
        let missing = json!({ "role": "agent", "peer_id": "web-01", "site_id": Uuid::new_v4() });
        let (status, _, _) = h.call("POST", "/api/tokens", Some(&cookie), Some(missing)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        h.enrollment = false;
        let (status, _, _) = h.call("POST", "/api/tokens", Some(&cookie), Some(relay)).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[sqlx::test]
    async fn unused_tokens_can_be_revoked_and_listed_by_state(pool: PgPool) {
        let h = Harness::new(&pool);
        let (_, site) = customer_site(&pool, "acme").await;
        let cookie = h.user("admin", Role::Administrator, GrantScope::Global).await;
        let mut ids = Vec::new();
        for peer in ["pending-1", "used-1", "expired-1"] {
            let request = json!({ "role": "agent", "peer_id": peer, "site_id": site });
            let (_, body, _) = h.call("POST", "/api/tokens", Some(&cookie), Some(request)).await;
            ids.push(body["id"].as_str().unwrap().to_owned());
        }
        sqlx::query("UPDATE enrollment_tokens SET used_at = now(), used_by_peer_id = peer_id WHERE peer_id = 'used-1'")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE enrollment_tokens SET created_at = now() - interval '2 hours', expires_at = now() - interval '1 hour' WHERE peer_id = 'expired-1'")
            .execute(&pool)
            .await
            .unwrap();
        for (state, peer) in [("pending", "pending-1"), ("used", "used-1"), ("expired", "expired-1")] {
            let (status, body, _) = h.call("GET", &format!("/api/tokens?state={state}"), Some(&cookie), None).await;
            assert_eq!(status, StatusCode::OK);
            let peers: Vec<&str> = body["tokens"].as_array().unwrap().iter().map(|t| t["peer_id"].as_str().unwrap()).collect();
            assert_eq!(peers, [peer], "{state}");
        }
        let (status, _, _) = h.call("GET", "/api/tokens?state=revoked", Some(&cookie), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        // Pages: newest first, two then one.
        let (_, page, _) = h.call("GET", "/api/tokens?limit=2", Some(&cookie), None).await;
        assert_eq!(page["tokens"].as_array().unwrap().len(), 2);
        let cursor = page["next_cursor"].as_str().unwrap();
        let (_, rest, _) = h.call("GET", &format!("/api/tokens?limit=2&cursor={cursor}"), Some(&cookie), None).await;
        assert_eq!(rest["tokens"].as_array().unwrap().len(), 1);
        assert_eq!(rest["next_cursor"], Value::Null);

        let uri = |i: usize| format!("/api/tokens/{}", ids[i]);
        let (status, _, _) = h.call("DELETE", &uri(1), Some(&cookie), None).await;
        assert_eq!(status, StatusCode::CONFLICT);
        let (status, _, _) = h.call("DELETE", &uri(0), Some(&cookie), None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _, _) = h.call("DELETE", &uri(0), Some(&cookie), None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _, _) = h.call("DELETE", &uri(2), Some(&cookie), None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, body, _) = h.call("GET", "/api/tokens", Some(&cookie), None).await;
        assert_eq!(body["tokens"].as_array().unwrap().len(), 1);
        let audit = audit(&pool, "enrollment_token.revoke").await;
        let targets: Vec<&str> = audit.iter().map(|a| a.1.as_str()).collect();
        assert_eq!(targets, ["pending-1", "expired-1"]);
        assert_eq!(audit[0].2["token_id"], ids[0]);

        // Without the CSRF header, nothing is deleted.
        let request = Request::delete(uri(1)).header("cookie", &cookie).body(Body::empty()).unwrap();
        let response = h.app().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
}
