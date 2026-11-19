//! Dependencies through the API against a real PostgreSQL (ADR-0152):
//! setting them, what is refused and why, the pages' views with automatic
//! ones and suggestions, the unreachable state, what an incident hides,
//! audit, and the same through `/api/v1` with a key. Ignored without
//! `DATABASE_URL`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users;
use monitorflow_server::api::{v1, ApiSettings, ApiState, Role};
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
    v1::app(ApiState::new(
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

async fn session(pool: &PgPool, username: &str, role: Role) -> String {
    users::create_user(pool, username, &Secret::new(PASSWORD), role, "test")
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

async fn with_key(
    pool: &PgPool,
    key: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {key}"))
        .header("content-type", "application/json")
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let (status, body, _) = send(pool, request).await;
    (status, body)
}

async fn one(pool: &PgPool, sql: &'static str, binds: &[&str]) -> Uuid {
    let mut query = sqlx::query_scalar(sql);
    for bind in binds {
        query = query.bind(*bind);
    }
    query.fetch_one(pool).await.unwrap()
}

struct Fleet {
    site: Uuid,
    customer: Uuid,
    relay: Uuid,
    web: Uuid,
    switch: Uuid,
    firewall: Uuid,
}

/// One site with a relay, web-1 behind it, and two devices: sw-core, a
/// switch with a ping check, and fw-1, a firewall.
async fn fleet(pool: &PgPool) -> Fleet {
    let site = one(
        pool,
        "INSERT INTO sites (name) VALUES ('paris') RETURNING id",
        &[],
    )
    .await;
    let customer: Uuid = sqlx::query_scalar("SELECT customer_id FROM sites WHERE id = $1")
        .bind(site)
        .fetch_one(pool)
        .await
        .unwrap();
    let site_text = site.to_string();
    let relay = one(
        pool,
        "INSERT INTO relays (peer_id, site_id, name) VALUES ('gw-1', $1::uuid, 'gw-1') RETURNING id",
        &[&site_text],
    )
    .await;
    let relay_text = relay.to_string();
    let web = one(
        pool,
        "INSERT INTO servers (peer_id, site_id, hostname, os_family, relay_id, last_seen_at)
         VALUES ('web-1', $1::uuid, 'web-1', 'debian', $2::uuid, now()) RETURNING id",
        &[&site_text, &relay_text],
    )
    .await;
    let device = |name: &'static str, template: &'static str| {
        let site_text = site_text.clone();
        async move {
            let id = one(
                pool,
                "INSERT INTO servers (peer_id, site_id, hostname, os_family, kind, address,
                                      package_updates, device_template)
                 VALUES ($2, $1::uuid, $2, 'device', 'device', '192.0.2.1', false, $3) RETURNING id",
                &[&site_text, name, template],
            )
            .await;
            sqlx::query(
                "INSERT INTO sensor_configs (server_id, kind, name, interval_ms, probe)
                 VALUES ($1, 'ping', 'ping', 60000, 'central')",
            )
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
            id
        }
    };
    let switch = device("sw-core", "switch").await;
    let firewall = device("fw-1", "firewall").await;
    Fleet {
        site,
        customer,
        relay,
        web,
        switch,
        firewall,
    }
}

fn names(list: &Value, key: &str) -> Vec<String> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|d| d[key]["name"].as_str().unwrap().to_owned())
        .collect()
}

