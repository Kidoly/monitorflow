//! Slack, Teams and Discord channels (ADR-0077): the API keeps their URLs
//! secret and only takes each service's own, and delivery reaches a fake
//! service in its format, honoring rate limits, without the URL ever
//! appearing in what is shown or logged.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::{OriginalUri, State};
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::db::MIGRATOR;
use monitorflow_server::grpc::Registry;
use monitorflow_server::notify::{self, Chat, Coverage, Destination, Dispatcher, Webhook};
use monitorflow_server::secrets::{self, Column};
use serde_json::{json, Value};
use sqlx::migrate::Migrator;
use sqlx::PgPool;
use tokio::net::TcpListener;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery";
const CHANNELS: &str = "/api/notifications/channels";
const SLACK_URL: &str = "https://hooks.slack.com/services/T0/B0/SLACKTOKEN";
const NEW_SLACK_URL: &str = "https://hooks.slack.com/services/T0/B0/NEWTOKEN";
/// The migration that added these kinds.
const CHAT_MIGRATION: i64 = 20261110090000;

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

async fn admin(pool: &PgPool) -> String {
    login(pool, "root", GrantScope::Global).await
}

/// An administrator of `scope`.
async fn login(pool: &PgPool, name: &str, scope: GrantScope) -> String {
    users::create_user_with(
        pool,
        name,
        &Secret::new(PASSWORD),
        Role::Administrator,
        scope,
        "test",
    )
    .await
    .unwrap();
    let body = json!({ "username": name, "password": PASSWORD });
    let (status, _, cookie) = call(pool, "POST", "/api/auth/login", None, Some(body)).await;
    assert_eq!(status, StatusCode::OK);
    cookie.unwrap()
}

/// A fake chat service: records each request and plays `script`, then
/// answers 200.
#[derive(Clone)]
struct Fake {
    seen: Arc<Mutex<Vec<(String, Value, Instant)>>>,
    script: Arc<Mutex<VecDeque<Answer>>>,
}

#[derive(Clone)]
struct Answer {
    status: u16,
    headers: Vec<(&'static str, &'static str)>,
    body: &'static str,
}

fn answer(status: u16, body: &'static str) -> Answer {
    Answer {
        status,
        headers: Vec::new(),
        body,
    }
}

async fn on_request(
    State(fake): State<Fake>,
    OriginalUri(uri): OriginalUri,
    body: Bytes,
) -> Response {
    let body = serde_json::from_slice(&body).unwrap_or(Value::Null);
    fake.seen
        .lock()
        .unwrap()
        .push((uri.to_string(), body, Instant::now()));
    let Answer {
        status,
        headers,
        body,
    } = fake
        .script
        .lock()
        .unwrap()
        .pop_front()
        .unwrap_or_else(|| answer(200, "ok"));
    let mut response = (StatusCode::from_u16(status).unwrap(), body).into_response();
    for (name, value) in headers {
        response.headers_mut().insert(name, value.parse().unwrap());
    }
    response
}

/// The fake's webhook URL, with a token in the path as real ones have.
async fn service(script: Vec<Answer>) -> (String, Fake) {
    let fake = Fake {
        seen: Arc::default(),
        script: Arc::new(Mutex::new(script.into())),
    };
    let app = axum::Router::new()
        .fallback(on_request)
        .with_state(fake.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://{}/services/T0/B0/FAKETOKEN",
        listener.local_addr().unwrap()
    );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, fake)
}

fn chat() -> Chat {
    Chat {
        name: "#ops".into(),
        host: "hooks.slack.com".into(),
    }
}

/// A channel saved directly: the API takes only the services' own hosts.
async fn channel(pool: &PgPool, destination: Destination, url: &str) -> Uuid {
    notify::insert(
        pool,
        &destination,
        Some(&Secret::new(url)),
        Coverage::Everything,
        false,
    )
    .await
    .unwrap()
}

/// A customer with one site: `(customer, site)`.
async fn customer_and_site(pool: &PgPool, name: &str) -> (Uuid, Uuid) {
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
    (customer, site)
}

