//! Threshold endpoints against a real PostgreSQL: site scoping, validation,
//! audit rows and the revision the engine reloads on. Ignored without
//! `DATABASE_URL`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use monitorflow_common::redact::Secret;
use monitorflow_server::alerting::AlertStore;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users;
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery";
const DISK: &str = "monitorflow_disk_used_ratio";
const MEMORY: &str = "monitorflow_memory_available_ratio";
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
         WHERE action LIKE 'threshold.%' ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

/// The revision the engine polls for `server`.
async fn revision(pool: &PgPool, server: Uuid) -> u64 {
    let revisions = AlertStore::new(pool.clone())
        .revisions(&[server])
        .await
        .unwrap();
    revisions[0].1
}

/// The engine's tables stay untouched by the API.
async fn assert_engine_tables_empty(pool: &PgPool) {
    let count: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM incidents) + (SELECT count(*) FROM incident_events)
                + (SELECT count(*) FROM alert_states)",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(count, 0);
}

fn origins(body: &Value) -> Vec<(String, Value, String, bool)> {
    body["thresholds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            (
                t["metric"].as_str().unwrap().to_owned(),
                t["instance"].clone(),
                t["origin"].as_str().unwrap().to_owned(),
                t["enabled"].as_bool().unwrap(),
            )
        })
        .collect()
}

