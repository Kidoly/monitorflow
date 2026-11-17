//! The tested update channel (ADR-0147): a central server with a valid
//! license key checks the release `tested.json` names, others the latest
//! one. Against a real PostgreSQL and an HTTP server shaped like GitHub's
//! release downloads, with throwaway release and license keys. Ignored
//! without `DATABASE_URL`.
#![cfg(feature = "enterprise")]

use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine;
use monitorflow_common::redact::Secret;
use monitorflow_common::release::{sha256, PublicKey};
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::server_update::ServerUpdateSettings;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::edition::{update_channel_with, Channel, Verifier};
use monitorflow_server::grpc::Registry;
use monitorflow_server::release_check;
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde_json::{json, Value};
use sqlx::PgPool;
use tempfile::TempDir;
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";
const LATEST: &str = "99.1.0";
const TESTED: &str = "99.0.0";

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

fn pair() -> Ed25519KeyPair {
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
    Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap()
}

fn spki(pair: &Ed25519KeyPair) -> Vec<u8> {
    let mut der = vec![
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ];
    der.extend_from_slice(pair.public_key().as_ref());
    der
}

/// The files of release `version`, signed by `key`, in `dir`.
fn release(dir: &Path, key: &Ed25519KeyPair, version: &str) {
    std::fs::create_dir_all(dir).unwrap();
    let image = |name: &str| format!("registry.test/monitorflow-{name}@sha256:{}", "b".repeat(64));
    let images = json!({"version": version, "images": {
        "server": image("server"), "web": image("web"), "updater": image("updater")}});
    let mut sums = String::new();
    for (name, bytes) in [
        ("VERSION", format!("{version}\n").into_bytes()),
        ("images.json", images.to_string().into_bytes()),
    ] {
        std::fs::write(dir.join(name), &bytes).unwrap();
        let hex: String = sha256(&bytes).iter().map(|b| format!("{b:02x}")).collect();
        sums.push_str(&format!("{hex}  {name}\n"));
    }
    std::fs::write(dir.join("SHA256SUMS"), &sums).unwrap();
    std::fs::write(dir.join("SHA256SUMS.sig"), key.sign(sums.as_bytes())).unwrap();
}

/// `tested.json` naming `version`, signed by `key`, in `dir`, as the
/// release workflow's promotion writes it.
fn promote(dir: &Path, key: &Ed25519KeyPair, version: &str) {
    let file =
        json!({"channel": "tested", "version": version, "promoted": "2026-12-08"}).to_string();
    std::fs::write(dir.join("tested.json"), &file).unwrap();
    std::fs::write(dir.join("tested.json.sig"), key.sign(file.as_bytes())).unwrap();
}

/// Serves `root` as GitHub serves releases: `latest/` at
/// `…/releases/latest/download/`, `v<version>/` at
/// `…/releases/download/v<version>/`. Returns the latest-release URL.
async fn serve(root: PathBuf) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let read =
        |path: PathBuf| async move { std::fs::read(path).map_err(|_| StatusCode::NOT_FOUND) };
    let latest = root.join("latest");
    let app = axum::Router::new()
        .route(
            "/o/r/releases/latest/download/{name}",
            axum::routing::get(
                move |axum::extract::Path(name): axum::extract::Path<String>| {
                    read(latest.join(name))
                },
            ),
        )
        .route(
            "/o/r/releases/download/{tag}/{name}",
            axum::routing::get(
                move |axum::extract::Path((tag, name)): axum::extract::Path<(String, String)>| {
                    read(root.join(tag).join(name))
                },
            ),
        );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}/o/r/releases/latest/download")
}

/// A license key of `tier` until `expires`, signed by `key`.
fn license(key: &Ed25519KeyPair, tier: &str, expires: &str) -> String {
    let payload = json!({
        "format": 2, "id": uuid::Uuid::new_v4(), "customer": "Example Corp", "tier": tier,
        "managed_servers": 10, "server_instances": 1, "issued": "2026-01-01", "expires": expires,
    })
    .to_string();
    let b64 = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
    format!(
        "-----BEGIN MONITORFLOW LICENSE-----\n{}\n-----END MONITORFLOW LICENSE-----\n\
         -----BEGIN MONITORFLOW LICENSE SIGNATURE-----\n{}\n-----END MONITORFLOW LICENSE SIGNATURE-----\n",
        b64(payload.as_bytes()),
        b64(key.sign(payload.as_bytes()).as_ref()),
    )
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

struct Harness {
    pool: PgPool,
    settings: ApiSettings,
    cookie: String,
}

impl Harness {
    async fn new(
        pool: PgPool,
        source: &str,
        release_key: &[u8],
        license_key: &Ed25519KeyPair,
    ) -> Self {
        let b64 = base64::engine::general_purpose::STANDARD.encode(spki(license_key));
        let pem = format!("-----BEGIN PUBLIC KEY-----\n{b64}\n-----END PUBLIC KEY-----\n");
        let settings = ApiSettings {
            server_update: ServerUpdateSettings {
                dir: None,
                backup_dir: None,
                source: source.into(),
                keys: vec![PublicKey::from_spki(release_key).unwrap()],
                client: reqwest::Client::new(),
            },
            license_keys: Verifier::from_pem(&pem),
            ..ApiSettings::default()
        };
        users::create_user_with(
            &pool,
            "root",
            &Secret::new(PASSWORD),
            Role::Administrator,
            GrantScope::Global,
            "test",
        )
        .await
        .unwrap();
        let mut h = Self {
            pool,
            settings,
            cookie: String::new(),
        };
        let request = Request::post("/api/auth/login")
            .header(CSRF_HEADER, "1")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({ "username": "root", "password": PASSWORD }).to_string(),
            ))
            .unwrap();
        let response = h.router().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        h.cookie = response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        h
    }

    fn router(&self) -> axum::Router {
        api::router(ApiState::new(
            self.pool.clone(),
            Registry::default(),
            self.settings.clone(),
        ))
    }

    async fn call(&self, method: &str, uri: &str) -> Value {
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header(CSRF_HEADER, "1")
            .header("cookie", &self.cookie)
            .body(Body::empty())
            .unwrap();
        let response = self.router().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn check(&self) -> Value {
        self.call("POST", "/api/server-update/check").await
    }
}

