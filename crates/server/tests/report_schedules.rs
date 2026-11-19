//! Scheduled availability reports (ADR-0155): who manages schedules, the
//! sender emailing the latest period once with the PDF and the CSV of one
//! customer only, claims between central servers, failures told on the
//! schedule, and "Send now". Against PostgreSQL and an SMTP receiver on
//! loopback. Ignored without `DATABASE_URL`.

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine;
use chrono::{DateTime, TimeDelta, Utc};
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::availability::schedule::{latest, Frequency};
use monitorflow_server::availability::sender;
use monitorflow_server::grpc::Registry;
use serde_json::{json, Value};
use sqlx::PgPool;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery";
const SCHEDULES: &str = "/api/report-schedules";
const PARIS: chrono_tz::Tz = chrono_tz::Europe::Paris;

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
    cookie: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value, Option<String>) {
    let state = ApiState::new(pool.clone(), Registry::default(), ApiSettings::default());
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header(CSRF_HEADER, "1")
        .header("content-type", "application/json");
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    let request = request
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let response = api::router(state).oneshot(request).await.unwrap();
    let status = response.status();
    let cookie = response
        .headers()
        .get("set-cookie")
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned());
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        cookie,
    )
}

async fn login(pool: &PgPool, name: &str, role: Role, scope: GrantScope) -> String {
    users::create_user_with(pool, name, &Secret::new(PASSWORD), role, scope, "test")
        .await
        .unwrap();
    let body = json!({ "username": name, "password": PASSWORD });
    let (status, _, cookie) = call(pool, "POST", "/api/auth/login", None, Some(body)).await;
    assert_eq!(status, StatusCode::OK);
    cookie.unwrap()
}

async fn smtp_server() -> (u16, Arc<Mutex<Vec<String>>>) {
    let mails = Arc::new(Mutex::new(Vec::new()));
    let store = mails.clone();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            let store = store.clone();
            tokio::spawn(async move {
                let (read, mut write) = socket.into_split();
                let mut lines = BufReader::new(read).lines();
                write.write_all(b"220 test ESMTP\r\n").await.unwrap();
                let mut data: Option<String> = None;
                while let Ok(Some(line)) = lines.next_line().await {
                    if let Some(body) = data.as_mut() {
                        if line == "." {
                            store.lock().unwrap().push(data.take().unwrap());
                            write.write_all(b"250 queued\r\n").await.unwrap();
                        } else {
                            body.push_str(&line);
                            body.push('\n');
                        }
                        continue;
                    }
                    let verb = line.get(..4).unwrap_or_default().to_ascii_uppercase();
                    let answer: &[u8] = match verb.as_str() {
                        "EHLO" | "HELO" => b"250 test\r\n",
                        "DATA" => {
                            data = Some(String::new());
                            b"354 go ahead\r\n"
                        }
                        "QUIT" => {
                            write.write_all(b"221 bye\r\n").await.unwrap();
                            break;
                        }
                        _ => b"250 ok\r\n",
                    };
                    write.write_all(answer).await.unwrap();
                }
            });
        }
    });
    (port, mails)
}

