//! Devices and probe checks through the API against a real PostgreSQL
//! (ADR-0148). Ignored without `DATABASE_URL`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users;
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
use monitorflow_server::probes::ProbeStore;
use monitorflow_server::sensor_config::SensorConfigStore;
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

fn app(pool: &PgPool) -> Router {
    api::router(ApiState::new(
        pool.clone(),
        Registry::default(),
        ApiSettings::default(),
    ))
}

async fn send(pool: &PgPool, request: Request<Body>) -> (StatusCode, Value, Option<String>) {
    let response = app(pool).oneshot(request).await.unwrap();
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

async fn session(pool: &PgPool, username: &str, role: Role, site: Option<Uuid>) -> String {
    let id = users::create_user(pool, username, &Secret::new(PASSWORD), role, "test")
        .await
        .unwrap();
    sqlx::query("UPDATE user_roles SET site_id = $2 WHERE user_id = $1")
        .bind(id)
        .bind(site)
        .execute(pool)
        .await
        .unwrap();
    let body = json!({ "username": username, "password": PASSWORD }).to_string();
    let request = Request::post("/api/auth/login")
        .header("content-type", "application/json")
        .header(CSRF_HEADER, "1")
        .body(Body::from(body))
        .unwrap();
    let (status, _, cookie) = send(pool, request).await;
    assert_eq!(status, StatusCode::OK);
    cookie.unwrap()
}

async fn call(
    pool: &PgPool,
    cookie: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("cookie", cookie)
        .header(CSRF_HEADER, "1")
        .header("content-type", "application/json")
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let (status, body, _) = send(pool, request).await;
    (status, body)
}

async fn site(pool: &PgPool, name: &str) -> Uuid {
    sqlx::query_scalar("INSERT INTO sites (name) VALUES ($1) RETURNING id")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn relay(pool: &PgPool, site: Uuid, peer: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO relays (peer_id, site_id, name) VALUES ($1, $2, $1) RETURNING id",
    )
    .bind(peer)
    .bind(site)
    .fetch_one(pool)
    .await
    .unwrap()
}

db_tests! {
    #[sqlx::test]
    async fn devices_are_added_with_a_ping_from_the_default_probe(pool: PgPool) {
        let paris = site(&pool, "paris").await;
        let lyon = site(&pool, "lyon").await;
        let operator = session(&pool, "op", Role::Operator, Some(paris)).await;
        let viewer = session(&pool, "viewer", Role::Viewer, Some(paris)).await;

        // Without a relay, the central server pings it.
        let (status, nas) = call(&pool, &operator, "POST", "/api/devices",
            Some(json!({"site_id": paris, "name": "NAS (backup)", "address": " 192.0.2.5 "}))).await;
        assert_eq!(status, StatusCode::CREATED, "{nas}");
        assert_eq!(nas["kind"], "device");
        assert_eq!(nas["status"], "device");
        assert_eq!(nas["state"], "ok");
        assert_eq!(nas["peer_id"], "nas-backup");
        assert_eq!(nas["hostname"], "NAS (backup)");
        assert_eq!(nas["address"], "192.0.2.5");
        assert_eq!(nas["os_family"], "device");
        assert_eq!(nas["package_updates"], false);
        assert_eq!(nas["awaiting_agent"], false);
        let id = nas["id"].as_str().unwrap().to_owned();
        let (_, rows) = call(&pool, &viewer, "GET", &format!("/api/servers/{id}/sensor_configs"), None).await;
        let ping = &rows["entries"][0];
        assert_eq!((ping["kind"].as_str(), ping["probe"].as_str(), ping["instance"].as_str()),
                   (Some("ping"), Some("central"), Some("central")));
        let (_, thresholds) = call(&pool, &viewer, "GET", &format!("/api/servers/{id}/thresholds"), None).await;
        assert_eq!(thresholds["entries"].as_array().unwrap().len(), 2, "{thresholds}");
        // What the device runs: its probe checks only, no built-in ones.
        let (_, effective) = call(&pool, &viewer, "GET", &format!("/api/servers/{id}/sensors"), None).await;
        let sensors = effective["sensors"].as_array().unwrap();
        assert_eq!(sensors.len(), 1, "{effective}");
        assert_eq!(sensors[0]["probe"], "central");
        assert!(SensorConfigStore::new(pool.clone()).load("nas-backup").await.unwrap().unwrap().sensors.is_empty());
        let sets = ProbeStore::new(pool.clone()).load().await.unwrap();
        assert_eq!(sets["central"].checks[0].config.params["host"], "192.0.2.5");

        // With a relay on the site, it is the default.
        let relay_id = relay(&pool, paris, "relay-paris").await;
        let (_, probes) = call(&pool, &viewer, "GET", &format!("/api/sites/{paris}/probes"), None).await;
        assert_eq!(probes["probes"].as_array().unwrap().len(), 2, "{probes}");
        assert_eq!(probes["probes"][0]["default"], false);
        assert_eq!(probes["probes"][1]["relay_id"], json!(relay_id));
        assert_eq!(probes["probes"][1]["default"], true);
        assert_eq!(probes["probes"][1]["online"], false);
        let (status, switch) = call(&pool, &operator, "POST", "/api/devices",
            Some(json!({"site_id": paris, "name": "NAS backup", "address": "switch.lan"}))).await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(switch["peer_id"], "nas-backup-2", "IDs stay unique");
        let sets = ProbeStore::new(pool.clone()).load().await.unwrap();
        assert_eq!(sets["relay-paris"].checks[0].config.instance, "relay-paris");

        // Who may: a viewer reads, a relay of another site is refused, and
        // another site is not found.
        let body = json!({"site_id": paris, "name": "x", "address": "x"});
        assert_eq!(call(&pool, &viewer, "POST", "/api/devices", Some(body)).await.0, StatusCode::FORBIDDEN);
        let other = relay(&pool, lyon, "relay-lyon").await;
        let (status, error) = call(&pool, &operator, "POST", "/api/devices",
            Some(json!({"site_id": paris, "name": "x", "address": "x", "probe": other}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
        let (status, _) = call(&pool, &operator, "POST", "/api/devices",
            Some(json!({"site_id": lyon, "name": "x", "address": "x"}))).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        for bad in [json!({"site_id": paris, "name": "", "address": "x"}),
                    json!({"site_id": paris, "name": "x", "address": "http://x"})] {
            assert_eq!(call(&pool, &operator, "POST", "/api/devices", Some(bad)).await.0, StatusCode::BAD_REQUEST);
        }

        // Renaming and moving it.
        let (status, moved) = call(&pool, &operator, "PUT", &format!("/api/devices/{id}"),
            Some(json!({"address": "192.0.2.6"}))).await;
        assert_eq!(status, StatusCode::OK, "{moved}");
        assert_eq!((moved["address"].as_str(), moved["hostname"].as_str()), (Some("192.0.2.6"), Some("NAS (backup)")));
        let audit: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action LIKE 'device.%'")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(audit, 3);
    }

    #[sqlx::test]
    async fn probe_checks_run_where_they_can(pool: PgPool) {
        let paris = site(&pool, "paris").await;
        let relay_id = relay(&pool, paris, "relay-paris").await;
        let operator = session(&pool, "op", Role::Operator, Some(paris)).await;
        let server: Uuid = sqlx::query_scalar(
            "INSERT INTO servers (peer_id, site_id, hostname, os_family) VALUES ('web-01', $1, 'web-01', 'debian') RETURNING id",
        )
        .bind(paris)
        .fetch_one(&pool)
        .await
        .unwrap();
        let post = |body: Value| {
            let (pool, operator) = (pool.clone(), operator.clone());
            async move { call(&pool, &operator, "POST", &format!("/api/servers/{server}/sensor_configs"), Some(body)).await }
        };
        // An agent never pings nor checks UDP.
        let (status, error) = post(json!({"kind": "ping", "interval_ms": 60000, "params": {"host": "192.0.2.1"}})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(error["error"].as_str().unwrap().contains("set `probe`"), "{error}");
        let (status, _) = post(json!({"kind": "port", "name": "dns", "interval_ms": 60000,
            "params": {"port": 53, "protocol": "udp", "host": "192.0.2.1"}})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        // From a probe, the host is named and the instance is the probe.
        let (status, _) = post(json!({"kind": "port", "name": "dns", "interval_ms": 60000,
            "params": {"port": 53, "protocol": "udp"}, "probe": "central"})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "no host");
        let (status, row) = post(json!({"kind": "port", "name": "dns", "interval_ms": 60000,
            "params": {"port": 53, "protocol": "udp", "host": "198.51.100.7"}, "probe": relay_id})).await;
        assert_eq!(status, StatusCode::CREATED, "{row}");
        assert_eq!((row["probe"].as_str(), row["instance"].as_str()), (Some("relay"), Some("relay-paris")));
        assert_eq!(row["probe_relay_id"], json!(relay_id));
        let (status, _) = post(json!({"kind": "ping", "interval_ms": 60000, "params": {"host": "198.51.100.7"},
            "probe": "central"})).await;
        assert_eq!(status, StatusCode::CREATED);
        let (status, error) = post(json!({"kind": "cpu", "interval_ms": 60000, "probe": "central"})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
        // The agent's configuration leaves them out; the effective list
        // shows them after the agent's.
        let agent = SensorConfigStore::new(pool.clone()).load("web-01").await.unwrap().unwrap();
        assert_eq!(agent.sensors.len(), 4, "the built-in checks only");
        let (_, effective) = call(&pool, &operator, "GET", &format!("/api/servers/{server}/sensors"), None).await;
        let names: Vec<_> = effective["sensors"].as_array().unwrap().iter()
            .map(|s| (s["name"].as_str().unwrap().to_owned(), s["probe"].clone())).collect();
        assert_eq!(names.len(), 6, "{names:?}");
        assert_eq!(names[4], ("dns".to_string(), json!("relay")));
        assert_eq!(names[5], ("ping".to_string(), json!("central")));
        // A site's row runs on agents.
        let (status, error) = call(&pool, &operator, "POST", &format!("/api/sites/{paris}/sensor_configs"),
            Some(json!({"kind": "ping", "interval_ms": 60000, "params": {"host": "x"}, "probe": "central"}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(error["error"].as_str().unwrap().contains("a server's own check"), "{error}");
        // Changing the probe of a row moves it.
        let id = row["id"].as_str().unwrap();
        let (status, moved) = call(&pool, &operator, "PUT", &format!("/api/sensor_configs/{id}"),
            Some(json!({"kind": "port", "name": "dns", "instance": "central", "interval_ms": 60000,
                        "params": {"port": 53, "protocol": "udp", "host": "198.51.100.7"}, "probe": "central"}))).await;
        assert_eq!(status, StatusCode::OK, "{moved}");
        assert_eq!(moved["probe"], "central");
        assert_eq!(moved["probe_relay_id"], Value::Null);
    }

    /// SNMP devices (ADR-0149): a template gives checks and thresholds,
    /// credentials are sealed and never come back, and probes get them.
    #[sqlx::test]
    async fn snmp_devices_from_a_template(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let paris = site(&pool, "paris").await;
        let operator = session(&pool, "op", Role::Operator, Some(paris)).await;
        let viewer = session(&pool, "viewer", Role::Viewer, Some(paris)).await;
        let (_, templates) = call(&pool, &viewer, "GET", "/api/devices/templates", None).await;
        let keys: Vec<_> = templates["templates"].as_array().unwrap().iter().map(|t| t["key"].as_str().unwrap().to_owned()).collect();
        assert_eq!(keys, ["switch", "router", "firewall", "printer", "ups", "nas", "generic"]);

        let body = json!({"site_id": paris, "name": "UPS room", "address": "192.0.2.30", "template": "ups",
                          "snmp": {"version": "v3", "user": "monitor", "auth_protocol": "sha256",
                                   "auth_password": "secret-auth-1", "priv_protocol": "aes128", "priv_password": "secret-priv-1"}});
        let (status, ups) = call(&pool, &operator, "POST", "/api/devices", Some(body)).await;
        assert_eq!(status, StatusCode::CREATED, "{ups}");
        assert!(!ups.to_string().contains("secret-"), "{ups}");
        let id = ups["id"].as_str().unwrap().to_owned();
        let (_, rows) = call(&pool, &viewer, "GET", &format!("/api/servers/{id}/sensor_configs"), None).await;
        let mut names: Vec<_> = rows["entries"].as_array().unwrap().iter()
            .map(|r| (r["name"].as_str().unwrap().to_owned(), r["params"]["what"].as_str().unwrap_or("").to_owned()))
            .collect();
        names.sort();
        assert_eq!(names, [("ping".into(), String::new()), ("snmp".into(), "system".into()), ("snmp_ups".into(), "ups".into())]);
        assert!(!rows.to_string().contains("secret-"));
        let (_, thresholds) = call(&pool, &viewer, "GET", &format!("/api/servers/{id}/thresholds"), None).await;
        let metrics: Vec<_> = thresholds["entries"].as_array().unwrap().iter().map(|t| t["metric"].as_str().unwrap().to_owned()).collect();
        assert!(metrics.contains(&"monitorflow_snmp_ups_on_battery".to_string()), "{metrics:?}");
        assert!(metrics.contains(&"monitorflow_snmp_up".to_string()));

        // What the API shows: never a secret.
        let (status, view) = call(&pool, &viewer, "GET", &format!("/api/devices/{id}/snmp"), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!((view["user"].as_str(), view["auth_password_set"].as_bool()), (Some("monitor"), Some(true)));
        assert!(!view.to_string().contains("secret-"), "{view}");
        let stored: String = sqlx::query_scalar("SELECT secret FROM snmp_credentials").fetch_one(&pool).await.unwrap();
        assert!(stored.starts_with("mfs1:") && !stored.contains("secret-"), "sealed");
        let audit: Vec<String> = sqlx::query_scalar("SELECT details::text FROM audit_log").fetch_all(&pool).await.unwrap();
        assert!(audit.iter().all(|d| !d.contains("secret-")), "{audit:?}");

        // What the probe gets: the opened credentials with each SNMP check.
        let sets = ProbeStore::new(pool.clone()).load().await.unwrap();
        let snmp_check = sets["central"].checks.iter().find(|c| c.kind == "snmp").unwrap();
        assert_eq!(snmp_check.config.params["auth_password"], "secret-auth-1");
        assert_eq!(snmp_check.config.params["host"], "192.0.2.30");

        // A change of port keeps the passwords; a viewer cannot change.
        let (status, view) = call(&pool, &operator, "PUT", &format!("/api/devices/{id}/snmp"),
            Some(json!({"version": "v3", "user": "monitor", "port": 1161, "auth_protocol": "sha256", "priv_protocol": "aes128"}))).await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!((view["port"].as_u64(), view["priv_password_set"].as_bool()), (Some(1161), Some(true)));
        let sets = ProbeStore::new(pool.clone()).load().await.unwrap();
        let snmp_check = sets["central"].checks.iter().find(|c| c.kind == "snmp").unwrap();
        assert_eq!((snmp_check.config.params["port"].as_str(), snmp_check.config.params["priv_password"].as_str()), ("1161", "secret-priv-1"));
        assert_eq!(call(&pool, &viewer, "PUT", &format!("/api/devices/{id}/snmp"), Some(json!({"version": "v2c", "community": "x"}))).await.0, StatusCode::FORBIDDEN);

        // Rows never hold credentials, and SNMP is for devices.
        let (status, error) = call(&pool, &operator, "POST", &format!("/api/servers/{id}/sensor_configs"),
            Some(json!({"kind": "snmp", "name": "snmp_if", "interval_ms": 60000, "params": {"what": "interfaces", "community": "x"}}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(error["error"].as_str().unwrap().contains("/snmp"), "{error}");
        let (status, _) = call(&pool, &operator, "POST", &format!("/api/servers/{id}/sensor_configs"),
            Some(json!({"kind": "snmp", "name": "snmp_if", "interval_ms": 60000, "params": {"what": "interfaces"}}))).await;
        assert_eq!(status, StatusCode::CREATED);
        for bad in [
            json!({"site_id": paris, "name": "x", "address": "192.0.2.31", "template": "toaster", "snmp": {"version": "v2c", "community": "c"}}),
            json!({"site_id": paris, "name": "x", "address": "192.0.2.31", "template": "ups"}),
            json!({"site_id": paris, "name": "x", "address": "192.0.2.31", "snmp": {"version": "v3", "user": "u", "auth_protocol": "sha", "auth_password": "short"}}),
        ] {
            assert_eq!(call(&pool, &operator, "POST", "/api/devices", Some(bad)).await.0, StatusCode::BAD_REQUEST);
        }
        assert_eq!(call(&pool, &operator, "DELETE", &format!("/api/devices/{id}/snmp"), None).await.0, StatusCode::NO_CONTENT);
        let sets = ProbeStore::new(pool.clone()).load().await.unwrap();
        assert!(sets["central"].checks.iter().all(|c| c.kind != "snmp"), "no credentials, no SNMP checks");
    }

    /// Detection from the central server against net-snmp, when
    /// `MF_SNMPD` names one (deploy/probe-test/run-e2e.sh starts it).
    #[sqlx::test]
    async fn detection_suggests_a_template(pool: PgPool) {
        let paris = site(&pool, "paris").await;
        let operator = session(&pool, "op", Role::Operator, Some(paris)).await;
        // Nothing listens on this port: no answer, said plainly.
        let closed = std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let (status, result) = call(&pool, &operator, "POST", "/api/devices/detect",
            Some(json!({"site_id": paris, "address": "127.0.0.1", "probe": "central",
                        "snmp": {"version": "v2c", "community": "c", "port": closed}}))).await;
        assert_eq!(status, StatusCode::OK, "{result}");
        assert_eq!((result["answers"].as_bool(), result["template"].clone()), (Some(false), Value::Null));
        assert!(result["error"].as_str().is_some(), "{result}");
        let Ok(addr) = std::env::var("MF_SNMPD") else { return };
        let (host, port) = addr.rsplit_once(':').unwrap();
        let (status, result) = call(&pool, &operator, "POST", "/api/devices/detect",
            Some(json!({"site_id": paris, "address": host, "probe": "central",
                        "snmp": {"version": "v2c", "community": "e2e-community", "port": port.parse::<u16>().unwrap()}}))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(result["answers"], true, "{result}");
        assert_eq!(result["name"], "nas-e2e");
        assert_eq!(result["template"], "nas");
    }
}
