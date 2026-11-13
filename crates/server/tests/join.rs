//! Additional central servers under an Enterprise license (ADR-0100): the
//! place a license gives, what happens without one or after expiry, the
//! API's list and removal, and the server binary refusing to start as an
//! additional central server without a license. Signed with throwaway key
//! pairs only. Ignored without `DATABASE_URL`.
#![cfg(feature = "enterprise")]

use std::process::Command;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine;
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::edition::{admission, admit_additional_with, store, JoinError, Verifier};
use monitorflow_server::grpc::Registry;
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;

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

struct Key {
    pair: Ed25519KeyPair,
    verifier: Verifier,
}

impl Key {
    fn new() -> Self {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
        let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let mut der = vec![
            0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
        ];
        der.extend_from_slice(pair.public_key().as_ref());
        let b64 = base64::engine::general_purpose::STANDARD.encode(der);
        let pem = format!("-----BEGIN PUBLIC KEY-----\n{b64}\n-----END PUBLIC KEY-----\n");
        Self {
            pair,
            verifier: Verifier::from_pem(&pem),
        }
    }

    /// A license for `servers` central servers until `expires`.
    fn license(&self, servers: u32, expires: &str) -> String {
        let payload = json!({
            "format": 1,
            "id": uuid::Uuid::new_v4(),
            "customer": "Example Corp",
            "edition": "enterprise",
            "server_instances": servers,
            "issued": "2026-01-01",
            "expires": expires,
        })
        .to_string();
        let b64 = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
        format!(
            "-----BEGIN MONITORFLOW LICENSE-----\n{}\n-----END MONITORFLOW LICENSE-----\n\
             -----BEGIN MONITORFLOW LICENSE SIGNATURE-----\n{}\n-----END MONITORFLOW LICENSE SIGNATURE-----\n",
            b64(payload.as_bytes()),
            b64(self.pair.sign(payload.as_bytes()).as_ref()),
        )
    }
}

/// Stores `text` as the license key, whatever it says: an expired one
/// cannot be installed through the API.
async fn put_license(pool: &PgPool, text: &str) {
    sqlx::query(
        "INSERT INTO license (singleton, text) VALUES (true, $1)
         ON CONFLICT (singleton) DO UPDATE SET text = EXCLUDED.text",
    )
    .bind(text)
    .execute(pool)
    .await
    .unwrap();
}

async fn admit(pool: &PgPool, key: &Key, name: &str) -> Result<bool, String> {
    admit_additional_with(pool, &key.verifier, name)
        .await
        .map_err(|e| match e {
            JoinError::Refused(why) => why,
            JoinError::Database(e) => panic!("{e}"),
        })
}

async fn added(pool: &PgPool) -> Vec<String> {
    sqlx::query_scalar("SELECT name FROM server_instance_admissions ORDER BY name")
        .fetch_all(pool)
        .await
        .unwrap()
}

