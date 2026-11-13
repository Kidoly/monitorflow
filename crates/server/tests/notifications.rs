//! Notifications (ADR-0060, ADR-0078): the triggers that fill the outbox,
//! delivery to a webhook and an SMTP server on loopback, routing per
//! customer and site, who manages which channel, the API's secrecy and its
//! test button. Ignored without `DATABASE_URL`.

use std::borrow::Cow;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, Request, StatusCode};
use chrono::{TimeDelta, Utc};
use monitorflow_common::redact::Secret;
use monitorflow_common::server_update::backup::{BackupKind, BackupRun, BackupState, Backups};
use monitorflow_common::server_update::{Status, STATUS_FILE};
use monitorflow_common::tls::PeerRole;
use monitorflow_server::alerting::offline::{self, OfflineSettings};
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::db;
use monitorflow_server::grpc::Registry;
use monitorflow_server::notify::send::{signature, SIGNATURE_HEADER, TIMESTAMP_HEADER};
use monitorflow_server::notify::{self, backup, Coverage, Destination, Dispatcher, Email, Webhook};
use monitorflow_server::release_check;
use serde_json::{json, Value};
use sqlx::migrate::Migrator;
use sqlx::PgPool;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery";
const CHANNELS: &str = "/api/notifications/channels";
const ROUTING_MIGRATION: i64 = 20261110100000;

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
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body, cookie)
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

/// A customer with a site, a server and a relay: `(customer, server, relay)`.
async fn place(pool: &PgPool, name: &str) -> (Uuid, Uuid, Uuid) {
    let customer: Uuid =
        sqlx::query_scalar("INSERT INTO customers (name) VALUES ($1) RETURNING id")
            .bind(name)
            .fetch_one(pool)
            .await
            .unwrap();
    let site: Uuid = sqlx::query_scalar(
        "INSERT INTO sites (name, customer_id) VALUES ($1 || '-site', $2) RETURNING id",
    )
    .bind(name)
    .bind(customer)
    .fetch_one(pool)
    .await
    .unwrap();
    let server = sqlx::query_scalar(
        "INSERT INTO servers (peer_id, site_id, hostname, os_family)
         VALUES ($1 || '-web', $2, $1 || '-web', 'debian') RETURNING id",
    )
    .bind(name)
    .bind(site)
    .fetch_one(pool)
    .await
    .unwrap();
    let relay = sqlx::query_scalar(
        "INSERT INTO relays (peer_id, site_id, name) VALUES ($1 || '-gw', $2, $1 || '-gw') RETURNING id",
    )
    .bind(name)
    .bind(site)
    .fetch_one(pool)
    .await
    .unwrap();
    (customer, server, relay)
}

/// Opens an incident on `server` and records `kinds` as its events.
async fn incident(pool: &PgPool, server: Uuid, kinds: &[(&str, Option<&str>)]) -> Uuid {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO incidents (server_id, metric, instance, severity, opened_at, last_value,
                                last_value_at, peak_value, threshold)
         VALUES ($1, 'monitorflow_cpu_usage_percent', '', 'warning', now(), 91, now(), 91, '{}')
         RETURNING id",
    )
    .bind(server)
    .fetch_one(pool)
    .await
    .unwrap();
    for (kind, severity) in kinds {
        sqlx::query(
            "INSERT INTO incident_events (incident_id, occurred_at, kind, severity, value)
             VALUES ($1, now(), $2, $3, 91)",
        )
        .bind(id)
        .bind(kind)
        .bind(severity)
        .execute(pool)
        .await
        .unwrap();
    }
    id
}

async fn outbox(pool: &PgPool) -> Vec<(String, Option<Uuid>, Value)> {
    sqlx::query_as("SELECT event, customer_id, details FROM notification_outbox ORDER BY id")
        .fetch_all(pool)
        .await
        .unwrap()
}

