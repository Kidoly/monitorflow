//! The default agent install source (ADR-0054) and the public address of
//! install commands: who reads and changes them, the audit, the
//! configuration's precedence, and the `install` and `address` objects of
//! a new token. Ignored without `DATABASE_URL`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use monitorflow_common::config::AgentInstallSource;
use monitorflow_common::redact::Secret;
use monitorflow_common::release::{self, PublicKey};
use monitorflow_server::agent_releases::{ReleaseCache, INSTALLER};
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{self, ApiSettings, ApiState, EnrollmentSettings, Role};
use monitorflow_server::grpc::Registry;
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery";
const SETTING: &str = "/api/settings/agent-install";
const ADDRESS: &str = "/api/settings/public-address";

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

async fn call(
    pool: &PgPool,
    settings: ApiSettings,
    method: &str,
    uri: &str,
    cookie: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value, Option<String>) {
    let state = ApiState::new(pool.clone(), Registry::default(), settings).with_enrollment(
        EnrollmentSettings {
            ca_fingerprint: [7; 32],
            server_names: vec!["mf.example".into()],
            grpc_port: 7443,
            enroll_port: 7445,
        },
    );
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
    let response = api::router(state).oneshot(request).await.unwrap();
    let status = response.status();
    let cookie = response
        .headers()
        .get("set-cookie")
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned());
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body, cookie)
}

