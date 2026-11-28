//! `POST /api/servers/{id}/jmx/test` against a real PostgreSQL and a
//! Jolokia stand-in over HTTPS with a self-signed certificate (ADR-0183):
//! the certificate is shown, Jolokia is read only once it is trusted by
//! its fingerprint, and the password never reaches a certificate that is
//! not. Ignored without `DATABASE_URL`.

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users;
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use serde_json::{json, Value};
use sqlx::PgPool;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery";
const JOLOKIA_PASSWORD: &str = "jolokia-secret-42";

async fn send(pool: &PgPool, request: Request<Body>) -> (StatusCode, Value, Option<String>) {
    let app = api::router(ApiState::new(
        pool.clone(),
        Registry::default(),
        ApiSettings::default(),
    ));
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let cookie = response
        .headers()
        .get("set-cookie")
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned());
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, body, cookie)
}

async fn session(pool: &PgPool, role: Role, site: Uuid) -> String {
    let id = users::create_user(pool, "op", &Secret::new(PASSWORD), role, "test")
        .await
        .unwrap();
    sqlx::query("UPDATE user_roles SET site_id = $2 WHERE user_id = $1")
        .bind(id)
        .bind(site)
        .execute(pool)
        .await
        .unwrap();
    let body = json!({ "username": "op", "password": PASSWORD }).to_string();
    let request = Request::post("/api/auth/login")
        .header("content-type", "application/json")
        .header(CSRF_HEADER, "1")
        .body(Body::from(body))
        .unwrap();
    let (status, _, cookie) = send(pool, request).await;
    assert_eq!(status, StatusCode::OK);
    cookie.unwrap()
}

async fn test(pool: &PgPool, cookie: &str, device: Uuid, body: Value) -> (StatusCode, Value) {
    let request = Request::post(format!("/api/servers/{device}/jmx/test"))
        .header("cookie", cookie)
        .header(CSRF_HEADER, "1")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let (status, body, _) = send(pool, request).await;
    (status, body)
}

/// A Jolokia stand-in on HTTPS answering bulk reads with the heap and a
/// runtime; its port and the head of every request that reached it.
async fn jolokia() -> (u16, Arc<Mutex<Vec<String>>>) {
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = rcgen::CertificateParams::new(vec!["jvm.example.test".to_owned()])
        .unwrap()
        .self_signed(&key)
        .unwrap();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![cert.der().clone()], key)
    .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let heads = Arc::new(Mutex::new(Vec::new()));
    let seen = heads.clone();
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let Ok(mut stream) = acceptor.accept(tcp).await else {
                continue;
            };
            let mut data = Vec::new();
            let mut buf = [0u8; 8192];
            let end = loop {
                let n = stream.read(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    break None;
                }
                data.extend_from_slice(&buf[..n]);
                if let Some(i) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                    break Some(i + 4);
                }
            };
            let Some(end) = end else { continue };
            let head = String::from_utf8_lossy(&data[..end]).to_string();
            let length: usize = head
                .to_ascii_lowercase()
                .lines()
                .find_map(|l| {
                    l.strip_prefix("content-length:")
                        .map(|v| v.trim().parse().unwrap_or(0))
                })
                .unwrap_or(0);
            while data.len() < end + length {
                let n = stream.read(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                data.extend_from_slice(&buf[..n]);
            }
            seen.lock().unwrap().push(head);
            let reads: Value = serde_json::from_slice(&data[end..]).unwrap_or(Value::Null);
            let mut answers = vec![json!({"status": 404}); reads.as_array().map_or(1, Vec::len)];
            answers[0] =
                json!({"status": 200, "value": {"HeapMemoryUsage": {"used": 1, "max": 2}}});
            if answers.len() > 4 {
                answers[4] = json!({"status": 200, "value": {"VmName": "OpenJDK 64-Bit Server VM", "VmVersion": "21.0.5", "Uptime": 1000}});
            }
            let text = Value::Array(answers).to_string();
            let reply = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
                text.len()
            );
            let _ = stream.write_all(reply.as_bytes()).await;
            let _ = stream.shutdown().await;
        }
    });
    (port, heads)
}

