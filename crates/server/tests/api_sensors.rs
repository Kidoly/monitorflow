//! Sensor configuration endpoints against a real PostgreSQL: site scoping,
//! validation, audit rows and revisions. Ignored without `DATABASE_URL`.

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

/// Marks every test in the block as ignored when `DATABASE_URL` is unset.
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

/// A user with one role, on `site` or globally, logged in.
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
        .header("content-type", "application/json");
    let request = request
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let (status, body, _) = send(pool, request).await;
    (status, body)
}

struct Fleet {
    paris: Uuid,
    lyon: Uuid,
    paris_server: Uuid,
    lyon_server: Uuid,
}

async fn fleet(pool: &PgPool) -> Fleet {
    let mut ids = Vec::new();
    for name in ["paris", "lyon"] {
        let site: Uuid = sqlx::query_scalar("INSERT INTO sites (name) VALUES ($1) RETURNING id")
            .bind(name)
            .fetch_one(pool)
            .await
            .unwrap();
        let server: Uuid = sqlx::query_scalar(
            "INSERT INTO servers (peer_id, site_id, hostname, os_family)
             VALUES ($1, $2, $1, 'debian') RETURNING id",
        )
        .bind(format!("agent-{name}"))
        .bind(site)
        .fetch_one(pool)
        .await
        .unwrap();
        ids.push((site, server));
    }
    Fleet {
        paris: ids[0].0,
        paris_server: ids[0].1,
        lyon: ids[1].0,
        lyon_server: ids[1].1,
    }
}

