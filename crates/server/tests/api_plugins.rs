//! Administration › Plugins (ADR-0103): publisher keys, inspecting and
//! installing signed files, updates, places, turning off and removing,
//! who may do it and the audit. Packages are signed with throwaway keys.
//! Ignored without `DATABASE_URL`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use monitorflow_common::redact::Secret;
use monitorflow_plugin::keys::PublicKey;
use monitorflow_plugin::package::{self, Package};
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair as _};
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

/// `(module (memory (export "memory") 1) (func (export "mf_run") (result i32) i32.const 0))`
const NOOP_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // magic, version
    0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7f, // type () -> i32
    0x03, 0x02, 0x01, 0x00, // one function
    0x05, 0x03, 0x01, 0x00, 0x01, // one memory of one page
    0x07, 0x13, 0x02, 0x06, b'm', b'e', b'm', b'o', b'r', b'y', 0x02, 0x00, 0x06, b'm', b'f', b'_',
    b'r', b'u', b'n', 0x00, 0x00, // exports
    0x0a, 0x06, 0x01, 0x04, 0x00, 0x41, 0x00, 0x0b, // i32.const 0
];

struct Signer {
    pair: Ed25519KeyPair,
    key: PublicKey,
}

impl Signer {
    fn new() -> Self {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
        let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let key = PublicKey::from_bytes(pair.public_key().as_ref()).unwrap();
        Self { pair, key }
    }

    /// A check plugin `web` of `version` calling `hosts`.
    fn package(&self, version: &str, hosts: &[&str]) -> Vec<u8> {
        let manifest = json!({
            "name": "web", "version": version, "kind": "check", "title": "Web address", "abi": 1,
            "capabilities": {"http": hosts},
            "settings": [{"key": "url", "label": "Address", "type": "url", "required": true}]
        });
        self.sign(manifest)
    }

    fn sign(&self, manifest: Value) -> Vec<u8> {
        Package::sign(&manifest.to_string(), NOOP_WASM, &self.pair)
            .unwrap()
            .to_bytes()
    }
}

async fn send(
    pool: &PgPool,
    method: &str,
    uri: &str,
    cookie: &str,
    body: Body,
) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header(CSRF_HEADER, "1")
        .header("content-type", "application/json")
        .header("cookie", cookie)
        .body(body)
        .unwrap();
    let state = ApiState::new(pool.clone(), Registry::default(), ApiSettings::default());
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

async fn json_call(
    pool: &PgPool,
    method: &str,
    uri: &str,
    cookie: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let body = body.map_or_else(Body::empty, |b| Body::from(b.to_string()));
    send(pool, method, uri, cookie, body).await
}

async fn file_call(pool: &PgPool, uri: &str, cookie: &str, file: &[u8]) -> (StatusCode, Value) {
    send(pool, "POST", uri, cookie, Body::from(file.to_vec())).await
}

async fn login(pool: &PgPool, name: &str, scope: GrantScope) -> String {
    users::create_user_with(
        pool,
        name,
        &Secret::new(PASSWORD),
        Role::Administrator,
        scope,
        "test",
    )
    .await
    .unwrap();
    let request = Request::builder()
        .method("POST")
        .uri("/api/auth/login")
        .header(CSRF_HEADER, "1")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({ "username": name, "password": PASSWORD }).to_string(),
        ))
        .unwrap();
    let state = ApiState::new(pool.clone(), Registry::default(), ApiSettings::default());
    let response = api::router(state).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap();
    cookie.split(';').next().unwrap().to_owned()
}

async fn customer_with_site(pool: &PgPool, name: &str) -> (Uuid, Uuid) {
    let customer: Uuid =
        sqlx::query_scalar("INSERT INTO customers (name) VALUES ($1) RETURNING id")
            .bind(name)
            .fetch_one(pool)
            .await
            .unwrap();
    let site: Uuid = sqlx::query_scalar(
        "INSERT INTO sites (customer_id, name) VALUES ($1, $2 || '-main') RETURNING id",
    )
    .bind(customer)
    .bind(name)
    .fetch_one(pool)
    .await
    .unwrap();
    (customer, site)
}

