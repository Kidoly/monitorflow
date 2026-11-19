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
use monitorflow_server::probes::ProbeStore;
use monitorflow_server::sensor_config::SensorConfigStore;
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
        assert_eq!(sensors, [("cpu", "default"), ("disk", "default"), ("diskio", "default"), ("memory", "default"), ("net", "default")]);

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
            assert_eq!(body, json!({ "error": "not found", "code": "not_found" }));
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
                ("diskio", "default", Value::Null),
                ("memory", "default", Value::Null),
                ("net", "default", Value::Null),
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
            Some(json!({ "kind": "cpu", "interval_ms": 10_000 })),
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
            call(&pool, &admin, "PUT", &uri, Some(json!({ "kind": "cpu", "interval_ms": 10_000 }))).await;
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
            (json!({ "kind": "cpu", "interval_ms": 5_000 }), "interval must be between 10 s and 24 h"),
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

    #[sqlx::test]
    async fn site_view_overlays_site_rows_on_defaults(pool: PgPool) {
        let f = fleet(&pool).await;
        let operator = session(&pool, "paris-op", Role::Operator, Some(f.paris)).await;
        for (owner, row) in [
            (format!("/api/sites/{}/sensor_configs", f.paris),
             json!({ "kind": "port", "name": "port_443", "interval_ms": 60_000, "params": { "port": 443 } })),
            (format!("/api/sites/{}/sensor_configs", f.paris),
             json!({ "kind": "disk", "interval_ms": 60_000, "enabled": false })),
            // A server row does not show in the site's view.
            (format!("/api/servers/{}/sensor_configs", f.paris_server),
             json!({ "kind": "systemd", "name": "service_nginx", "interval_ms": 60_000, "params": { "units": "nginx" } })),
        ] {
            let (status, body) = call(&pool, &operator, "POST", &owner, Some(row)).await;
            assert_eq!(status, StatusCode::CREATED, "{body}");
        }
        let (status, body) =
            call(&pool, &operator, "GET", &format!("/api/sites/{}/sensors", f.paris), None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["site_id"], json!(f.paris));
        let sensors: Vec<_> = body["sensors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| (s["name"].as_str().unwrap(), s["origin"].as_str().unwrap()))
            .collect();
        assert_eq!(sensors, [("cpu", "default"), ("diskio", "default"), ("memory", "default"), ("net", "default"), ("port_443", "site")]);
        assert_eq!(body["sensors"][4]["params"], json!({ "port": "443" }));

        for uri in [
            format!("/api/sites/{}/sensors", f.lyon),
            format!("/api/sites/{}/sensors", Uuid::new_v4()),
            "/api/sites/not-a-uuid/sensors".to_owned(),
        ] {
            let (status, _) = call(&pool, &operator, "GET", &uri, None).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
        }
    }

    #[sqlx::test]
    async fn operators_set_how_often_their_sites_builtin_checks_run(pool: PgPool) {
        let f = fleet(&pool).await;
        let operator = session(&pool, "paris-op", Role::Operator, Some(f.paris)).await;
        let viewer = session(&pool, "paris-viewer", Role::Viewer, Some(f.paris)).await;
        let uri = format!("/api/sites/{}/check-interval", f.paris);
        let intervals = |body: &Value| -> Vec<(String, u64)> {
            body["sensors"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| (s["name"].as_str().unwrap().to_owned(), s["interval_ms"].as_u64().unwrap()))
                .collect()
        };

        // The built-in interval until the site sets one.
        let (_, body) = call(&pool, &viewer, "GET", &format!("/api/sites/{}/sensors", f.paris), None).await;
        assert_eq!((body["check_interval_ms"].clone(), body["check_interval_origin"].clone()), (json!(60_000), json!("default")));

        // A custom check keeps its own interval.
        let (status, _) = call(
            &pool, &operator, "POST", &format!("/api/sites/{}/sensor_configs", f.paris),
            Some(json!({ "kind": "port", "name": "port_443", "interval_ms": 30_000, "params": { "port": 443 } })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let rev = revision(&pool, f.paris_server).await;
        let (status, body) = call(&pool, &operator, "PUT", &uri, Some(json!({ "interval_ms": 3_600_000 }))).await;
        assert_eq!((status, body), (StatusCode::OK, json!({ "interval_ms": 3_600_000 })));
        assert!(revision(&pool, f.paris_server).await > rev);
        assert_eq!(revision(&pool, f.lyon_server).await, 0);

        let (_, body) = call(&pool, &viewer, "GET", &format!("/api/servers/{}/sensors", f.paris_server), None).await;
        assert_eq!((body["check_interval_ms"].clone(), body["check_interval_origin"].clone()), (json!(3_600_000), json!("site")));
        let hourly = |name: &str| (name.to_owned(), 3_600_000);
        assert_eq!(
            intervals(&body),
            [hourly("cpu"), hourly("disk"), hourly("diskio"), hourly("memory"), hourly("net"), ("port_443".to_owned(), 30_000)]
        );
        let (_, body) = call(&pool, &viewer, "GET", &format!("/api/sites/{}/sensors", f.paris), None).await;
        assert_eq!(body["check_interval_ms"], 3_600_000);
        let (_, body) = call(&pool, &viewer, "GET", &format!("/api/servers/{}/sensors", f.lyon_server), None).await;
        assert_eq!(body, json!({ "error": "not found", "code": "not_found" }));

        // Out of bounds, unknown fields.
        for (bad, expected) in [
            (json!({ "interval_ms": 5_000 }), "interval must be between 10 s and 24 h"),
            (json!({ "interval_ms": 86_400_001u64 }), "interval must be between 10 s and 24 h"),
            (json!({ "interval_ms": 60_000, "site_id": "x" }), "unknown field `site_id`"),
        ] {
            let (status, body) = call(&pool, &operator, "PUT", &uri, Some(bad.clone())).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
            assert!(body["error"].as_str().unwrap().contains(expected), "{body}");
        }

        // Viewers may not; another site's operator does not see it.
        let (status, _) = call(&pool, &viewer, "PUT", &uri, Some(json!({ "interval_ms": 30_000 }))).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let lyon_op = session(&pool, "lyon-op", Role::Operator, Some(f.lyon)).await;
        for uri in [uri.clone(), format!("/api/sites/{}/check-interval", Uuid::new_v4()), "/api/sites/x/check-interval".into()] {
            let (status, _) = call(&pool, &lyon_op, "PUT", &uri, Some(json!({ "interval_ms": 30_000 }))).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
        }

        // Back to the built-in interval.
        let (status, body) = call(&pool, &operator, "PUT", &uri, Some(json!({ "interval_ms": null }))).await;
        assert_eq!((status, body), (StatusCode::OK, json!({ "interval_ms": null })));
        let (_, body) = call(&pool, &viewer, "GET", &format!("/api/sites/{}/sensors", f.paris), None).await;
        assert_eq!(body["check_interval_origin"], "default");

        // Audited with the value before and after.
        let rows: Vec<(String, Option<String>, Value)> = sqlx::query_as(
            "SELECT action, target_id, details FROM audit_log WHERE action = 'site.check_interval' ORDER BY id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        let site = f.paris.to_string();
        assert_eq!(
            rows,
            [
                ("site.check_interval".to_owned(), Some(site.clone()), json!({ "before": null, "after": 3_600_000 })),
                ("site.check_interval".to_owned(), Some(site), json!({ "before": 3_600_000, "after": null })),
            ]
        );
    }

    #[sqlx::test]
    async fn recommendations_follow_the_report_and_the_checks(pool: PgPool) {
        let f = fleet(&pool).await;
        let uri = format!("/api/servers/{}/recommendations", f.paris_server);
        let viewer = session(&pool, "paris-viewer", Role::Viewer, Some(f.paris)).await;
        let operator = session(&pool, "paris-op", Role::Operator, Some(f.paris)).await;

        // No report yet.
        let (status, body) = call(&pool, &viewer, "GET", &uri, None).await;
        assert_eq!((status, body), (StatusCode::OK, json!({ "reported_at": null, "recommendations": [] })));

        sqlx::query(
            "INSERT INTO server_discovery (server_id, report, reported_at)
             VALUES ($1, $2, now())",
        )
        .bind(f.paris_server)
        .bind(json!({
            "services": ["postgresql@16-main.service"],
            "ports": [{ "port": 5432, "loopback_only": false, "local": true }],
            "mounts": [],
        }))
        .execute(&pool)
        .await
        .unwrap();
        let (status, body) = call(&pool, &viewer, "GET", &uri, None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body["reported_at"].is_string());
        assert_eq!(
            body["recommendations"],
            json!([{
                "key": "postgresql",
                "title": "PostgreSQL is running",
                "evidence": ["postgresql@16-main.service is running", "port 5432 is listening"],
                "checks": [
                    { "kind": "systemd", "params": { "units": "postgresql@16-main.service" } },
                    { "kind": "port", "params": { "port": "5432", "host": "127.0.0.1" } },
                ],
                "dismissed_by": null,
                "dismissed_at": null,
            }])
        );

        // Lyon's people do not see it; viewers cannot dismiss.
        let lyon = session(&pool, "lyon-op", Role::Operator, Some(f.lyon)).await;
        let (status, _) = call(&pool, &lyon, "GET", &uri, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let key = json!({ "key": "postgresql" });
        let (status, _) = call(&pool, &viewer, "POST", &format!("{uri}/dismiss"), Some(key.clone())).await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        // Only a current recommendation is dismissed.
        let (status, _) = call(&pool, &operator, "POST", &format!("{uri}/dismiss"), Some(json!({ "key": "redis" }))).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = call(&pool, &operator, "POST", &format!("{uri}/dismiss"), Some(key.clone())).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, body) = call(&pool, &viewer, "GET", &uri, None).await;
        assert_eq!(body["recommendations"][0]["dismissed_by"], "paris-op");
        let (status, _) = call(&pool, &operator, "POST", &format!("{uri}/restore"), Some(key.clone())).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = call(&pool, &operator, "POST", &format!("{uri}/restore"), Some(key)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let actions: Vec<(String, Option<String>, Value)> = sqlx::query_as(
            "SELECT action, target_id, details FROM audit_log
             WHERE action LIKE 'check_recommendation.%' ORDER BY id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        let server = Some(f.paris_server.to_string());
        assert_eq!(
            actions,
            [
                ("check_recommendation.dismiss".to_owned(), server.clone(), json!({ "key": "postgresql" })),
                ("check_recommendation.restore".to_owned(), server, json!({ "key": "postgresql" })),
            ]
        );

        // The site checks the port and the server the service: nothing left.
        for (owner, row) in [
            (format!("/api/sites/{}/sensor_configs", f.paris), json!({ "kind": "port", "name": "port_5432", "interval_ms": 60_000, "params": { "port": 5432 } })),
            (
                format!("/api/servers/{}/sensor_configs", f.paris_server),
                json!({ "kind": "systemd", "name": "service_pg", "interval_ms": 60_000, "params": { "units": "postgresql@16-main.service" } }),
            ),
        ] {
            let (status, body) = call(&pool, &operator, "POST", &owner, Some(row)).await;
            assert_eq!(status, StatusCode::CREATED, "{body}");
        }
        let (_, body) = call(&pool, &viewer, "GET", &uri, None).await;
        assert_eq!(body["recommendations"], json!([]));
    }

    #[sqlx::test]
    async fn usage_counts_servers_over_the_sites_read(pool: PgPool) {
        let f = fleet(&pool).await;
        // A second Paris server, and a removed one that counts for nothing.
        for (peer, revoked) in [("agent-paris-2", false), ("agent-paris-old", true)] {
            sqlx::query(
                "INSERT INTO servers (peer_id, site_id, hostname, os_family, revoked_at)
                 VALUES ($1, $2, $1, 'debian', CASE WHEN $3 THEN now() END)",
            )
            .bind(peer)
            .bind(f.paris)
            .bind(revoked)
            .execute(&pool)
            .await
            .unwrap();
        }
        let insert = |site: Option<Uuid>, server: Option<Uuid>, name: &str, enabled: bool| {
            let pool = pool.clone();
            let name = name.to_owned();
            async move {
                sqlx::query(
                    "INSERT INTO sensor_configs (site_id, server_id, kind, name, interval_ms, params, enabled)
                     VALUES ($1, $2, 'port', $3, 60000, '{\"port\": 22}', $4)",
                )
                .bind(site)
                .bind(server)
                .bind(name)
                .bind(enabled)
                .execute(&pool)
                .await
                .unwrap();
            }
        };
        insert(Some(f.paris), None, "port_22", true).await;
        insert(None, Some(f.paris_server), "port_5432", true).await;
        insert(None, Some(f.paris_server), "port_8080", false).await;
        insert(None, Some(f.lyon_server), "port_5432", true).await;
        insert(Some(f.lyon), None, "port_443", true).await;

        let usage = |cookie: String| {
            let pool = pool.clone();
            async move {
                let (status, body) = call(&pool, &cookie, "GET", "/api/checks/usage", None).await;
                assert_eq!(status, StatusCode::OK);
                body["checks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|c| (c["name"].as_str().unwrap().to_owned(), c["servers"].as_i64().unwrap()))
                    .collect::<Vec<_>>()
            }
        };
        let paris = session(&pool, "paris-viewer", Role::Viewer, Some(f.paris)).await;
        assert_eq!(usage(paris).await, [("port_22".to_owned(), 2), ("port_5432".to_owned(), 1)]);
        let admin = session(&pool, "admin", Role::Administrator, None).await;
        assert_eq!(
            usage(admin).await,
            [("port_22".to_owned(), 2), ("port_5432".to_owned(), 2), ("port_443".to_owned(), 1)]
        );
    }

    #[sqlx::test]
    async fn website_checks_keep_their_password_sealed(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let f = fleet(&pool).await;
        let operator = session(&pool, "op", Role::Operator, Some(f.paris)).await;
        let uri = format!("/api/servers/{}/sensor_configs", f.paris_server);
        let row = |params: Value| {
            json!({"kind": "http", "instance": "shop", "interval_ms": 60_000, "params": params})
        };

        // Never in clear on the network, never over a plain URL.
        let (status, body) = call(&pool, &operator, "POST", &uri, Some(row(json!({
            "url": "http://shop.test/", "username": "monitor", "password": "s3cret"
        })))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body["error"].as_str().unwrap().contains("https"), "{body}");

        let (status, body) = call(&pool, &operator, "POST", &uri, Some(row(json!({
            "url": "https://shop.test/", "username": "monitor", "password": "s3cret",
            "contains": "Add to cart"
        })))).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body["secret_set"], true);
        assert!(body["params"].get("password").is_none(), "{body}");
        assert!(!body.to_string().contains("s3cret"));
        let id = body["id"].as_str().unwrap().to_owned();
        let stored: String = sqlx::query_scalar("SELECT secret FROM sensor_configs WHERE id = $1::uuid")
            .bind(&id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(stored.starts_with("mfs1:") && !stored.contains("s3cret"), "{stored}");

        // The agent gets it in clear, over its mTLS stream.
        let store = SensorConfigStore::new(pool.clone());
        let config = store.load("agent-paris").await.unwrap().unwrap();
        let check = config.sensors.iter().find(|s| s.kind == "http").unwrap();
        assert_eq!(check.params["password"], "s3cret");
        assert_eq!(check.params["username"], "monitor");

        // An update without it keeps it; "" removes it.
        let (status, body) = call(&pool, &operator, "PUT", &format!("/api/sensor_configs/{id}"),
            Some(row(json!({"url": "https://shop.test/", "username": "monitor"})))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["secret_set"], true);
        let config = store.load("agent-paris").await.unwrap().unwrap();
        let check = config.sensors.iter().find(|s| s.kind == "http").unwrap();
        assert_eq!(check.params["password"], "s3cret");
        // A kept password still needs https.
        let (status, _) = call(&pool, &operator, "PUT", &format!("/api/sensor_configs/{id}"),
            Some(row(json!({"url": "http://shop.test/", "username": "monitor"})))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        // Nor does it follow the check to another host or port.
        for url in ["https://other.test/", "https://shop.test:8443/"] {
            let (status, body) = call(&pool, &operator, "PUT", &format!("/api/sensor_configs/{id}"),
                Some(row(json!({"url": url, "username": "monitor"})))).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            assert!(body["error"].as_str().unwrap().contains("password"), "{body}");
        }
        let (status, body) = call(&pool, &operator, "PUT", &format!("/api/sensor_configs/{id}"),
            Some(row(json!({"url": "https://shop.test/cart", "username": "monitor"})))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["secret_set"], true);
        let (status, body) = call(&pool, &operator, "PUT", &format!("/api/sensor_configs/{id}"),
            Some(row(json!({"url": "https://shop.test/", "password": ""})))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["secret_set"], false);

        // Lists, the effective view and the audit log never show it.
        let (_, list) = call(&pool, &operator, "GET", &uri, None).await;
        let (_, effective) = call(&pool, &operator, "GET",
            &format!("/api/servers/{}/sensors", f.paris_server), None).await;
        for text in [list.to_string(), effective.to_string()] {
            assert!(!text.contains("s3cret"), "{text}");
        }
        for (_, _, details) in audit(&pool).await {
            assert!(!details.to_string().contains("s3cret"), "{details}");
        }
    }

    #[sqlx::test]
    async fn website_checks_run_by_a_probe_get_their_password_there(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let f = fleet(&pool).await;
        let operator = session(&pool, "op", Role::Operator, Some(f.paris)).await;
        let (status, body) = call(&pool, &operator, "POST",
            &format!("/api/servers/{}/sensor_configs", f.paris_server),
            Some(json!({"kind": "http", "instance": "shop", "interval_ms": 60_000,
                        "params": {"url": "https://shop.test/", "password": "t0ken"},
                        "probe": "central"}))).await;
        // Who runs it is the row's probe (ADR-0148).
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body["probe"], "central");

        let config = SensorConfigStore::new(pool.clone()).load("agent-paris").await.unwrap().unwrap();
        assert!(config.sensors.iter().all(|s| s.kind != "http"), "{:?}", config.sensors);
        let sets = ProbeStore::new(pool.clone()).load().await.unwrap();
        let check = &sets["central"].checks[0];
        assert_eq!(check.kind, "http");
        assert_eq!(check.config.params["password"], "t0ken");
        assert!(!check.config.params.contains_key("host"));

        // Without checks there is nothing to read; with one, the metrics
        // database answers (none here).
        let (status, body) = call(&pool, &operator, "GET",
            &format!("/api/servers/{}/website-checks", f.lyon_server), None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        let (status, _) = call(&pool, &operator, "GET",
            &format!("/api/servers/{}/website-checks", f.paris_server), None).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
    }
}