/// A webhook receiver on loopback that records each request, answering
/// `status`.
async fn receiver(status: StatusCode) -> (String, Arc<Mutex<Vec<(HeaderMap, Bytes)>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let store = seen.clone();
    let app = axum::Router::new().route(
        "/hook",
        axum::routing::post(move |headers: HeaderMap, body: Bytes| {
            let store = store.clone();
            async move {
                store.lock().unwrap().push((headers, body));
                status
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/hook", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, seen)
}

/// The smallest SMTP server lettre talks to without TLS: it offers no
/// STARTTLS and returns each message's DATA.
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

/// Texts the webhook `hooks` received, in order, forgetting them.
fn texts(hooks: &Mutex<Vec<(HeaderMap, Bytes)>>) -> Vec<String> {
    let mut texts: Vec<String> = hooks
        .lock()
        .unwrap()
        .drain(..)
        .map(|(_, body)| {
            let payload: Value = serde_json::from_slice(&body).unwrap();
            payload["text"].as_str().unwrap().to_owned()
        })
        .collect();
    texts.sort();
    texts
}

async fn offline_check(pool: &PgPool, up: &[(PeerRole, &str)]) {
    let online = up
        .iter()
        .map(|&(role, id)| ((role, id.to_owned()), Utc::now()))
        .collect();
    let started = Utc::now() - TimeDelta::days(1);
    offline::check(pool, &online, started, &OfflineSettings::default())
        .await
        .unwrap();
}

/// Writes the `status.json` of an updater whose last backup failed.
fn failed_backup(dir: &std::path::Path, last_success: Option<chrono::DateTime<Utc>>) {
    let status = Status {
        updater_version: "0.1.0".into(),
        heartbeat_at: Utc::now(),
        running: None,
        update: None,
        retention: None,
        backups: Backups {
            last: Some(BackupRun {
                id: Uuid::new_v4().to_string(),
                kind: BackupKind::Daily,
                requested_by: String::new(),
                state: BackupState::Failed,
                error: Some("pg_dump: no space left on device".into()),
                warning: None,
                file: None,
                started_at: Utc::now(),
                finished_at: Some(Utc::now()),
            }),
            last_success_at: last_success,
            ..Backups::default()
        },
    };
    std::fs::write(dir.join(STATUS_FILE), serde_json::to_vec(&status).unwrap()).unwrap();
}

/// A second site of `customer` with a server: `(site, server)`.
async fn other_site(pool: &PgPool, customer: Uuid, name: &str) -> (Uuid, Uuid) {
    let site: Uuid =
        sqlx::query_scalar("INSERT INTO sites (name, customer_id) VALUES ($1, $2) RETURNING id")
            .bind(name)
            .bind(customer)
            .fetch_one(pool)
            .await
            .unwrap();
    let server = sqlx::query_scalar(
        "INSERT INTO servers (peer_id, site_id, hostname, os_family)
         VALUES ($1 || '-web', $2, $1 || '-web', 'debian') RETURNING id",
    )
    .bind(name)
    .bind(site)
    .fetch_one(pool)
    .await
    .unwrap();
    (site, server)
}

async fn site_of(pool: &PgPool, server: Uuid) -> Uuid {
    sqlx::query_scalar("SELECT site_id FROM servers WHERE id = $1")
        .bind(server)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// A webhook channel on loopback, by the name its tests call it.
struct Listener {
    name: &'static str,
    seen: Arc<Mutex<Vec<(HeaderMap, Bytes)>>>,
}

async fn listener(pool: &PgPool, name: &'static str, coverage: Coverage) -> Listener {
    let (url, seen) = receiver(StatusCode::OK).await;
    let hook = Destination::Webhook(Webhook { url });
    notify::insert(pool, &hook, None, coverage, false)
        .await
        .unwrap();
    Listener { name, seen }
}

/// Delivers the outbox and names the listeners that got `count` messages,
/// failing if any got another number.
async fn heard(dispatcher: &Dispatcher, listeners: &[Listener], count: usize) -> Vec<&'static str> {
    dispatcher.pass().await.unwrap();
    let mut names = Vec::new();
    for listener in listeners {
        let got = listener.seen.lock().unwrap().drain(..).count();
        assert!(
            got == 0 || got == count,
            "{}: {got} messages",
            listener.name
        );
        if got > 0 {
            names.push(listener.name);
        }
    }
    names
}

fn parts(response: (StatusCode, Value, Option<String>)) -> (StatusCode, Value) {
    (response.0, response.1)
}

fn fast(pool: &PgPool) -> Dispatcher {
    Dispatcher::new(pool.clone(), "https://mf.example")
        .with_retries(vec![Duration::ZERO])
        .without_waits()
}

db_tests! {
    #[sqlx::test]
    async fn triggers_fill_the_outbox_from_incidents_and_updates(pool: PgPool) {
        let (customer, server, relay) = place(&pool, "acme").await;
        incident(
            &pool,
            server,
            &[("opened", Some("warning")), ("escalated", Some("critical")),
              ("deescalated", Some("warning")), ("resolved", None)],
        )
        .await;
        let events = outbox(&pool).await;
        let kinds: Vec<&str> = events.iter().map(|(e, _, _)| e.as_str()).collect();
        assert_eq!(kinds, ["incident.opened", "incident.escalated", "incident.resolved"]);
        let (_, owner, details) = &events[0];
        assert_eq!(*owner, Some(customer));
        assert_eq!(details["server"]["name"], "acme-web");
        assert_eq!(details["site"]["name"], "acme-site");
        assert_eq!(details["customer"]["name"], "acme");
        assert_eq!(details["incident"]["metric"], "monitorflow_cpu_usage_percent");
        assert_eq!(events[1].2["incident"]["severity"], "critical");
        assert_eq!(events[2].2["incident"]["severity"], "warning");
        sqlx::query("DELETE FROM notification_outbox").execute(&pool).await.unwrap();

        // An update outside a campaign that fails notifies; a campaign's
        // own commands do not, nor do other kinds.
        for kind in ["apply_updates", "shell"] {
            sqlx::query(
                "INSERT INTO commands (server_id, kind, payload, expires_at)
                 VALUES ($1, $2, '', now() + interval '1 hour')",
            )
            .bind(server)
            .bind(kind)
            .execute(&pool)
            .await
            .unwrap();
        }
        sqlx::query(
            "UPDATE commands SET state = 'failed', finished_at = now(), error = 'dpkg was interrupted'",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO relay_commands (relay_id, kind, payload, version, expires_at)
             VALUES ($1, 'self_update', '', '1.2.0', now() + interval '1 hour')",
        )
        .bind(relay)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("UPDATE relay_commands SET state = 'expired', finished_at = now()")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO auto_rollouts (kind, version, state, halted_at, reason)
             VALUES ('agent', '1.3.0', 'halted', now(), '2 updates failed')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO campaigns (customer_id, name, state, started_at, finished_at, error)
             VALUES ($1, 'May', 'running', now(), NULL, NULL)",
        )
        .bind(customer)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("UPDATE campaigns SET state = 'failed', finished_at = now(), error = 'wave 1 failed'")
            .execute(&pool)
            .await
            .unwrap();
        let events = outbox(&pool).await;
        assert_eq!(events.len(), 4, "{events:?}");
        assert!(events.iter().all(|(e, _, _)| e == "update.failed"));
        assert_eq!(events[0].2["update"]["kind"], "apply_updates");
        assert_eq!(events[0].2["update"]["error"], "dpkg was interrupted");
        assert_eq!(events[0].1, Some(customer));
        assert_eq!(events[1].2["relay"]["name"], "acme-gw");
        assert_eq!(events[1].2["update"]["version"], "1.2.0");
        assert_eq!(events[1].1, Some(customer));
        assert_eq!(events[2].2["update"]["kind"], "automatic_rollout");
        assert_eq!(events[2].1, None);
        assert_eq!(events[3].2["update"]["kind"], "campaign");
        assert_eq!(events[3].2["update"]["error"], "wave 1 failed");
        assert_eq!(events[3].1, Some(customer));
    }

    #[sqlx::test]
    async fn a_command_parked_for_approval_notifies(pool: PgPool) {
        let (customer, server, _) = place(&pool, "acme").await;
        let remediation: Uuid = sqlx::query_scalar(
            "INSERT INTO remediation_incidents (customer_id, server_id, metric, max_commands, max_duration_s)
             VALUES ($1, $2, 'monitorflow_cpu_usage_percent', 5, 600) RETURNING id",
        )
        .bind(customer)
        .bind(server)
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO remediation_incident_events
                 (incident_id, seq, kind, actor_kind, actor_id, payload, prev_hash, hash)
             VALUES ($1, 1, 'ai_proposal', 'ai', 'test', '{}', decode(repeat('00', 32), 'hex'),
                     decode(repeat('00', 32), 'hex'))",
        )
        .bind(remediation)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO remediation_pending_commands
                 (incident_id, proposal_seq, command, diagnosis, reasoning, tier, rule, reason)
             VALUES ($1, 1, 'systemctl restart nginx', 'd', 'r', 'high_impact', '{}', 'unknown')",
        )
        .bind(remediation)
        .execute(&pool)
        .await
        .unwrap();
        let events = outbox(&pool).await;
        assert_eq!(events.len(), 1);
        let (event, owner, details) = &events[0];
        assert_eq!((event.as_str(), *owner), ("approval.waiting", Some(customer)));
        assert_eq!(details["approval"]["command"], "systemctl restart nginx");
        assert_eq!(details["server"]["name"], "acme-web");
    }

    #[sqlx::test]
    async fn events_reach_signed_webhooks_and_email_of_their_customer(pool: PgPool) {
        let (acme, acme_server, _) = place(&pool, "acme").await;
        let (globex, _, _) = place(&pool, "globex").await;
        let (url, hooks) = receiver(StatusCode::NO_CONTENT).await;
        let (port, mails) = smtp_server().await;
        let hook = Destination::Webhook(Webhook { url });
        notify::insert(&pool, &hook, Some(&Secret::new("s3cret")), Coverage::Customer(acme), false).await.unwrap();
        let email = Destination::Email(Email {
            host: "127.0.0.1".into(),
            port,
            username: String::new(),
            from: "mf@example.com".into(),
            to: vec!["ops@example.com".into()],
        });
        let mail_channel = notify::insert(&pool, &email, None, Coverage::Everything, false).await.unwrap();
        let (other_url, other_hooks) = receiver(StatusCode::OK).await;
        let other = Destination::Webhook(Webhook { url: other_url });
        notify::insert(&pool, &other, None, Coverage::Customer(globex), false).await.unwrap();

        incident(&pool, acme_server, &[("opened", Some("critical"))]).await;
        let dispatcher = fast(&pool);
        assert_eq!(dispatcher.pass().await.unwrap(), 1);
        assert_eq!(dispatcher.pass().await.unwrap(), 0, "claimed events are not sent again");

        let hooks = hooks.lock().unwrap().clone();
        assert_eq!(hooks.len(), 1);
        let (headers, body) = &hooks[0];
        let payload: Value = serde_json::from_slice(body).unwrap();
        assert_eq!(payload["version"], 1);
        assert_eq!(payload["event"], "incident.opened");
        assert_eq!(payload["text"], "Critical: cpu_usage_percent is 91 on acme-web (acme / acme-site)");
        assert!(payload["url"].as_str().unwrap().starts_with("https://mf.example/incidents/view/?id="));
        let timestamp: i64 = headers[TIMESTAMP_HEADER].to_str().unwrap().parse().unwrap();
        assert_eq!(headers[SIGNATURE_HEADER].to_str().unwrap(), signature("s3cret", timestamp, body));
        assert_eq!(headers["x-monitorflow-event"], "incident.opened");
        assert!(other_hooks.lock().unwrap().is_empty(), "another customer's channel");

        let mails = mails.lock().unwrap().clone();
        assert_eq!(mails.len(), 1);
        assert!(mails[0].contains("Subject: [MonitorFlow] Critical: cpu_usage_percent"), "{}", mails[0]);
        assert!(mails[0].contains("Server: acme-web"), "{}", mails[0]);
        let (sent, error): (Option<chrono::DateTime<chrono::Utc>>, Option<String>) = sqlx::query_as(
            "SELECT last_sent_at, last_error FROM notification_channels WHERE id = $1",
        )
        .bind(mail_channel)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(sent.is_some() && error.is_none());
    }

    #[sqlx::test]
    async fn a_single_company_reads_no_customer_until_a_second_one_exists(pool: PgPool) {
        // The install's own customer, renamed, is the only one (ADR-0104).
        let own: Uuid = sqlx::query_scalar("UPDATE customers SET name = 'acme' RETURNING id")
            .fetch_one(&pool)
            .await
            .unwrap();
        let site: Uuid = sqlx::query_scalar(
            "INSERT INTO sites (name, customer_id) VALUES ('paris', $1) RETURNING id",
        )
        .bind(own)
        .fetch_one(&pool)
        .await
        .unwrap();
        let server: Uuid = sqlx::query_scalar(
            "INSERT INTO servers (peer_id, site_id, hostname, os_family)
             VALUES ('web-1', $1, 'web-1', 'debian') RETURNING id",
        )
        .bind(site)
        .fetch_one(&pool)
        .await
        .unwrap();
        let (url, hooks) = receiver(StatusCode::NO_CONTENT).await;
        let (port, mails) = smtp_server().await;
        notify::insert(&pool, &Destination::Webhook(Webhook { url }), None, Coverage::Everything, false)
            .await
            .unwrap();
        let email = Destination::Email(Email {
            host: "127.0.0.1".into(),
            port,
            username: String::new(),
            from: "mf@example.com".into(),
            to: vec!["ops@example.com".into()],
        });
        notify::insert(&pool, &email, None, Coverage::Everything, false).await.unwrap();
        let dispatcher = fast(&pool);

        incident(&pool, server, &[("opened", Some("critical"))]).await;
        assert_eq!(dispatcher.pass().await.unwrap(), 1);
        let mail = mails.lock().unwrap()[0].clone();
        assert!(mail.contains("Critical: cpu_usage_percent is 91 on web-1 (paris)"), "{mail}");
        assert!(mail.contains("Site: paris") && !mail.contains("acme"), "{mail}");
        // A webhook's payload does not change with the number of customers.
        let payload: Value = serde_json::from_slice(&hooks.lock().unwrap()[0].1).unwrap();
        assert_eq!(payload["text"], "Critical: cpu_usage_percent is 91 on web-1 (acme / paris)");

        let (_, other, _) = place(&pool, "globex").await;
        incident(&pool, other, &[("opened", Some("critical"))]).await;
        assert_eq!(dispatcher.pass().await.unwrap(), 1);
        let mail = mails.lock().unwrap()[1].clone();
        assert!(mail.contains("on globex-web (globex / globex-site)"), "{mail}");
        assert!(mail.contains("Customer: globex"), "{mail}");
    }

    #[sqlx::test]
    async fn offline_servers_and_relays_notify_with_the_right_ending(pool: PgPool) {
        let (customer, _, relay) = place(&pool, "acme").await;
        for table in ["servers", "relays"] {
            // SQL-safe: `table` is a literal name from this test.
            sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE {table} SET last_seen_at = now() - interval '1 hour'")))
                .execute(&pool)
                .await
                .unwrap();
        }
        let (url, hooks) = receiver(StatusCode::OK).await;
        let hook = Destination::Webhook(Webhook { url });
        notify::insert(&pool, &hook, None, Coverage::Customer(customer), false).await.unwrap();
        let dispatcher = fast(&pool);

        offline_check(&pool, &[]).await;
        let events = outbox(&pool).await;
        assert_eq!(events.len(), 2, "{events:?}");
        assert!(events.iter().all(|(e, owner, _)| e == "incident.opened" && *owner == Some(customer)));
        let on_relay = events.iter().find(|(_, _, d)| !d["relay"].is_null()).unwrap();
        assert_eq!(on_relay.2["relay"]["id"], relay.to_string());
        assert_eq!(on_relay.2["server"], Value::Null);
        assert_eq!(on_relay.2["site"]["name"], "acme-site");
        dispatcher.pass().await.unwrap();
        assert_eq!(
            texts(&hooks),
            [
                "Critical: acme-web (acme / acme-site) is offline",
                "Critical: relay acme-gw (acme / acme-site) is offline",
            ]
        );

        // The server reconnects; the relay is revoked, which is not back.
        sqlx::query("UPDATE relays SET revoked_at = now() WHERE id = $1")
            .bind(relay)
            .execute(&pool)
            .await
            .unwrap();
        offline_check(&pool, &[(PeerRole::Agent, "acme-web")]).await;
        dispatcher.pass().await.unwrap();
        assert_eq!(
            texts(&hooks),
            [
                "Resolved: acme-web (acme / acme-site) is back online",
                "Resolved: relay acme-gw (acme / acme-site) was removed",
            ]
        );
    }

    #[sqlx::test]
    async fn a_failed_backup_notifies_once_per_outage(pool: PgPool) {
        let (customer, _, _) = place(&pool, "acme").await;
        let (url, hooks) = receiver(StatusCode::OK).await;
        let (scoped_url, scoped) = receiver(StatusCode::OK).await;
        let all = Destination::Webhook(Webhook { url });
        notify::insert(&pool, &all, None, Coverage::Everything, false).await.unwrap();
        let one = Destination::Webhook(Webhook { url: scoped_url });
        notify::insert(&pool, &one, None, Coverage::Customer(customer), false).await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let dispatcher = fast(&pool);

        // Without a status, or an updater that has not reported, nothing.
        assert!(!backup::check(&pool, dir.path()).await.unwrap());

        let since = Utc::now() - TimeDelta::days(2);
        failed_backup(dir.path(), Some(since));
        assert!(backup::check(&pool, dir.path()).await.unwrap());
        dispatcher.pass().await.unwrap();
        assert_eq!(texts(&hooks), ["Backup of the server failed: pg_dump: no space left on device"]);
        assert!(scoped.lock().unwrap().is_empty(), "server-wide: not one customer's channel");

        // The hourly retry fails too: the same outage.
        failed_backup(dir.path(), Some(since));
        assert!(!backup::check(&pool, dir.path()).await.unwrap());

        // A success in between makes the next failure a new outage.
        failed_backup(dir.path(), Some(Utc::now()));
        assert!(backup::check(&pool, dir.path()).await.unwrap());
        // A server that never had a backup has one outage too.
        failed_backup(dir.path(), None);
        assert!(backup::check(&pool, dir.path()).await.unwrap());
        failed_backup(dir.path(), None);
        assert!(!backup::check(&pool, dir.path()).await.unwrap());
    }

    #[sqlx::test]
    async fn the_api_never_returns_secrets_and_tests_in_plain_words(pool: PgPool) {
        let (customer, _, _) = place(&pool, "acme").await;
        let root = login(&pool, "root", Role::Administrator, GrantScope::Global).await;
        let boss = login(&pool, "boss", Role::Administrator, GrantScope::Customer(customer)).await;

        let (status, body, _) = call(&pool, "GET", CHANNELS, Some(&boss), None).await;
        assert_eq!((status, body), (StatusCode::OK, json!({ "channels": [] })));

        let (url, hooks) = receiver(StatusCode::OK).await;
        let request = json!({"kind": "webhook", "settings": {"url": url}, "secret": "hunter2-secret"});
        let (status, created, _) = call(&pool, "POST", CHANNELS, Some(&root), Some(request)).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        assert_eq!(created["has_secret"], true);
        assert_eq!(created["customer_id"], Value::Null);
        let id = created["id"].as_str().unwrap().to_owned();

        let (status, list, _) = call(&pool, "GET", CHANNELS, Some(&root), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(list["channels"].as_array().unwrap().len(), 1);
        assert!(!list.to_string().contains("hunter2"));

        // An update without a secret keeps it; the customer scope changes.
        let update = json!({"kind": "webhook", "settings": {"url": url}, "customer_id": customer});
        let (status, updated, _) =
            call(&pool, "PUT", &format!("{CHANNELS}/{id}"), Some(&root), Some(update)).await;
        assert_eq!(status, StatusCode::OK, "{updated}");
        assert_eq!(updated["has_secret"], true);
        assert_eq!(updated["customer_name"], "acme");

        let (status, result, _) =
            call(&pool, "POST", &format!("{CHANNELS}/{id}/test"), Some(&root), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(result, json!({"ok": true, "error": null}));
        let hooks = hooks.lock().unwrap().clone();
        assert_eq!(hooks.len(), 1);
        assert!(hooks[0].0.contains_key(SIGNATURE_HEADER));

        // Nothing listens on a port just freed: the error says so.
        let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dead = format!("http://{}/hook", closed.local_addr().unwrap());
        drop(closed);
        let request = json!({"kind": "webhook", "settings": {"url": dead}});
        let (_, created, _) = call(&pool, "POST", CHANNELS, Some(&root), Some(request)).await;
        let dead_id = created["id"].as_str().unwrap().to_owned();
        let (_, result, _) =
            call(&pool, "POST", &format!("{CHANNELS}/{dead_id}/test"), Some(&root), None).await;
        assert_eq!(result["ok"], false);
        let error = result["error"].as_str().unwrap();
        assert!(error.starts_with("Could not connect to 127.0.0.1"), "{error}");
        let (_, list, _) = call(&pool, "GET", CHANNELS, Some(&root), None).await;
        assert_eq!(list["channels"][1]["last_error"], error);

        let bad = json!({"kind": "email", "settings": {"host": "smtp.x.io", "to": []}});
        let (status, body, _) = call(&pool, "POST", CHANNELS, Some(&root), Some(bad)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.to_string().contains("at least one address"), "{body}");

        let (status, _, _) =
            call(&pool, "DELETE", &format!("{CHANNELS}/{dead_id}"), Some(&root), None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let audit: Vec<String> = sqlx::query_scalar(
            "SELECT details::text FROM audit_log WHERE action LIKE 'notification.%' ORDER BY id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(audit.len(), 4);
        assert!(audit.iter().all(|d| !d.contains("hunter2")));
    }

    #[sqlx::test]
    async fn every_event_reaches_only_the_channels_that_cover_it(pool: PgPool) {
        let (acme, paris_server, paris_relay) = place(&pool, "acme").await;
        let (globex, globex_server, _) = place(&pool, "globex").await;
        let paris = site_of(&pool, paris_server).await;
        let globex_site = site_of(&pool, globex_server).await;
        let (lyon, lyon_server) = other_site(&pool, acme, "lyon").await;
        let listeners = [
            listener(&pool, "everything", Coverage::Everything).await,
            listener(&pool, "acme", Coverage::Customer(acme)).await,
            listener(&pool, "paris", Coverage::Site(paris)).await,
            listener(&pool, "lyon", Coverage::Site(lyon)).await,
            listener(&pool, "globex", Coverage::Customer(globex)).await,
            listener(&pool, "globex-site", Coverage::Site(globex_site)).await,
        ];
        let dispatcher = fast(&pool);
        let at_paris = ["everything", "acme", "paris"];

        // Incidents, each kind; at the site's server, its neighbour's site,
        // and another customer.
        let kinds = [("opened", Some("warning")), ("escalated", Some("critical")), ("resolved", None)];
        incident(&pool, paris_server, &kinds).await;
        assert_eq!(heard(&dispatcher, &listeners, 3).await, at_paris);
        incident(&pool, lyon_server, &kinds).await;
        assert_eq!(heard(&dispatcher, &listeners, 3).await, ["everything", "acme", "lyon"]);
        incident(&pool, globex_server, &kinds).await;
        assert_eq!(heard(&dispatcher, &listeners, 3).await, ["everything", "globex", "globex-site"]);

        // An offline relay names the site through the relay.
        sqlx::query("UPDATE relays SET last_seen_at = now() - interval '1 hour' WHERE id = $1")
            .bind(paris_relay)
            .execute(&pool)
            .await
            .unwrap();
        offline_check(&pool, &[]).await;
        assert_eq!(heard(&dispatcher, &listeners, 1).await, at_paris);

        // A command waiting for approval.
        let remediation: Uuid = sqlx::query_scalar(
            "INSERT INTO remediation_incidents (customer_id, server_id, metric, max_commands, max_duration_s)
             VALUES ($1, $2, 'monitorflow_cpu_usage_percent', 5, 600) RETURNING id",
        )
        .bind(acme)
        .bind(paris_server)
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO remediation_incident_events
                 (incident_id, seq, kind, actor_kind, actor_id, payload, prev_hash, hash)
             VALUES ($1, 1, 'ai_proposal', 'ai', 'test', '{}', decode(repeat('00', 32), 'hex'),
                     decode(repeat('00', 32), 'hex'))",
        )
        .bind(remediation)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO remediation_pending_commands
                 (incident_id, proposal_seq, command, diagnosis, reasoning, tier, rule, reason)
             VALUES ($1, 1, 'systemctl restart nginx', 'd', 'r', 'high_impact', '{}', 'unknown')",
        )
        .bind(remediation)
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(heard(&dispatcher, &listeners, 1).await, at_paris);

        // Updates of a server, either kind, and of a relay.
        for kind in ["apply_updates", "self_update"] {
            sqlx::query(
                "INSERT INTO commands (server_id, kind, payload, expires_at)
                 VALUES ($1, $2, '', now() + interval '1 hour')",
            )
            .bind(paris_server)
            .bind(kind)
            .execute(&pool)
            .await
            .unwrap();
        }
        sqlx::query("UPDATE commands SET state = 'failed', finished_at = now()")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(heard(&dispatcher, &listeners, 2).await, at_paris);
        sqlx::query(
            "INSERT INTO relay_commands (relay_id, kind, payload, version, expires_at)
             VALUES ($1, 'self_update', '', '1.2.0', now() + interval '1 hour')",
        )
        .bind(paris_relay)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("UPDATE relay_commands SET state = 'expired', finished_at = now()")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(heard(&dispatcher, &listeners, 1).await, at_paris);

        // A campaign names a site, or its customer as a whole.
        for (site, expected) in [(Some(paris), &at_paris[..]), (None, &["everything", "acme"][..])] {
            sqlx::query(
                "INSERT INTO campaigns (customer_id, site_id, name, state, started_at)
                 VALUES ($1, $2, 'May', 'running', now())",
            )
            .bind(acme)
            .bind(site)
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query("UPDATE campaigns SET state = 'failed', finished_at = now(), error = 'x' WHERE state = 'running'")
                .execute(&pool)
                .await
                .unwrap();
            assert_eq!(heard(&dispatcher, &listeners, 1).await, expected);
        }

        // Server-wide: a halted rollout, a failed backup, a new release.
        sqlx::query(
            "INSERT INTO auto_rollouts (kind, version, state, halted_at, reason)
             VALUES ('agent', '1.3.0', 'halted', now(), '2 updates failed')",
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(heard(&dispatcher, &listeners, 1).await, ["everything"]);
        let dir = tempfile::tempdir().unwrap();
        failed_backup(dir.path(), None);
        assert!(backup::check(&pool, dir.path()).await.unwrap());
        assert_eq!(heard(&dispatcher, &listeners, 1).await, ["everything"]);
        let (new, running) = ("9.9.9".parse().unwrap(), "1.0.0".parse().unwrap());
        assert!(release_check::record(&pool, &new, None, &running).await.unwrap());
        assert_eq!(heard(&dispatcher, &listeners, 1).await, ["everything"]);
    }

    #[sqlx::test(migrations = false)]
    async fn existing_channels_keep_their_reach_after_the_migration(pool: PgPool) {
        // The schema as released before routing, with data in it.
        let before = Migrator {
            migrations: Cow::Owned(
                db::MIGRATOR.iter().filter(|m| m.version < ROUTING_MIGRATION).cloned().collect(),
            ),
            ..Migrator::DEFAULT
        };
        before.run(&pool).await.unwrap();
        let (customer, server, _) = place(&pool, "acme").await;
        let (url, everything) = receiver(StatusCode::OK).await;
        let (scoped_url, scoped) = receiver(StatusCode::OK).await;
        for (url, owner) in [(url, None), (scoped_url, Some(customer))] {
            sqlx::query(
                "INSERT INTO notification_channels (kind, settings, secret, customer_id)
                 VALUES ('webhook', $1, 's3cret', $2)",
            )
            .bind(json!({ "url": url }))
            .bind(owner)
            .execute(&pool)
            .await
            .unwrap();
        }
        // Waiting in the outbox across the upgrade.
        incident(&pool, server, &[("opened", Some("warning"))]).await;
        sqlx::query("INSERT INTO notification_outbox (event, details) VALUES ('update.available', '{}')")
            .execute(&pool)
            .await
            .unwrap();

        db::migrate(&pool).await.unwrap();
        let channels = notify::channels(&pool).await.unwrap();
        let coverage: Vec<Coverage> = channels.iter().map(|c| c.coverage).collect();
        assert_eq!(coverage.len(), 2);
        assert!(coverage.contains(&Coverage::Everything));
        assert!(coverage.contains(&Coverage::Customer(customer)));
        assert!(channels.iter().all(|c| !c.public_only && c.secret.is_some()));

        fast(&pool).pass().await.unwrap();
        assert_eq!(everything.lock().unwrap().len(), 2);
        assert_eq!(scoped.lock().unwrap().len(), 1);
        // The old row names its site too.
        let site: Option<Uuid> = sqlx::query_scalar(
            "SELECT site_id FROM notification_outbox WHERE event = 'incident.opened'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(site, Some(site_of(&pool, server).await));
    }

    #[sqlx::test]
    async fn a_channel_leaves_with_its_customer_or_site(pool: PgPool) {
        let (customer, kept_server, _) = place(&pool, "acme").await;
        let (site, _) = other_site(&pool, customer, "lyon").await;
        let empty: Uuid = sqlx::query_scalar("INSERT INTO customers (name) VALUES ('gone') RETURNING id")
            .fetch_one(&pool)
            .await
            .unwrap();
        // The site has a server: only an empty site can go.
        sqlx::query("DELETE FROM servers WHERE site_id = $1").bind(site).execute(&pool).await.unwrap();
        let listeners = [
            listener(&pool, "everything", Coverage::Everything).await,
            listener(&pool, "site", Coverage::Site(site)).await,
            listener(&pool, "customer", Coverage::Customer(empty)).await,
        ];
        sqlx::query("DELETE FROM sites WHERE id = $1").bind(site).execute(&pool).await.unwrap();
        sqlx::query("DELETE FROM customers WHERE id = $1").bind(empty).execute(&pool).await.unwrap();

        let left = notify::channels(&pool).await.unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].coverage, Coverage::Everything);
        incident(&pool, kept_server, &[("opened", Some("warning"))]).await;
        assert_eq!(heard(&fast(&pool), &listeners, 1).await, ["everything"]);

        // Both scopes at once is no scope the code can read.
        let both = sqlx::query(
            "INSERT INTO notification_channels (kind, settings, customer_id, site_id)
             VALUES ('webhook', '{}', $1, (SELECT id FROM sites LIMIT 1))",
        )
        .bind(customer)
        .execute(&pool)
        .await;
        assert!(both.unwrap_err().to_string().contains("notification_channels_one_scope"));
    }

    #[sqlx::test]
    async fn administrators_manage_the_channels_of_what_they_administer(pool: PgPool) {
        let (acme, acme_server, _) = place(&pool, "acme").await;
        let (globex, _, _) = place(&pool, "globex").await;
        let paris = site_of(&pool, acme_server).await;
        let (lyon, _) = other_site(&pool, acme, "lyon").await;
        let root = login(&pool, "root", Role::Administrator, GrantScope::Global).await;
        let boss = login(&pool, "boss", Role::Administrator, GrantScope::Customer(acme)).await;
        let local = login(&pool, "local", Role::Administrator, GrantScope::Site(paris)).await;
        let operator = login(&pool, "operator", Role::Operator, GrantScope::Customer(acme)).await;
        let viewer = login(&pool, "viewer", Role::Viewer, GrantScope::Global).await;

        let hook = |scope: Value| {
            let mut body = json!({"kind": "webhook", "settings": {"url": "https://hooks.example/x"}});
            body.as_object_mut().unwrap().extend(scope.as_object().unwrap().clone());
            body
        };
        let everything = hook(json!({}));
        let of_acme = hook(json!({ "customer_id": acme }));
        let of_paris = hook(json!({ "site_id": paris }));
        let of_lyon = hook(json!({ "site_id": lyon }));
        let of_globex = hook(json!({ "customer_id": globex }));
        let create = |who: String, body: Value| {
            let pool = pool.clone();
            async move { parts(call(&pool, "POST", CHANNELS, Some(&who), Some(body)).await) }
        };

        // Who creates what.
        for (who, body, expected) in [
            (&root, &everything, StatusCode::CREATED),
            (&root, &of_globex, StatusCode::CREATED),
            (&boss, &of_acme, StatusCode::CREATED),
            (&boss, &of_lyon, StatusCode::CREATED),
            (&local, &of_paris, StatusCode::CREATED),
            (&boss, &everything, StatusCode::FORBIDDEN),
            (&boss, &of_globex, StatusCode::FORBIDDEN),
            (&local, &everything, StatusCode::FORBIDDEN),
            (&local, &of_acme, StatusCode::FORBIDDEN),
            (&local, &of_lyon, StatusCode::FORBIDDEN),
            (&operator, &of_acme, StatusCode::FORBIDDEN),
            (&viewer, &everything, StatusCode::FORBIDDEN),
        ] {
            let (status, body) = create(who.clone(), body.clone()).await;
            assert_eq!(status, expected, "{body}");
        }

        // An ID that exists and one that does not answer alike.
        let nothing = hook(json!({ "customer_id": Uuid::new_v4() }));
        assert_eq!(create(boss.clone(), nothing.clone()).await.0, StatusCode::FORBIDDEN);
        assert_eq!(create(root.clone(), nothing).await.0, StatusCode::BAD_REQUEST);
        let unknown = hook(json!({ "site_id": Uuid::new_v4() }));
        assert_eq!(create(boss.clone(), unknown.clone()).await.0, StatusCode::FORBIDDEN);
        assert_eq!(create(root.clone(), unknown).await.0, StatusCode::BAD_REQUEST);
        let both = hook(json!({ "customer_id": acme, "site_id": paris }));
        assert_eq!(create(root.clone(), both).await.0, StatusCode::BAD_REQUEST);

        // Who sees what: a site channel names its customer and site.
        let list = |who: String| {
            let pool = pool.clone();
            async move {
                let (status, body, _) = call(&pool, "GET", CHANNELS, Some(&who), None).await;
                assert_eq!(status, StatusCode::OK);
                body["channels"].as_array().unwrap().clone()
            }
        };
        assert_eq!(list(root.clone()).await.len(), 5);
        assert_eq!(list(boss.clone()).await.len(), 3);
        let seen = list(local.clone()).await;
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0]["site_name"], "acme-site");
        assert_eq!(seen[0]["customer_name"], "acme");
        assert_eq!(seen[0]["customer_id"], acme.to_string());
        for who in [&operator, &viewer] {
            let (status, _, _) = call(&pool, "GET", CHANNELS, Some(who), None).await;
            assert_eq!(status, StatusCode::FORBIDDEN);
        }

        // Out of reach is 404 for each action; a customer's own moves
        // between its sites, not out of them.
        let all = list(root.clone()).await;
        let id_of = |f: &dyn Fn(&Value) -> bool| {
            all.iter().find(|c| f(c)).unwrap()["id"].as_str().unwrap().to_owned()
        };
        let global_id = id_of(&|c| c["customer_id"].is_null());
        let globex_id = id_of(&|c| c["customer_id"] == globex.to_string());
        let lyon_id = id_of(&|c| c["site_id"] == lyon.to_string());
        for id in [&global_id, &globex_id, &lyon_id] {
            let uri = format!("{CHANNELS}/{id}");
            for (method, uri, body) in [
                ("PUT", uri.clone(), Some(of_paris.clone())),
                ("POST", format!("{uri}/test"), None),
                ("DELETE", uri.clone(), None),
            ] {
                let (status, _, _) = call(&pool, method, &uri, Some(&local), body).await;
                assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}");
            }
        }
        let uri = format!("{CHANNELS}/{lyon_id}");
        let (status, moved, _) = call(&pool, "PUT", &uri, Some(&boss), Some(of_paris.clone())).await;
        assert_eq!(status, StatusCode::OK, "{moved}");
        assert_eq!(moved["site_name"], "acme-site");
        let (status, _, _) = call(&pool, "PUT", &uri, Some(&boss), Some(of_globex)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _, _) = call(&pool, "PUT", &uri, Some(&boss), Some(everything)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _, _) = call(&pool, "DELETE", &uri, Some(&local), None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        // The audit names the coverage.
        let audit: Vec<Value> = sqlx::query_scalar(
            "SELECT details FROM audit_log WHERE action = 'notification.channel_create'",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert!(audit.iter().any(|d| d["site_id"] == paris.to_string()));
    }

    #[sqlx::test]
    async fn a_saved_secret_stays_with_its_destination(pool: PgPool) {
        let (acme, _, _) = place(&pool, "acme").await;
        let root = login(&pool, "root", Role::Administrator, GrantScope::Global).await;
        let boss = login(&pool, "boss", Role::Administrator, GrantScope::Customer(acme)).await;
        let mail = |host: &str, secret: Option<&str>, to: &str| {
            json!({"kind": "email", "secret": secret, "customer_id": acme, "settings": {
                "host": host, "username": "ops@msp.example", "to": [to]}})
        };
        let request = mail("smtp.msp.example", Some("msp-password"), "a@acme.example");
        let (status, created, _) = call(&pool, "POST", CHANNELS, Some(&root), Some(request)).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let uri = format!("{CHANNELS}/{}", created["id"].as_str().unwrap());

        // The MSP's password does not follow a new server...
        let steal = mail("smtp.evil.example", None, "a@acme.example");
        let (status, body, _) = call(&pool, "PUT", &uri, Some(&boss), Some(steal)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.to_string().contains("again"), "{body}");
        // ...for a global administrator either; recipients may change...
        let moved = mail("smtp.other.example", None, "a@acme.example");
        let (status, _, _) = call(&pool, "PUT", &uri, Some(&root), Some(moved)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let more = mail("smtp.msp.example", None, "b@acme.example");
        let (status, body, _) = call(&pool, "PUT", &uri, Some(&boss), Some(more)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["has_secret"], true);
        // ...and a new server with a new password is the customer's own.
        let own = mail("smtp.acme.example", Some("their-own"), "b@acme.example");
        let (status, body, _) = call(&pool, "PUT", &uri, Some(&boss), Some(own)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let stored = notify::channels(&pool).await.unwrap();
        assert_eq!(stored[0].secret.as_ref().unwrap().expose(), "their-own");
    }

    #[sqlx::test]
    async fn destinations_chosen_below_global_reach_only_public_addresses(pool: PgPool) {
        let (acme, _, _) = place(&pool, "acme").await;
        let root = login(&pool, "root", Role::Administrator, GrantScope::Global).await;
        let boss = login(&pool, "boss", Role::Administrator, GrantScope::Customer(acme)).await;
        let (url, hooks) = receiver(StatusCode::OK).await;
        let (port, mails) = smtp_server().await;
        let name = url.replace("127.0.0.1", "localhost");
        let webhook = |url: &str| json!({"kind": "webhook", "customer_id": acme, "settings": {"url": url}});
        let email = |to: &str| json!({"kind": "email", "customer_id": acme, "settings": {
            "host": "127.0.0.1", "port": port, "from": "mf@example.com", "to": [to]}});

        // A customer administrator's address on the server's side, written
        // or behind a name, neither by a test nor by the dispatcher.
        let test = |uri: String| {
            let pool = pool.clone();
            let boss = boss.clone();
            async move { call(&pool, "POST", &format!("{uri}/test"), Some(&boss), None).await.1 }
        };
        let mut channels = Vec::new();
        for request in [webhook(&url), webhook(&name), email("a@acme.example")] {
            let (status, created, _) = call(&pool, "POST", CHANNELS, Some(&boss), Some(request)).await;
            assert_eq!(status, StatusCode::CREATED, "{created}");
            let uri = format!("{CHANNELS}/{}", created["id"].as_str().unwrap());
            let result = test(uri.clone()).await;
            assert_eq!(result["ok"], false, "{result}");
            assert!(result["error"].as_str().unwrap().contains("private address"), "{result}");
            channels.push(uri);
        }
        sqlx::query(
            "INSERT INTO notification_outbox (event, customer_id, details)
             VALUES ('update.failed', $1, '{}')",
        )
        .bind(acme)
        .execute(&pool)
        .await
        .unwrap();
        fast(&pool).pass().await.unwrap();
        assert!(hooks.lock().unwrap().is_empty());
        assert!(mails.lock().unwrap().is_empty());

        // A global administrator's save vouches for it.
        let (status, _, _) = call(&pool, "PUT", &channels[0], Some(&root), Some(webhook(&url))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(test(channels[0].clone()).await["ok"], true);
        assert_eq!(hooks.lock().unwrap().len(), 1);

        // A customer administrator keeps it working by changing recipients
        // of a relay the MSP set, but aiming it anew limits it.
        let (status, relay, _) = call(&pool, "POST", CHANNELS, Some(&root), Some(email("a@acme.example"))).await;
        assert_eq!(status, StatusCode::CREATED, "{relay}");
        let uri = format!("{CHANNELS}/{}", relay["id"].as_str().unwrap());
        let (status, _, _) = call(&pool, "PUT", &uri, Some(&boss), Some(email("b@acme.example"))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(test(uri.clone()).await["ok"], true);
        assert_eq!(mails.lock().unwrap().len(), 1);
        let (status, _, _) = call(&pool, "PUT", &uri, Some(&boss), Some(webhook(&url))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(test(uri).await["ok"], false);
    }

    #[sqlx::test]
    async fn a_webhook_shows_only_its_origin_and_an_edit_without_url_keeps_it(pool: PgPool) {
        let (acme, _, _) = place(&pool, "acme").await;
        let root = login(&pool, "root", Role::Administrator, GrantScope::Global).await;
        let boss = login(&pool, "boss", Role::Administrator, GrantScope::Customer(acme)).await;
        let url = "https://psa.example:8443/hooks/T0KEN?key=QSECRET";
        let hook = |url: &str| json!({"kind": "webhook", "customer_id": acme, "settings": {"url": url}});
        let mut signed = hook(url);
        signed["secret"] = json!("signing-secret");
        let (status, created, _) = call(&pool, "POST", CHANNELS, Some(&root), Some(signed)).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        assert_eq!(created["settings"], json!({"origin": "https://psa.example:8443"}));
        let uri = format!("{CHANNELS}/{}", created["id"].as_str().unwrap());

        // Neither the MSP's nor the customer's administrator reads the path.
        for user in [&root, &boss] {
            let (status, list, _) = call(&pool, "GET", CHANNELS, Some(user), None).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(list["channels"][0]["settings"], json!({"origin": "https://psa.example:8443"}));
            assert!(!list.to_string().contains("T0KEN"), "{list}");
            assert!(!list.to_string().contains("QSECRET"), "{list}");
        }

        // An edit without a URL keeps it, and with it the signing secret
        // and the MSP's vouching.
        for settings in [json!({"url": ""}), json!({})] {
            let edit = json!({"kind": "webhook", "customer_id": acme, "settings": settings,
                              "follow_up_only": true});
            let (status, body, _) = call(&pool, "PUT", &uri, Some(&boss), Some(edit)).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            let stored = notify::channels(&pool).await.unwrap();
            assert_eq!(stored[0].destination, Destination::Webhook(Webhook { url: url.into() }));
            assert_eq!(stored[0].secret.as_ref().unwrap().expose(), "signing-secret");
            assert!(!stored[0].public_only);
        }

        // A new URL is a new target, though the origin stays the same.
        let moved = hook("https://psa.example:8443/hooks/OTHER");
        let (status, body, _) = call(&pool, "PUT", &uri, Some(&boss), Some(moved)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.to_string().contains("signing secret"), "{body}");

        // A new channel needs one.
        let (status, body, _) = call(&pool, "POST", CHANNELS, Some(&boss), Some(hook(""))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.to_string().contains("Enter the URL"), "{body}");
    }
}
