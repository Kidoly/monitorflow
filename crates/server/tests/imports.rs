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
    Alert, Channel, Check, Direction, Every, Fidelity, Host, Inventory, Measure, Origin, Os,
    Repeat, Rule, Scan, ScanSnmp, Severity, Silence, SilenceScope, Smtp, Target, Threshold, Watch,
    Website, Window, EVERY_MONTH,
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
        agentless: false,
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
                    no_data: false,
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
                    no_data: false,
                }),
                origin: origin("item", "PostgreSQL port"),
                fidelity: Fidelity::Exact,
            },
        ],
        thresholds: vec![Threshold {
            judged: Default::default(),
            conditions: Vec::new(),
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
        scans: vec![],
        services: vec![],
        dependencies: vec![],
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

    /// A check run from a probe is stored with the probe as its instance:
    /// a second import still finds it (seen in the real Zabbix test).
    #[sqlx::test]
    async fn probe_checks_are_found_by_a_second_import(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let paris = site(&pool, "paris").await;
        let (cookie, admin) = login(&pool, "alice", Role::Administrator, GrantScope::Global).await;
        let mut with_udp = inventory();
        with_udp.checks.push(Check {
            host: "2".into(),
            watch: Watch::UdpPort {
                address: None,
                port: 123,
                query: Some("ntp".into()),
            },
            interval_s: Some(300),
            alert: None,
            origin: origin("item", "NTP answers"),
            fidelity: Fidelity::Exact,
        });
        let first = preview(&pool, paris, admin, &with_udp).await;
        let (status, _) = call(&pool, "POST", &format!("/api/imports/{first}/confirm"), &cookie, Some(json!({}))).await;
        assert_eq!(status, StatusCode::OK);
        let probe: Option<String> = sqlx::query_scalar("SELECT probe FROM sensor_configs WHERE name = 'udp_123'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(probe.is_some(), "the UDP check runs from a probe");
        let second = preview(&pool, paris, admin, &with_udp).await;
        let (_, run) = call(&pool, "GET", &format!("/api/imports/{second}"), &cookie, None).await;
        assert_eq!(run["counts"]["to_create"], 0, "{:#}", run["items"]);
    }

    /// A host without an agent becomes a device with its probe checks,
    /// and the undo removes it (ADR-0148).
    #[sqlx::test]
    async fn devices_are_imported_and_undone(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let paris = site(&pool, "paris").await;
        let relay: Uuid = sqlx::query_scalar("INSERT INTO relays (peer_id, site_id, name) VALUES ('relay-paris', $1, 'relay-paris') RETURNING id")
            .bind(paris)
            .fetch_one(&pool)
            .await
            .unwrap();
        let (cookie, admin) = login(&pool, "alice", Role::Administrator, GrantScope::Global).await;
        let switch = Host {
            key: "sw".into(),
            name: "core-switch".into(),
            visible_name: None,
            addresses: vec!["192.0.2.2".into()],
            os: Os::Unknown,
            groups: vec![],
            origin: origin("host", "core-switch"),
            agentless: true,
        };
        let probe = |watch: Watch, name: &str| Check {
            host: "sw".into(),
            watch,
            interval_s: Some(60),
            alert: Some(Alert { severity: Severity::Critical, for_s: 60, no_data: false }),
            origin: origin("item", name),
            fidelity: Fidelity::Exact,
        };
        let inventory = Inventory {
            hosts: vec![switch],
            checks: vec![
                probe(Watch::Ping { address: None }, "ICMP ping"),
                probe(Watch::Snmp { group: "interfaces".into() }, "Interfaces"),
            ],
            ..Inventory::default()
        };
        let id = preview(&pool, paris, admin, &inventory).await;
        let (status, run) = call(&pool, "POST", &format!("/api/imports/{id}/confirm"), &cookie, None).await;
        assert_eq!(status, StatusCode::OK, "{run}");
        assert_eq!(run["result"]["failed"], json!([]), "{run}");
        let device: (String, Option<String>, bool) =
            sqlx::query_as("SELECT kind, address, awaiting_agent FROM servers WHERE peer_id = 'core-switch'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(device, ("device".into(), Some("192.0.2.2".into()), false));
        let checks: Vec<(String, String, Option<String>, Option<Uuid>)> = sqlx::query_as(
            "SELECT name, instance, probe, probe_relay_id FROM sensor_configs ORDER BY name",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            checks,
            [
                ("ping".into(), "relay-paris".into(), Some("relay".into()), Some(relay)),
                ("snmp_if".into(), "relay-paris".into(), Some("relay".into()), Some(relay)),
            ]
        );
        assert_eq!(count(&pool, "SELECT count(*) FROM thresholds WHERE metric = 'monitorflow_ping_up'").await, 1);
        let (status, run) = call(&pool, "POST", &format!("/api/imports/{id}/undo"), &cookie, None).await;
        assert_eq!(status, StatusCode::OK, "{run}");
        assert_eq!(run["result"]["kept_servers"], json!([]));
        assert_eq!(count(&pool, "SELECT count(*) FROM servers").await, 0);
    }

    #[sqlx::test]
    async fn zabbix_trigger_dependencies_are_imported_and_undone(pool: PgPool) {
        use monitorflow_import::source::File;
        use monitorflow_import::zabbix::Zabbix;
        use monitorflow_import::{Context, Input, Source};

        monitorflow_server::secrets::install_for_tests();
        let paris = site(&pool, "paris").await;
        let (cookie, admin) = login(&pool, "alice", Role::Administrator, GrantScope::Global).await;
        let export = File {
            name: "dependencies.yaml".into(),
            content: include_bytes!("../../import/tests/fixtures/zabbix/7.0/dependencies.yaml").to_vec(),
        };
        let inventory = Zabbix
            .read(Input::Files(vec![export]), &Context::new(Utc::now()))
            .await
            .unwrap();
        let id = preview(&pool, paris, admin, &inventory).await;
        let (_, run) = call(&pool, "GET", &format!("/api/imports/{id}"), &cookie, None).await;
        let kinds: Vec<(&str, &str)> = run["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|i| i["kind"] == "dependency")
            .map(|i| (i["id"].as_str().unwrap(), i["list"].as_str().unwrap()))
            .collect();
        assert_eq!(
            kinds,
            [("dependency:app-01:sw-core", "exact"), ("dependency:app-01/port_443:sw-core", "approximated")],
            "{run}"
        );
        let (status, run) = call(&pool, "POST", &format!("/api/imports/{id}/confirm"), &cookie, None).await;
        assert_eq!(status, StatusCode::OK, "{run}");
        assert_eq!(run["result"]["failed"], json!([]), "{run}");
        assert_eq!(run["result"]["created"]["dependency"], 2, "{run}");
        let rows: Vec<(String, Option<String>, String, Option<String>)> = sqlx::query_as(
            "SELECT c.hostname, d.check_name, p.hostname, d.parent_check_name
             FROM dependencies d JOIN servers c ON c.id = d.server_id
             JOIN servers p ON p.id = d.parent_server_id ORDER BY d.check_name NULLS FIRST",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            rows,
            [
                ("app-01".into(), None, "sw-core".into(), None),
                ("app-01".into(), Some("port_443".into()), "sw-core".into(), None),
            ]
        );
        let (status, run) = call(&pool, "POST", &format!("/api/imports/{id}/undo"), &cookie, None).await;
        assert_eq!(status, StatusCode::OK, "{run}");
        assert_eq!(count(&pool, "SELECT count(*) FROM dependencies").await, 0);
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
    async fn a_web_check_becomes_a_website_check_without_its_password(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let paris = site(&pool, "paris").await;
        let (cookie, admin) = login(&pool, "alice", Role::Administrator, GrantScope::Global).await;
        let mut shop = Website::new("https://shop.example.com/");
        shop.status = Some("200,301".into());
        shop.contains = Some("Welcome".into());
        shop.username = Some("monitor".into());
        shop.credentials = true;
        shop.timeout_s = Some(10);
        let inventory = Inventory {
            hosts: vec![Host {
                key: "1".into(),
                name: "web-01".into(),
                visible_name: None,
                addresses: vec![],
                os: Os::Debian,
                groups: vec![],
                origin: origin("host", "web-01"),
                agentless: false,
            }],
            checks: vec![Check {
                host: "1".into(),
                watch: Watch::web(shop),
                interval_s: Some(60),
                alert: None,
                origin: origin("web_scenario", "Shop"),
                fidelity: Fidelity::Exact,
            }],
            thresholds: vec![Threshold {
                judged: Default::default(),
                conditions: Vec::new(),
                hosts: vec!["1".into()],
                measure: Measure::WebResponseTime { url: "https://shop.example.com/".into() },
                direction: Direction::Above,
                warning: Some(2.0),
                critical: None,
                for_s: 60,
                recovery: None,
                origin: origin("trigger", "Shop is slow"),
                fidelity: Fidelity::Exact,
            }],
            ..Inventory::default()
        };
        let id = preview(&pool, paris, admin, &inventory).await;
        let (status, run) = call(&pool, "GET", &format!("/api/imports/{id}"), &cookie, None).await;
        assert_eq!(status, StatusCode::OK, "{run}");
        let check = run["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["id"] == "check:web-01:http_shop_example_com")
            .unwrap_or_else(|| panic!("{run}"));
        assert_eq!(check["list"], "approximated");
        assert_eq!(check["note"]["code"], "web_password");

        let (status, run) = call(&pool, "POST", &format!("/api/imports/{id}/confirm"), &cookie, None).await;
        assert_eq!(status, StatusCode::OK, "{run}");
        assert_eq!(run["result"]["failed"], json!([]), "{run}");
        // The check, its alert, and the alerts the web interface adds,
        // the response time one from the source.
        assert_eq!(
            run["result"]["created"],
            json!({"server": 1, "check": 1, "threshold": 4}),
            "{run}"
        );
        let (kind, name, interval, timeout, params, sealed): (String, String, i64, Option<i64>, Value, bool) =
            sqlx::query_as(
                "SELECT kind, name, interval_ms, timeout_ms, params, secret IS NOT NULL FROM sensor_configs",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            (kind.as_str(), name.as_str(), interval, timeout, sealed),
            ("http", "http_shop_example_com", 60_000, Some(10_000), false)
        );
        assert_eq!(
            params,
            json!({"url": "https://shop.example.com/", "status": "200,301", "contains": "Welcome"})
        );
        type Row = (String, String, Option<f64>, Option<f64>, i32);
        let thresholds: Vec<Row> = sqlx::query_as(
            "SELECT metric, comparison, warning, critical, duration_s FROM thresholds ORDER BY metric",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            thresholds,
            vec![
                ("monitorflow_http_shop_example_com_cert_days_left".into(), "<".into(), Some(14.0), Some(7.0), 0),
                ("monitorflow_http_shop_example_com_cert_ok".into(), "<".into(), None, Some(1.0), 300),
                ("monitorflow_http_shop_example_com_response_seconds".into(), ">".into(), Some(2.0), None, 60),
                ("monitorflow_http_shop_example_com_up".into(), "<".into(), None, Some(1.0), 120),
            ]
        );

        // Importing it again creates nothing.
        let again = preview(&pool, paris, admin, &inventory).await;
        let (_, run) = call(&pool, "GET", &format!("/api/imports/{again}"), &cookie, None).await;
        let fresh: Vec<&Value> = run["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|i| i["present"] == false)
            .collect();
        assert!(fresh.is_empty(), "{fresh:#?}");
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

    #[sqlx::test]
    async fn network_scans_import_once_with_their_credentials_and_undo(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let paris = site(&pool, "paris").await;
        let (cookie, admin) = login(&pool, "alice", Role::Administrator, GrantScope::Global).await;
        let inventory = Inventory {
            scans: vec![Scan {
                name: "Office network".into(),
                targets: vec!["192.0.2.1-254".into()],
                ports: Some(vec![22, 443]),
                every: Some(Every::Day),
                snmp: Some(ScanSnmp {
                    version: "v2c".into(),
                    port: 161,
                    community: Some(Secret::new("recorded-community")),
                    ..ScanSnmp::default()
                }),
                group: None,
                origin: origin("discovery_rule", "Office network"),
                fidelity: Fidelity::Exact,
            }],
            ..Inventory::default()
        };
        let id = preview(&pool, paris, admin, &inventory).await;
        let (status, run) = call(&pool, "GET", &format!("/api/imports/{id}"), &cookie, None).await;
        assert_eq!(status, StatusCode::OK, "{run}");
        let item = &run["items"][0];
        assert_eq!(item["id"], "network_scan:Office network", "{run}");
        assert_eq!(item["create"]["addresses"], 254);
        assert_eq!(item["create"]["probe"], "central");
        assert!(!run.to_string().contains("recorded-community"));

        let (status, run) = call(&pool, "POST", &format!("/api/imports/{id}/confirm"), &cookie, None).await;
        assert_eq!(status, StatusCode::OK, "{run}");
        assert_eq!(run["result"]["failed"], json!([]), "{run}");
        assert_eq!(run["result"]["created"], json!({"network_scan": 1}), "{run}");
        type Row = (Uuid, Vec<String>, Vec<i32>, String, Option<String>, bool, Option<String>, Option<Uuid>);
        let (scan, targets, ports, probe, every, notify, sealed, by): Row = sqlx::query_as(
            "SELECT id, targets, ports, probe, every, notify, snmp_secret, created_by FROM network_scans
             WHERE site_id = $1",
        )
        .bind(paris)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            (targets, ports, probe.as_str(), every.as_deref(), notify, by),
            (vec!["192.0.2.1-254".to_owned()], vec![22, 443], "central", Some("day"), false, Some(admin))
        );
        let credentials = monitorflow_server::netscan::open(&sealed.unwrap()).unwrap();
        assert_eq!(credentials.secrets.community.as_deref(), Some("recorded-community"));
        // The confirmation is audited, the scan among what it created.
        let details: Value = sqlx::query_scalar(
            "SELECT details FROM audit_log WHERE action = 'import.confirm'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(details["created"]["network_scan"], 1);
        // Its scans run on the schedule, none now.
        assert_eq!(count(&pool, "SELECT count(*) FROM network_scan_runs").await, 0);

        // A second import finds it there.
        let second = preview(&pool, paris, admin, &inventory).await;
        let (_, run) = call(&pool, "GET", &format!("/api/imports/{second}"), &cookie, None).await;
        assert_eq!(run["counts"]["to_create"], 0, "{run}");

        let (status, run) = call(&pool, "POST", &format!("/api/imports/{id}/undo"), &cookie, None).await;
        assert_eq!(status, StatusCode::OK, "{run}");
        assert_eq!(run["result"]["removed"]["network_scan"], 1, "{run}");
        let left: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM network_scans WHERE id = $1)")
            .bind(scan)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(!left);
    }

    #[sqlx::test]
    async fn business_services_import_once_and_undo(pool: PgPool) {
        use monitorflow_import::model::{Member, Service};
        monitorflow_server::secrets::install_for_tests();
        let paris = site(&pool, "paris").await;
        let (cookie, admin) = login(&pool, "alice", Role::Administrator, GrantScope::Global).await;
        let mut inventory = inventory();
        inventory.services.push(Service {
            name: "Shop".into(),
            members: vec![
                Member { host: "1".into(), check: None },
                Member {
                    host: "2".into(),
                    check: Some(Watch::Port { address: None, port: 5432 }),
                },
            ],
            at_least: None,
            target: Some(99.95),
            origin: origin("service", "Shop"),
            fidelity: Fidelity::Exact,
        });
        let id = preview(&pool, paris, admin, &inventory).await;
        let (status, run) = call(&pool, "POST", &format!("/api/imports/{id}/confirm"), &cookie, None).await;
        assert_eq!(status, StatusCode::OK, "{run}");
        assert_eq!(run["result"]["created"]["service"], 1, "{run}");
        let (rule, target, members): (String, Option<f64>, Value) = sqlx::query_as(
            "SELECT b.rule, b.target,
                    (SELECT jsonb_agg(jsonb_build_array(s.hostname, m.check_name) ORDER BY m.position)
                     FROM business_service_members m JOIN servers s ON s.id = m.server_id
                     WHERE m.service_id = b.id)
             FROM business_services b JOIN sites si ON si.customer_id = b.customer_id
             WHERE si.id = $1",
        )
        .bind(paris)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!((rule.as_str(), target), ("all", Some(99.95)));
        assert_eq!(members, json!([["web-01", null], ["db-01", "port_5432"]]));

        // A second import finds it there.
        let second = preview(&pool, paris, admin, &inventory).await;
        let (_, run) = call(&pool, "GET", &format!("/api/imports/{second}"), &cookie, None).await;
        assert_eq!(run["counts"]["to_create"], 0, "{run}");

        let (status, run) = call(&pool, "POST", &format!("/api/imports/{id}/undo"), &cookie, None).await;
        assert_eq!(status, StatusCode::OK, "{run}");
        assert_eq!(run["result"]["removed"]["service"], 1);
        assert_eq!(count(&pool, "SELECT count(*) FROM business_services").await, 0);
    }

    /// PRTG custom SNMP sensors with their lookup files (ADR-0157): custom
    /// checks of devices, the lookups they name, and their channels' rules.
    #[sqlx::test]
    async fn custom_snmp_sensors_of_prtg_are_imported(pool: PgPool) {
        use monitorflow_import::source::{Context, File, Input, Source};
        monitorflow_server::secrets::install_for_tests();
        let paris = site(&pool, "paris").await;
        let (cookie, admin) = login(&pool, "alice", Role::Administrator, GrantScope::Global).await;
        let file = |name: &str, text: &str| File { name: name.into(), content: text.as_bytes().to_vec() };
        let input = Input::Files(vec![
            file("PRTG Configuration.dat", include_str!("../../import/tests/fixtures/prtg/configuration-snmp.dat")),
            file("raid.ovl", include_str!("../../import/tests/fixtures/prtg/raid.ovl")),
            file("ha.ovl", include_str!("../../import/tests/fixtures/prtg/ha.ovl")),
        ]);
        let inventory = monitorflow_import::prtg::Prtg.read(input, &Context::new(Utc::now())).await.unwrap();
        let id = preview(&pool, paris, admin, &inventory).await;
        let (status, run) = call(&pool, "POST", &format!("/api/imports/{id}/confirm"), &cookie, None).await;
        assert_eq!(status, StatusCode::OK, "{run}");
        assert_eq!(run["result"]["failed"], json!([]), "{run}");
        let checks: Vec<(String, String, Value)> = sqlx::query_as(
            "SELECT s.peer_id, c.name, c.params FROM sensor_configs c JOIN servers s ON s.id = c.server_id
             WHERE c.kind = 'snmp_custom' ORDER BY s.peer_id, c.name",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        let names: Vec<(&str, &str)> = checks.iter().map(|(p, n, _)| (p.as_str(), n.as_str())).collect();
        assert_eq!(names, [
            ("fw-edge", "snmp_firewall_health"), ("fw-edge", "snmp_firmware"), ("fw-edge", "snmp_sessions"),
            ("nas-files", "snmp_power_source"), ("nas-files", "snmp_raid_volumes"), ("nas-files", "snmp_temperature"),
        ]);
        let lookups: Vec<(Uuid, String)> = sqlx::query_as("SELECT id, name FROM snmp_lookups ORDER BY name").fetch_all(&pool).await.unwrap();
        assert_eq!(lookups.iter().map(|l| l.1.as_str()).collect::<Vec<_>>(), ["oid.test.hastate", "oid.test.raidstatus"]);
        let raid = &checks[4].2;
        assert_eq!(raid["table"], json!("1.3.6.1.4.1.6574.3.1.1"));
        assert_eq!(raid["lookup1"], json!(lookups[1].0.to_string()));
        assert_eq!(checks[0].2["lookup3"], json!(lookups[0].0.to_string()));
        assert_eq!(checks[5].2["divide1"], json!("10"));
        let rules: Vec<(String, String, Option<f64>, Option<f64>)> = sqlx::query_as(
            "SELECT metric, comparison, warning, critical FROM thresholds WHERE metric LIKE 'monitorflow_snmp_%' ORDER BY metric",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        let metric = |m: &str| rules.iter().find(|r| r.0 == m).unwrap_or_else(|| panic!("no {m} in {rules:?}")).clone();
        assert_eq!(metric("monitorflow_snmp_sessions_sessions"), ("monitorflow_snmp_sessions_sessions".into(), ">".into(), Some(5000.0), Some(8000.0)));
        assert_eq!(metric("monitorflow_snmp_firewall_health_cpu").2, Some(80.0));
        assert_eq!(metric("monitorflow_snmp_firewall_health_ha_state").3, Some(2.0));
        assert_eq!(metric("monitorflow_snmp_raid_volumes_status_state").1, ">=");
        assert_eq!(metric("monitorflow_snmp_firmware_firmware_state").3, Some(2.0));
        // The checks run as the probe reads them, once the device has credentials.
        let nas: Uuid = sqlx::query_scalar("SELECT id FROM servers WHERE peer_id = 'nas-files'").fetch_one(&pool).await.unwrap();
        let (status, _) = call(&pool, "PUT", &format!("/api/devices/{nas}/snmp"), &cookie, Some(json!({"version": "v2c", "community": "nas"}))).await;
        assert_eq!(status, StatusCode::OK);
        let sets = monitorflow_server::probes::ProbeStore::new(pool.clone()).load().await.unwrap();
        let sent: Vec<_> = sets.values().flat_map(|s| &s.checks).filter(|c| c.kind == "snmp_custom").collect();
        assert_eq!(sent.len(), 3, "the NAS's checks; the firewall has no credentials yet");
        for c in sent {
            monitorflow_snmp::custom::build(&c.config.params).unwrap();
        }
        // Undo removes the checks and rules; lookups stay for other checks.
        let (status, _) = call(&pool, "POST", &format!("/api/imports/{id}/undo"), &cookie, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(count(&pool, "SELECT count(*) FROM sensor_configs WHERE kind = 'snmp_custom'").await, 0);
        assert_eq!(count(&pool, "SELECT count(*) FROM snmp_lookups").await, 2);
    }

    /// A connection reaches private addresses only for a global
    /// administrator (ADR-0163): an administrator of one site cannot make
    /// the central server probe its own side of the network.
    #[sqlx::test]
    async fn only_a_global_administrator_connects_to_private_addresses(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let paris = site(&pool, "paris").await;
        let (global, _) = login(&pool, "alice", Role::Administrator, GrantScope::Global).await;
        let (local, _) = login(&pool, "bob", Role::Administrator, GrantScope::Site(paris)).await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        for url in [format!("http://127.0.0.1:{port}"), format!("http://localhost:{port}")] {
            let request = json!({"source": "prometheus", "site_id": paris, "connection": {"url": url}});
            let (status, body) = call(&pool, "POST", "/api/imports", &local, Some(request)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            assert!(body.to_string().contains("private address"), "{body}");
        }
        let reached = tokio::time::timeout(std::time::Duration::from_millis(200), listener.accept()).await;
        assert!(reached.is_err(), "the connection reached a private address");
        // The same address for a global administrator: the server connects.
        let accept = tokio::spawn(async move { listener.accept().await.is_ok() });
        let request = json!({"source": "prometheus", "site_id": paris, "connection": {"url": format!("http://127.0.0.1:{port}")}});
        let (status, _) = call(&pool, "POST", "/api/imports", &global, Some(request)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(accept.await.unwrap());
        assert_eq!(count(&pool, "SELECT count(*) FROM import_runs").await, 0);
    }
}
