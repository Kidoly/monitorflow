//! Revoking servers and relays from the API (ADR-0029): rows, certificates,
//! the CA's CRL, live streams and the audit log. Ignored without
//! `DATABASE_URL`.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Utc;
use monitorflow_common::config::TlsPaths;
use monitorflow_common::redact::Secret;
use monitorflow_common::tls::{Identity, PeerRole};
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::ca::{CaOptions, CertificateAuthority, Serial};
use monitorflow_server::crl::CrlPublisher;
use monitorflow_server::enroll::server_cert;
use monitorflow_server::grpc::{CloseReason, Counters, PeerInfo, Registry};
use serde_json::{json, Value};
use sqlx::PgPool;
use tempfile::TempDir;
use tower::ServiceExt;
use uuid::Uuid;
use x509_parser::prelude::FromDer;
use x509_parser::revocation_list::CertificateRevocationList;

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
    registry: Registry,
    ca: Arc<CertificateAuthority>,
    crl: Option<Arc<CrlPublisher>>,
    _dir: TempDir,
    customer: Uuid,
    site: Uuid,
    server: Uuid,
    relay: Uuid,
}

impl Harness {
    async fn new(pool: &PgPool) -> Self {
        let dir = TempDir::new().unwrap();
        let ca = Arc::new(
            CertificateAuthority::open_or_create(&dir.path().join("ca"), CaOptions::default())
                .unwrap(),
        );
        let paths = TlsPaths {
            ca_cert: dir.path().join("ca.pem"),
            cert: dir.path().join("cert.pem"),
            key: dir.path().join("key.pem"),
        };
        server_cert::ensure(
            &ca,
            &paths,
            &server_cert::parse_names(&["localhost".into()]),
        )
        .unwrap();
        let identity = Identity::load(&paths).unwrap();
        let (crl, _rx) = CrlPublisher::start(ca.clone(), identity).await.unwrap();

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
        let relay = one("INSERT INTO relays (peer_id, site_id, name) VALUES ('relay-1', $1, 'relay-1') RETURNING id")
            .bind(site)
            .fetch_one(pool)
            .await
            .unwrap();
        let server = one(
            "INSERT INTO servers (peer_id, site_id, relay_id, hostname, os_family)
                          VALUES ('agent-1', $1, $2, 'web-1', 'debian') RETURNING id",
        )
        .bind(site)
        .bind(relay)
        .fetch_one(pool)
        .await
        .unwrap();
        // Per peer: a live certificate, an expired one and a revoked one.
        for (role, peer, base) in [("agent", "agent-1", 0x10), ("relay", "relay-1", 0x20)] {
            sqlx::query(
                "INSERT INTO certificates (serial, peer_role, peer_id, not_before, not_after, revoked_at, revocation_reason)
                 VALUES ($1, $3, $4, now() - interval '1 day', now() + interval '30 days', NULL, NULL),
                        ($2, $3, $4, now() - interval '9 days', now() - interval '1 day', NULL, NULL),
                        ($5, $3, $4, now() - interval '9 days', now() + interval '1 day', now(), 'superseded')",
            )
            .bind(vec![base as u8])
            .bind(vec![base as u8 + 1])
            .bind(role)
            .bind(peer)
            .bind(vec![base as u8 + 2])
            .execute(pool)
            .await
            .unwrap();
        }
        Self {
            pool: pool.clone(),
            registry: Registry::default(),
            ca,
            crl: Some(Arc::new(crl)),
            _dir: dir,
            customer,
            site,
            server,
            relay,
        }
    }