async fn actions(pool: &PgPool) -> Vec<String> {
    sqlx::query_scalar("SELECT action FROM audit_log WHERE action LIKE 'plugin%' ORDER BY id")
        .fetch_all(pool)
        .await
        .unwrap()
}

async fn revision(pool: &PgPool, site: Uuid) -> i64 {
    sqlx::query_scalar("SELECT sensor_revision FROM sites WHERE id = $1")
        .bind(site)
        .fetch_one(pool)
        .await
        .unwrap()
}

db_tests! {
    #[sqlx::test(migrations = "./migrations")]
    async fn a_global_administrator_installs_places_updates_and_removes_a_plugin(pool: PgPool) {
        let root = login(&pool, "root", GrantScope::Global).await;
        let (acme, acme_site) = customer_with_site(&pool, "acme").await;
        let (_, other_site) = customer_with_site(&pool, "other").await;
        let signer = Signer::new();
        let v1 = signer.package("1.0.0", &["{settings.url}"]);

        // An unknown signer is refused, with what to do.
        let (status, body) = file_call(&pool, "/api/plugins/inspect", &root, &v1).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"].as_str().unwrap().contains(&signer.key.fingerprint()), "{body}");

        // Trust the publisher.
        let add = json!({"name": "Acme tools", "public_key": signer.key.to_base64()});
        let (status, publisher) = json_call(&pool, "POST", "/api/plugins/publishers", &root, Some(add.clone())).await;
        assert_eq!(status, StatusCode::CREATED, "{publisher}");
        assert_eq!(publisher["fingerprint"], signer.key.fingerprint());
        let (status, _) = json_call(&pool, "POST", "/api/plugins/publishers", &root, Some(add)).await;
        assert_eq!(status, StatusCode::CONFLICT);
        let bad = json!({"name": "x", "public_key": "not a key"});
        let (status, _) = json_call(&pool, "POST", "/api/plugins/publishers", &root, Some(bad)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // Inspect shows it in words and stores nothing.
        let (status, shown) = file_call(&pool, "/api/plugins/inspect", &root, &v1).await;
        assert_eq!(status, StatusCode::OK, "{shown}");
        assert_eq!(shown["publisher"], "Acme tools");
        assert_eq!(shown["capabilities"], json!(["Runs on your servers as a check", "Connects to the address in its Address setting"]));
        assert_eq!(shown["update"], Value::Null);
        assert_eq!(shown["sha256"], package::hex(&package::sha256(&v1)));
        let (_, list) = json_call(&pool, "GET", "/api/plugins", &root, None).await;
        assert_eq!(list["plugins"], json!([]));

        // Install needs the checksum of what was shown.
        let (status, _) = file_call(&pool, &format!("/api/plugins?sha256={}", "00".repeat(32)), &root, &v1).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let before = revision(&pool, acme_site).await;
        let (status, plugin) = file_call(&pool, &format!("/api/plugins?sha256={}", shown["sha256"].as_str().unwrap()), &root, &v1).await;
        assert_eq!(status, StatusCode::CREATED, "{plugin}");
        assert_eq!(plugin["places"], json!([]));
        assert!(revision(&pool, acme_site).await > before);
        let id = plugin["id"].as_str().unwrap().to_owned();

        // Not newer, then an update that asks for another host.
        let (status, body) = file_call(&pool, "/api/plugins/inspect", &root, &signer.package("1.0.0", &[])).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"].as_str().unwrap().contains("must be newer"));
        let v2 = signer.package("1.1.0", &["{settings.url}", "api.example.com"]);
        let (_, shown) = file_call(&pool, "/api/plugins/inspect", &root, &v2).await;
        assert_eq!(shown["update"]["from_version"], "1.0.0");
        assert_eq!(shown["update"]["adds"], json!(["Connects to api.example.com"]));
        let (status, plugin) = file_call(&pool, &format!("/api/plugins?sha256={}", package::hex(&package::sha256(&v2))), &root, &v2).await;
        assert_eq!(status, StatusCode::OK, "{plugin}");
        assert_eq!(plugin["version"], "1.1.0");

        // Another signer cannot replace it.
        let thief = Signer::new();
        let add = json!({"name": "Someone else", "public_key": thief.key.to_pem()});
        assert_eq!(json_call(&pool, "POST", "/api/plugins/publishers", &root, Some(add)).await.0, StatusCode::CREATED);
        let (status, body) = file_call(&pool, "/api/plugins/inspect", &root, &thief.package("9.0.0", &[])).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"].as_str().unwrap().contains("another publisher"));

        // Places: acme's site; a customer and a site at once is refused.
        let places = json!({"places": [{"site_id": acme_site}]});
        let (status, plugin) = json_call(&pool, "PUT", &format!("/api/plugins/{id}/places"), &root, Some(places)).await;
        assert_eq!(status, StatusCode::OK, "{plugin}");
        assert_eq!(plugin["places"][0]["customer_id"], json!(acme));
        assert_eq!(plugin["places"][0]["site_name"], "acme-main");
        let both = json!({"places": [{"site_id": acme_site, "customer_id": acme}]});
        assert_eq!(json_call(&pool, "PUT", &format!("/api/plugins/{id}/places"), &root, Some(both)).await.0, StatusCode::BAD_REQUEST);
        let missing = json!({"places": [{"site_id": Uuid::new_v4()}]});
        assert_eq!(json_call(&pool, "PUT", &format!("/api/plugins/{id}/places"), &root, Some(missing)).await.0, StatusCode::BAD_REQUEST);

        // The usable list shows each administrator only their places.
        let acme_admin = login(&pool, "acme-admin", GrantScope::Customer(acme)).await;
        let (_, usable) = json_call(&pool, "GET", "/api/plugins/usable", &acme_admin, None).await;
        assert_eq!(usable["plugins"][0]["site_ids"], json!([acme_site]));
        let other_admin = login(&pool, "other-admin", GrantScope::Site(other_site)).await;
        let (_, usable) = json_call(&pool, "GET", "/api/plugins/usable", &other_admin, None).await;
        assert_eq!(usable["plugins"], json!([]));

        // Turned off, it is usable nowhere.
        let off = json!({"enabled": false});
        let (status, plugin) = json_call(&pool, "PUT", &format!("/api/plugins/{id}/enabled"), &root, Some(off)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(plugin["enabled"], false);
        let (_, usable) = json_call(&pool, "GET", "/api/plugins/usable", &acme_admin, None).await;
        assert_eq!(usable["plugins"], json!([]));

        // Customer administrators manage nothing here.
        for (method, uri) in [("GET", "/api/plugins"), ("GET", "/api/plugins/publishers")] {
            assert_eq!(json_call(&pool, method, uri, &acme_admin, None).await.0, StatusCode::FORBIDDEN);
        }
        assert_eq!(file_call(&pool, "/api/plugins/inspect", &acme_admin, &v2).await.0, StatusCode::FORBIDDEN);
        assert_eq!(json_call(&pool, "DELETE", &format!("/api/plugins/{id}"), &acme_admin, None).await.0, StatusCode::FORBIDDEN);

        // A check using it holds the removal; its publisher stays while it is installed.
        sqlx::query("INSERT INTO sensor_configs (site_id, kind, name, instance, interval_ms, params, enabled)
                     VALUES ($1, 'plugin', 'shop', '', 60000, '{\"plugin\": \"web\", \"url\": \"https://shop.example\"}', true)")
            .bind(acme_site)
            .execute(&pool)
            .await
            .unwrap();
        let (status, body) = json_call(&pool, "DELETE", &format!("/api/plugins/{id}"), &root, None).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(body["error"].as_str().unwrap().contains("1 check(s)"));
        let publisher_id = publisher["id"].as_str().unwrap();
        let (status, body) = json_call(&pool, "DELETE", &format!("/api/plugins/publishers/{publisher_id}"), &root, None).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(body["error"].as_str().unwrap().contains("web"));
        sqlx::query("DELETE FROM sensor_configs").execute(&pool).await.unwrap();
        assert_eq!(json_call(&pool, "DELETE", &format!("/api/plugins/{id}"), &root, None).await.0, StatusCode::NO_CONTENT);
        assert_eq!(json_call(&pool, "DELETE", &format!("/api/plugins/publishers/{publisher_id}"), &root, None).await.0, StatusCode::NO_CONTENT);

        assert_eq!(
            actions(&pool).await,
            [
                "plugin_publisher.add", "plugin.install", "plugin.update", "plugin_publisher.add",
                "plugin.places", "plugin.disable", "plugin.remove", "plugin_publisher.remove",
            ]
        );
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn a_channel_plugin_is_a_channel_where_it_is_turned_on(pool: PgPool) {
        let root = login(&pool, "root", GrantScope::Global).await;
        let (acme, _) = customer_with_site(&pool, "acme").await;
        let acme_admin = login(&pool, "acme-admin", GrantScope::Customer(acme)).await;
        let signer = Signer::new();
        let add = json!({"name": "Pager Inc", "public_key": signer.key.to_base64()});
        assert_eq!(json_call(&pool, "POST", "/api/plugins/publishers", &root, Some(add)).await.0, StatusCode::CREATED);
        let file = signer.sign(json!({
            "name": "pager", "version": "1.0.0", "kind": "channel", "title": "Pager", "abi": 1,
            "settings": [
                {"key": "room", "label": "Room", "type": "text", "required": true},
                {"key": "token", "label": "Token", "type": "secret", "required": true}
            ]
        }));
        let (status, plugin) = file_call(&pool, &format!("/api/plugins?sha256={}", package::hex(&package::sha256(&file))), &root, &file).await;
        assert_eq!(status, StatusCode::CREATED, "{plugin}");
        let id = plugin["id"].as_str().unwrap().to_owned();

        let channel = |values: Value, secret: Option<&str>, customer: Option<Uuid>| {
            json!({
                "kind": "plugin",
                "settings": {"plugin": "pager", "name": "ops", "values": values},
                "secret": secret,
                "customer_id": customer,
            })
        };
        let channels = "/api/notifications/channels";
        // Nowhere yet.
        let ask = channel(json!({"room": "ops"}), Some(r#"{"token": "t-secret-1"}"#), Some(acme));
        let (status, body) = json_call(&pool, "POST", channels, &acme_admin, Some(ask.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"].as_str().unwrap().contains("not turned on"), "{body}");

        let places = json!({"places": [{"customer_id": acme}]});
        assert_eq!(json_call(&pool, "PUT", &format!("/api/plugins/{id}/places"), &root, Some(places)).await.0, StatusCode::OK);
        let (status, created) = json_call(&pool, "POST", channels, &acme_admin, Some(ask)).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        assert_eq!(created["kind"], "plugin");
        assert_eq!(created["has_secret"], true);
        assert_eq!(created["settings"]["values"], json!({"room": "ops"}));
        assert!(!created.to_string().contains("t-secret-1"));

        // Settings are checked against the manifest.
        for (values, secret, expected) in [
            (json!({"room": "ops"}), None, "Token is required"),
            (json!({"room": "ops", "token": "x"}), Some(r#"{"token": "x"}"#), "is a secret setting"),
            (json!({"room": "ops"}), Some(r#"{"room": "x", "token": "y"}"#), "not a secret setting"),
            (json!({}), Some(r#"{"token": "x"}"#), "Room is required"),
        ] {
            let (status, body) = json_call(&pool, "POST", channels, &acme_admin, Some(channel(values, secret, Some(acme)))).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{expected}");
            assert!(body["error"].as_str().unwrap().contains(expected), "{expected}: {body}");
        }
        // Not for everything: it is allowed for acme only.
        let everywhere = channel(json!({"room": "ops"}), Some(r#"{"token": "t"}"#), None);
        let (status, _) = json_call(&pool, "POST", channels, &root, Some(everywhere)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // Its test runs the plugin.
        let channel_id = created["id"].as_str().unwrap();
        let test = format!("{channels}/{channel_id}/test");
        let (status, result) = json_call(&pool, "POST", &test, &acme_admin, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(result["ok"], true, "{result}");

        // Another room is another target: the secret is typed again.
        let moved = channel(json!({"room": "elsewhere"}), None, Some(acme));
        let (status, body) = json_call(&pool, "PUT", &format!("{channels}/{channel_id}"), &acme_admin, Some(moved)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"].as_str().unwrap().contains("again"), "{body}");
        let renamed = json!({"kind": "plugin", "settings": {"plugin": "pager", "name": "on call", "values": {"room": "ops"}}, "customer_id": acme});
        assert_eq!(json_call(&pool, "PUT", &format!("{channels}/{channel_id}"), &acme_admin, Some(renamed)).await.0, StatusCode::OK);

        // Turned off, the channel says so.
        let off = json!({"enabled": false});
        assert_eq!(json_call(&pool, "PUT", &format!("/api/plugins/{id}/enabled"), &root, Some(off)).await.0, StatusCode::OK);
        let (_, result) = json_call(&pool, "POST", &test, &acme_admin, None).await;
        assert_eq!(result["ok"], false);
        assert!(result["error"].as_str().unwrap().contains("turned off"), "{result}");

        // In use, it stays installed.
        assert_eq!(json_call(&pool, "DELETE", &format!("/api/plugins/{id}"), &root, None).await.0, StatusCode::CONFLICT);
        let audit: Vec<Value> = sqlx::query_scalar("SELECT details FROM audit_log WHERE action LIKE 'notification.%'")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert!(!audit.is_empty() && !audit.iter().any(|d| d.to_string().contains("t-secret-1")));
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn a_check_plugin_is_a_check_where_it_is_turned_on(pool: PgPool) {
        let root = login(&pool, "root", GrantScope::Global).await;
        let (acme, acme_site) = customer_with_site(&pool, "acme").await;
        let (_, other_site) = customer_with_site(&pool, "other").await;
        let acme_admin = login(&pool, "acme-admin", GrantScope::Customer(acme)).await;
        let signer = Signer::new();
        let add = json!({"name": "Acme tools", "public_key": signer.key.to_base64()});
        json_call(&pool, "POST", "/api/plugins/publishers", &root, Some(add)).await;
        let file = signer.package("1.0.0", &["{settings.url}"]);
        let (_, plugin) = file_call(&pool, &format!("/api/plugins?sha256={}", package::hex(&package::sha256(&file))), &root, &file).await;
        let id = plugin["id"].as_str().unwrap().to_owned();
        let row = |params: Value| json!({"kind": "plugin", "name": "shop", "interval_ms": 60000, "params": params});
        let rows = format!("/api/sites/{acme_site}/sensor_configs");

        let good = row(json!({"plugin": "web", "url": "https://shop.example"}));
        let (status, body) = json_call(&pool, "POST", &rows, &acme_admin, Some(good.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"].as_str().unwrap().contains("not a check plugin turned on"), "{body}");

        let places = json!({"places": [{"site_id": acme_site}]});
        json_call(&pool, "PUT", &format!("/api/plugins/{id}/places"), &root, Some(places)).await;
        let (status, created) = json_call(&pool, "POST", &rows, &acme_admin, Some(good.clone())).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        for (params, expected) in [
            (json!({"plugin": "web"}), "Address is required"),
            (json!({"plugin": "web", "url": "ftp://x"}), "Address"),
            (json!({"plugin": "web", "url": "https://x.example", "other": 1}), "not a setting"),
            (json!({"url": "https://x.example"}), "must name a check plugin"),
        ] {
            let mut ask = row(params);
            ask["name"] = json!("shop_two");
            let (status, body) = json_call(&pool, "POST", &rows, &acme_admin, Some(ask)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{expected}");
            assert!(body["error"].as_str().unwrap().contains(expected), "{expected}: {body}");
        }
        // Another site of another customer is not a place of it.
        let other = format!("/api/sites/{other_site}/sensor_configs");
        let (status, _) = json_call(&pool, "POST", &other, &root, Some(good)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        // A disabled row only removes a check, so it needs no plugin.
        let mut off = row(json!({"plugin": "gone"}));
        off["enabled"] = json!(false);
        off["name"] = json!("old");
        assert_eq!(json_call(&pool, "POST", &rows, &acme_admin, Some(off)).await.0, StatusCode::CREATED);
        let update = format!("/api/sensor_configs/{}", created["id"].as_str().unwrap());
        let changed = row(json!({"plugin": "web", "url": "https://other.example"}));
        assert_eq!(json_call(&pool, "PUT", &update, &acme_admin, Some(changed)).await.0, StatusCode::OK);
    }
}