/// A customer with a site, a server offline for `minutes` an hour into the
/// latest month, and an email channel to `port` covering it.
async fn customer(pool: &PgPool, name: &str, minutes: i64, port: u16) -> (Uuid, Uuid) {
    let customer: Uuid =
        sqlx::query_scalar("INSERT INTO customers (name) VALUES ($1) RETURNING id")
            .bind(name)
            .fetch_one(pool)
            .await
            .unwrap();
    let site: Uuid =
        sqlx::query_scalar("INSERT INTO sites (name, customer_id) VALUES ($1, $2) RETURNING id")
            .bind(format!("{name}-site"))
            .bind(customer)
            .fetch_one(pool)
            .await
            .unwrap();
    let server: Uuid = sqlx::query_scalar(
        "INSERT INTO servers (peer_id, site_id, hostname, os_family, created_at)
         VALUES ($1, $2, $1, 'debian', '2026-01-01T00:00:00Z') RETURNING id",
    )
    .bind(format!("{name}-web"))
    .bind(site)
    .fetch_one(pool)
    .await
    .unwrap();
    let start = latest(Frequency::Monthly, PARIS, Utc::now()).0 + TimeDelta::hours(1);
    let incident: Uuid = sqlx::query_scalar(
        "INSERT INTO incidents (server_id, metric, instance, severity, state, opened_at,
                                resolved_at, last_value, last_value_at, peak_value, threshold)
         VALUES ($1, 'offline', '', 'critical', 'resolved', $2, $3, 0, $2, 0, '{}') RETURNING id",
    )
    .bind(server)
    .bind(start)
    .bind(start + TimeDelta::minutes(minutes))
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO incident_events (incident_id, occurred_at, kind, severity)
         VALUES ($1, $2, 'opened', 'critical'), ($1, $3, 'resolved', NULL)",
    )
    .bind(incident)
    .bind(start)
    .bind(start + TimeDelta::minutes(minutes))
    .execute(pool)
    .await
    .unwrap();
    let channel: Uuid = sqlx::query_scalar(
        "INSERT INTO notification_channels (kind, settings, customer_id)
         VALUES ('email', $1, $2) RETURNING id",
    )
    .bind(
        json!({"host": "127.0.0.1", "port": port, "from": "reports@example.com",
                 "to": [format!("ops@{name}.example")]}),
    )
    .bind(customer)
    .fetch_one(pool)
    .await
    .unwrap();
    (customer, channel)
}