    async fn call(&self, uri: &str, cookie: &str, body: Value) -> (StatusCode, Value) {
        let mut state = ApiState::new(
            self.pool.clone(),
            self.registry.clone(),
            ApiSettings::default(),
        );
        if let Some(crl) = &self.crl {
            state = state.with_crl(crl.clone());
        }
        let request = Request::post(uri)
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
            self.registry.clone(),
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

    fn revoked_in_crl(&self) -> Vec<Vec<u8>> {
        let der = self.crl.as_ref().unwrap().subscribe().borrow().clone();
        let (_, crl) = CertificateRevocationList::from_der(der.as_ref()).unwrap();
        crl.iter_revoked_certificates()
            .map(|r| r.raw_serial().to_vec())
            .collect()
    }

    async fn revoked_rows(&self, table: &str) -> bool {
        sqlx::query_scalar(&format!(
            "SELECT bool_or(revoked_at IS NOT NULL) FROM {table}"
        ))
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }
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

db_tests! {
    #[sqlx::test]
    async fn rejecting_a_pending_host_revokes_its_certificates(pool: PgPool) {
        let h = Harness::new(&pool).await;
        // agent-1 enrolled through a key that asks for approval (ADR-0058).
        sqlx::query("DELETE FROM servers").execute(&pool).await.unwrap();
        sqlx::query(
            "WITH k AS (INSERT INTO install_keys (key_hash, site_id, expires_at, require_approval)
                        VALUES (decode(repeat('ab', 32), 'hex'), $1, now() + interval '1 day', true)
                        RETURNING id)
             INSERT INTO pending_hosts (peer_id, site_id, install_key_id, hostname)
             SELECT 'agent-1', $1, id, 'web-1' FROM k",
        )
        .bind(h.site)
        .execute(&pool)
        .await
        .unwrap();
        let viewer = h.login("viewer", Role::Viewer, GrantScope::Customer(h.customer)).await;
        let (status, _) = h.call("/api/pending-hosts/agent-1/reject", &viewer, json!({})).await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        let cookie = h.login("boss", Role::Administrator, GrantScope::Customer(h.customer)).await;
        let (status, body) = h.call("/api/pending-hosts/agent-1/reject", &cookie, json!({})).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        assert_eq!(h.revoked_in_crl(), [vec![0x10]]);
        let (status, _) = h.call("/api/pending-hosts/agent-1/reject", &cookie, json!({})).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let audit: Vec<String> = sqlx::query_scalar(
            "SELECT target_id FROM audit_log WHERE action = 'pending_host.reject'",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(audit, ["agent-1"]);
    }

    #[sqlx::test]
    async fn revoking_a_server_updates_rows_crl_streams_and_audit(pool: PgPool) {
        let h = Harness::new(&pool).await;
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let stream = h.registry.register(live("agent-1", PeerRole::Agent, Some("relay-1")), tx).unwrap();
        let cookie = h.login("boss", Role::Administrator, GrantScope::Customer(h.customer)).await;
        let uri = format!("/api/servers/{}/revoke", h.server);

        let (status, body) = h.call(&uri, &cookie, json!({ "reason": "key_compromise" })).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["serials"], json!(["10"]));
        assert_eq!(body["reason"], "key_compromise");
        assert_eq!(body["closed_streams"], 1);
        assert_eq!(stream.close.await, Ok(CloseReason::Revoked));
        assert!(h.registry.get("agent-1").is_none());

        assert!(h.revoked_rows("servers").await);
        assert!(!h.revoked_rows("relays").await);
        let certs: Vec<(Vec<u8>, Option<String>)> = sqlx::query_as(
            "SELECT serial, revocation_reason FROM certificates WHERE peer_id = 'agent-1' ORDER BY serial",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(certs, [
            (vec![0x10], Some("key_compromise".into())),
            (vec![0x11], None),
            (vec![0x12], Some("superseded".into())),
        ]);
        assert!(h.ca.is_revoked(&Serial::from_bytes(&[0x10])));
        assert!(!h.ca.is_revoked(&Serial::from_bytes(&[0x11])));
        assert_eq!(h.revoked_in_crl(), [vec![0x10]]);

        let audit: Vec<(String, String, Value)> = sqlx::query_as(
            "SELECT action, target_id, details FROM audit_log WHERE action LIKE '%.revoke'",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(audit.len(), 1);
        assert_eq!((audit[0].0.as_str(), audit[0].1.as_str()), ("server.revoke", h.server.to_string().as_str()));
        assert_eq!(audit[0].2["peer_id"], "agent-1");
        assert_eq!(audit[0].2["serials"], json!(["10"]));

        let (status, _) = h.call(&uri, &cookie, json!({})).await;
        assert_eq!(status, StatusCode::CONFLICT);
    }

    #[sqlx::test]
    async fn revoking_a_relay_closes_the_streams_it_forwards(pool: PgPool) {
        let h = Harness::new(&pool).await;
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let relay = h.registry.register(live("relay-1", PeerRole::Relay, None), tx.clone()).unwrap();
        let agent = h.registry.register(live("agent-1", PeerRole::Agent, Some("relay-1")), tx.clone()).unwrap();
        let _direct = h.registry.register(live("agent-2", PeerRole::Agent, None), tx).unwrap();
        let cookie = h.login("root", Role::Administrator, GrantScope::Global).await;

        let (status, body) = h.call(&format!("/api/relays/{}/revoke", h.relay), &cookie, json!({})).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["reason"], "unspecified");
        assert_eq!(body["serials"], json!(["20"]));
        assert_eq!(body["closed_streams"], 2);
        assert_eq!(relay.close.await, Ok(CloseReason::Revoked));
        assert_eq!(agent.close.await, Ok(CloseReason::Revoked));
        assert!(h.registry.get("agent-2").is_some());
        assert!(h.revoked_rows("relays").await);
        assert!(!h.revoked_rows("servers").await);
        assert_eq!(h.revoked_in_crl(), [vec![0x20]]);
        let actions: Vec<String> = sqlx::query_scalar("SELECT action FROM audit_log WHERE action = 'relay.revoke'")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(actions, ["relay.revoke"]);
    }

    #[sqlx::test]
    async fn revocation_needs_an_administrator_of_the_customer(pool: PgPool) {
        let mut h = Harness::new(&pool).await;
        let other: Uuid = sqlx::query_scalar("INSERT INTO customers (name) VALUES ('other') RETURNING id")
            .fetch_one(&pool)
            .await
            .unwrap();
        let cases = [
            ("viewer", Role::Viewer, GrantScope::Global, StatusCode::FORBIDDEN),
            ("operator", Role::Operator, GrantScope::Customer(h.customer), StatusCode::FORBIDDEN),
            ("siteadmin", Role::Administrator, GrantScope::Site(h.site), StatusCode::FORBIDDEN),
            ("otheradmin", Role::Administrator, GrantScope::Customer(other), StatusCode::NOT_FOUND),
        ];
        for (name, role, scope, expected) in cases {
            let cookie = h.login(name, role, scope).await;
            for uri in [format!("/api/servers/{}/revoke", h.server), format!("/api/relays/{}/revoke", h.relay)] {
                let (status, _) = h.call(&uri, &cookie, json!({})).await;
                assert_eq!(status, expected, "{name} {uri}");
            }
        }
        let admin = h.login("admin", Role::Administrator, GrantScope::Global).await;
        let uri = format!("/api/servers/{}/revoke", h.server);
        let (status, _) = h.call(&uri, &admin, json!({ "reason": "boredom" })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = h.call(&format!("/api/servers/{}/revoke", Uuid::new_v4()), &admin, json!({})).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        h.crl = None;
        let (status, _) = h.call(&uri, &admin, json!({})).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);

        // Nothing was revoked by any of these.
        assert!(!h.revoked_rows("servers").await && !h.revoked_rows("relays").await);
        assert!(!h.ca.is_revoked(&Serial::from_bytes(&[0x10])));
        let audited: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action LIKE '%.revoke'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(audited, 0);
    }
}
