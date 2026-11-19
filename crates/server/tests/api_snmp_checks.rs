//! Custom SNMP checks of devices through the API against a real PostgreSQL
//! (ADR-0157), and Test and Browse against the snmpsim of
//! `deploy/snmpx-test` when `MF_SNMPX_SIM` names it. Ignored without
//! `DATABASE_URL`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users;
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
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

/// The snmpsim of `deploy/snmpx-test` as `(address, port)`, when set.
fn simulator() -> Option<(String, u16)> {
    let addr = std::env::var("MF_SNMPX_SIM").ok()?;
    let (host, port) = addr.rsplit_once(':')?;
    Some((host.to_owned(), port.parse().ok()?))
}

fn firewall_check() -> Value {
    json!({"name": "snmp_fw", "probe": "central", "channels": [
        {"oid": "1.3.6.1.4.1.12356.101.4.1.8.0", "name": "Sessions", "unit": "sessions",
         "limits": {"warning": 5000, "critical": 8000, "duration_s": 300}},
        {"oid": "1.3.6.1.2.1.2.2.1.10.1", "name": "Inbound", "type": "counter", "multiply": 8},
        {"oid": "1.3.6.1.4.1.12356.101.4.1.1.0", "name": "Firmware", "type": "text", "match": "matches", "expect": "^v7\\."},
        {"oid": "1.3.6.1.4.1.12356.101.13.2.1.1.15.1", "name": "HA state", "lookup": "builtin:ok_warning_failed"}
    ]})
}

async fn rules(pool: &PgPool, device: &str) -> Vec<(String, String, Option<f64>, Option<f64>)> {
    sqlx::query_as("SELECT metric, comparison, warning, critical FROM thresholds WHERE server_id = $1::uuid ORDER BY metric")
        .bind(device)
        .fetch_all(pool)
        .await
        .unwrap()
}

