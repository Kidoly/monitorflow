//! Alert grouping (ADR-0080): when the dispatcher sends a customer's
//! events, how it groups them per channel, and the list a group links to,
//! against PostgreSQL and receivers on loopback. Ignored without
//! `DATABASE_URL`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::http::{Request, StatusCode};
use chrono::{TimeDelta, Utc};
use monitorflow_common::redact::Secret;
use monitorflow_server::alerting::offline::{self, Action, Peer};
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
use monitorflow_server::notify::{self, Chat, Coverage, Destination, Dispatcher, Webhook};
use serde_json::{json, Value};
use sqlx::PgPool;
use tokio::net::TcpListener;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery";
const AFTER: Duration = Duration::from_secs(300);

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

/// A customer with sites `paris` and `lyon`.
#[derive(Debug, Clone, Copy)]
struct Customer {
    id: Uuid,
    paris: Uuid,
    lyon: Uuid,
}

async fn customer(pool: &PgPool, name: &str) -> Customer {
    let id = sqlx::query_scalar("INSERT INTO customers (name) VALUES ($1) RETURNING id")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap();
    let mut sites = Vec::new();
    for site in ["paris", "lyon"] {
        let site: Uuid = sqlx::query_scalar(
            "INSERT INTO sites (name, customer_id) VALUES ($1 || '-' || $2, $3) RETURNING id",
        )
        .bind(name)
        .bind(site)
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap();
        sites.push(site);
    }
    Customer {
        id,
        paris: sites[0],
        lyon: sites[1],
    }
}

async fn server(pool: &PgPool, site: Uuid, name: &str, relay: Option<Uuid>) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO servers (peer_id, site_id, hostname, os_family, relay_id)
         VALUES ($1, $2, $1, 'debian', $3) RETURNING id",
    )
    .bind(name)
    .bind(site)
    .bind(relay)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn relay(pool: &PgPool, site: Uuid, name: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO relays (peer_id, site_id, name) VALUES ($1, $2, $1) RETURNING id",
    )
    .bind(name)
    .bind(site)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Opens or resolves an offline incident the way the offline rule does.
async fn down(pool: &PgPool, peer: Peer) -> Uuid {
    let since = Utc::now() - TimeDelta::minutes(6);
    offline::apply(pool, peer, &Action::Open { since }, AFTER)
        .await
        .unwrap()
        .unwrap()
}

async fn back(pool: &PgPool, peer: Peer, incident: Uuid) {
    let action = Action::Resolve {
        id: incident,
        at: Utc::now(),
        reason: "back_online",
    };
    offline::apply(pool, peer, &action, AFTER).await.unwrap();
}