db_tests! {
    #[sqlx::test]
    async fn viewers_read_their_sites_only(pool: PgPool) {
        let f = fleet(&pool).await;
        let viewer = session(&pool, "paris-viewer", Role::Viewer, Some(f.paris)).await;

        let uri = format!("/api/servers/{}/thresholds/effective", f.paris_server);
        let (status, body) = call(&pool, &viewer, "GET", &uri, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["site_id"], json!(f.paris));
        assert_eq!(body["revision"], 0);
        let view: Vec<_> = body["thresholds"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| (t["metric"].as_str().unwrap(), t["origin"].as_str().unwrap(), t["threshold_id"].clone()))
            .collect();
        assert_eq!(
            view,
            [
                ("monitorflow_cpu_usage_ratio", "default", Value::Null),
                (DISK, "default", Value::Null),
                (MEMORY, "default", Value::Null),
            ]
        );
        assert_eq!(body["thresholds"][1]["critical"], 0.95);

        // Lyon's resources do not exist for a Paris viewer.
        for uri in [
            format!("/api/servers/{}/thresholds/effective", f.lyon_server),
            format!("/api/servers/{}/thresholds", f.lyon_server),
            format!("/api/sites/{}/thresholds", f.lyon),
            format!("/api/servers/{}/thresholds/effective", Uuid::new_v4()),
            "/api/sites/not-a-uuid/thresholds".to_owned(),
        ] {
            let (status, body) = call(&pool, &viewer, "GET", &uri, None).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
            assert_eq!(body, json!({ "error": "not found" }));
        }

        let (status, body) =
            call(&pool, &viewer, "GET", &format!("/api/sites/{}/thresholds", f.paris), None).await;
        assert_eq!((status, body), (StatusCode::OK, json!({ "entries": [] })));
        let threshold = json!({ "metric": DISK, "comparison": ">", "warning": 0.5 });
        let (status, _) = call(
            &pool, &viewer, "POST", &format!("/api/sites/{}/thresholds", f.paris), Some(threshold.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = call(
            &pool, &viewer, "POST", &format!("/api/servers/{}/thresholds", f.lyon_server), Some(threshold),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(audit(&pool).await.is_empty());
        assert_eq!(revision(&pool, f.paris_server).await, 0);
    }

    #[sqlx::test]
    async fn operators_change_rows_with_audit_and_revision_bumps(pool: PgPool) {
        let f = fleet(&pool).await;
        let operator = session(&pool, "paris-op", Role::Operator, Some(f.paris)).await;
        let store = AlertStore::new(pool.clone());
        let rev0 = revision(&pool, f.paris_server).await;

        // A site default, a server override of it, and a built-in default
        // muted for the site.
        let (status, site_row) = call(
            &pool, &operator, "POST", &format!("/api/sites/{}/thresholds", f.paris),
            Some(json!({ "metric": DISK, "comparison": ">", "warning": 0.7, "critical": 0.9,
                         "duration_s": 120, "hysteresis": 0.01 })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{site_row}");
        assert_eq!(site_row["site_id"], json!(f.paris));
        assert_eq!(site_row["server_id"], Value::Null);
        assert_eq!(site_row["instance"], Value::Null);
        let rev1 = revision(&pool, f.paris_server).await;
        assert!(rev1 > rev0);

        let (status, server_row) = call(
            &pool, &operator, "POST", &format!("/api/servers/{}/thresholds", f.paris_server),
            Some(json!({ "metric": DISK, "instance": "/data", "comparison": ">=", "critical": 0.99 })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{server_row}");
        let rev2 = revision(&pool, f.paris_server).await;
        assert!(rev2 > rev1);
        let (status, mute) = call(
            &pool, &operator, "POST", &format!("/api/sites/{}/thresholds", f.paris),
            Some(json!({ "metric": MEMORY, "comparison": "<", "enabled": false })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{mute}");
        assert!(revision(&pool, f.paris_server).await > rev2);
        // Lyon's server is untouched.
        assert_eq!(revision(&pool, f.lyon_server).await, 0);

        // The engine's load sees what the API wrote.
        let loaded = store.load_rules("agent-paris").await.unwrap().unwrap();
        assert_eq!(loaded.revision, revision(&pool, f.paris_server).await);
        assert_eq!(loaded.rules.lookup(DISK, "/").unwrap().warning, Some(0.7));
        assert_eq!(loaded.rules.lookup(DISK, "/data").unwrap().critical, Some(0.99));
        assert!(loaded.rules.lookup(MEMORY, "").is_none());

        let (_, body) = call(
            &pool, &operator, "GET", &format!("/api/servers/{}/thresholds/effective", f.paris_server), None,
        )
        .await;
        assert_eq!(body["revision"], json!(loaded.revision));
        let with_origin = |metric: &str, instance: Value, origin: &str, enabled: bool| {
            (metric.to_owned(), instance, origin.to_owned(), enabled)
        };
        assert_eq!(
            origins(&body),
            [
                with_origin("monitorflow_cpu_usage_ratio", json!("total"), "default", true),
                with_origin(DISK, Value::Null, "site", true),
                with_origin(DISK, json!("/data"), "server", true),
                with_origin(MEMORY, Value::Null, "site", false),
            ]
        );
        assert_eq!(body["thresholds"][1]["threshold_id"], site_row["id"]);
        assert_eq!(body["thresholds"][3]["threshold_id"], mute["id"]);

        // Same metric and instance twice for one owner.
        let (status, body) = call(
            &pool, &operator, "POST", &format!("/api/sites/{}/thresholds", f.paris),
            Some(json!({ "metric": DISK, "comparison": ">", "warning": 0.6 })),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(
            body["error"],
            "a threshold on `monitorflow_disk_used_ratio` for every instance already exists here"
        );

        // Update, then delete; each bumps the revision.
        let rev3 = revision(&pool, f.paris_server).await;
        let uri = format!("/api/thresholds/{}", site_row["id"].as_str().unwrap());
        let (status, updated) = call(
            &pool, &operator, "PUT", &uri,
            Some(json!({ "metric": DISK, "comparison": ">", "warning": 0.75 })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{updated}");
        assert_eq!(updated["critical"], Value::Null);
        assert_eq!(updated["duration_s"], 0);
        let rev4 = revision(&pool, f.paris_server).await;
        assert!(rev4 > rev3);
        let (status, _) = call(&pool, &operator, "DELETE", &uri, None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(revision(&pool, f.paris_server).await > rev4);
        let (status, _) = call(&pool, &operator, "DELETE", &uri, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (status, list) =
            call(&pool, &operator, "GET", &format!("/api/sites/{}/thresholds", f.paris), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(list["entries"].as_array().unwrap().len(), 1);

        let rows = audit(&pool).await;
        let actions: Vec<_> = rows.iter().map(|r| r.0.as_str()).collect();
        assert_eq!(
            actions,
            ["threshold.create", "threshold.create", "threshold.create", "threshold.update", "threshold.delete"]
        );
        assert_eq!(rows[0].1.as_deref(), site_row["id"].as_str());
        assert_eq!(rows[0].2["warning"], 0.7);
        assert_eq!(rows[1].2["server_id"], json!(f.paris_server));
        assert_eq!(rows[2].2["enabled"], false);
        assert_eq!(rows[3].2["before"]["warning"], 0.7);
        assert_eq!(rows[3].2["after"]["warning"], 0.75);
        assert_eq!(rows[4].2["metric"], DISK);
        assert_engine_tables_empty(&pool).await;
    }

    #[sqlx::test]
    async fn other_sites_rows_cannot_be_changed(pool: PgPool) {
        let f = fleet(&pool).await;
        let lyon_row: Uuid = sqlx::query_scalar(
            "INSERT INTO thresholds (server_id, metric, comparison, warning)
             VALUES ($1, $2, '>', 0.5) RETURNING id",
        )
        .bind(f.lyon_server)
        .bind(DISK)
        .fetch_one(&pool)
        .await
        .unwrap();
        let rev = revision(&pool, f.lyon_server).await;
        let body = json!({ "metric": DISK, "comparison": ">", "warning": 0.6 });
        let operator = session(&pool, "paris-op", Role::Operator, Some(f.paris)).await;
        let uri = format!("/api/thresholds/{lyon_row}");
        let (status, _) = call(&pool, &operator, "PUT", &uri, Some(body.clone())).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = call(&pool, &operator, "DELETE", &uri, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // A global viewer sees it but may not change it.
        let viewer = session(&pool, "viewer", Role::Viewer, None).await;
        let (status, list) =
            call(&pool, &viewer, "GET", &format!("/api/servers/{}/thresholds", f.lyon_server), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(list["entries"][0]["id"], json!(lyon_row));
        let (status, _) = call(&pool, &viewer, "DELETE", &uri, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(revision(&pool, f.lyon_server).await, rev);
        // A global administrator may.
        let admin = session(&pool, "admin", Role::Administrator, None).await;
        let (status, _) = call(&pool, &admin, "PUT", &uri, Some(body)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(revision(&pool, f.lyon_server).await > rev);
        assert_eq!(revision(&pool, f.paris_server).await, 0);
        assert_eq!(audit(&pool).await.len(), 1);
    }

    #[sqlx::test]
    async fn invalid_rows_are_refused_with_a_reason(pool: PgPool) {
        let f = fleet(&pool).await;
        let operator = session(&pool, "op", Role::Operator, None).await;
        let uri = format!("/api/servers/{}/thresholds", f.paris_server);
        let cases = [
            (json!({ "metric": "node_load1", "comparison": ">", "warning": 1 }), "metric must be `monitorflow_`"),
            (json!({ "metric": DISK, "comparison": "!=", "warning": 1 }), "comparison must be one of"),
            (json!({ "metric": DISK, "comparison": ">" }), "a warning or a critical value is required"),
            (json!({ "metric": DISK, "comparison": ">", "warning": 0.9, "critical": 0.5 }), "critical must not be below warning"),
            (json!({ "metric": DISK, "comparison": "<=", "warning": 0.1, "critical": 0.5 }), "critical must not be above warning"),
            (json!({ "metric": DISK, "comparison": ">", "warning": 1, "duration_s": 90_000 }), "duration_s must be between 0 and 86400"),
            (json!({ "metric": DISK, "comparison": ">", "warning": 1, "duration_s": -1 }), "duration_s"),
            (json!({ "metric": DISK, "comparison": ">", "warning": 1, "hysteresis": -1 }), "hysteresis must be"),
            (json!({ "metric": DISK, "comparison": ">", "warning": "high" }), "warning"),
            (json!({ "comparison": ">", "warning": 1 }), "missing field `metric`"),
            (json!({ "metric": DISK, "comparison": ">", "warning": 1, "site_id": "x" }), "unknown field `site_id`"),
        ];
        for (body, expected) in cases {
            let (status, response) = call(&pool, &operator, "POST", &uri, Some(body.clone())).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            let message = response["error"].as_str().unwrap();
            assert!(message.contains(expected), "{body}: {message}");
        }
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM thresholds")
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
        let viewer = session(&pool, "paris-viewer", Role::Viewer, Some(f.paris)).await;
        let operator = session(&pool, "paris-op", Role::Operator, Some(f.paris)).await;
        let site_row = json!({ "metric": DISK, "comparison": ">", "warning": 0.7, "critical": 0.9 });
        let (status, created) = call(
            &pool, &operator, "POST", &format!("/api/sites/{}/thresholds", f.paris), Some(site_row),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        // A server row does not show in the site's view.
        let (status, _) = call(
            &pool, &operator, "POST", &format!("/api/servers/{}/thresholds", f.paris_server),
            Some(json!({ "metric": MEMORY, "comparison": "<", "enabled": false })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let uri = format!("/api/sites/{}/thresholds/effective", f.paris);
        let (status, body) = call(&pool, &viewer, "GET", &uri, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["site_id"], json!(f.paris));
        assert_eq!(
            origins(&body),
            [
                ("monitorflow_cpu_usage_ratio".to_owned(), json!("total"), "default".to_owned(), true),
                (DISK.to_owned(), Value::Null, "site".to_owned(), true),
                (MEMORY.to_owned(), Value::Null, "default".to_owned(), true),
            ]
        );
        assert_eq!(body["thresholds"][1]["threshold_id"], created["id"]);
        assert_eq!(body["thresholds"][1]["warning"], 0.7);

        for uri in [
            format!("/api/sites/{}/thresholds/effective", f.lyon),
            format!("/api/sites/{}/thresholds/effective", Uuid::new_v4()),
        ] {
            let (status, _) = call(&pool, &viewer, "GET", &uri, None).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
        }
    }
}