db_tests! {
    #[sqlx::test(migrations = "./migrations")]
    async fn without_a_license_no_central_server_is_added(pool: PgPool) {
        let key = Key::new();
        let why = admit(&pool, &key, "host2").await.unwrap_err();
        assert!(why.starts_with("Adding a central server needs a MonitorFlow Enterprise license"), "{why}");
        assert!(why.contains("Administration > License"), "{why}");
        assert!(why.contains("https://github.com/Kidoly/monitorflow#licence"), "{why}");
        assert!(why.contains("The first central server keeps running"), "{why}");
        assert!(added(&pool).await.is_empty());
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn a_license_gives_places_and_added_servers_keep_them(pool: PgPool) {
        let key = Key::new();
        // Two central servers: the first, and one added.
        put_license(&pool, &key.license(2, "2099-12-31")).await;
        assert_eq!(admit(&pool, &key, "host2").await, Ok(true));
        let audit: Vec<Value> = sqlx::query_scalar(
            "SELECT details FROM audit_log WHERE action = 'server_instance.add'",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0]["server_instances"], 2);
        // A restart, an update: it starts again without taking a place.
        assert_eq!(admit(&pool, &key, "host2").await, Ok(false));
        let why = admit(&pool, &key, "host3").await.unwrap_err();
        assert!(why.starts_with("The license allows 2 central servers, the first one included, and there are already 2"), "{why}");

        // Expired: what was added keeps starting, nothing else is added.
        put_license(&pool, &key.license(5, "2020-01-31")).await;
        assert_eq!(admit(&pool, &key, "host2").await, Ok(false));
        let why = admit(&pool, &key, "host3").await.unwrap_err();
        assert!(why.starts_with("The license expired on 2020-01-31: central servers already added keep running"), "{why}");

        // Removed altogether: the same.
        sqlx::query("DELETE FROM license").execute(&pool).await.unwrap();
        assert_eq!(admit(&pool, &key, "host2").await, Ok(false));
        assert!(admit(&pool, &key, "host3").await.is_err());

        // A key this build no longer trusts.
        put_license(&pool, &Key::new().license(5, "2099-12-31")).await;
        let why = admit(&pool, &key, "host3").await.unwrap_err();
        assert!(why.starts_with("The license key no longer verifies"), "{why}");

        // Freeing a place lets another server in.
        put_license(&pool, &key.license(2, "2099-12-31")).await;
        let mut db = pool.acquire().await.unwrap();
        assert!(admission::remove(&mut db, "host2").await.unwrap());
        assert_eq!(admit(&pool, &key, "host3").await, Ok(true));
        assert_eq!(added(&pool).await, ["host3"]);
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn two_servers_joining_at_once_share_one_place(pool: PgPool) {
        let key = Key::new();
        put_license(&pool, &key.license(2, "2099-12-31")).await;
        let (a, b) = tokio::join!(admit(&pool, &key, "host2"), admit(&pool, &key, "host3"));
        assert_eq!([a.is_ok(), b.is_ok()].iter().filter(|ok| **ok).count(), 1, "{a:?} {b:?}");
        assert_eq!(added(&pool).await.len(), 1);
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn the_installer_asks_first_without_adding(pool: PgPool) {
        let key = Key::new();
        let mut db = pool.acquire().await.unwrap();
        assert!(admission::check(&mut db, &key.verifier, store::today()).await.unwrap().is_err());
        put_license(&pool, &key.license(3, "2099-12-31")).await;
        let license = admission::check(&mut db, &key.verifier, store::today()).await.unwrap().unwrap();
        assert_eq!(license.server_instances, 3);
        assert!(added(&pool).await.is_empty());
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn the_license_page_lists_added_servers_and_frees_places(pool: PgPool) {
        let key = Key::new();
        put_license(&pool, &key.license(3, "2099-12-31")).await;
        admit(&pool, &key, "host2").await.unwrap();
        sqlx::query("INSERT INTO server_instances (id, hostname, version) VALUES (gen_random_uuid(), 'host2', '0.3.0')")
            .execute(&pool)
            .await
            .unwrap();
        users::create_user_with(&pool, "admin", &Secret::new("correct horse battery"), Role::Administrator, GrantScope::Global, "test")
            .await
            .unwrap();
        let settings = || ApiSettings { license_keys: key.verifier.clone(), ..ApiSettings::default() };
        let send = |method: &'static str, uri: &'static str, cookie: Option<String>, body: Option<Value>| {
            let state = ApiState::new(pool.clone(), Registry::default(), settings());
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
            async move { api::router(state).oneshot(request).await.unwrap() }
        };
        let login = send("POST", "/api/auth/login", None, Some(json!({ "username": "admin", "password": "correct horse battery" }))).await;
        let cookie = login.headers()["set-cookie"].to_str().unwrap().split(';').next().unwrap().to_owned();

        let response = send("GET", "/api/license", Some(cookie.clone()), None).await;
        let body: Value = serde_json::from_slice(&axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap()).unwrap();
        assert_eq!(body["state"], "valid");
        assert_eq!(body["added"][0]["name"], "host2");
        assert_eq!(body["added"][0]["running"], true);

        let response = send("DELETE", "/api/license/servers/host2", Some(cookie.clone()), None).await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let response = send("DELETE", "/api/license/servers/host2", Some(cookie), None).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(added(&pool).await.is_empty());
        let removed: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = 'server_instance.remove'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(removed, 1);
    }

    /// The real binary, set up as an additional central server on a
    /// database without a license: it refuses to start and says why. The
    /// test build trusts no license key, so no license could help here.
    #[sqlx::test(migrations = "./migrations")]
    async fn the_server_refuses_to_start_as_an_additional_one_without_a_license(pool: PgPool) {
        let database: String = sqlx::query_scalar("SELECT current_database()").fetch_one(&pool).await.unwrap();
        let mut url = url_of_database(&database);
        url.push_str("?sslmode=disable");
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("server.toml");
        std::fs::write(&config, "").unwrap();
        // The first central server's secrets key, as the join file brings
        // it (ADR-0125), so that only the license is missing.
        monitorflow_server::secrets::Keyring::open_or_create(dir.path(), true).unwrap();
        let output = tokio::task::spawn_blocking(move || {
            Command::new(env!("CARGO_BIN_EXE_monitorflow-server"))
                .arg("--config")
                .arg(&config)
                .env("MONITORFLOW_DATABASE_URL", url)
                .env("MONITORFLOW_ADDITIONAL_INSTANCE", "true")
                .env("MONITORFLOW_DATA_DIR", dir.path())
                .env("HOSTNAME", "host2")
                .output()
                .unwrap()
        })
        .await
        .unwrap();
        assert!(!output.status.success());
        let said = String::from_utf8_lossy(&output.stderr);
        assert!(said.contains("this additional central server does not start: Adding a central server needs a MonitorFlow Enterprise license"), "{said}");
        assert!(added(&pool).await.is_empty());
    }
}

/// `DATABASE_URL` pointed at `database`.
fn url_of_database(database: &str) -> String {
    let base = std::env::var("DATABASE_URL").unwrap();
    let (server, _) = base.rsplit_once('/').unwrap();
    format!("{server}/{database}")
}
