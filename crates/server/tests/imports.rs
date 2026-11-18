//! Imports from another monitoring tool (ADR-0146) against PostgreSQL:
//! a planned preview, its confirmation through the API, a second import
//! of the same configuration, the undo, and who may do what. Ignored
//! without `DATABASE_URL`.

use std::collections::HashSet;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{TimeDelta, Utc};
use monitorflow_common::redact::Secret;
use monitorflow_import::model::{
    Alert, Channel, Check, Direction, Fidelity, Host, Inventory, Measure, Origin, Os, Repeat, Rule,
    Severity, Silence, SilenceScope, Smtp, Target, Threshold, Watch, Window, EVERY_MONTH,
};
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
use monitorflow_server::import::{self, store};
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

async fn call(
    pool: &PgPool,
    method: &str,
    uri: &str,
    cookie: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let state = ApiState::new(pool.clone(), Registry::default(), ApiSettings::default());
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header(CSRF_HEADER, "1")
        .header("content-type", "application/json")
        .header("cookie", cookie)
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let response = api::router(state).oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 16 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn login(pool: &PgPool, name: &str, role: Role, scope: GrantScope) -> (String, Uuid) {
    let id = users::create_user_with(pool, name, &Secret::new(PASSWORD), role, scope, "test")
        .await
        .unwrap();
    let state = ApiState::new(pool.clone(), Registry::default(), ApiSettings::default());
    let request = Request::builder()
        .method("POST")
        .uri("/api/auth/login")
        .header(CSRF_HEADER, "1")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"username": name, "password": PASSWORD}).to_string(),
        ))
        .unwrap();
    let response = api::router(state).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    (cookie, id)
}

async fn site(pool: &PgPool, name: &str) -> Uuid {
    sqlx::query_scalar("INSERT INTO sites (name) VALUES ($1) RETURNING id")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap()
}

fn origin(kind: &str, name: &str) -> Origin {
    Origin::new(kind, name)
}

/// Two hosts with a service check and its alert, a CPU threshold, a
/// maintenance and two channels.
fn inventory() -> Inventory {
    let host = |key: &str, name: &str| Host {
        key: key.into(),
        name: name.into(),
        visible_name: None,
        addresses: vec![],
        os: Os::Debian,
        groups: vec![],
        origin: origin("host", name),
    };
    // One time for every call, so two imports read the same windows.
    static NOW: std::sync::OnceLock<chrono::DateTime<Utc>> = std::sync::OnceLock::new();
    let now = *NOW.get_or_init(Utc::now);
    Inventory {
        hosts: vec![host("1", "web-01"), host("2", "db-01")],
        checks: vec![
            Check {
                host: "1".into(),
                watch: Watch::Service {
                    name: "nginx".into(),
                },
                interval_s: Some(30),
                alert: Some(Alert {
                    severity: Severity::Critical,
                    for_s: 60,
                }),
                origin: origin("item", "nginx running"),
                fidelity: Fidelity::Exact,
            },
            Check {
                host: "2".into(),
                watch: Watch::Port {
                    address: None,
                    port: 5432,
                },
                interval_s: None,
                alert: Some(Alert {
                    severity: Severity::Critical,
                    for_s: 0,
                }),
                origin: origin("item", "PostgreSQL port"),
                fidelity: Fidelity::Exact,
            },
        ],
        thresholds: vec![Threshold {
            hosts: vec!["1".into(), "2".into()],
            measure: Measure::CpuUsage,
            direction: Direction::Above,
            warning: Some(0.8),
            critical: Some(0.95),
            for_s: 300,
            recovery: None,
            origin: origin("trigger", "High CPU"),
            fidelity: Fidelity::Exact,
        }],
        silences: vec![Silence {
            scope: SilenceScope::Hosts(vec!["2".into()]),
            checks: vec![],
            starts_at: now + TimeDelta::hours(1),
            ends_at: now + TimeDelta::hours(3),
            reason: "Database upgrade".into(),
            origin: origin("maintenance", "Database upgrade"),
            fidelity: Fidelity::Exact,
        }],
        channels: vec![
            Channel {
                name: "Ops".into(),
                target: Target::Email {
                    to: vec!["ops@example.com".into()],
                    smtp: Some(Smtp {
                        host: "smtp.example.com".into(),
                        port: 587,
                        username: "alerts@example.com".into(),
                        from: String::new(),
                        password: Some(Secret::new("mail-password")),
                    }),
                },
                origin: origin("media_type", "Email"),
                fidelity: Fidelity::Exact,
            },
            Channel {
                name: "Hook".into(),
                target: Target::Webhook {
                    url: Secret::new("https://hooks.example.com/t/s3cret"),
                },
                origin: origin("media_type", "Webhook"),
                fidelity: Fidelity::Exact,
            },
        ],
        windows: vec![],
        unmovable: vec![],
    }
}

