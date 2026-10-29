//! Notifications (ADR-0058): the triggers that fill the outbox, delivery
//! to a webhook and an SMTP server on loopback, customer scoping, the API's
//! secrecy and its test button. Ignored without `DATABASE_URL`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, Request, StatusCode};
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
use monitorflow_server::notify::send::{signature, SIGNATURE_HEADER, TIMESTAMP_HEADER};
use monitorflow_server::notify::{self, Destination, Dispatcher, Email, Webhook};
use serde_json::{json, Value};
use sqlx::PgPool;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery";
const CHANNELS: &str = "/api/notifications/channels";

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

fn fast(pool: &PgPool) -> Dispatcher {
    Dispatcher::new(pool.clone(), "https://mf.example").with_retries(vec![Duration::ZERO])
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
        notify::insert(&pool, &hook, Some(&Secret::new("s3cret")), Some(acme)).await.unwrap();
        let email = Destination::Email(Email {
            host: "127.0.0.1".into(),
            port,
            username: String::new(),
            from: "mf@example.com".into(),
            to: vec!["ops@example.com".into()],
        });
        let mail_channel = notify::insert(&pool, &email, None, None).await.unwrap();
        let (other_url, other_hooks) = receiver(StatusCode::OK).await;
        let other = Destination::Webhook(Webhook { url: other_url });
        notify::insert(&pool, &other, None, Some(globex)).await.unwrap();

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
    async fn the_api_never_returns_secrets_and_tests_in_plain_words(pool: PgPool) {
        let (customer, _, _) = place(&pool, "acme").await;
        let root = login(&pool, "root", Role::Administrator, GrantScope::Global).await;
        let boss = login(&pool, "boss", Role::Administrator, GrantScope::Customer(customer)).await;

        let (status, _, _) = call(&pool, "GET", CHANNELS, Some(&boss), None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);

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
}