db_tests! {
    #[sqlx::test]
    async fn custom_checks_with_their_limits(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let paris = site(&pool, "paris").await;
        let operator = session(&pool, "op", Role::Operator, Some(paris)).await;
        let viewer = session(&pool, "viewer", Role::Viewer, Some(paris)).await;
        let (address, port) = simulator().unwrap_or(("192.0.2.60".into(), 161));
        let device = json!({"site_id": paris, "name": "fw-edge", "address": address, "probe": "central",
                            "snmp": {"version": "v2c", "community": "firewall", "port": port}});
        let (status, fw) = call(&pool, &operator, "POST", "/api/devices", Some(device)).await;
        assert_eq!(status, StatusCode::CREATED, "{fw}");
        let id = fw["id"].as_str().unwrap().to_owned();
        let uri = format!("/api/devices/{id}/snmp-checks");

        assert_eq!(call(&pool, &viewer, "POST", &uri, Some(firewall_check())).await.0, StatusCode::FORBIDDEN);
        let (status, check) = call(&pool, &operator, "POST", &uri, Some(firewall_check())).await;
        assert_eq!(status, StatusCode::CREATED, "{check}");
        assert_eq!(check["instance"], json!("central"));
        let keys: Vec<&str> = check["channels"].as_array().unwrap().iter().map(|c| c["key"].as_str().unwrap()).collect();
        assert_eq!(keys, ["sessions", "inbound", "firmware", "ha"]);
        assert_eq!(check["channels"][0]["limits"]["critical"], json!(8000.0));
        assert_eq!(check["channels"][3]["state_metric"], json!("monitorflow_snmp_fw_ha_state"));
        assert_eq!(rules(&pool, &id).await.into_iter().filter(|r| r.0.starts_with("monitorflow_snmp_fw")).collect::<Vec<_>>(), [
            ("monitorflow_snmp_fw_firmware_state".to_string(), ">=".to_string(), Some(1.0), Some(2.0)),
            ("monitorflow_snmp_fw_ha_state".into(), ">=".into(), Some(1.0), Some(2.0)),
            ("monitorflow_snmp_fw_sessions".into(), ">".into(), Some(5000.0), Some(8000.0)),
        ]);
        assert_eq!(call(&pool, &operator, "POST", &uri, Some(firewall_check())).await.0, StatusCode::CONFLICT);
        let mut bad = firewall_check();
        bad["name"] = json!("snmp_bad");
        bad["channels"][2]["limits"] = json!({"warning": 1});
        let (status, error) = call(&pool, &operator, "POST", &uri, Some(bad)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(error["error"].as_str().unwrap().contains("is a text"), "{error}");
        // The generic routes see the same row, flat.
        let (_, rows) = call(&pool, &viewer, "GET", &format!("/api/servers/{id}/sensor_configs"), None).await;
        let row = rows["entries"].as_array().unwrap().iter().find(|r| r["name"] == json!("snmp_fw")).unwrap();
        assert_eq!(row["kind"], json!("snmp_custom"));
        assert_eq!(row["params"]["lookup4"], json!("builtin:ok_warning_failed"));

        // A change drops a channel and the sessions' limits.
        let mut changed = firewall_check();
        changed["channels"].as_array_mut().unwrap().remove(3);
        changed["channels"][0]["limits"] = Value::Null;
        let (status, after) = call(&pool, &operator, "PUT", &format!("{uri}/snmp_fw"), Some(changed)).await;
        assert_eq!(status, StatusCode::OK, "{after}");
        assert_eq!(after["channels"].as_array().unwrap().len(), 3);
        let left: Vec<String> = rules(&pool, &id).await.into_iter().map(|r| r.0).filter(|m| m.starts_with("monitorflow_snmp_fw")).collect();
        assert_eq!(left, ["monitorflow_snmp_fw_firmware_state"]);
        let (_, list) = call(&pool, &viewer, "GET", &uri, None).await;
        assert_eq!(list["checks"].as_array().unwrap().len(), 1);
        assert_eq!(call(&pool, &operator, "PUT", &format!("{uri}/snmp_nope"), Some(firewall_check())).await.0, StatusCode::NOT_FOUND);

        if simulator().is_some() {
            // Test and Browse against the simulator, from the central server.
            let (status, read) = call(&pool, &operator, "POST", &format!("/api/devices/{id}/snmp/read"),
                Some(json!({"oids": ["1.3.6.1.2.1.1.5.0", "1.3.6.1.4.1.12356.101.4.1.8.0"]}))).await;
            assert_eq!(status, StatusCode::OK, "{read}");
            assert_eq!(read["objects"][0]["name"], json!("sysName.0"));
            assert_eq!(read["objects"][0]["value"], json!("fw-edge"));
            assert_eq!(read["objects"][1]["name"], json!("fgSysSesCount"), "named by the library");
            assert_eq!(read["objects"][1]["type"], json!("Gauge32"));
            let (_, walked) = call(&pool, &operator, "POST", &format!("/api/devices/{id}/snmp/read"),
                Some(json!({"walk": "1.3.6.1.4.1", "max": 3}))).await;
            assert_eq!((walked["objects"].as_array().unwrap().len(), walked["cut"].as_bool()), (3, Some(true)));
            assert_eq!(call(&pool, &viewer, "POST", &format!("/api/devices/{id}/snmp/read"), Some(json!({"walk": "1.3.6.1"}))).await.0, StatusCode::FORBIDDEN);
            let (status, tested) = call(&pool, &operator, "POST", &format!("/api/devices/{id}/snmp/test"), Some(firewall_check())).await;
            assert_eq!(status, StatusCode::OK, "{tested}");
            let channels = &tested["rows"][0]["channels"];
            assert!(channels[0]["value"].as_f64().unwrap() >= 1000.0, "{tested}");
            assert!(channels[1]["note"].as_str().unwrap().contains("second reading"));
            assert_eq!((channels[2]["state"].as_u64(), channels[2]["text"].as_str()), (Some(0), Some("v7.2.5")));
            assert_eq!((channels[3]["state"].as_u64(), channels[3]["text"].as_str()), (Some(0), Some("OK")));
        }

        assert_eq!(call(&pool, &operator, "DELETE", &format!("{uri}/snmp_fw"), None).await.0, StatusCode::NO_CONTENT);
        assert!(rules(&pool, &id).await.iter().all(|r| !r.0.starts_with("monitorflow_snmp_fw")));
        let actions: Vec<String> = sqlx::query_scalar("SELECT action FROM audit_log WHERE action LIKE 'snmp_check.%' ORDER BY id").fetch_all(&pool).await.unwrap();
        assert_eq!(actions, ["snmp_check.create", "snmp_check.update", "snmp_check.delete"]);
        let audit: Vec<String> = sqlx::query_scalar("SELECT details::text FROM audit_log").fetch_all(&pool).await.unwrap();
        assert!(audit.iter().all(|d| !d.contains("firewall")), "no community in the audit log");
    }
}