async fn audit(pool: &PgPool) -> Vec<(String, Option<String>, Value)> {
    sqlx::query_as(
        "SELECT action, target_id, details FROM audit_log
         WHERE action LIKE 'sensor_config.%' ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn revision(pool: &PgPool, server: Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT si.sensor_revision + s.sensor_revision
         FROM servers s JOIN sites si ON si.id = s.site_id WHERE s.id = $1",
    )
    .bind(server)
    .fetch_one(pool)
    .await
    .unwrap()
}

db_tests! {
    #[sqlx::test]
    async fn viewers_read_their_sites_only(pool: PgPool) {
        let f = fleet(&pool).await;
        let viewer = session(&pool, "paris-viewer", Role::Viewer, Some(f.paris)).await;

        let (status, body) =
            call(&pool, &viewer, "GET", &format!("/api/servers/{}/sensors", f.paris_server), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["site_id"], json!(f.paris));
        assert_eq!(body["revision"], 0);
        assert_eq!(body["applied_revision"], Value::Null);
        assert_eq!(body["in_sync"], false);
        let sensors: Vec<_> = body["sensors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| (s["name"].as_str().unwrap(), s["origin"].as_str().unwrap()))
            .collect();
        assert_eq!(sensors, [("cpu", "default"), ("disk", "default"), ("memory", "default")]);

        // Lyon's resources do not exist for a Paris viewer.
        for uri in [
            format!("/api/servers/{}/sensors", f.lyon_server),
            format!("/api/servers/{}/sensor_configs", f.lyon_server),
            format!("/api/sites/{}/sensor_configs", f.lyon),
            format!("/api/servers/{}/sensors", Uuid::new_v4()),
            "/api/servers/not-a-uuid/sensors".to_owned(),
        ] {
            let (status, body) = call(&pool, &viewer, "GET", &uri, None).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
            assert_eq!(body, json!({ "error": "not found" }));
        }

        // Reading is allowed, changing is not.
        let (status, body) =
            call(&pool, &viewer, "GET", &format!("/api/sites/{}/sensor_configs", f.paris), None).await;
        assert_eq!((status, body), (StatusCode::OK, json!({ "entries": [] })));
        let row = json!({ "kind": "cpu", "interval_ms": 5_000 });
        let (status, _) = call(
            &pool, &viewer, "POST", &format!("/api/sites/{}/sensor_configs", f.paris), Some(row.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = call(
            &pool, &viewer, "POST", &format!("/api/sites/{}/sensor_configs", f.lyon), Some(row),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(audit(&pool).await.is_empty());
    }

    #[sqlx::test]
    async fn operators_change_rows_with_audit_and_revision_bumps(pool: PgPool) {
        let f = fleet(&pool).await;
        let operator = session(&pool, "paris-op", Role::Operator, Some(f.paris)).await;
        let rev0 = revision(&pool, f.paris_server).await;

        // A site row, then a server override of it.
        let (status, site_row) = call(
            &pool, &operator, "POST", &format!("/api/sites/{}/sensor_configs", f.paris),
            Some(json!({ "kind": "cpu", "interval_ms": 30_000, "params": { "per_core": false } })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{site_row}");
        assert_eq!(site_row["site_id"], json!(f.paris));
        assert_eq!(site_row["server_id"], Value::Null);
        assert_eq!(site_row["name"], "cpu");
        assert_eq!(site_row["params"], json!({ "per_core": false }));
        let rev1 = revision(&pool, f.paris_server).await;
        assert!(rev1 > rev0);

        let (status, server_row) = call(
            &pool, &operator, "POST", &format!("/api/servers/{}/sensor_configs", f.paris_server),
            Some(json!({ "kind": "disk", "instance": "/data", "name": "disk_data", "interval_ms": 10_000,
                         "timeout_ms": 2_000, "params": { "mounts_exclude": "/boot" } })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{server_row}");
        assert!(revision(&pool, f.paris_server).await > rev1);
        // Lyon's server is untouched.
        assert_eq!(revision(&pool, f.lyon_server).await, 0);

        // The agent acknowledged an older revision: drift.
        sqlx::query("UPDATE servers SET sensor_revision_applied = $2, sensor_applied_at = now() WHERE id = $1")
            .bind(f.paris_server)
            .bind(rev1)
            .execute(&pool)
            .await
            .unwrap();
        let (_, body) =
            call(&pool, &operator, "GET", &format!("/api/servers/{}/sensors", f.paris_server), None).await;
        assert_eq!(body["revision"], revision(&pool, f.paris_server).await);
        assert_eq!(body["applied_revision"], rev1);
        assert_eq!(body["in_sync"], false);
        let sensors: Vec<_> = body["sensors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| (s["name"].as_str().unwrap(), s["origin"].as_str().unwrap(), s["entry_id"].clone()))
            .collect();
        assert_eq!(
            sensors,
            [
                ("cpu", "site", site_row["id"].clone()),
                ("disk", "default", Value::Null),
                ("disk_data", "server", server_row["id"].clone()),
                ("memory", "default", Value::Null),
            ]
        );
        assert_eq!(body["sensors"][0]["params"], json!({ "per_core": "false" }));

        let (status, list) =
            call(&pool, &operator, "GET", &format!("/api/servers/{}/sensor_configs", f.paris_server), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(list["entries"].as_array().unwrap().len(), 1);

        // Same name and instance twice for one owner.
        let (status, body) = call(
            &pool, &operator, "POST", &format!("/api/sites/{}/sensor_configs", f.paris),
            Some(json!({ "kind": "cpu", "interval_ms": 1_000 })),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"], "sensor `cpu` instance `` is already configured here");

        // Update, then delete.
        let rev2 = revision(&pool, f.paris_server).await;
        let uri = format!("/api/sensor_configs/{}", site_row["id"].as_str().unwrap());
        let (status, updated) = call(
            &pool, &operator, "PUT", &uri, Some(json!({ "kind": "cpu", "interval_ms": 15_000, "enabled": false })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{updated}");
        assert_eq!(updated["interval_ms"], 15_000);
        assert_eq!(updated["enabled"], false);
        assert_eq!(updated["params"], json!({}));
        assert!(revision(&pool, f.paris_server).await > rev2);
        let (status, _) = call(&pool, &operator, "DELETE", &uri, None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = call(&pool, &operator, "DELETE", &uri, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let rows = audit(&pool).await;
        let actions: Vec<_> = rows.iter().map(|r| r.0.as_str()).collect();
        assert_eq!(
            actions,
            ["sensor_config.create", "sensor_config.create", "sensor_config.update", "sensor_config.delete"]
        );
        assert_eq!(rows[0].1.as_deref(), site_row["id"].as_str());
        assert_eq!(rows[0].2["param_names"], json!(["per_core"]));
        assert_eq!(rows[0].2["interval_ms"], 30_000);
        // Parameter values are not recorded.
        assert!(!rows[1].2.to_string().contains("/boot"), "{}", rows[1].2);
        assert_eq!(rows[2].2["before"]["interval_ms"], 30_000);
        assert_eq!(rows[2].2["after"]["enabled"], false);
        assert_eq!(rows[3].2["name"], "cpu");
    }

    #[sqlx::test]
    async fn other_sites_rows_cannot_be_changed(pool: PgPool) {
        let f = fleet(&pool).await;
        let lyon_row: Uuid = sqlx::query_scalar(
            "INSERT INTO sensor_configs (server_id, kind, name, interval_ms)
             VALUES ($1, 'cpu', 'cpu', 5000) RETURNING id",
        )
        .bind(f.lyon_server)
        .fetch_one(&pool)
        .await
        .unwrap();
        let rev = revision(&pool, f.lyon_server).await;
        let operator = session(&pool, "paris-op", Role::Operator, Some(f.paris)).await;
        let uri = format!("/api/sensor_configs/{lyon_row}");
        let (status, _) =
            call(&pool, &operator, "PUT", &uri, Some(json!({ "kind": "cpu", "interval_ms": 1_000 }))).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = call(&pool, &operator, "DELETE", &uri, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // A global viewer sees it but may not change it.
        let viewer = session(&pool, "viewer", Role::Viewer, None).await;
        let (status, _) = call(&pool, &viewer, "DELETE", &uri, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        // A global administrator may.
        let admin = session(&pool, "admin", Role::Administrator, None).await;
        let (status, _) =
            call(&pool, &admin, "PUT", &uri, Some(json!({ "kind": "cpu", "interval_ms": 1_000 }))).await;
        assert_eq!(status, StatusCode::OK);
        assert!(revision(&pool, f.lyon_server).await > rev);
        assert_eq!(revision(&pool, f.paris_server).await, 0);
    }

    #[sqlx::test]
    async fn invalid_rows_are_refused_with_a_reason(pool: PgPool) {
        let f = fleet(&pool).await;
        let operator = session(&pool, "op", Role::Operator, None).await;
        let uri = format!("/api/sites/{}/sensor_configs", f.paris);
        let cases = [
            (json!({ "kind": "gpu", "interval_ms": 1_000 }), "unknown sensor kind `gpu`"),
            (json!({ "kind": "cpu", "interval_ms": 10 }), "interval"),
            (json!({ "kind": "cpu", "interval_ms": 1_000, "params": { "per_cores": true } }), "unknown parameter `per_cores`"),
            (json!({ "kind": "cpu" }), "missing field `interval_ms`"),
            (json!({ "kind": "cpu", "interval_ms": 1_000, "owner": "x" }), "unknown field `owner`"),
        ];
        for (body, expected) in cases {
            let (status, response) = call(&pool, &operator, "POST", &uri, Some(body.clone())).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            let message = response["error"].as_str().unwrap();
            assert!(message.contains(expected), "{body}: {message}");
        }
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM sensor_configs")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
        assert_eq!(revision(&pool, f.paris_server).await, 0);
        assert!(audit(&pool).await.is_empty());
    }
}
