//! Silences of chosen checks and maintenance windows (ADR-0151): what the
//! dispatcher holds and sends, against PostgreSQL and a webhook receiver on
//! loopback, how windows are written as silences, around daylight saving
//! changes with a fixed clock, and what the server and incident views say.
//! Ignored without `DATABASE_URL`.

use std::sync::{Arc, Mutex};

use axum::body::{Body, Bytes};
use axum::http::{Request, StatusCode};
use chrono::{DateTime, TimeDelta, Utc};
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
use monitorflow_server::notify::maintenance::write;
use monitorflow_server::notify::{self, Coverage, Destination, Dispatcher, Webhook};
use serde_json::{json, Value};
use sqlx::PgPool;
use tokio::net::TcpListener;
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

async fn get(pool: &PgPool, uri: &str, cookie: &str) -> Value {
    let state = ApiState::new(pool.clone(), Registry::default(), ApiSettings::default());
    let request = Request::builder()
        .uri(uri)
        .header(CSRF_HEADER, "1")
        .header("cookie", cookie)
        .body(Body::empty())
        .unwrap();
    let response = api::router(state).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{uri}");
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn login(pool: &PgPool) -> String {
    users::create_user_with(
        pool,
        "root",
        &Secret::new(PASSWORD),
        Role::Administrator,
        GrantScope::Global,
        "test",
    )
    .await
    .unwrap();
    let state = ApiState::new(pool.clone(), Registry::default(), ApiSettings::default());
    let body = json!({ "username": "root", "password": PASSWORD });
    let request = Request::builder()
        .method("POST")
        .uri("/api/auth/login")
        .header(CSRF_HEADER, "1")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = api::router(state).oneshot(request).await.unwrap();
    let cookie = response.headers()["set-cookie"].to_str().unwrap();
    cookie.split(';').next().unwrap().to_owned()
}

/// A site with two servers, `web` running nginx.
struct Place {
    site: Uuid,
    web: Uuid,
    db: Uuid,
}

async fn place(pool: &PgPool) -> Place {
    let customer: Uuid =
        sqlx::query_scalar("INSERT INTO customers (name) VALUES ('acme') RETURNING id")
            .fetch_one(pool)
            .await
            .unwrap();
    let site = sqlx::query_scalar(
        "INSERT INTO sites (name, customer_id) VALUES ('paris', $1) RETURNING id",
    )
    .bind(customer)
    .fetch_one(pool)
    .await
    .unwrap();
    let mut servers = Vec::new();
    for name in ["web-1", "db-1"] {
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO servers (peer_id, site_id, hostname, os_family)
             VALUES ($1, $2, $1, 'debian') RETURNING id",
        )
        .bind(name)
        .bind(site)
        .fetch_one(pool)
        .await
        .unwrap();
        servers.push(id);
    }
    sqlx::query(
        "INSERT INTO sensor_configs (server_id, kind, name, instance, interval_ms, params)
         VALUES ($1, 'systemd', 'service_nginx', '', 60000, '{\"unit\": \"nginx.service\"}')",
    )
    .bind(servers[0])
    .execute(pool)
    .await
    .unwrap();
    Place {
        site,
        web: servers[0],
        db: servers[1],
    }
}