/// Plans `inventory` into `site` as a global administrator would, and
/// stores the preview.
async fn preview(pool: &PgPool, site: Uuid, user: Uuid, inventory: &Inventory) -> Uuid {
    let existing = store::existing(pool, inventory, site, |_| true)
        .await
        .unwrap();
    let planned = import::plan(inventory, site, true, &existing, Utc::now());
    store::create(
        pool,
        "test",
        site,
        "export.yaml",
        &planned.items,
        &planned.secrets,
        user,
    )
    .await
    .unwrap()
}

async fn count(pool: &PgPool, sql: &'static str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
}

db_tests! {
    #[sqlx::test]
    async fn a_preview_creates_nothing_until_confirmed(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let paris = site(&pool, "paris").await;
        let (cookie, admin) = login(&pool, "alice", Role::Administrator, GrantScope::Global).await;
        let id = preview(&pool, paris, admin, &inventory()).await;
        assert_eq!(count(&pool, "SELECT count(*) FROM servers").await, 0);
        let sealed: String = sqlx::query_scalar("SELECT secrets FROM import_runs").fetch_one(&pool).await.unwrap();
        assert!(sealed.starts_with("mfs1:") && !sealed.contains("s3cret"));

        let (status, run) = call(&pool, "GET", &format!("/api/imports/{id}"), &cookie, None).await;
        assert_eq!(status, StatusCode::OK, "{run}");
        assert_eq!(run["state"], "preview");
        assert_eq!(run["site_name"], "paris");
        assert!(run["expires_at"].is_string());
        assert!(!run.to_string().contains("s3cret") && !run.to_string().contains("mail-password"));
        let ids: Vec<&str> = run["items"].as_array().unwrap().iter().map(|i| i["id"].as_str().unwrap()).collect();
        assert!(ids.contains(&"server:web-01") && ids.contains(&"check:web-01:service_nginx"), "{ids:?}");

        // Leave db-01 out: its check, threshold and silence go with it.
        let (status, run) = call(
            &pool, "POST", &format!("/api/imports/{id}/confirm"), &cookie,
            Some(json!({"skip": ["server:db-01"]})),
        ).await;
        assert_eq!(status, StatusCode::OK, "{run}");
        assert_eq!(run["state"], "confirmed");
        assert_eq!(
            run["result"]["created"],
            json!({"server": 1, "check": 1, "threshold": 2, "channel": 2}),
            "{run}"
        );
        assert_eq!(run["result"]["failed"], json!([]));
        let (peer, awaiting, family): (String, bool, String) =
            sqlx::query_as("SELECT peer_id, awaiting_agent, os_family FROM servers")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!((peer.as_str(), awaiting, family.as_str()), ("web-01", true, "debian"));
        let (kind, interval, params): (String, i64, Value) =
            sqlx::query_as("SELECT kind, interval_ms, params FROM sensor_configs")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!((kind.as_str(), interval, params), ("systemd", 30_000, json!({"units": "nginx"})));
        let thresholds: Vec<(String, Option<f64>, Option<f64>)> =
            sqlx::query_as("SELECT metric, warning, critical FROM thresholds ORDER BY metric")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            thresholds,
            vec![
                ("monitorflow_cpu_usage_ratio".into(), Some(0.8), Some(0.95)),
                ("monitorflow_service_nginx_active".into(), None, Some(1.0)),
            ]
        );
        assert_eq!(count(&pool, "SELECT count(*) FROM silences").await, 0);
        let channels: Vec<(String, bool, Option<Uuid>)> = sqlx::query_as(
            "SELECT kind, secret IS NOT NULL, site_id FROM notification_channels ORDER BY kind",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(channels, vec![("email".into(), true, None), ("webhook".into(), false, None)]);
        let url: String = sqlx::query_scalar("SELECT settings->>'url' FROM notification_channels WHERE kind = 'webhook'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(url, "https://hooks.example.com/t/s3cret");
        assert_eq!(count(&pool, "SELECT count(*) FROM import_runs WHERE secrets IS NULL").await, 1);
        assert_eq!(count(&pool, "SELECT count(*) FROM import_created").await, 6);
        let audited: Vec<String> = sqlx::query_scalar("SELECT action FROM audit_log WHERE action LIKE 'import.%'")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(audited, vec!["import.confirm"]);

        // The fleet shows it waiting for its agent; no offline alert.
        let (_, servers) = call(&pool, "GET", "/api/servers", &cookie, None).await;
        assert_eq!(servers["servers"][0]["state"], "waiting", "{servers}");
        assert_eq!(servers["servers"][0]["awaiting_agent"], true);
        let (_, summary) = call(&pool, "GET", "/api/servers/summary", &cookie, None).await;
        assert_eq!((summary["waiting"].as_u64(), summary["offline"].as_u64()), (Some(1), Some(0)));
        let watched = monitorflow_server::alerting::offline::load(&pool).await.unwrap();
        assert!(watched.is_empty());

        // Twice is refused.
        let (status, body) = call(&pool, "POST", &format!("/api/imports/{id}/confirm"), &cookie, None).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
    }

    #[sqlx::test]
    async fn importing_twice_creates_nothing_twice(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let paris = site(&pool, "paris").await;
        let (cookie, admin) = login(&pool, "alice", Role::Administrator, GrantScope::Global).await;
        let first = preview(&pool, paris, admin, &inventory()).await;
        let (status, _) = call(&pool, "POST", &format!("/api/imports/{first}/confirm"), &cookie, Some(json!({}))).await;
        assert_eq!(status, StatusCode::OK);
        let tables = "SELECT (SELECT count(*) FROM servers) + (SELECT count(*) FROM sensor_configs)
                           + (SELECT count(*) FROM thresholds) + (SELECT count(*) FROM silences)
                           + (SELECT count(*) FROM notification_channels)";
        let before = count(&pool, tables).await;
        assert_eq!(before, 2 + 2 + 4 + 1 + 2);

        let second = preview(&pool, paris, admin, &inventory()).await;
        let (_, run) = call(&pool, "GET", &format!("/api/imports/{second}"), &cookie, None).await;
        let creates: Vec<&Value> = run["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|i| i["create"].is_object() && i["present"] == false)
            .collect();
        assert!(creates.is_empty(), "{creates:#?}");
        assert_eq!(run["counts"]["to_create"], 0);
        let (_, run) = call(&pool, "POST", &format!("/api/imports/{second}/confirm"), &cookie, None).await;
        assert_eq!(run["result"]["created"], json!({}), "{run}");
        assert_eq!(count(&pool, tables).await, before);
    }

    #[sqlx::test]
    async fn undo_removes_what_it_made_but_servers_an_agent_took_over(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let paris = site(&pool, "paris").await;
        let (cookie, admin) = login(&pool, "alice", Role::Administrator, GrantScope::Global).await;
        // A channel that was there before stays.
        sqlx::query("INSERT INTO notification_channels (kind, settings) VALUES ('webhook', '{\"url\": \"https://other.example.com/\"}')")
            .execute(&pool)
            .await
            .unwrap();
        let id = preview(&pool, paris, admin, &inventory()).await;
        call(&pool, "POST", &format!("/api/imports/{id}/confirm"), &cookie, None).await;
        // web-01's agent arrived.
        sqlx::query("UPDATE servers SET awaiting_agent = false, last_seen_at = now() WHERE peer_id = 'web-01'")
            .execute(&pool)
            .await
            .unwrap();
        let (status, run) = call(&pool, "POST", &format!("/api/imports/{id}/undo"), &cookie, None).await;
        assert_eq!(status, StatusCode::OK, "{run}");
        assert_eq!(run["state"], "undone");
        assert_eq!(run["result"]["kept_servers"], json!(["web-01"]));
        assert_eq!(run["result"]["removed"]["server"], 1);
        assert_eq!(run["result"]["removed"]["channel"], 2);
        let servers: Vec<String> = sqlx::query_scalar("SELECT peer_id FROM servers").fetch_all(&pool).await.unwrap();
        assert_eq!(servers, vec!["web-01"]);
        // web-01 keeps its check and both thresholds; db-01's went with it.
        assert_eq!(count(&pool, "SELECT count(*) FROM sensor_configs").await, 1);
        assert_eq!(count(&pool, "SELECT count(*) FROM thresholds").await, 2);
        assert_eq!(count(&pool, "SELECT count(*) FROM silences").await, 0);
        assert_eq!(count(&pool, "SELECT count(*) FROM notification_channels").await, 1);
        let (status, _) = call(&pool, "POST", &format!("/api/imports/{id}/undo"), &cookie, None).await;
        assert_eq!(status, StatusCode::CONFLICT);
        let audited: i64 = count(&pool, "SELECT count(*) FROM audit_log WHERE action = 'import.undo'").await;
        assert_eq!(audited, 1);
    }

    #[sqlx::test]
    async fn only_administrators_of_the_site_see_and_confirm(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let paris = site(&pool, "paris").await;
        let lyon = site(&pool, "lyon").await;
        let (_, admin) = login(&pool, "alice", Role::Administrator, GrantScope::Global).await;
        let (lyon_admin, _) = login(&pool, "bob", Role::Administrator, GrantScope::Site(lyon)).await;
        let (operator, _) = login(&pool, "carol", Role::Operator, GrantScope::Site(paris)).await;
        let id = preview(&pool, paris, admin, &inventory()).await;
        for cookie in [&lyon_admin, &operator] {
            let (status, _) = call(&pool, "GET", &format!("/api/imports/{id}"), cookie, None).await;
            assert_eq!(status, StatusCode::NOT_FOUND);
            let (status, _) = call(&pool, "POST", &format!("/api/imports/{id}/confirm"), cookie, None).await;
            assert_eq!(status, StatusCode::NOT_FOUND);
        }
        let (status, list) = call(&pool, "GET", "/api/imports", &lyon_admin, None).await;
        assert_eq!((status, list["imports"].as_array().map(Vec::len)), (StatusCode::OK, Some(0)));
        let (status, _) = call(&pool, "GET", "/api/imports", &operator, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        // Reading into a site one does not administer.
        let request = json!({"source": "test", "site_id": paris, "files": [{"name": "a.yaml", "content": "x"}]});
        let (status, _) = call(&pool, "POST", "/api/imports", &lyon_admin, Some(request)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = call(&pool, "POST", "/api/imports", &operator, Some(json!({"source": "x", "site_id": paris}))).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        // Unknown source, and neither files nor a connection.
        let (status, body) = call(&pool, "POST", "/api/imports", &lyon_admin, Some(json!({"source": "nothing"}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        let (status, sources) = call(&pool, "GET", "/api/imports/sources", &lyon_admin, None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(sources["sources"].is_array());
    }

    #[sqlx::test]
    async fn an_old_preview_cannot_be_confirmed(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let paris = site(&pool, "paris").await;
        let (cookie, admin) = login(&pool, "alice", Role::Administrator, GrantScope::Global).await;
        let id = preview(&pool, paris, admin, &inventory()).await;
        sqlx::query("UPDATE import_runs SET created_at = now() - interval '25 hours'")
            .execute(&pool)
            .await
            .unwrap();
        let (status, body) = call(&pool, "POST", &format!("/api/imports/{id}/confirm"), &cookie, None).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(count(&pool, "SELECT count(*) FROM servers").await, 0);
        let skip: HashSet<String> = HashSet::new();
        assert!(store::confirm(&pool, id, &skip, admin, false, Utc::now()).await.is_err());
    }

    #[sqlx::test]
    async fn windows_and_check_silences_import_once_and_undo(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let paris = site(&pool, "paris").await;
        let (cookie, admin) = login(&pool, "alice", Role::Administrator, GrantScope::Global).await;
        let mut inventory = inventory();
        let now = Utc::now();
        inventory.silences.push(Silence {
            scope: SilenceScope::Hosts(vec!["2".into()]),
            checks: vec![Watch::Port { address: None, port: 5432 }],
            starts_at: now + TimeDelta::hours(1),
            ends_at: now + TimeDelta::hours(2),
            reason: "Database port work".into(),
            origin: origin("silence", "port work"),
            fidelity: Fidelity::Exact,
        });
        inventory.windows.push(Window {
            scope: SilenceScope::Hosts(vec!["1".into(), "2".into()]),
            checks: vec![Watch::Service { name: "nginx".into() }],
            rule: Rule {
                repeat: Repeat::Daily,
                every: 1,
                weekdays: 0,
                month_day: None,
                month_week: None,
                months: EVERY_MONTH,
                start_time: chrono::NaiveTime::from_hms_opt(3, 0, 0).unwrap(),
                duration_minutes: 60,
                time_zone: Some("Europe/Paris".into()),
                starts_on: None,
                ends_on: None,
            },
            reason: "Nightly restart".into(),
            origin: origin("maintenance", "Nightly restart"),
            fidelity: Fidelity::Exact,
        });
        let id = preview(&pool, paris, admin, &inventory).await;
        let (status, run) = call(&pool, "POST", &format!("/api/imports/{id}/confirm"), &cookie, None).await;
        assert_eq!(status, StatusCode::OK, "{run}");
        assert_eq!(run["result"]["created"]["window"], 1, "{run}");
        assert_eq!(run["result"]["failed"], json!([]));
        let (repeat, zone, checks, servers): (String, String, Value, i64) = sqlx::query_as(
            "SELECT repeat, time_zone, checks,
                    (SELECT count(*) FROM maintenance_window_servers ws WHERE ws.window_id = w.id)
             FROM maintenance_windows w",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!((repeat.as_str(), zone.as_str(), servers), ("daily", "Europe/Paris", 2));
        assert_eq!(checks, json!([{"name": "service_nginx", "instance": ""}]));
        // Its occurrences are written at once, one per server.
        assert!(count(&pool, "SELECT count(*) FROM silences WHERE window_id IS NOT NULL").await >= 2);
        let port: Value = sqlx::query_scalar("SELECT checks FROM silences WHERE reason = 'Database port work'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(port, json!([{"name": "port_5432", "instance": ""}]));

        // A second import finds both there.
        let second = preview(&pool, paris, admin, &inventory).await;
        let (_, run) = call(&pool, "GET", &format!("/api/imports/{second}"), &cookie, None).await;
        assert_eq!(run["counts"]["to_create"], 0, "{run}");

        let (status, run) = call(&pool, "POST", &format!("/api/imports/{id}/undo"), &cookie, None).await;
        assert_eq!(status, StatusCode::OK, "{run}");
        assert_eq!(run["result"]["removed"]["window"], 1);
        assert_eq!(count(&pool, "SELECT count(*) FROM maintenance_windows").await, 0);
        assert_eq!(count(&pool, "SELECT count(*) FROM silences").await, 0);
    }
}
