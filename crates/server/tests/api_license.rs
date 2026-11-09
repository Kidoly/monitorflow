//! Administration › License (ADR-0100): installing, reading and removing a
//! license key, who may, and the audit entries. Signed with a throwaway
//! key pair, never the real one. Ignored without `DATABASE_URL`.
#![cfg(feature = "enterprise")]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine;
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::edition::Verifier;
use monitorflow_server::grpc::Registry;
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;

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

/// A throwaway signing key and the verifier that trusts it.
fn test_key() -> (Ed25519KeyPair, Verifier) {
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
    let mut der = vec![
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ];
    der.extend_from_slice(pair.public_key().as_ref());
    let b64 = base64::engine::general_purpose::STANDARD.encode(der);
    let pem = format!("-----BEGIN PUBLIC KEY-----\n{b64}\n-----END PUBLIC KEY-----\n");
    (pair, Verifier::from_pem(&pem))
}

fn license(pair: &Ed25519KeyPair, expires: &str) -> String {
    let payload = json!({
        "format": 1,
        "id": "6f1c1d1e-8a43-4c55-9d43-3c2f6c0e9a01",
        "customer": "Example Corp",
        "edition": "enterprise",
        "server_instances": 3,
        "issued": "2026-11-01",
        "expires": expires,
    })
    .to_string();
    let b64 = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
    format!(
        "-----BEGIN MONITORFLOW LICENSE-----\n{}\n-----END MONITORFLOW LICENSE-----\n\
         -----BEGIN MONITORFLOW LICENSE SIGNATURE-----\n{}\n-----END MONITORFLOW LICENSE SIGNATURE-----\n",
        b64(payload.as_bytes()),
        b64(pair.sign(payload.as_bytes()).as_ref()),
    )
}

struct Harness {
    pool: PgPool,
    verifier: Verifier,
}

impl Harness {
    async fn send(
        &self,
        method: &str,
        uri: &str,
        cookie: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let settings = ApiSettings {
            license_keys: self.verifier.clone(),
            ..ApiSettings::default()
        };
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
        let body = json!({ "username": name, "password": PASSWORD });
        let state = ApiState::new(
            self.pool.clone(),
            Registry::default(),
            ApiSettings::default(),
        );
        let request = Request::post("/api/auth/login")
            .header(CSRF_HEADER, "1")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
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

    async fn audit(&self, action: &str) -> Vec<Value> {
        sqlx::query_scalar("SELECT details FROM audit_log WHERE action = $1 ORDER BY id")
            .bind(action)
            .fetch_all(&self.pool)
            .await
            .unwrap()
    }
}

db_tests! {
    #[sqlx::test(migrations = "./migrations")]
    async fn install_read_and_remove(pool: PgPool) {
        let (pair, verifier) = test_key();
        let h = Harness { pool, verifier };
        let admin = h.login("admin", Role::Administrator, GrantScope::Global).await;

        // A single server: nothing installed, nothing wrong.
        let (status, body) = h.send("GET", "/api/license", &admin, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["available"], true);
        assert_eq!(body["state"], "none");

        let text = license(&pair, "2099-12-31");
        let (status, body) = h
            .send("PUT", "/api/license", &admin, Some(json!({ "text": text })))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["state"], "valid");
        assert_eq!(body["license"]["customer"], "Example Corp");
        assert_eq!(body["license"]["server_instances"], 3);
        assert_eq!(body["license"]["expires"], "2099-12-31");
        assert_eq!(body["installed_by"], "admin");
        let audit = h.audit("license.install").await;
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0]["customer"], "Example Corp");

        let (status, _) = h.send("DELETE", "/api/license", &admin, None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, body) = h.send("GET", "/api/license", &admin, None).await;
        assert_eq!(body["state"], "none");
        assert_eq!(h.audit("license.remove").await.len(), 1);
        let (status, _) = h.send("DELETE", "/api/license", &admin, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn refuses_bad_keys_in_plain_words(pool: PgPool) {
        let (pair, verifier) = test_key();
        let (other, _) = test_key();
        let h = Harness { pool, verifier };
        let admin = h.login("admin", Role::Administrator, GrantScope::Global).await;

        let cases = [
            (license(&other, "2099-12-31"), "signature does not verify"),
            (license(&pair, "2020-01-01"), "This license expired on 2020-01-01"),
            ("my license".to_owned(), "This is not a MonitorFlow license key"),
        ];
        for (text, said) in cases {
            let (status, body) = h
                .send("PUT", "/api/license", &admin, Some(json!({ "text": text })))
                .await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert!(body["error"].as_str().unwrap().contains(said), "{body}");
        }
        let (_, body) = h.send("GET", "/api/license", &admin, None).await;
        assert_eq!(body["state"], "none");
        assert!(h.audit("license.install").await.is_empty());
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn global_administrators_only(pool: PgPool) {
        let (pair, verifier) = test_key();
        let h = Harness { pool, verifier };
        let operator = h.login("op", Role::Operator, GrantScope::Global).await;
        let text = license(&pair, "2099-12-31");
        assert_eq!(h.send("GET", "/api/license", &operator, None).await.0, StatusCode::FORBIDDEN);
        assert_eq!(
            h.send("PUT", "/api/license", &operator, Some(json!({ "text": text }))).await.0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(h.send("DELETE", "/api/license", &operator, None).await.0, StatusCode::FORBIDDEN);
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn a_key_the_build_stops_trusting_shows_as_invalid(pool: PgPool) {
        let (pair, verifier) = test_key();
        let mut h = Harness { pool, verifier };
        let admin = h.login("admin", Role::Administrator, GrantScope::Global).await;
        let text = license(&pair, "2099-12-31");
        h.send("PUT", "/api/license", &admin, Some(json!({ "text": text }))).await;
        // Another build, whose keys do not include this one.
        h.verifier = test_key().1;
        let (_, body) = h.send("GET", "/api/license", &admin, None).await;
        assert_eq!(body["state"], "invalid");
        assert!(body["problem"].as_str().unwrap().contains("signature"));
    }
}
