//! Lookups, MIB files and object names of custom SNMP checks through the
//! API against a real PostgreSQL (ADR-0157). Ignored without `DATABASE_URL`.

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

const NAS_MIB: &str = r#"
TEST-NAS-MIB DEFINITIONS ::= BEGIN
IMPORTS MODULE-IDENTITY, OBJECT-TYPE, Integer32, enterprises FROM SNMPv2-SMI
        DisplayString FROM SNMPv2-TC;
testNas MODULE-IDENTITY LAST-UPDATED "202611190000Z" ORGANIZATION "Test" CONTACT-INFO "none"
    DESCRIPTION "A test NAS." ::= { enterprises 99999 }
storage OBJECT IDENTIFIER ::= { testNas 3 }
raidTable OBJECT-TYPE SYNTAX SEQUENCE OF RaidEntry MAX-ACCESS not-accessible STATUS current
    DESCRIPTION "Volumes." ::= { storage 1 }
raidEntry OBJECT-TYPE SYNTAX RaidEntry MAX-ACCESS not-accessible STATUS current
    DESCRIPTION "A volume." INDEX { raidIndex } ::= { raidTable 1 }
RaidEntry ::= SEQUENCE { raidIndex Integer32, raidName DisplayString, raidStatus INTEGER }
raidIndex OBJECT-TYPE SYNTAX Integer32 MAX-ACCESS not-accessible STATUS current DESCRIPTION "Index." ::= { raidEntry 1 }
raidName OBJECT-TYPE SYNTAX DisplayString MAX-ACCESS read-only STATUS current DESCRIPTION "Name." ::= { raidEntry 2 }
raidStatus OBJECT-TYPE SYNTAX INTEGER { normal(1), degraded(11), crashed(12) } MAX-ACCESS read-only
    STATUS current DESCRIPTION "State." ::= { raidEntry 3 }
END
"#;

const EXTRA_MIB: &str = r#"
TEST-NAS-EXTRA-MIB DEFINITIONS ::= BEGIN
IMPORTS OBJECT-TYPE FROM SNMPv2-SMI testNas FROM TEST-NAS-MIB;
fanSpeed OBJECT-TYPE SYNTAX INTEGER UNITS "rpm" MAX-ACCESS read-only STATUS current
    DESCRIPTION "Fan." ::= { testNas 7 }
END
"#;