#[cfg_attr(
    not(db_tests),
    ignore = "DATABASE_URL is not set; run `docker compose -f deploy/docker-compose.dev.yml up -d postgres` and export DATABASE_URL from .env.example"
)]
#[sqlx::test]
async fn a_jolokia_certificate_is_trusted_by_its_fingerprint_only(pool: PgPool) {
    let site: Uuid = sqlx::query_scalar("INSERT INTO sites (name) VALUES ('paris') RETURNING id")
        .fetch_one(&pool)
        .await
        .unwrap();
    let device: Uuid = sqlx::query_scalar(
        "INSERT INTO servers (peer_id, site_id, hostname, os_family, kind, address, package_updates)
         VALUES ('jvm-1', $1, 'jvm-1', 'device', 'device', '127.0.0.1', false) RETURNING id",
    )
    .bind(site)
    .fetch_one(&pool)
    .await
    .unwrap();
    let (port, heads) = jolokia().await;
    let operator = session(&pool, Role::Operator, site).await;
    let given = json!({"port": port, "user": "monitorflow", "password": JOLOKIA_PASSWORD});

    // Not trusted yet: shown, nothing read, no password sent.
    let (status, first) = test(&pool, &operator, device, given.clone()).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["answers"], true);
    assert_eq!(first["probe"], "central");
    assert_eq!(first["certificate"]["trusted"], false);
    assert_eq!(first["certificate"]["by_roots"], false);
    assert_eq!(first["reads"], Value::Null, "{first}");
    assert!(heads.lock().unwrap().is_empty());
    let print = first["certificate"]["fingerprint"]
        .as_str()
        .unwrap()
        .to_owned();

    // Another fingerprint is refused the same way.
    let mut wrong = given.clone();
    wrong["fingerprint"] = json!("AB:".repeat(31) + "AB");
    let (_, refused) = test(&pool, &operator, device, wrong).await;
    assert_eq!(refused["certificate"]["trusted"], false);
    assert_eq!(refused["reads"], Value::Null);
    assert!(heads.lock().unwrap().is_empty());

    // Pinned: read, with the password.
    let mut pinned = given.clone();
    pinned["fingerprint"] = json!(print.to_lowercase().replace(':', ""));
    let (_, second) = test(&pool, &operator, device, pinned).await;
    assert_eq!(second["certificate"]["trusted"], true, "{second}");
    assert_eq!(second["reads"], true, "{second}");
    assert_eq!(second["version"], "21.0.5");
    let heads = heads.lock().unwrap().clone();
    assert_eq!(heads.len(), 1);
    assert!(heads[0]
        .to_ascii_lowercase()
        .contains("authorization: basic "));

    // A check with the fingerprint is accepted; `what` is not for a check.
    let check = |params: Value| json!({"kind": "jmx", "name": "jmx", "interval_ms": 60000, "params": params});
    let rows = format!("/api/servers/{device}/sensor_configs");
    let request = |body: Value| {
        Request::post(rows.as_str())
            .header("cookie", operator.as_str())
            .header(CSRF_HEADER, "1")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    let (status, row, _) = send(
        &pool,
        request(check(
            json!({"port": port.to_string(), "tls": "verify", "fingerprint": print}),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{row}");
    assert_eq!(row["params"]["fingerprint"], print);
    let (status, error, _) = send(
        &pool,
        request(check(json!({"tls": "verify", "what": "test"}))),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");

    // A server with an agent names the probe the test runs from.
    let agent: Uuid = sqlx::query_scalar(
        "INSERT INTO servers (peer_id, site_id, hostname, os_family, kind, package_updates)
         VALUES ('web-1', $1, 'web-1', 'debian', 'agent', false) RETURNING id",
    )
    .bind(site)
    .fetch_one(&pool)
    .await
    .unwrap();
    let (status, error) = test(&pool, &operator, agent, json!({"host": "127.0.0.1"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert!(error.to_string().contains("set `probe`"), "{error}");
}