async fn login(pool: &PgPool, name: &str, role: Role, scope: GrantScope) -> String {
    users::create_user_with(pool, name, &Secret::new(PASSWORD), role, scope, "test")
        .await
        .unwrap();
    let body = json!({ "username": name, "password": PASSWORD });
    let (status, _, cookie) = call(
        pool,
        ApiSettings::default(),
        "POST",
        "/api/auth/login",
        None,
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    cookie.unwrap()
}

async fn customer_site(pool: &PgPool) -> (Uuid, Uuid) {
    let customer: Uuid =
        sqlx::query_scalar("INSERT INTO customers (name) VALUES ('acme') RETURNING id")
            .fetch_one(pool)
            .await
            .unwrap();
    let site = sqlx::query_scalar(
        "INSERT INTO sites (name, customer_id) VALUES ('paris', $1) RETURNING id",
    )
    .bind(customer)
    .fetch_one(pool)
    .await
    .unwrap();
    (customer, site)
}

fn hex(bytes: &[u8]) -> String {
    release::sha256(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A signed release of `version` with a binary and the installer, cached
/// under `mirror`; returns the key.
fn cached_release(mirror: &std::path::Path, version: &str) -> PublicKey {
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
    let mut der = vec![
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ];
    der.extend_from_slice(pair.public_key().as_ref());
    let dir = mirror.join("agent").join(version);
    std::fs::create_dir_all(&dir).unwrap();
    let mut sums = String::new();
    for (name, bytes) in [
        ("VERSION", format!("{version}\n")),
        ("monitorflow-agent-linux-x86_64", "agent".into()),
        (INSTALLER, "#!/bin/sh\n".into()),
    ] {
        std::fs::write(dir.join(name), &bytes).unwrap();
        sums.push_str(&format!("{}  {name}\n", hex(bytes.as_bytes())));
    }
    std::fs::write(dir.join("SHA256SUMS"), &sums).unwrap();
    std::fs::write(
        dir.join("SHA256SUMS.sig"),
        pair.sign(sums.as_bytes()).as_ref(),
    )
    .unwrap();
    PublicKey::from_spki(&der).unwrap()
}

async fn audit_details(pool: &PgPool) -> Vec<Value> {
    sqlx::query_scalar(
        "SELECT details FROM audit_log WHERE action = 'settings.agent_install' ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

db_tests! {
    #[sqlx::test]
    async fn global_administrators_change_the_default_and_it_is_audited(pool: PgPool) {
        let (customer, site) = customer_site(&pool).await;
        let global = login(&pool, "root", Role::Administrator, GrantScope::Global).await;
        let admin = login(&pool, "boss", Role::Administrator, GrantScope::Customer(customer)).await;
        let site_admin = login(&pool, "local", Role::Administrator, GrantScope::Site(site)).await;
        let get = |cookie: String| {
            let pool = pool.clone();
            async move { call(&pool, ApiSettings::default(), "GET", SETTING, Some(&cookie), None).await }
        };
        let put = |cookie: String, body: Value| {
            let pool = pool.clone();
            async move { call(&pool, ApiSettings::default(), "PUT", SETTING, Some(&cookie), Some(body)).await }
        };

        // Readable by whoever creates tokens.
        let (status, view, _) = get(admin.clone()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            view,
            json!({
                "source": "github",
                "custom_url": null,
                "source_from_config": false,
                "custom_url_from_config": false,
                "github_url": "https://github.com/Kidoly/monitorflow/releases/latest/download",
                "central": null
            })
        );
        assert_eq!(get(site_admin).await.0, StatusCode::FORBIDDEN);
        assert_eq!(call(&pool, ApiSettings::default(), "GET", SETTING, None, None).await.0, StatusCode::UNAUTHORIZED);

        // Changed by global administrators only.
        let central = json!({ "source": "central" });
        assert_eq!(put(admin.clone(), central.clone()).await.0, StatusCode::FORBIDDEN);
        for bad in [
            json!({ "source": "custom" }),
            json!({ "source": "custom", "custom_url": "http://files.example/mf" }),
            json!({ "source": "custom", "custom_url": "https://user@files.example" }),
            json!({ "source": "custom", "custom_url": "https://files.example/$(id)" }),
            json!({ "source": "relay" }),
            json!({ "source": "github", "extra": 1 }),
        ] {
            let (status, body, _) = put(global.clone(), bad.clone()).await;
            assert!(status == StatusCode::BAD_REQUEST, "{bad}: {status} {body}");
        }
        assert!(audit_details(&pool).await.is_empty());

        let (status, view, _) = put(global.clone(), json!({ "source": "custom", "custom_url": "https://files.example/mf/" })).await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!(view["source"], "custom");
        assert_eq!(view["custom_url"], "https://files.example/mf");
        let (_, view, _) = put(global.clone(), central).await;
        assert_eq!((view["source"].as_str(), &view["custom_url"]), (Some("central"), &Value::Null));
        assert_eq!(get(admin).await.1["source"], "central");
        assert_eq!(
            audit_details(&pool).await,
            [
                json!({ "old": { "source": null, "custom_url": null },
                        "new": { "source": "custom", "custom_url": "https://files.example/mf" } }),
                json!({ "old": { "source": "custom", "custom_url": "https://files.example/mf" },
                        "new": { "source": "central", "custom_url": null } }),
            ]
        );
    }

    #[sqlx::test]
    async fn the_configuration_wins(pool: PgPool) {
        let global = login(&pool, "root", Role::Administrator, GrantScope::Global).await;
        let mut settings = ApiSettings::default();
        settings.agent_install.source = Some(AgentInstallSource::Custom);
        settings.agent_install.custom_url = Some("https://config.example/mf".into());
        let put = |body: Value| {
            let (pool, settings, global) = (pool.clone(), settings.clone(), global.clone());
            async move { call(&pool, settings, "PUT", SETTING, Some(&global), Some(body)).await }
        };
        let (status, body, _) = put(json!({ "source": "github" })).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body["error"].as_str().unwrap().contains("agent_install_source"));
        let (status, _, _) = put(json!({ "source": "custom", "custom_url": "https://other.example" })).await;
        assert_eq!(status, StatusCode::CONFLICT);
        // Repeating the configuration is fine.
        let (status, view, _) = put(json!({ "source": "custom" })).await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!(view["custom_url"], "https://config.example/mf");
        assert_eq!(view["source_from_config"], true);
        assert_eq!(view["custom_url_from_config"], true);
    }

    #[sqlx::test]
    async fn new_tokens_carry_every_source_and_the_central_release(pool: PgPool) {
        let (customer, site) = customer_site(&pool).await;
        let admin = login(&pool, "boss", Role::Administrator, GrantScope::Customer(customer)).await;
        let mirror = tempfile::TempDir::new().unwrap();
        let key = cached_release(mirror.path(), "0.3.0");
        let mut settings = ApiSettings::default();
        settings.agent_releases.cache = ReleaseCache::in_mirror_dir(mirror.path());
        settings.agent_releases.keys = vec![key];
        let request = json!({ "role": "agent", "peer_id": "web-01", "site_id": site });
        let (status, created, _) =
            call(&pool, settings.clone(), "POST", "/api/tokens", Some(&admin), Some(request)).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let install = &created["install"];
        assert_eq!(install["source"], "github");
        let sums = std::fs::read(mirror.path().join("agent/0.3.0/SHA256SUMS")).unwrap();
        assert_eq!(
            install["central"],
            json!({
                "version": "0.3.0",
                "path": "/agent/0.3.0",
                "sums_sha256": hex(&sums),
                "installer_sha256": hex(b"#!/bin/sh\n"),
                "msi_sha256": null
            })
        );
        // A cache whose signature no built-in key verifies offers nothing.
        settings.agent_releases.keys = Vec::new();
        let (_, view, _) = call(&pool, settings, "GET", SETTING, Some(&admin), None).await;
        assert_eq!(view["central"], Value::Null);
    }

    #[sqlx::test]
    async fn global_administrators_change_the_public_address(pool: PgPool) {
        let (customer, site) = customer_site(&pool).await;
        let global = login(&pool, "root", Role::Administrator, GrantScope::Global).await;
        let admin = login(&pool, "boss", Role::Administrator, GrantScope::Customer(customer)).await;
        let site_admin = login(&pool, "local", Role::Administrator, GrantScope::Site(site)).await;
        let call = |method: &'static str, uri: &'static str, cookie: String, body: Option<Value>| {
            let pool = pool.clone();
            async move { call(&pool, ApiSettings::default(), method, uri, Some(&cookie), body).await }
        };
        let audit = || async {
            sqlx::query_scalar::<_, Value>(
                "SELECT details FROM audit_log WHERE action = 'settings.public_address' ORDER BY id",
            )
            .fetch_all(&pool)
            .await
            .unwrap()
        };

        // The first server name by default, readable by whoever creates tokens.
        let (status, view, _) = call("GET", ADDRESS, admin.clone(), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            view,
            json!({
                "address": "mf.example",
                "default": "mf.example",
                "names": ["mf.example"],
                "certified": true,
                "upstream_url": "https://mf.example:7443",
                "enroll_url": "https://mf.example:7445"
            })
        );
        assert_eq!(call("GET", ADDRESS, site_admin, None).await.0, StatusCode::FORBIDDEN);

        // Changed by global administrators only, to a host without a port.
        let body = json!({ "address": "10.0.0.2" });
        assert_eq!(call("PUT", ADDRESS, admin.clone(), Some(body.clone())).await.0, StatusCode::FORBIDDEN);
        for bad in [json!({ "address": "mf.example:7443" }), json!({ "address": "a b" }), json!({ "host": "x" })] {
            let (status, body, _) = call("PUT", ADDRESS, global.clone(), Some(bad.clone())).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
        }
        let (status, view, _) = call("PUT", ADDRESS, global.clone(), Some(body)).await;
        assert_eq!(status, StatusCode::OK, "{view}");
        // Accepted, but the certificate does not cover it.
        assert_eq!(view["address"], "10.0.0.2");
        assert_eq!(view["certified"], false);
        assert_eq!(view["upstream_url"], "https://10.0.0.2:7443");

        // New tokens use it.
        let request = json!({ "role": "agent", "peer_id": "web-01", "site_id": site });
        let (status, created, _) = call("POST", "/api/tokens", admin.clone(), Some(request)).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        assert_eq!(created["upstream_url"], "https://10.0.0.2:7443");
        assert_eq!(created["enroll_url"], "https://10.0.0.2:7445");
        assert_eq!(created["address"], view);

        // Back to the default, stored as nothing.
        let (_, view, _) = call("PUT", ADDRESS, global, Some(json!({ "address": "MF.example" }))).await;
        assert_eq!((view["address"].as_str(), &view["certified"]), (Some("mf.example"), &json!(true)));
        let stored: Option<String> = sqlx::query_scalar("SELECT public_address FROM server_settings")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(stored, None);
        assert_eq!(
            audit().await,
            [json!({ "old": null, "new": "10.0.0.2" }), json!({ "old": "10.0.0.2", "new": null })]
        );
    }
}