/// A webhook on loopback for everything, recording each message's text.
async fn channel(pool: &PgPool) -> (Dispatcher, Arc<Mutex<Vec<String>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let store = seen.clone();
    let app = axum::Router::new().route(
        "/hook",
        axum::routing::post(move |body: Bytes| {
            let store = store.clone();
            async move {
                let body: Value = serde_json::from_slice(&body).unwrap();
                store
                    .lock()
                    .unwrap()
                    .push(body["text"].as_str().unwrap().to_owned());
                StatusCode::OK
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/hook", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let destination = Destination::Webhook(Webhook { url });
    notify::insert(pool, &destination, None, Coverage::Everything, false)
        .await
        .unwrap();
    let dispatcher = Dispatcher::new(pool.clone(), "https://mf.example")
        .with_retries(Vec::new())
        .without_pace()
        .without_waits();
    (dispatcher, seen)
}

async fn settle(dispatcher: &Dispatcher) {
    for _ in 0..3 {
        dispatcher.pass().await.unwrap();
    }
}

fn taken(seen: &Mutex<Vec<String>>) -> Vec<String> {
    std::mem::take(&mut seen.lock().unwrap())
}

/// Opens a critical incident of `metric` on `server`.
async fn open(pool: &PgPool, server: Uuid, metric: &str, instance: &str) -> Uuid {
    let id = sqlx::query_scalar(
        "INSERT INTO incidents (server_id, metric, instance, severity, opened_at, last_value,
                                last_value_at, peak_value, threshold)
         VALUES ($1, $2, $3, 'critical', now(), 1, now(), 1, '{}') RETURNING id",
    )
    .bind(server)
    .bind(metric)
    .bind(instance)
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO incident_events (incident_id, occurred_at, kind, severity, value)
         VALUES ($1, now(), 'opened', 'critical', 1)",
    )
    .bind(id)
    .execute(pool)
    .await
    .unwrap();
    id
}

async fn resolve(pool: &PgPool, incident: Uuid) {
    sqlx::query("UPDATE incidents SET state = 'resolved', resolved_at = now() WHERE id = $1")
        .bind(incident)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO incident_events (incident_id, occurred_at, kind, value)
         VALUES ($1, now(), 'resolved', 0)",
    )
    .bind(incident)
    .execute(pool)
    .await
    .unwrap();
}

/// A silence of `checks` of `server` for the next hour.
async fn silence_checks(pool: &PgPool, server: Uuid, checks: Value) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO silences (server_id, starts_at, ends_at, reason, checks)
         VALUES ($1, now() - interval '1 minute', now() + interval '1 hour', 'rebuild', $2)
         RETURNING id",
    )
    .bind(server)
    .bind(checks)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// A daily window on `site` running from an hour ago for three hours, in
/// UTC, narrowed to `servers` and `checks` when given.
async fn window(pool: &PgPool, site: Uuid, servers: &[Uuid], checks: Option<Value>) -> Uuid {
    let start = Utc::now() - TimeDelta::hours(1);
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO maintenance_windows (site_id, checks, repeat, start_time, duration_minutes,
                                          time_zone, starts_on, reason, whole_site)
         VALUES ($1, $2, 'daily', $3, 180, 'UTC', $4, 'Patch night', $5) RETURNING id",
    )
    .bind(site)
    .bind(checks)
    .bind(start.time())
    .bind(start.date_naive() - TimeDelta::days(1))
    .bind(servers.is_empty())
    .fetch_one(pool)
    .await
    .unwrap();
    for server in servers {
        sqlx::query(
            "INSERT INTO maintenance_window_servers (window_id, server_id) VALUES ($1, $2)",
        )
        .bind(id)
        .bind(server)
        .execute(pool)
        .await
        .unwrap();
    }
    id
}

/// The window's occurrences: server, start, end, ended.
async fn occurrences(
    pool: &PgPool,
    window: Uuid,
) -> Vec<(Option<Uuid>, DateTime<Utc>, DateTime<Utc>, bool)> {
    sqlx::query_as(
        "SELECT server_id, starts_at, ends_at, ended_at IS NOT NULL FROM silences
         WHERE window_id = $1 ORDER BY starts_at, server_id",
    )
    .bind(window)
    .fetch_all(pool)
    .await
    .unwrap()
}

fn utc(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .unwrap()
        .with_timezone(&Utc)
}