db_tests! {
    #[sqlx::test]
    async fn an_operator_puts_servers_behind_a_switch(pool: PgPool) {
        let f = fleet(&pool).await;
        let admin = session(&pool, "alice", Role::Administrator).await;
        // Suggested before anything is set: the switch and the firewall.
        let uri = format!("/api/servers/{}/dependencies", f.web);
        let (status, page) = call(&pool, &admin, "GET", &uri, None).await;
        assert_eq!(status, StatusCode::OK, "{page}");
        let suggested: Vec<&str> = page["suggestions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["parent"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(suggested.len(), 2, "{page}");
        assert!(page["suggestions"].as_array().unwrap().iter().all(|s| s["reason"] == "network_device"));
        // Its relay is automatic, and cannot be removed.
        assert_eq!(names(&page["parents"], "parent"), ["gw-1"]);
        assert_eq!(page["parents"][0]["automatic"], true);
        assert_eq!(page["parents"][0]["can_change"], false);

        let body = json!({"parent": {"server_id": f.switch},
                          "children": [{"server_id": f.web}, {"server_id": f.firewall, "check": {"name": "ping"}}]});
        let (status, created) = call(&pool, &admin, "POST", "/api/dependencies", Some(body.clone())).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        assert_eq!(created["dependencies"].as_array().unwrap().len(), 2);
        let audited: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = 'dependency.create'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(audited, 2);
        // Twice is refused, as one that loops.
        let (status, error) = call(&pool, &admin, "POST", "/api/dependencies", Some(body)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{error}");
        assert_eq!(error["error"], "web-1 already depends on sw-core.");
        let loop_body = json!({"parent": {"server_id": f.web}, "children": [{"server_id": f.switch}]});
        let (status, error) = call(&pool, &admin, "POST", "/api/dependencies", Some(loop_body)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{error}");
        assert_eq!(
            error["error"],
            "sw-core cannot depend on web-1: web-1 already depends on sw-core (web-1, then sw-core)."
        );
        // The switch now lists what is behind it; the web server no longer
        // gets it suggested.
        let uri = format!("/api/servers/{}/dependencies", f.switch);
        let (_, page) = call(&pool, &admin, "GET", &uri, None).await;
        assert_eq!(page["children"].as_array().unwrap().len(), 2, "{page}");
        assert_eq!(page["behind"].as_array().unwrap().len(), 2, "{page}");
        let uri = format!("/api/servers/{}/dependencies", f.web);
        let (_, page) = call(&pool, &admin, "GET", &uri, None).await;
        assert_eq!(names(&page["suggestions"], "parent"), ["fw-1"]);
        let (_, list) = call(&pool, &admin, "GET", &format!("/api/dependencies?server_id={}", f.web), None).await;
        assert_eq!(list["dependencies"].as_array().unwrap().len(), 1, "{list}");
        assert_eq!(list["dependencies"][0]["created_by"], "alice");

        // The switch's ping is critical: web-1 is unreachable, everywhere.
        let incident: Uuid = sqlx::query_scalar(
            "INSERT INTO incidents (server_id, metric, instance, severity, opened_at, last_value,
                                    last_value_at, peak_value, threshold)
             VALUES ($1, 'monitorflow_ping_up', '', 'critical', now(), 0, now(), 0, '{}') RETURNING id",
        )
        .bind(f.switch)
        .fetch_one(&pool)
        .await
        .unwrap();
        let (_, servers) = call(&pool, &admin, "GET", "/api/servers", None).await;
        let web = servers["servers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["hostname"] == "web-1")
            .unwrap();
        assert_eq!(web["state"], "unreachable", "{web}");
        assert_eq!(web["unreachable"]["because"]["name"], "sw-core");
        assert_eq!(web["unreachable"]["incident_id"], json!(incident));
        let (_, detail) = call(&pool, &admin, "GET", &format!("/api/servers/{}", f.web), None).await;
        assert_eq!(detail["state"], "unreachable");
        let (_, summary) = call(&pool, &admin, "GET", "/api/servers/summary", None).await;
        assert_eq!(summary["unreachable"], 1, "{summary}");
        let uri = format!("/api/servers/{}/dependencies", f.firewall);
        let (_, page) = call(&pool, &admin, "GET", &uri, None).await;
        assert!(page["unreachable"].is_null());
        assert_eq!(page["unreachable_checks"][0]["check"]["name"], "ping", "{page}");
        let (_, shown) = call(&pool, &admin, "GET", &format!("/api/incidents/{incident}"), None).await;
        let mut hides: Vec<&str> = shown["hides"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["name"].as_str().unwrap())
            .collect();
        hides.sort();
        assert_eq!(hides, ["fw-1", "web-1"], "{shown}");

        // Removed: audited, and web-1 is reachable again.
        let id = list["dependencies"][0]["id"].as_str().unwrap().to_owned();
        let (status, _) = call(&pool, &admin, "DELETE", &format!("/api/dependencies/{id}"), None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = call(&pool, &admin, "DELETE", &format!("/api/dependencies/{id}"), None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (_, detail) = call(&pool, &admin, "GET", &format!("/api/servers/{}", f.web), None).await;
        assert_ne!(detail["state"], "unreachable");
        let removed: Value = sqlx::query_scalar("SELECT details FROM audit_log WHERE action = 'dependency.remove'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(removed["child"]["server_id"], json!(f.web));
        assert_eq!(removed["parent"]["server_id"], json!(f.switch));
        let _ = f.site;
    }

    #[sqlx::test]
    async fn what_makes_no_sense_is_refused_in_words(pool: PgPool) {
        let f = fleet(&pool).await;
        let admin = session(&pool, "alice", Role::Administrator).await;
        let post = |body: Value| {
            let pool = pool.clone();
            let admin = admin.clone();
            async move { call(&pool, &admin, "POST", "/api/dependencies", Some(body)).await }
        };
        let cases = [
            (json!({"parent": {"server_id": f.web}, "children": []}), "Choose at least one"),
            (json!({"parent": {"server_id": f.web}, "children": [{"server_id": f.web}]}), "web-1 cannot depend on itself."),
            (json!({"parent": {"server_id": f.web, "check": {"name": "cpu"}}, "children": [{"server_id": f.web}]}), "web-1 cannot depend on itself."),
            (json!({"parent": {"server_id": f.switch}, "children": [{"relay_id": f.relay}]}), "A relay can be a parent only"),
            (json!({"parent": {"server_id": f.switch, "check": {"name": "nope"}}, "children": [{"server_id": f.web}]}), "sw-core has no check `nope`."),
            (json!({"parent": {"server_id": f.switch, "check": {"name": "Bad name"}}, "children": [{"server_id": f.web}]}), "is not a check name"),
            (json!({"parent": {}, "children": [{"server_id": f.web}]}), "Give each end"),
        ];
        for (body, words) in cases {
            let (status, error) = post(body.clone()).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body} {error}");
            assert!(error["error"].as_str().unwrap().contains(words), "{body} {error}");
        }
        // A check may depend on another server's check.
        let (status, error) = post(json!({"parent": {"server_id": f.firewall, "check": {"name": "ping"}},
                                          "children": [{"server_id": f.web, "check": {"name": "cpu"}}]})).await;
        assert_eq!(status, StatusCode::CREATED, "{error}");
        // Another customer's device, for an administrator of everything.
        let other = one(&pool, "INSERT INTO customers (name) VALUES ('globex') RETURNING id", &[]).await;
        let other_site: Uuid = sqlx::query_scalar("INSERT INTO sites (name, customer_id) VALUES ('lyon', $1) RETURNING id")
            .bind(other)
            .fetch_one(&pool)
            .await
            .unwrap();
        let theirs: Uuid = sqlx::query_scalar(
            "INSERT INTO servers (peer_id, site_id, hostname, os_family) VALUES ('db-9', $1, 'db-9', 'debian') RETURNING id",
        )
        .bind(other_site)
        .fetch_one(&pool)
        .await
        .unwrap();
        let (status, error) = post(json!({"parent": {"server_id": theirs}, "children": [{"server_id": f.web}]})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
        assert!(error["error"].as_str().unwrap().contains("same customer"), "{error}");
        // A viewer reads, never changes.
        let viewer = session(&pool, "victor", Role::Viewer).await;
        let (status, _) = call(&pool, &viewer, "GET", "/api/dependencies", None).await;
        assert_eq!(status, StatusCode::OK);
        let body = json!({"parent": {"server_id": f.switch}, "children": [{"server_id": f.web}]});
        let (status, _) = call(&pool, &viewer, "POST", "/api/dependencies", Some(body)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[sqlx::test]
    async fn a_key_does_all_of_it_through_v1(pool: PgPool) {
        let f = fleet(&pool).await;
        let admin = session(&pool, "alice", Role::Administrator).await;
        let (status, made) = call(
            &pool,
            &admin,
            "POST",
            "/api/api-keys",
            Some(json!({"name": "ci", "customer_id": f.customer, "role": "operator"})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{made}");
        let key = made["secret"].as_str().unwrap().to_owned();
        let body = json!({"parent": {"relay_id": f.relay}, "children": [{"server_id": f.switch}]});
        let (status, created) = with_key(&pool, &key, "POST", "/api/v1/dependencies", Some(body)).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let uri = format!("/api/v1/relays/{}/dependencies", f.relay);
        let (status, page) = with_key(&pool, &key, "GET", &uri, None).await;
        assert_eq!(status, StatusCode::OK, "{page}");
        let mut behind = names(&page["children"], "child");
        behind.sort();
        assert_eq!(behind, ["sw-core", "web-1"]);
        let id = created["dependencies"][0]["id"].as_str().unwrap().to_owned();
        let (status, _) = with_key(&pool, &key, "DELETE", &format!("/api/v1/dependencies/{id}"), None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let api_key: Option<String> = sqlx::query_scalar(
            "SELECT details ->> 'api_key' FROM audit_log WHERE action = 'dependency.remove'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(api_key.is_some());
    }
}