/// The decoded attachment `name` of `mail`, in base64, quoted-printable
/// or as it is.
fn attachment(mail: &str, name: &str) -> Option<Vec<u8>> {
    // Long names are split as RFC 2231 says: `filename*0="..."`.
    let at = mail.find(&format!("=\"{name}\""))?;
    let rest = &mail[at..];
    let start = rest.find("\n\n")? + 2;
    let headers = &rest[..start];
    let end = rest[start..].find("\n--").map_or(rest.len(), |e| start + e);
    let text = &rest[start..end];
    if headers.contains("base64") {
        let text: String = text.split_whitespace().collect();
        return base64::engine::general_purpose::STANDARD.decode(text).ok();
    }
    if headers.contains("quoted-printable") {
        let joined = text.replace("=\n", "");
        let bytes = joined.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'=' && i + 2 < bytes.len() {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 3;
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        return Some(out);
    }
    Some(text.as_bytes().to_vec())
}

fn schedule(customer: Uuid, channel: Uuid) -> Value {
    json!({"customer_id": customer, "time_zone": "Europe/Paris", "channel_id": channel})
}

db_tests! {
    #[sqlx::test]
    async fn the_latest_month_is_mailed_once_with_its_files(pool: PgPool) {
        let (port, mails) = smtp_server().await;
        let (acme, channel) = customer(&pool, "acme", 30, port).await;
        let (globex, globex_channel) = customer(&pool, "globex", 90, port).await;
        let admin = login(&pool, "ada", Role::Administrator, GrantScope::Customer(acme)).await;
        let (status, view, _) =
            call(&pool, "POST", SCHEDULES, Some(&admin), Some(schedule(acme, channel))).await;
        assert_eq!(status, StatusCode::CREATED, "{view}");
        assert_eq!(view["frequency"], "monthly");
        assert_eq!(view["channel_recipients"], json!(["ops@acme.example"]));
        // Made now, it starts with the next month: nothing is due.
        assert_eq!(sender::pass(&pool, Utc::now()).await.unwrap(), 0);
        // As if made a month ago: the latest month is due.
        sqlx::query("UPDATE report_schedules SET last_period_end = '2020-01-01T00:00:00Z'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(sender::pass(&pool, Utc::now()).await.unwrap(), 1);
        assert_eq!(sender::pass(&pool, Utc::now()).await.unwrap(), 0);
        let mail = mails.lock().unwrap().pop().unwrap();
        assert!(mail.contains("Subject: Availability report, acme, "), "{mail}");
        assert!(mail.contains("ops@acme.example"));
        let (from, to) = latest(Frequency::Monthly, PARIS, Utc::now());
        let day = |at: DateTime<Utc>| at.with_timezone(&PARIS).format("%Y-%m-%d").to_string();
        let stem = format!("availability-acme-{}-{}", day(from), day(to));
        let csv = String::from_utf8(attachment(&mail, &format!("{stem}.csv")).unwrap()).unwrap();
        assert!(csv.contains("server,acme-web,"), "{csv}");
        assert!(csv.contains(",1800,"), "{csv}");
        assert!(!csv.contains("globex"));
        let pdf = attachment(&mail, &format!("{stem}.pdf")).unwrap();
        assert!(pdf.starts_with(b"%PDF-"));
        assert!(!pdf.windows(6).any(|w| w == b"globex"));
        let audit: Value = sqlx::query_scalar(
            "SELECT details FROM audit_log WHERE action = 'report.sent'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(audit["recipients"], json!(["ops@acme.example"]));
        assert_eq!(audit["manual"], false);

        // Acme's administrator never sees globex's schedule nor uses its
        // channel.
        let root = login(&pool, "root", Role::Administrator, GrantScope::Global).await;
        let (status, _, _) =
            call(&pool, "POST", SCHEDULES, Some(&root), Some(schedule(globex, globex_channel))).await;
        assert_eq!(status, StatusCode::CREATED);
        let (_, list, _) = call(&pool, "GET", SCHEDULES, Some(&admin), None).await;
        assert_eq!(list["schedules"].as_array().unwrap().len(), 1);
        let (status, _, _) =
            call(&pool, "POST", SCHEDULES, Some(&admin), Some(schedule(acme, globex_channel))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let operator = login(&pool, "olga", Role::Operator, GrantScope::Customer(acme)).await;
        let (status, _, _) = call(&pool, "GET", SCHEDULES, Some(&operator), None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[sqlx::test]
    async fn claims_failures_and_send_now(pool: PgPool) {
        let (port, mails) = smtp_server().await;
        let (acme, channel) = customer(&pool, "acme", 30, port).await;
        let admin = login(&pool, "root", Role::Administrator, GrantScope::Global).await;
        let mut body = schedule(acme, channel);
        body["recipients"] = json!(["boss@acme.example"]);
        body["pdf"] = json!(false);
        let (_, view, _) = call(&pool, "POST", SCHEDULES, Some(&admin), Some(body)).await;
        let id = view["id"].as_str().unwrap().to_owned();
        sqlx::query("UPDATE report_schedules SET last_period_end = NULL,
                     claimed_until = now() + interval '5 minutes'")
            .execute(&pool)
            .await
            .unwrap();
        // Held by another central server.
        assert_eq!(sender::pass(&pool, Utc::now()).await.unwrap(), 0);
        // An SMTP server that is gone: the error is on the schedule.
        let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gone = closed.local_addr().unwrap().port();
        drop(closed);
        sqlx::query("UPDATE notification_channels SET settings = jsonb_set(settings, '{port}', $1)")
            .bind(json!(gone))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE report_schedules SET claimed_until = NULL")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(sender::pass(&pool, Utc::now()).await.unwrap(), 0);
        let (_, list, _) = call(&pool, "GET", SCHEDULES, Some(&admin), None).await;
        assert!(list["schedules"][0]["last_error"].as_str().unwrap().contains("127.0.0.1"));
        // Not tried again within 15 minutes.
        let retry: Option<DateTime<Utc>> = sqlx::query_scalar("SELECT retry_at FROM report_schedules")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(retry.unwrap() > Utc::now() + TimeDelta::minutes(14));
        // Fixed, and sent now by hand: to its own recipients, CSV only.
        sqlx::query("UPDATE notification_channels SET settings = jsonb_set(settings, '{port}', $1)")
            .bind(json!(port))
            .execute(&pool)
            .await
            .unwrap();
        let uri = format!("{SCHEDULES}/{id}/send");
        let (status, sent, _) = call(&pool, "POST", &uri, Some(&admin), None).await;
        assert_eq!(status, StatusCode::OK, "{sent}");
        assert_eq!(sent["ok"], true, "{sent}");
        assert_eq!(sent["recipients"], json!(["boss@acme.example"]));
        let mail = mails.lock().unwrap().pop().unwrap();
        assert!(mail.contains("boss@acme.example") && !mail.contains("ops@acme.example"));
        assert!(mail.contains(".csv\"") && !mail.contains(".pdf\""), "{mail}");
        // A send by hand leaves the schedule's own sending as planned.
        let end: Option<DateTime<Utc>> =
            sqlx::query_scalar("SELECT last_period_end FROM report_schedules")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(end, None);
        let (status, _, _) = call(&pool, "DELETE", &format!("{SCHEDULES}/{id}"), Some(&admin), None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }
}