db_tests! {
    #[sqlx::test]
    async fn a_silenced_check_is_quiet_while_the_rest_of_the_server_pages(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let p = place(&pool).await;
        let (dispatcher, seen) = channel(&pool).await;
        let admin = login(&pool).await;
        let id = silence_checks(&pool, p.web, json!([{"name": "service_nginx"}])).await;

        let nginx = open(&pool, p.web, "monitorflow_service_nginx_active", "nginx.service").await;
        let disk = open(&pool, p.web, "monitorflow_disk_used_ratio", "/").await;
        let offline = open(&pool, p.web, "offline", "").await;
        settle(&dispatcher).await;
        let sent = taken(&seen);
        assert_eq!(sent.len(), 2, "{sent:?}");
        assert!(sent.iter().all(|text| !text.contains("service_nginx")), "{sent:?}");

        // The server is not silenced; the nginx incident is.
        let server = get(&pool, &format!("/api/servers/{}", p.web), &admin).await;
        assert!(server["silenced_until"].is_null());
        assert!(server["silence"].is_null());
        let shown = get(&pool, &format!("/api/incidents/{nginx}"), &admin).await;
        assert!(!shown["silenced_until"].is_null());
        assert_eq!(shown["in_maintenance"], false);
        for other in [disk, offline] {
            let shown = get(&pool, &format!("/api/incidents/{other}"), &admin).await;
            assert!(shown["silenced_until"].is_null());
        }

        // Still open at the end: told once.
        sqlx::query("UPDATE silences SET ended_at = now() WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        settle(&dispatcher).await;
        let sent = taken(&seen);
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert!(sent[0].contains("service_nginx"), "{sent:?}");
    }

    #[sqlx::test]
    async fn a_check_opened_and_resolved_during_its_silence_says_nothing(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let p = place(&pool).await;
        let (dispatcher, seen) = channel(&pool).await;
        // The check's instance narrows it to one unit.
        let id = silence_checks(
            &pool,
            p.web,
            json!([{"name": "service_nginx", "instance": "nginx.service"}]),
        )
        .await;
        let nginx = open(&pool, p.web, "monitorflow_service_nginx_active", "nginx.service").await;
        settle(&dispatcher).await;
        resolve(&pool, nginx).await;
        settle(&dispatcher).await;
        sqlx::query("UPDATE silences SET ended_at = now() WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        settle(&dispatcher).await;
        assert!(taken(&seen).is_empty());
    }

    #[sqlx::test]
    async fn a_running_window_holds_its_servers_and_says_in_maintenance(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let p = place(&pool).await;
        let (dispatcher, seen) = channel(&pool).await;
        let admin = login(&pool).await;
        let id = window(&pool, p.site, &[p.web], None).await;
        // The running occurrence and those of the next two days.
        assert_eq!(write::refresh(&pool, Utc::now()).await.unwrap(), 3);
        // Written once, however often it runs.
        assert_eq!(write::refresh(&pool, Utc::now()).await.unwrap(), 0);
        let rows = occurrences(&pool, id).await;
        assert_eq!(rows[0].0, Some(p.web));
        assert!(rows.iter().all(|r| r.0 == Some(p.web)));

        let held = open(&pool, p.web, "monitorflow_disk_used_ratio", "/").await;
        open(&pool, p.db, "monitorflow_disk_used_ratio", "/").await;
        settle(&dispatcher).await;
        let sent = taken(&seen);
        assert_eq!(sent.len(), 1, "only db-1 is told: {sent:?}");
        assert!(sent[0].contains("db-1"), "{sent:?}");

        let server = get(&pool, &format!("/api/servers/{}", p.web), &admin).await;
        assert_eq!(server["state"], "silenced");
        assert_eq!(server["silence"]["window"], true);
        assert_eq!(server["silence"]["reason"], "Patch night");
        let shown = get(&pool, &format!("/api/incidents/{held}"), &admin).await;
        assert_eq!(shown["in_maintenance"], true);

        // A changed rule ends the running occurrence and writes the new
        // one in the same transaction: nothing is released.
        let mut tx = pool.begin().await.unwrap();
        write::stop(&mut tx, id, Utc::now()).await.unwrap();
        sqlx::query("UPDATE maintenance_windows SET reason = 'Patches', updated_at = now() WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .unwrap();
        let stored = write::load(&mut tx, id).await.unwrap().unwrap();
        assert_eq!(write::write(&mut tx, &stored, Utc::now()).await.unwrap(), 3);
        tx.commit().await.unwrap();
        settle(&dispatcher).await;
        assert!(taken(&seen).is_empty());

        // Ending the running occurrence by hand: it is not written again,
        // and what it held is told.
        sqlx::query(
            "UPDATE silences SET ended_at = now()
             WHERE window_id = $1 AND ended_at IS NULL AND starts_at <= now()",
        )
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(write::refresh(&pool, Utc::now()).await.unwrap(), 0);
        settle(&dispatcher).await;
        let sent = taken(&seen);
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert!(sent[0].contains("web-1"), "{sent:?}");
    }

    #[sqlx::test]
    async fn a_window_of_a_site_and_checks_writes_one_row_per_occurrence(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let p = place(&pool).await;
        let (dispatcher, seen) = channel(&pool).await;
        let id = window(&pool, p.site, &[], Some(json!([{"name": "service_nginx"}]))).await;
        assert_eq!(write::refresh(&pool, Utc::now()).await.unwrap(), 3);
        assert_eq!(occurrences(&pool, id).await[0].0, None);
        open(&pool, p.web, "monitorflow_service_nginx_active", "nginx.service").await;
        open(&pool, p.db, "monitorflow_disk_used_ratio", "/").await;
        settle(&dispatcher).await;
        let sent = taken(&seen);
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert!(sent[0].contains("db-1"), "{sent:?}");

        // Removed: nothing more is written.
        let mut tx = pool.begin().await.unwrap();
        write::stop(&mut tx, id, Utc::now()).await.unwrap();
        sqlx::query("UPDATE maintenance_windows SET removed_at = now() WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(write::refresh(&pool, Utc::now()).await.unwrap(), 0);
        settle(&dispatcher).await;
        let sent = taken(&seen);
        assert_eq!(sent.len(), 1, "the held incident is told: {sent:?}");
    }

    #[sqlx::test]
    async fn windows_are_written_two_days_ahead_in_local_time(pool: PgPool) {
        let p = place(&pool).await;
        // Saturdays 22:00 for 8 hours in Paris; clocks go forward on
        // Sunday 2026-03-29 and back on Sunday 2026-10-25.
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO maintenance_windows (site_id, repeat, weekdays, start_time,
                                              duration_minutes, time_zone, starts_on, reason)
             VALUES ($1, 'weekly', 32, '22:00', 480, 'Europe/Paris', '2026-01-01', 'Night')
             RETURNING id",
        )
        .bind(p.site)
        .fetch_one(&pool)
        .await
        .unwrap();
        // On Thursday, Saturday is too far ahead.
        assert_eq!(write::refresh(&pool, utc("2026-03-26T12:00:00Z")).await.unwrap(), 0);
        assert_eq!(write::refresh(&pool, utc("2026-03-27T12:00:00Z")).await.unwrap(), 1);
        assert_eq!(write::refresh(&pool, utc("2026-10-23T12:00:00Z")).await.unwrap(), 1);
        let rows = occurrences(&pool, id).await;
        let times: Vec<_> = rows.iter().map(|r| (r.1, r.2)).collect();
        assert_eq!(
            times,
            vec![
                // Seven real hours, then nine, both to 06:00 local.
                (utc("2026-03-28T21:00:00Z"), utc("2026-03-29T04:00:00Z")),
                (utc("2026-10-24T20:00:00Z"), utc("2026-10-25T05:00:00Z")),
            ]
        );
    }

    #[sqlx::test]
    async fn a_window_whose_servers_are_removed_does_not_widen(pool: PgPool) {
        let p = place(&pool).await;
        let id = window(&pool, p.site, &[p.web], None).await;
        sqlx::query("DELETE FROM servers WHERE id = $1")
            .bind(p.web)
            .execute(&pool)
            .await
            .unwrap();
        // Not the whole site: nothing at all.
        assert_eq!(write::refresh(&pool, Utc::now()).await.unwrap(), 0);
        assert!(occurrences(&pool, id).await.is_empty());
    }
}