/// A receiver on loopback recording each request's body.
async fn receiver() -> (String, Arc<Mutex<Vec<Value>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let store = seen.clone();
    let app = axum::Router::new().fallback(move |body: Bytes| {
        let store = store.clone();
        async move {
            store
                .lock()
                .unwrap()
                .push(serde_json::from_slice(&body).unwrap_or(Value::Null));
            StatusCode::OK
        }
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{address}/services/T0/B0/TOKEN"), seen)
}

/// A Slack channel for `coverage`, recording each body.
async fn slack(pool: &PgPool, coverage: Coverage) -> Arc<Mutex<Vec<Value>>> {
    let (url, seen) = receiver().await;
    let named = Chat {
        name: "#ops".into(),
        host: "hooks.slack.com".into(),
    };
    notify::insert(
        pool,
        &Destination::Slack(named),
        Some(&Secret::new(url)),
        coverage,
        false,
    )
    .await
    .unwrap();
    seen
}

/// A generic webhook for everything, recording each body.
async fn webhook(pool: &PgPool) -> Arc<Mutex<Vec<Value>>> {
    let (url, seen) = receiver().await;
    let destination = Destination::Webhook(Webhook { url });
    notify::insert(pool, &destination, None, Coverage::Everything, false)
        .await
        .unwrap();
    seen
}

fn dispatcher(pool: &PgPool) -> Dispatcher {
    Dispatcher::new(pool.clone(), "https://mf.example")
        .with_retries(Vec::new())
        .without_pace()
}

fn taken(seen: &Mutex<Vec<Value>>) -> Vec<Value> {
    std::mem::take(&mut seen.lock().unwrap())
}

/// The title of each Slack message.
fn titles(seen: &Mutex<Vec<Value>>) -> Vec<String> {
    taken(seen)
        .iter()
        .map(|body| {
            body["blocks"][0]["text"]["text"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}

/// Moves the outbox's clock back by `seconds`, as if they had passed.
async fn wait(pool: &PgPool, seconds: i64) {
    sqlx::query(
        "UPDATE notification_outbox
         SET created_at = created_at - make_interval(secs => $1),
             sent_at = sent_at - make_interval(secs => $1)",
    )
    .bind(seconds as f64)
    .execute(pool)
    .await
    .unwrap();
}

async fn call(
    pool: &PgPool,
    uri: &str,
    cookie: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value, Option<String>) {
    let state = ApiState::new(pool.clone(), Registry::default(), ApiSettings::default());
    let method = if body.is_some() { "POST" } else { "GET" };
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

async fn login(pool: &PgPool, name: &str, scope: GrantScope) -> String {
    users::create_user_with(
        pool,
        name,
        &Secret::new(PASSWORD),
        Role::Viewer,
        scope,
        "test",
    )
    .await
    .unwrap();
    let body = json!({ "username": name, "password": PASSWORD });
    call(pool, "/api/auth/login", None, Some(body))
        .await
        .2
        .unwrap()
}

db_tests! {
    #[sqlx::test]
    async fn a_site_going_dark_is_one_message_and_its_return_another(pool: PgPool) {
        let acme = customer(&pool, "acme").await;
        let chat = slack(&pool, Coverage::Everything).await;
        let hook = webhook(&pool).await;
        let dispatcher = dispatcher(&pool);
        let mut incidents = Vec::new();
        for n in 0..40 {
            let id = server(&pool, acme.paris, &format!("web-{n}"), None).await;
            incidents.push((Peer::Server(id), down(&pool, Peer::Server(id)).await));
        }
        // Not yet: the first event of a quiet customer waits a little.
        assert_eq!(dispatcher.pass().await.unwrap(), 0);
        wait(&pool, 10).await;
        assert_eq!(dispatcher.pass().await.unwrap(), 40);
        assert_eq!(titles(&chat), ["Critical: 40 servers are offline at acme / acme-paris"]);
        // Webhooks keep one request per event, as their receivers expect.
        let requests = taken(&hook);
        assert_eq!(requests.len(), 40);
        assert!(requests.iter().all(|r| r["event"] == "incident.opened"));

        for (peer, incident) in incidents {
            back(&pool, peer, incident).await;
        }
        // Within the minute after the outage was told: still waiting.
        wait(&pool, 30).await;
        assert_eq!(dispatcher.pass().await.unwrap(), 0);
        wait(&pool, 30).await;
        assert_eq!(dispatcher.pass().await.unwrap(), 40);
        assert_eq!(titles(&chat), ["Resolved: 40 servers are back online at acme / acme-paris"]);
        assert_eq!(taken(&hook).len(), 40);
    }

    #[sqlx::test]
    async fn the_window_survives_a_restart(pool: PgPool) {
        let acme = customer(&pool, "acme").await;
        let chat = slack(&pool, Coverage::Everything).await;
        let first = server(&pool, acme.paris, "web-1", None).await;
        let second = server(&pool, acme.lyon, "web-2", None).await;
        down(&pool, Peer::Server(first)).await;
        wait(&pool, 10).await;
        dispatcher(&pool).pass().await.unwrap();
        assert_eq!(titles(&chat), ["Critical: web-1 (acme / acme-paris) is offline"]);

        // Another event half way through the window, then a restart.
        wait(&pool, 30).await;
        down(&pool, Peer::Server(second)).await;
        let restarted = dispatcher(&pool);
        assert_eq!(restarted.pass().await.unwrap(), 0, "the window is in the database");
        wait(&pool, 30).await;
        assert_eq!(restarted.pass().await.unwrap(), 1);
        assert_eq!(titles(&chat), ["Critical: web-2 (acme / acme-lyon) is offline"]);
    }

    #[sqlx::test]
    async fn the_wait_is_per_customer(pool: PgPool) {
        let acme = customer(&pool, "acme").await;
        let globex = customer(&pool, "globex").await;
        let chat = slack(&pool, Coverage::Everything).await;
        let dispatcher = dispatcher(&pool);
        let a = server(&pool, acme.paris, "a-1", None).await;
        down(&pool, Peer::Server(a)).await;
        wait(&pool, 10).await;
        dispatcher.pass().await.unwrap();
        assert_eq!(titles(&chat).len(), 1);
        // acme just spoke; globex is quiet and waits only its first ten
        // seconds.
        let a2 = server(&pool, acme.paris, "a-2", None).await;
        let g = server(&pool, globex.paris, "g-1", None).await;
        down(&pool, Peer::Server(a2)).await;
        down(&pool, Peer::Server(g)).await;
        sqlx::query(
            "UPDATE notification_outbox SET created_at = created_at - interval '10 s'
             WHERE customer_id = $1",
        )
        .bind(globex.id)
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(dispatcher.pass().await.unwrap(), 1);
        assert_eq!(titles(&chat), ["Critical: g-1 (globex / globex-paris) is offline"]);
    }

    #[sqlx::test]
    async fn a_relay_speaks_once_for_the_servers_behind_it(pool: PgPool) {
        let acme = customer(&pool, "acme").await;
        let chat = slack(&pool, Coverage::Everything).await;
        let gateway = relay(&pool, acme.paris, "gw-1").await;
        for n in 0..40 {
            server(&pool, acme.paris, &format!("web-{n}"), Some(gateway)).await;
        }
        sqlx::query("UPDATE servers SET last_seen_at = now() - interval '10 minutes'")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE relays SET last_seen_at = now() - interval '10 minutes'")
            .execute(&pool)
            .await
            .unwrap();
        // The offline rule opens only the relay's incident (ADR-0072).
        let changes = offline::check(&pool, &Default::default(), Utc::now() - TimeDelta::days(1), &Default::default())
            .await
            .unwrap();
        assert_eq!(changes.len(), 1, "{changes:?}");
        let dispatcher = dispatcher(&pool).without_waits();
        dispatcher.pass().await.unwrap();
        assert_eq!(
            titles(&chat),
            ["Critical: relay gw-1 (acme / acme-paris) is offline; the 40 servers behind it cannot report"]
        );
    }

    #[sqlx::test]
    async fn a_channel_sees_only_the_members_in_its_scope(pool: PgPool) {
        let acme = customer(&pool, "acme").await;
        let globex = customer(&pool, "globex").await;
        let everything = slack(&pool, Coverage::Everything).await;
        let acme_channel = slack(&pool, Coverage::Customer(acme.id)).await;
        let paris = slack(&pool, Coverage::Site(acme.paris)).await;
        let lyon = slack(&pool, Coverage::Site(acme.lyon)).await;
        let globex_channel = slack(&pool, Coverage::Customer(globex.id)).await;
        for (site, name) in [(acme.paris, "p-1"), (acme.paris, "p-2"), (acme.lyon, "l-1"), (globex.paris, "g-1"), (globex.paris, "g-2")] {
            let id = server(&pool, site, name, None).await;
            down(&pool, Peer::Server(id)).await;
        }
        dispatcher(&pool).without_waits().pass().await.unwrap();
        // A group never mixes customers: one message each.
        let mut all = titles(&everything);
        all.sort();
        assert_eq!(all, [
            "Critical: 2 servers are offline at globex / globex-paris",
            "Critical: 3 servers are offline in 2 sites of acme",
        ]);
        assert_eq!(titles(&acme_channel), ["Critical: 3 servers are offline in 2 sites of acme"]);
        assert_eq!(titles(&paris), ["Critical: 2 servers are offline at acme / acme-paris"]);
        assert_eq!(titles(&lyon), ["Critical: l-1 (acme / acme-lyon) is offline"]);
        let body = taken(&globex_channel).remove(0).to_string();
        assert!(!body.contains("p-1") && !body.contains("acme"), "{body}");
    }

    #[sqlx::test]
    async fn silenced_members_are_held_and_named_when_it_ends(pool: PgPool) {
        let acme = customer(&pool, "acme").await;
        let chat = slack(&pool, Coverage::Everything).await;
        let silence: Uuid = sqlx::query_scalar(
            "INSERT INTO silences (site_id, starts_at, ends_at, reason)
             VALUES ($1, now() - interval '1 minute', now() + interval '1 hour', 'move')
             RETURNING id",
        )
        .bind(acme.paris)
        .fetch_one(&pool)
        .await
        .unwrap();
        for (site, name) in [(acme.paris, "p-1"), (acme.paris, "p-2"), (acme.lyon, "l-1"), (acme.lyon, "l-2")] {
            let id = server(&pool, site, name, None).await;
            down(&pool, Peer::Server(id)).await;
        }
        let dispatcher = dispatcher(&pool).without_waits();
        dispatcher.pass().await.unwrap();
        assert_eq!(titles(&chat), ["Critical: 2 servers are offline at acme / acme-lyon"]);

        sqlx::query("UPDATE silences SET ended_at = now() WHERE id = $1")
            .bind(silence)
            .execute(&pool)
            .await
            .unwrap();
        for _ in 0..3 {
            dispatcher.pass().await.unwrap();
        }
        let sent = taken(&chat);
        assert_eq!(sent.len(), 1, "{sent:#?}");
        let body = sent[0].to_string();
        assert!(body.contains("Critical after a silence at acme / acme-paris: 2 incidents still open"), "{body}");
        assert!(body.contains("p-1 (acme / acme-paris) is offline") && body.contains("p-2"), "{body}");
        let left: i64 = sqlx::query_scalar("SELECT count(*) FROM notification_outbox WHERE sent_at IS NULL")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(left, 0);
    }

    #[sqlx::test]
    async fn the_link_lists_the_group_to_those_who_may_read_it(pool: PgPool) {
        let acme = customer(&pool, "acme").await;
        let chat = slack(&pool, Coverage::Everything).await;
        let mut ids = Vec::new();
        for (site, name) in [(acme.paris, "p-1"), (acme.lyon, "l-1")] {
            let server = server(&pool, site, name, None).await;
            ids.push(down(&pool, Peer::Server(server)).await);
        }
        let alone = server(&pool, acme.paris, "p-2", None).await;
        let unrelated = down(&pool, Peer::Server(alone)).await;
        sqlx::query("DELETE FROM notification_outbox WHERE details #>> '{incident,id}' = $1")
            .bind(unrelated.to_string())
            .execute(&pool)
            .await
            .unwrap();
        dispatcher(&pool).without_waits().pass().await.unwrap();
        let sent = taken(&chat);
        let url = sent[0]["blocks"].to_string();
        let group: Uuid = url.split("group=").nth(1).unwrap()[..36].parse().unwrap();

        let everyone = login(&pool, "ana", GrantScope::Global).await;
        let (status, list, _) = call(&pool, &format!("/api/incidents?group={group}"), Some(&everyone), None).await;
        assert_eq!(status, StatusCode::OK);
        let mut listed: Vec<Uuid> = list["incidents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["id"].as_str().unwrap().parse().unwrap())
            .collect();
        listed.sort();
        ids.sort();
        assert_eq!(listed, ids);
        let (_, detail, _) = call(&pool, &format!("/api/incidents/{}", ids[0]), Some(&everyone), None).await;
        assert_eq!(detail["notification_groups"][0]["id"], group.to_string());
        assert_eq!(detail["notification_groups"][0]["incidents"], 2);
        let (_, detail, _) = call(&pool, &format!("/api/incidents/{unrelated}"), Some(&everyone), None).await;
        assert_eq!(detail["notification_groups"], json!([]));

        // A site grant sees its own part of the group only.
        let paris = login(&pool, "pat", GrantScope::Site(acme.paris)).await;
        let (_, list, _) = call(&pool, &format!("/api/incidents?group={group}"), Some(&paris), None).await;
        let hosts: Vec<&str> = list["incidents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["hostname"].as_str().unwrap())
            .collect();
        assert_eq!(hosts, ["p-1"]);
        let (status, _, _) = call(&pool, "/api/incidents?group=nope", Some(&everyone), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
}