db_tests! {
    #[sqlx::test]
    async fn lookups_belong_to_the_customer(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let paris = site(&pool, "paris").await;
        let admin = session(&pool, "admin", Role::Operator, None).await;
        let site_operator = session(&pool, "siteop", Role::Operator, Some(paris)).await;
        let viewer = session(&pool, "viewer", Role::Viewer, Some(paris)).await;
        let (_, list) = call(&pool, &viewer, "GET", "/api/snmp-lookups", None).await;
        let builtins = list["lookups"].as_array().unwrap();
        assert_eq!(builtins.len(), 6);
        assert!(builtins.iter().all(|l| l["builtin"] == json!(true) && l["can_change"] == json!(false)));
        let (status, one) = call(&pool, &viewer, "GET", "/api/snmp-lookups/builtin:if_status", None).await;
        assert_eq!((status, one["entries"][1]["name"].as_str()), (StatusCode::OK, Some("Down")));

        let body = json!({"name": "RAID state", "entries": [
            {"value": 1, "name": "Normal", "state": "ok"},
            {"value": 10, "max": 19, "name": "Degraded", "state": "critical"}]});
        // Viewers and operators of one site only read.
        assert_eq!(call(&pool, &viewer, "POST", "/api/snmp-lookups", Some(body.clone())).await.0, StatusCode::FORBIDDEN);
        assert_eq!(call(&pool, &site_operator, "POST", "/api/snmp-lookups", Some(body.clone())).await.0, StatusCode::FORBIDDEN);
        let (status, made) = call(&pool, &admin, "POST", "/api/snmp-lookups", Some(body.clone())).await;
        assert_eq!(status, StatusCode::CREATED, "{made}");
        assert_eq!(made["otherwise"], json!("warning"));
        let id = made["id"].as_str().unwrap().to_owned();
        assert_eq!(call(&pool, &admin, "POST", "/api/snmp-lookups", Some(body.clone())).await.0, StatusCode::CONFLICT);
        let mut bad = body.clone();
        bad["entries"] = json!([{"value": 5, "max": 1, "name": "x", "state": "ok"}]);
        assert_eq!(call(&pool, &admin, "POST", "/api/snmp-lookups", Some(bad)).await.0, StatusCode::BAD_REQUEST);
        let (_, list) = call(&pool, &site_operator, "GET", "/api/snmp-lookups", None).await;
        assert!(list.to_string().contains(&id), "a site's operator reads its customer's lookups");

        // A check names it; then it cannot go.
        let device = json!({"site_id": paris, "name": "nas", "address": "192.0.2.50", "snmp": {"version": "v2c", "community": "c"}});
        let (status, nas) = call(&pool, &site_operator, "POST", "/api/devices", Some(device)).await;
        assert_eq!(status, StatusCode::CREATED, "{nas}");
        let check = json!({"kind": "snmp_custom", "name": "snmp_raid", "interval_ms": 60000,
            "params": {"table": "1.3.6.1.4.1.99999.3.1.1", "oid1": "1.3.6.1.4.1.99999.3.1.1.3", "lookup1": id}});
        let (status, row) = call(&pool, &site_operator, "POST", &format!("/api/servers/{}/sensor_configs", nas["id"].as_str().unwrap()), Some(check)).await;
        assert_eq!(status, StatusCode::CREATED, "{row}");
        let (_, one) = call(&pool, &viewer, "GET", &format!("/api/snmp-lookups/{id}"), None).await;
        assert_eq!(one["used_by"], json!(1));
        let mut renamed = body;
        renamed["name"] = json!("Volume state");
        let (status, changed) = call(&pool, &admin, "PUT", &format!("/api/snmp-lookups/{id}"), Some(renamed)).await;
        assert_eq!((status, changed["name"].as_str()), (StatusCode::OK, Some("Volume state")));
        assert_eq!(call(&pool, &admin, "PUT", "/api/snmp-lookups/builtin:if_status", Some(json!({"name": "x", "entries": []}))).await.0, StatusCode::BAD_REQUEST);
        assert_eq!(call(&pool, &admin, "DELETE", &format!("/api/snmp-lookups/{id}"), None).await.0, StatusCode::CONFLICT);
        sqlx::query("DELETE FROM sensor_configs WHERE name = 'snmp_raid'").execute(&pool).await.unwrap();
        assert_eq!(call(&pool, &admin, "DELETE", &format!("/api/snmp-lookups/{id}"), None).await.0, StatusCode::NO_CONTENT);
        let actions: Vec<String> = sqlx::query_scalar("SELECT action FROM audit_log WHERE action LIKE 'snmp_lookup.%' ORDER BY id").fetch_all(&pool).await.unwrap();
        assert_eq!(actions, ["snmp_lookup.create", "snmp_lookup.update", "snmp_lookup.delete"]);
    }

    #[sqlx::test]
    async fn mib_files_name_objects(pool: PgPool) {
        let paris = site(&pool, "paris").await;
        let admin = session(&pool, "admin", Role::Operator, None).await;
        let viewer = session(&pool, "viewer", Role::Viewer, Some(paris)).await;
        let file = |name: &str, text: &str| json!({"name": name, "text": text});
        // Alone, the extra module names what is missing.
        let (status, error) = call(&pool, &admin, "POST", "/api/snmp-mibs", Some(json!({"files": [file("extra.txt", EXTRA_MIB)]}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(error["error"].as_str().unwrap().contains("TEST-NAS-MIB (for testNas)"), "{error}");
        let (status, error) = call(&pool, &admin, "POST", "/api/snmp-mibs", Some(json!({"files": [file("x.txt", "not a MIB")]}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(error["error"].as_str().unwrap().contains("x.txt"), "{error}");
        assert_eq!(call(&pool, &viewer, "POST", "/api/snmp-mibs", Some(json!({"files": [file("nas.txt", NAS_MIB)]}))).await.0, StatusCode::FORBIDDEN);
        let (status, uploaded) = call(&pool, &admin, "POST", "/api/snmp-mibs", Some(json!({"files": [file("../nas.txt", NAS_MIB)]}))).await;
        assert_eq!(status, StatusCode::CREATED, "{uploaded}");
        assert_eq!(uploaded["mibs"][0]["module"], json!("TEST-NAS-MIB"));
        assert_eq!(uploaded["mibs"][0]["file_name"], json!("nas.txt"));
        assert_eq!(uploaded["offered_lookups"][0]["name"], json!("raidStatus"));
        assert_eq!(uploaded["offered_lookups"][0]["entries"][2]["state"], json!("critical"));
        // Now the extra module reads, against what was uploaded.
        let (status, _) = call(&pool, &admin, "POST", "/api/snmp-mibs", Some(json!({"files": [file("extra.txt", EXTRA_MIB)]}))).await;
        assert_eq!(status, StatusCode::CREATED);
        let (_, list) = call(&pool, &viewer, "GET", "/api/snmp-mibs", None).await;
        assert_eq!(list["mibs"].as_array().unwrap().len(), 2);

        let (_, found) = call(&pool, &viewer, "GET", "/api/snmp-objects?q=raidstat", None).await;
        let raid = found["objects"].as_array().unwrap().iter().find(|o| o["source"] == json!("mib")).unwrap().clone();
        assert_eq!(raid["oid"], json!("1.3.6.1.4.1.99999.3.1.1.3"));
        assert_eq!(raid["kind"], json!("column"));
        assert_eq!(raid["suggestion"]["table"], json!("1.3.6.1.4.1.99999.3.1.1"));
        assert_eq!(raid["suggestion"]["name_column"], json!("1.3.6.1.4.1.99999.3.1.1.2"), "{raid}");
        // The library answers too.
        let (_, found) = call(&pool, &viewer, "GET", "/api/snmp-objects?q=sessions", None).await;
        assert!(found.to_string().contains("1.3.6.1.4.1.12356.101.4.1.8.0"), "{found}");
        let (_, named) = call(&pool, &viewer, "GET", "/api/snmp-objects?oids=1.3.6.1.4.1.99999.3.1.1.3.0,1.3.6.1.4.1.99999.7.0,1.3.6.1.2.1.1.5.0,1.9.9", None).await;
        assert_eq!(named["names"]["1.3.6.1.4.1.99999.3.1.1.3.0"], json!("raidStatus.0"));
        assert_eq!(named["names"]["1.3.6.1.4.1.99999.7.0"], json!("fanSpeed.0"));
        assert_eq!(named["names"]["1.3.6.1.2.1.1.5.0"], json!("sysName.0"));
        assert!(named["names"].get("1.9.9").is_none());
        assert_eq!(call(&pool, &viewer, "GET", "/api/snmp-objects", None).await.0, StatusCode::BAD_REQUEST);

        // Uploading a module again replaces it.
        let (_, again) = call(&pool, &admin, "POST", "/api/snmp-mibs", Some(json!({"files": [file("nas2.txt", NAS_MIB)]}))).await;
        let (_, list) = call(&pool, &viewer, "GET", "/api/snmp-mibs", None).await;
        assert_eq!(list["mibs"].as_array().unwrap().len(), 2);
        let id = again["mibs"][0]["id"].as_str().unwrap();
        assert_eq!(call(&pool, &viewer, "DELETE", &format!("/api/snmp-mibs/{id}"), None).await.0, StatusCode::FORBIDDEN);
        assert_eq!(call(&pool, &admin, "DELETE", &format!("/api/snmp-mibs/{id}"), None).await.0, StatusCode::NO_CONTENT);
        let (_, named) = call(&pool, &viewer, "GET", "/api/snmp-objects?oids=1.3.6.1.4.1.99999.3.1.1.3.0", None).await;
        assert!(!named.to_string().contains("raidStatus"), "{named}");
        let actions: Vec<String> = sqlx::query_scalar("SELECT action FROM audit_log WHERE action LIKE 'snmp_mib.%' ORDER BY id").fetch_all(&pool).await.unwrap();
        assert_eq!(actions, ["snmp_mib.upload", "snmp_mib.upload", "snmp_mib.upload", "snmp_mib.delete"]);
    }
}