db_tests! {
    #[sqlx::test(migrations = "./migrations")]
    async fn subscribers_follow_the_promoted_release(pool: PgPool) {
        let release_key = pair();
        let license_key = pair();
        let root = TempDir::new().unwrap();
        // 99.1.0 is the latest release; 99.0.0 was promoted, and 99.1.0
        // carries the channel file forward.
        release(&root.path().join("latest"), &release_key, LATEST);
        release(&root.path().join(format!("v{LATEST}")), &release_key, LATEST);
        release(&root.path().join(format!("v{TESTED}")), &release_key, TESTED);
        promote(&root.path().join("latest"), &release_key, TESTED);
        let source = serve(root.path().to_owned()).await;
        let h = Harness::new(pool.clone(), &source, &spki(&release_key), &license_key).await;

        // Community: the regular channel, as before.
        let body = h.check().await;
        assert_eq!(body["channel"], "regular", "{body}");
        assert_eq!(body["latest"]["version"], LATEST, "{body}");
        assert_eq!(h.call("GET", "/api/server-update").await["channel"], "regular");

        // Any tier: the tested channel.
        for tier in ["basic", "standard", "premium", "enterprise"] {
            put_license(&pool, &license(&license_key, tier, "2099-12-31")).await;
            let body = h.check().await;
            assert_eq!(body["channel"], "tested", "{tier}: {body}");
            assert_eq!(body["error"], Value::Null, "{body}");
            assert_eq!(body["latest"]["version"], TESTED, "{body}");
            assert_eq!(
                body["latest"]["release_notes_url"],
                source.replace("/releases/latest/download", &format!("/releases/tag/v{TESTED}"))
            );
        }
        assert_eq!(h.call("GET", "/api/server-update").await["channel"], "tested");

        // The daily check follows the same channel.
        let channel = update_channel_with(&pool, &h.settings.license_keys).await.unwrap();
        assert_eq!(channel, Channel::Tested);
        let running = "0.1.0".parse().unwrap();
        assert!(release_check::check(&pool, &h.settings.server_update, channel, &running).await);
        let seen: Vec<String> = sqlx::query_scalar("SELECT version FROM seen_releases ORDER BY version")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert!(seen.contains(&TESTED.to_owned()), "{seen:?}");

        // Expired: back to the regular channel; nothing else changes.
        put_license(&pool, &license(&license_key, "standard", "2020-01-01")).await;
        let body = h.check().await;
        assert_eq!(body["channel"], "regular");
        assert_eq!(body["latest"]["version"], LATEST);
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn a_channel_file_that_does_not_verify_offers_nothing(pool: PgPool) {
        let release_key = pair();
        let license_key = pair();
        let root = TempDir::new().unwrap();
        release(&root.path().join("latest"), &release_key, LATEST);
        release(&root.path().join(format!("v{TESTED}")), &release_key, TESTED);
        let source = serve(root.path().to_owned()).await;
        let h = Harness::new(pool.clone(), &source, &spki(&release_key), &license_key).await;
        put_license(&pool, &license(&license_key, "basic", "2099-12-31")).await;

        // No promotion yet: nothing offered, never the regular release.
        let body = h.check().await;
        assert_eq!(body["channel"], "tested");
        assert_eq!(body["latest"], Value::Null);
        assert!(body["error"].as_str().unwrap().contains("tested.json"), "{body}");

        // Signed by another key.
        promote(&root.path().join("latest"), &pair(), TESTED);
        let body = h.check().await;
        assert_eq!(body["latest"], Value::Null);
        assert!(body["error"].as_str().unwrap().contains("does not verify"), "{body}");

        // It names a release whose files are not there.
        promote(&root.path().join("latest"), &release_key, "99.0.5");
        let body = h.check().await;
        assert_eq!(body["latest"], Value::Null);
        assert!(body["error"].as_str().unwrap().contains("v99.0.5"), "{body}");
    }
}