async fn public_only(pool: &PgPool, id: &str) -> bool {
    sqlx::query_scalar("SELECT public_only FROM notification_channels WHERE id = $1::uuid")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

fn fast(pool: &PgPool) -> Dispatcher {
    Dispatcher::new(pool.clone(), "https://mf.example")
        .with_retries(vec![Duration::ZERO, Duration::ZERO])
        .without_pace()
        .without_waits()
}

async fn events(pool: &PgPool, count: usize) {
    for i in 0..count {
        sqlx::query(
            "INSERT INTO notification_outbox (event, details) VALUES ('backup.failed', $1)",
        )
        .bind(json!({ "backup": { "error": format!("disk {i} full"), "last_success_at": i.to_string() } }))
        .execute(pool)
        .await
        .unwrap();
    }
}

async fn status(pool: &PgPool, id: Uuid) -> Option<String> {
    sqlx::query_scalar("SELECT last_error FROM notification_channels WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// The stored secret of channel `id`, which must be sealed (ADR-0110),
/// opened.
async fn stored_secret(pool: &PgPool, id: &str) -> Option<String> {
    let stored: Option<String> =
        sqlx::query_scalar("SELECT secret FROM notification_channels WHERE id = $1::uuid")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap();
    stored.map(|value| {
        assert!(value.starts_with("mfs1:"), "stored sealed: {value}");
        secrets::open(Column::NotificationChannels, &value)
            .unwrap()
            .expose()
            .to_owned()
    })
}

db_tests! {
    #[sqlx::test]
    async fn the_url_is_written_and_never_read_back(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let cookie = admin(&pool).await;
        let cookie = Some(cookie.as_str());
        let body = json!({ "kind": "slack", "settings": { "name": " #ops " }, "secret": SLACK_URL });
        let (status, view, _) = call(&pool, "POST", CHANNELS, cookie, Some(body)).await;
        assert_eq!(status, StatusCode::CREATED, "{view}");
        assert_eq!(view["kind"], "slack");
        assert_eq!(view["settings"], json!({ "name": "#ops", "host": "hooks.slack.com" }));
        assert_eq!(view["has_secret"], true);
        assert!(!view.to_string().contains("SLACKTOKEN"));
        let id = view["id"].as_str().unwrap();
        assert_eq!(stored_secret(&pool, id).await.as_deref(), Some(SLACK_URL));

        let (_, list, _) = call(&pool, "GET", CHANNELS, cookie, None).await;
        assert_eq!(list["channels"].as_array().unwrap().len(), 1);
        assert!(!list.to_string().contains("SLACKTOKEN"));

        // An edit without a URL keeps it, and its host.
        let uri = format!("{CHANNELS}/{id}");
        let body = json!({ "kind": "slack", "settings": { "name": "#alerts" }, "secret": null });
        let (status, view, _) = call(&pool, "PUT", &uri, cookie, Some(body)).await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!(view["settings"], json!({ "name": "#alerts", "host": "hooks.slack.com" }));
        assert_eq!(stored_secret(&pool, id).await.as_deref(), Some(SLACK_URL));

        let body = json!({ "kind": "slack", "settings": {}, "secret": NEW_SLACK_URL });
        let (status, _, _) = call(&pool, "PUT", &uri, cookie, Some(body)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(stored_secret(&pool, id).await.as_deref(), Some(NEW_SLACK_URL));

        let (status, _, _) = call(&pool, "DELETE", &uri, cookie, None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let audit: Vec<String> = sqlx::query_scalar(
            "SELECT details::text FROM audit_log WHERE action LIKE 'notification.%' ORDER BY id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(audit.len(), 4);
        assert!(audit[0].contains("hooks.slack.com"));
        assert!(audit.iter().all(|d| !d.contains("TOKEN")), "{audit:?}");
    }

    #[sqlx::test]
    async fn only_the_services_own_urls_are_accepted(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let cookie = admin(&pool).await;
        let cookie = Some(cookie.as_str());
        let cases = [
            (json!({ "kind": "slack", "settings": {} }), "Paste the Slack webhook URL"),
            (json!({ "kind": "teams", "settings": {}, "secret": "" }), "Paste the Teams webhook URL"),
            (
                json!({ "kind": "discord", "settings": {}, "secret": SLACK_URL }),
                "not a Discord webhook URL",
            ),
            (
                json!({ "kind": "slack", "settings": {}, "secret": "http://127.0.0.1:5432/services/a/b/c" }),
                "not a Slack incoming webhook URL",
            ),
            (
                json!({ "kind": "slack", "settings": {},
                        "secret": "https://169.254.169.254/services/a/b/c" }),
                "not a Slack incoming webhook URL",
            ),
            (
                json!({ "kind": "teams", "settings": {},
                        "secret": "https://acme.webhook.office.com/webhookb2/SLACKTOKEN" }),
                "retired",
            ),
            (
                json!({ "kind": "slack", "settings": { "name": "x".repeat(81) }, "secret": SLACK_URL }),
                "under 80",
            ),
        ];
        for (body, words) in cases {
            let (status, answer, _) = call(&pool, "POST", CHANNELS, cookie, Some(body.clone())).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            let text = answer.to_string();
            assert!(text.contains(words), "{text}");
            assert!(!text.contains("SLACKTOKEN"), "{text}");
        }
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM notification_channels")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[sqlx::test]
    async fn a_saved_url_stays_with_its_service(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let cookie = admin(&pool).await;
        let cookie = Some(cookie.as_str());
        let chat_id = channel(&pool, Destination::Slack(chat()), SLACK_URL).await;
        let hook = Destination::Webhook(Webhook { url: "https://hooks.example/x".into() });
        let hook_id = channel(&pool, hook, "signing-secret").await;

        // Kept, a stored URL would become a signing key, and a key a URL:
        // ADR-0078's rule asks for it again.
        let to_hook = json!({ "kind": "webhook", "settings": { "url": "https://hooks.example/y" } });
        let to_discord = json!({ "kind": "discord", "settings": {} });
        let to_slack = json!({ "kind": "slack", "settings": {} });
        for (id, body, words) in [
            (chat_id, to_hook, "again"),
            (chat_id, to_discord, "again"),
            (hook_id, to_slack, "Paste the Slack webhook URL"),
        ] {
            let uri = format!("{CHANNELS}/{id}");
            let (status, answer, _) = call(&pool, "PUT", &uri, cookie, Some(body)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert!(answer.to_string().contains(words), "{answer}");
        }
        assert_eq!(stored_secret(&pool, &chat_id.to_string()).await.as_deref(), Some(SLACK_URL));

        // With the URL typed again, the change is a new channel in place.
        let body = json!({ "kind": "discord", "settings": {},
                           "secret": "https://discord.com/api/webhooks/1/new" });
        let uri = format!("{CHANNELS}/{chat_id}");
        let (status, view, _) = call(&pool, "PUT", &uri, cookie, Some(body)).await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!(view["settings"]["host"], "discord.com");
    }

    #[sqlx::test]
    async fn chat_channels_follow_coverage_and_who_manages_it(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let (acme, paris) = customer_and_site(&pool, "acme").await;
        let (globex, _) = customer_and_site(&pool, "globex").await;
        let root = admin(&pool).await;
        let boss = login(&pool, "boss", GrantScope::Customer(acme)).await;
        let local = login(&pool, "local", GrantScope::Site(paris)).await;
        let slack = |scope: Value| {
            let mut body = json!({ "kind": "slack", "settings": { "name": "#acme" }, "secret": SLACK_URL });
            body.as_object_mut().unwrap().extend(scope.as_object().unwrap().clone());
            body
        };

        // What each administrator manages, and nothing wider.
        for (cookie, scope, expected) in [
            (&boss, json!({}), StatusCode::FORBIDDEN),
            (&boss, json!({ "customer_id": globex }), StatusCode::FORBIDDEN),
            (&local, json!({ "customer_id": acme }), StatusCode::FORBIDDEN),
            (&local, json!({ "site_id": paris }), StatusCode::CREATED),
        ] {
            let (status, answer, _) = call(&pool, "POST", CHANNELS, Some(cookie), Some(slack(scope))).await;
            assert_eq!(status, expected, "{answer}");
        }
        let (status, view, _) =
            call(&pool, "POST", CHANNELS, Some(&boss), Some(slack(json!({ "customer_id": acme })))).await;
        assert_eq!(status, StatusCode::CREATED, "{view}");
        assert_eq!(view["customer_id"], acme.to_string());
        let id = view["id"].as_str().unwrap().to_owned();
        assert!(public_only(&pool, &id).await, "chosen below global administrator");

        // A customer administrator renames it: the URL stays, as does the
        // limit, without the URL typed again.
        let uri = format!("{CHANNELS}/{id}");
        let body = json!({ "kind": "slack", "settings": { "name": "#acme-ops" }, "customer_id": acme });
        let (status, view, _) = call(&pool, "PUT", &uri, Some(&boss), Some(body.clone())).await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!(stored_secret(&pool, &id).await.as_deref(), Some(SLACK_URL));
        assert!(public_only(&pool, &id).await);

        // A global administrator's save vouches for it.
        let (status, _, _) = call(&pool, "PUT", &uri, Some(&root), Some(body)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(!public_only(&pool, &id).await);

        // A new URL from the customer administrator is a new target.
        let body = json!({ "kind": "slack", "settings": {}, "secret": NEW_SLACK_URL, "customer_id": acme });
        let (status, _, _) = call(&pool, "PUT", &uri, Some(&boss), Some(body)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(public_only(&pool, &id).await);

        // The site administrator does not see the customer's channel.
        let (status, _, _) = call(&pool, "PUT", &uri, Some(&local),
            Some(json!({ "kind": "slack", "settings": {}, "site_id": paris }))).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (_, list, _) = call(&pool, "GET", CHANNELS, Some(&local), None).await;
        assert_eq!(list["channels"].as_array().unwrap().len(), 1);
        assert!(!list.to_string().contains("TOKEN"));
    }

    #[sqlx::test]
    async fn a_limited_chat_channel_never_reaches_a_private_address(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        // Saving refuses addresses; a row written another way is still
        // stopped before any connection.
        let (url, fake) = service(Vec::new()).await;
        let id = notify::insert(
            &pool,
            &Destination::Slack(chat()),
            Some(&Secret::new(url.as_str())),
            Coverage::Everything,
            true,
        )
        .await
        .unwrap();
        let loaded = notify::load(&pool, id).await.unwrap().unwrap();
        let dispatcher = Dispatcher::new(pool.clone(), "https://mf.example");
        let error = dispatcher.test(&loaded).await.unwrap().unwrap_err().message;
        assert!(error.contains("127.0.0.1 is a private address"), "{error}");
        assert!(!error.contains("FAKETOKEN"), "{error}");
        assert!(fake.seen.lock().unwrap().is_empty());
    }

    #[sqlx::test]
    async fn the_test_button_reaches_each_service_in_its_format(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let cookie = admin(&pool).await;
        let cookie = Some(cookie.as_str());
        let (url, fake) = service(Vec::new()).await;
        let chat = chat();
        for destination in [
            Destination::Slack(chat.clone()),
            Destination::Teams(chat.clone()),
            Destination::Discord(chat),
        ] {
            let id = channel(&pool, destination, &url).await;
            let uri = format!("{CHANNELS}/{id}/test");
            let (status, result, _) = call(&pool, "POST", &uri, cookie, None).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(result, json!({ "ok": true, "error": null }));
        }
        let seen = fake.seen.lock().unwrap();
        assert_eq!(seen[0].1["blocks"][0]["type"], "header");
        assert!(seen[0].1["text"].as_str().unwrap().contains("this channel works"));
        assert_eq!(seen[1].1["type"], "message");
        assert_eq!(seen[1].1["attachments"][0]["content"]["type"], "AdaptiveCard");
        assert_eq!(seen[2].1["allowed_mentions"], json!({ "parse": [] }));
        assert_eq!(seen[2].0, "/services/T0/B0/FAKETOKEN?wait=true");
        assert_eq!(seen[0].0, "/services/T0/B0/FAKETOKEN");
    }

    #[sqlx::test]
    async fn an_event_arrives_with_its_server_name_harmless(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let customer: Uuid = sqlx::query_scalar("INSERT INTO customers (name) VALUES ('acme') RETURNING id")
            .fetch_one(&pool)
            .await
            .unwrap();
        let site: Uuid = sqlx::query_scalar("INSERT INTO sites (name, customer_id) VALUES ('paris', $1) RETURNING id")
            .bind(customer)
            .fetch_one(&pool)
            .await
            .unwrap();
        let server: Uuid = sqlx::query_scalar(
            "INSERT INTO servers (peer_id, site_id, hostname, os_family)
             VALUES ('p1', $1, '<!channel> *web*', 'debian') RETURNING id",
        )
        .bind(site)
        .fetch_one(&pool)
        .await
        .unwrap();
        let (url, fake) = service(Vec::new()).await;
        channel(&pool, Destination::Slack(chat()), &url).await;
        let incident: Uuid = sqlx::query_scalar(
            "INSERT INTO incidents (server_id, metric, instance, severity, opened_at, last_value,
                                    last_value_at, peak_value, threshold)
             VALUES ($1, 'monitorflow_cpu_usage_percent', '', 'critical', now(), 91, now(), 91, '{}')
             RETURNING id",
        )
        .bind(server)
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO incident_events (incident_id, occurred_at, kind, severity, value)
             VALUES ($1, now(), 'opened', 'critical', 91)",
        )
        .bind(incident)
        .execute(&pool)
        .await
        .unwrap();

        fast(&pool).pass().await.unwrap();
        let seen = fake.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        let body = seen[0].1.to_string();
        assert!(!body.contains("<!channel>"), "{body}");
        assert!(body.contains("&lt;!channel&gt; *web*"), "{body}");
        assert!(body.contains("Severity: Critical"), "{body}");
        assert!(body.contains(&format!("https://mf.example/incidents/view/?id={incident}")), "{body}");
    }

    #[sqlx::test]
    async fn a_rate_limit_is_waited_out_as_the_service_asks(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let limited = Answer {
            status: 429,
            headers: vec![("retry-after", "1")],
            body: "rate limited",
        };
        let (url, fake) = service(vec![limited]).await;
        let id = channel(&pool, Destination::Slack(chat()), &url).await;
        events(&pool, 1).await;
        fast(&pool).pass().await.unwrap();
        assert_eq!(status(&pool, id).await, None);
        let seen = fake.seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert!(seen[1].2.duration_since(seen[0].2) >= Duration::from_secs(1));
    }

    #[sqlx::test]
    async fn a_limit_that_stays_stops_the_pass_for_that_channel(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let limited = Answer {
            status: 429,
            headers: Vec::new(),
            body: r#"{"message":"You are being rate limited.","retry_after":0.01,"global":false}"#,
        };
        let (url, fake) = service(vec![limited.clone(), limited.clone(), limited]).await;
        let id = channel(&pool, Destination::Discord(chat()), &url).await;
        events(&pool, 4).await;
        assert_eq!(fast(&pool).pass().await.unwrap(), 4);
        // Three tries of the first, and nothing thrown at a closed door.
        assert_eq!(fake.seen.lock().unwrap().len(), 3);
        let error = status(&pool, id).await.unwrap();
        assert!(error.contains("Discord is limiting"), "{error}");
    }

    #[sqlx::test]
    async fn a_burst_goes_out_gathered(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let (url, fake) = service(Vec::new()).await;
        channel(&pool, Destination::Teams(chat()), &url).await;
        events(&pool, 12).await;
        fast(&pool).pass().await.unwrap();
        let seen = fake.seen.lock().unwrap();
        assert_eq!(seen.len(), Destination::Teams(chat()).chat().unwrap().0.burst());
        let last = seen.last().unwrap().1.to_string();
        assert!(last.contains("8 more notifications"), "{last}");
        assert!(last.contains("disk 11 full"), "{last}");
        assert!(last.contains("/incidents/"), "{last}");
    }

    #[sqlx::test]
    async fn messages_keep_the_services_pace(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let (url, fake) = service(Vec::new()).await;
        channel(&pool, Destination::Slack(chat()), &url).await;
        events(&pool, 2).await;
        Dispatcher::new(pool.clone(), "https://mf.example")
            .without_waits()
            .pass()
            .await
            .unwrap();
        let seen = fake.seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert!(seen[1].2.duration_since(seen[0].2) >= Duration::from_secs(1));
    }

    #[sqlx::test]
    async fn failures_read_plainly_and_never_show_the_url(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        // A refusal Slack explains, a redirect, which is not followed, and
        // a service that repeats the URL back.
        let moved = Answer {
            status: 302,
            headers: vec![("location", "http://127.0.0.1:9/elsewhere")],
            body: "",
        };
        let (url, fake) = service(vec![
            answer(404, "no_service"),
            moved,
            answer(500, "bad path /services/T0/B0/FAKETOKEN"),
        ])
        .await;
        let id = channel(&pool, Destination::Slack(chat()), &url).await;
        let dispatcher = Dispatcher::new(pool.clone(), "https://mf.example");
        let loaded = notify::load(&pool, id).await.unwrap().unwrap();

        let error = dispatcher.test(&loaded).await.unwrap().unwrap_err().message;
        assert!(error.contains("Slack no longer knows this webhook"), "{error}");
        let error = dispatcher.test(&loaded).await.unwrap().unwrap_err().message;
        assert!(error.contains("not followed"), "{error}");
        assert_eq!(fake.seen.lock().unwrap().len(), 2);
        let error = dispatcher.test(&loaded).await.unwrap().unwrap_err().message;
        assert!(!error.contains("FAKETOKEN"), "{error}");
        assert_eq!(status(&pool, id).await.as_deref(), Some(error.as_str()));

        // Nothing listens: the transport error names only the host, also
        // for a plain webhook, whose URL used to come through.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let closed = format!("http://{}/services/T0/B0/FAKETOKEN", listener.local_addr().unwrap());
        drop(listener);
        let down = channel(&pool, Destination::Discord(chat()), &closed).await;
        let hook = Destination::Webhook(Webhook { url: closed.clone() });
        let plain = channel(&pool, hook, "signing").await;
        for id in [down, plain] {
            let loaded = notify::load(&pool, id).await.unwrap().unwrap();
            let error = dispatcher.test(&loaded).await.unwrap().unwrap_err().message;
            assert!(error.starts_with("Could not connect to 127.0.0.1"), "{error}");
            assert!(!error.contains("FAKETOKEN"), "{error}");
        }
    }

    #[sqlx::test(migrations = false)]
    async fn the_migration_leaves_existing_channels_alone(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let before = Migrator {
            migrations: MIGRATOR
                .migrations
                .iter()
                .filter(|m| m.version < CHAT_MIGRATION)
                .cloned()
                .collect(),
            ..Migrator::DEFAULT
        };
        before.run(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO notification_channels (kind, settings, secret, last_error)
             VALUES ('webhook', '{\"url\": \"https://hooks.slack.com/services/T/B/X\"}', 's', 'e'),
                    ('email', '{\"host\": \"smtp.x.io\", \"port\": 587, \"to\": [\"a@x.io\"]}', NULL, NULL)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let rows = "SELECT to_jsonb(n)::text FROM notification_channels n ORDER BY id";
        let saved: Vec<String> = sqlx::query_scalar(rows).fetch_all(&pool).await.unwrap();

        MIGRATOR.run(&pool).await.unwrap();
        let after: Vec<String> = sqlx::query_scalar(rows).fetch_all(&pool).await.unwrap();
        // Later migrations add columns; the ones that were there keep
        // their values.
        assert_eq!(saved.len(), after.len());
        for (saved, after) in saved.iter().zip(&after) {
            let saved: serde_json::Map<String, Value> = serde_json::from_str(saved).unwrap();
            let after: Value = serde_json::from_str(after).unwrap();
            for (column, value) in saved {
                assert_eq!(after[&column], value, "{column}");
            }
        }
        let loaded = notify::channels(&pool).await.unwrap();
        assert_eq!(loaded.len(), 2);
        assert!(loaded.iter().all(|c| c.destination.chat().is_none()));
    }
}
