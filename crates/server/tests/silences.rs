//! Silences (ADR-0079): what the dispatcher says at each boundary, against
//! PostgreSQL and a webhook receiver on loopback, and the API's permissions.
//! Ignored without `DATABASE_URL`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::http::{Request, StatusCode};
use chrono::{DateTime, TimeDelta, Utc};
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
const SILENCES: &str = "/api/silences";

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

/// A customer with a site, a server and a relay.
#[derive(Debug, Clone, Copy)]
struct Place {
    customer: Uuid,
    site: Uuid,
    server: Uuid,
    relay: Uuid,
}

async fn place(pool: &PgPool, name: &str) -> Place {
    let customer = sqlx::query_scalar("INSERT INTO customers (name) VALUES ($1) RETURNING id")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap();
    let site = sqlx::query_scalar(
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
        "INSERT INTO relays (peer_id, site_id, name)
         VALUES ($1 || '-gw', $2, $1 || '-gw') RETURNING id",
    )
    .bind(name)
    .bind(site)
    .fetch_one(pool)
    .await
    .unwrap();
    Place {
        customer,
        site,
        server,
        relay,
    }
}

/// A webhook on loopback, set up as a channel for everyone, recording the
/// `text` of each message; and a dispatcher sending to it.
async fn channel(pool: &PgPool) -> (Dispatcher, Arc<Mutex<Vec<String>>>) {
    let (dispatcher, seen, _) = scoped(pool, Coverage::Everything).await;
    (dispatcher, seen)
}

/// [`channel`] for `coverage`, with its ID.
async fn scoped(pool: &PgPool, coverage: Coverage) -> (Dispatcher, Arc<Mutex<Vec<String>>>, Uuid) {
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
    let id = notify::insert(pool, &destination, None, coverage, false)
        .await
        .unwrap();
    let dispatcher = Dispatcher::new(pool.clone(), "https://mf.example")
        .with_retries(Vec::new())
        .without_pace();
    (dispatcher, seen, id)
}

/// A fake chat service answering `script`'s statuses in turn, then 200,
/// recording each JSON body; set up as a channel of `destination`'s kind.
async fn chat(
    pool: &PgPool,
    destination: fn(Chat) -> Destination,
    script: Vec<u16>,
) -> Arc<Mutex<Vec<String>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let script = Arc::new(Mutex::new(std::collections::VecDeque::from(script)));
    let store = seen.clone();
    let app = axum::Router::new().fallback(move |body: Bytes| {
        let (store, script) = (store.clone(), script.clone());
        async move {
            store
                .lock()
                .unwrap()
                .push(String::from_utf8_lossy(&body).into_owned());
            let status = script.lock().unwrap().pop_front().unwrap_or(200);
            let mut response =
                axum::response::IntoResponse::into_response(StatusCode::from_u16(status).unwrap());
            if status == 429 {
                response
                    .headers_mut()
                    .insert("retry-after", "0".parse().unwrap());
            }
            response
        }
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://{}/services/T0/B0/TOKEN",
        listener.local_addr().unwrap()
    );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let named = Chat {
        name: "#ops".into(),
        host: "hooks.slack.com".into(),
    };
    notify::insert(
        pool,
        &destination(named),
        Some(&Secret::new(url)),
        Coverage::Everything,
        false,
    )
    .await
    .unwrap();
    seen
}

/// Opens a critical incident on `server` for the disk `instance`.
async fn open_disk(pool: &PgPool, server: Uuid, instance: &str) -> Uuid {
    let id = sqlx::query_scalar(
        "INSERT INTO incidents (server_id, metric, instance, severity, opened_at, last_value,
                                last_value_at, peak_value, threshold)
         VALUES ($1, 'monitorflow_disk_used_percent', $2, 'critical', now(), 99, now(), 99, '{}')
         RETURNING id",
    )
    .bind(server)
    .bind(instance)
    .fetch_one(pool)
    .await
    .unwrap();
    event(pool, id, "opened", Some("critical")).await;
    id
}

/// Passes enough for a release to go out.
async fn settle(dispatcher: &Dispatcher) {
    for _ in 0..3 {
        dispatcher.pass().await.unwrap();
    }
}

fn taken(seen: &Mutex<Vec<String>>) -> Vec<String> {
    std::mem::take(&mut seen.lock().unwrap())
}

/// Scope of a silence: the columns set on it.
#[derive(Debug, Clone, Copy, Default)]
struct Scope {
    server: Option<Uuid>,
    relay: Option<Uuid>,
    site: Option<Uuid>,
    customer: Option<Uuid>,
}

/// A silence from `from` to `to` minutes from now.
async fn silence(pool: &PgPool, scope: Scope, from: i64, to: i64) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO silences (server_id, relay_id, site_id, customer_id, starts_at, ends_at,
                               reason)
         VALUES ($1, $2, $3, $4, $5, $6, 'planned maintenance') RETURNING id",
    )
    .bind(scope.server)
    .bind(scope.relay)
    .bind(scope.site)
    .bind(scope.customer)
    .bind(Utc::now() + TimeDelta::minutes(from))
    .bind(Utc::now() + TimeDelta::minutes(to))
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Ends the silence the way the API does.
async fn end(pool: &PgPool, silence: Uuid) {
    sqlx::query("UPDATE silences SET ended_at = now() WHERE id = $1")
        .bind(silence)
        .execute(pool)
        .await
        .unwrap();
}

async fn open(pool: &PgPool, server: Uuid, severity: &str) -> Uuid {
    let id = sqlx::query_scalar(
        "INSERT INTO incidents (server_id, metric, instance, severity, opened_at, last_value,
                                last_value_at, peak_value, threshold)
         VALUES ($1, 'monitorflow_cpu_usage_percent', '', $2, now(), 91, now(), 91, '{}')
         RETURNING id",
    )
    .bind(server)
    .bind(severity)
    .fetch_one(pool)
    .await
    .unwrap();
    event(pool, id, "opened", Some(severity)).await;
    id
}

async fn event(pool: &PgPool, incident: Uuid, kind: &str, severity: Option<&str>) {
    sqlx::query(
        "INSERT INTO incident_events (incident_id, occurred_at, kind, severity, value)
         VALUES ($1, now(), $2, $3, 91)",
    )
    .bind(incident)
    .bind(kind)
    .bind(severity)
    .execute(pool)
    .await
    .unwrap();
}

/// Changes the incident's severity like the engine.
async fn change(pool: &PgPool, incident: Uuid, kind: &str, severity: &str) {
    sqlx::query(
        "UPDATE incidents SET severity = $2, last_value = 97, peak_value = 97 WHERE id = $1",
    )
    .bind(incident)
    .bind(severity)
    .execute(pool)
    .await
    .unwrap();
    event(pool, incident, kind, Some(severity)).await;
}

async fn resolve(pool: &PgPool, incident: Uuid) {
    sqlx::query("UPDATE incidents SET state = 'resolved', resolved_at = now() WHERE id = $1")
        .bind(incident)
        .execute(pool)
        .await
        .unwrap();
    event(pool, incident, "resolved", None).await;
}

/// Kinds and details of the incident's events, in order.
async fn timeline(pool: &PgPool, incident: Uuid) -> Vec<(String, Value)> {
    sqlx::query_as("SELECT kind, details FROM incident_events WHERE incident_id = $1 ORDER BY id")
        .bind(incident)
        .fetch_all(pool)
        .await
        .unwrap()
}

async fn audited(pool: &PgPool, action: &str) -> Vec<Value> {
    sqlx::query_scalar("SELECT details FROM audit_log WHERE action = $1 ORDER BY id")
        .bind(action)
        .fetch_all(pool)
        .await
        .unwrap()
}

/// What `POST /api/silences` takes, for an hour.
fn request(scope: &str, target: Option<Uuid>) -> Value {
    json!({
        "scope": scope,
        "target_id": target,
        "ends_at": Utc::now() + TimeDelta::hours(1),
        "reason": "planned maintenance",
    })
}

async fn post(pool: &PgPool, cookie: &str, body: Value) -> StatusCode {
    call(pool, "POST", SILENCES, Some(cookie), Some(body))
        .await
        .0
}

async fn listed(pool: &PgPool, cookie: &str, query: &str) -> Vec<Value> {
    let (status, body, _) = call(
        pool,
        "GET",
        &format!("{SILENCES}{query}"),
        Some(cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    body["silences"].as_array().unwrap().clone()
}

/// The `silenced` and `silenced_until` of an incident through the API.
async fn sign(pool: &PgPool, cookie: &str, incident: Uuid) -> (bool, bool) {
    let uri = format!("/api/incidents/{incident}");
    let (status, body, _) = call(pool, "GET", &uri, Some(cookie), None).await;
    assert_eq!(status, StatusCode::OK);
    (
        body["silenced"].as_bool().unwrap(),
        !body["silenced_until"].is_null(),
    )
}

db_tests! {
    #[sqlx::test]
    async fn a_silenced_incident_opens_quietly_and_is_told_once_at_the_end(pool: PgPool) {
        let a = place(&pool, "a").await;
        let (dispatcher, seen) = channel(&pool).await;
        let admin = login(&pool, "root", Role::Administrator, GrantScope::Global).await;
        let id = silence(&pool, Scope { server: Some(a.server), ..Scope::default() }, -1, 60).await;

        let incident = open(&pool, a.server, "critical").await;
        settle(&dispatcher).await;
        assert!(taken(&seen).is_empty(), "a silence stops the notification");

        // It stays an open incident, with a sign.
        let state: String = sqlx::query_scalar("SELECT state FROM incidents WHERE id = $1")
            .bind(incident)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(state, "open");
        assert_eq!(sign(&pool, &admin, incident).await, (true, true));
        let (_, list, _) = call(&pool, "GET", "/api/incidents?state=open", Some(&admin), None).await;
        assert_eq!(list["incidents"][0]["silenced"], true);
        assert!(!list["incidents"][0]["silenced_until"].is_null());
        let uri = format!("/api/servers/{}", a.server);
        let (_, server, _) = call(&pool, "GET", &uri, Some(&admin), None).await;
        assert!(!server["silenced_until"].is_null());

        end(&pool, id).await;
        settle(&dispatcher).await;
        assert_eq!(
            taken(&seen),
            ["Still open after a silence. Critical: cpu_usage_percent is 91 on a-web (a / a-site)"]
        );
        settle(&dispatcher).await;
        assert!(taken(&seen).is_empty(), "told once");

        let events = timeline(&pool, incident).await;
        let kinds: Vec<&str> = events.iter().map(|e| e.0.as_str()).collect();
        assert_eq!(kinds, ["opened", "silenced", "silence_ended"]);
        assert_eq!(events[1].1["silence_id"], id.to_string());
        assert_eq!(events[2].1["notified"], true);
        // Over: the incident remembers, but is not silenced now.
        assert_eq!(sign(&pool, &admin, incident).await, (true, false));

        // Later events go out as usual.
        resolve(&pool, incident).await;
        settle(&dispatcher).await;
        assert_eq!(taken(&seen), ["Resolved: cpu_usage_percent on a-web (a / a-site)"]);
    }

    #[sqlx::test]
    async fn opened_and_resolved_during_a_silence_says_nothing_but_stays_in_history(pool: PgPool) {
        let a = place(&pool, "a").await;
        let (dispatcher, seen) = channel(&pool).await;
        let admin = login(&pool, "root", Role::Administrator, GrantScope::Global).await;
        let id = silence(&pool, Scope { site: Some(a.site), ..Scope::default() }, -1, 60).await;

        let incident = open(&pool, a.server, "warning").await;
        change(&pool, incident, "escalated", "critical").await;
        dispatcher.pass().await.unwrap();
        resolve(&pool, incident).await;
        settle(&dispatcher).await;
        end(&pool, id).await;
        settle(&dispatcher).await;

        assert!(taken(&seen).is_empty());
        let events = timeline(&pool, incident).await;
        let last = events.last().unwrap();
        assert_eq!(last.0, "silence_ended");
        assert_eq!(last.1["notified"], false);
        assert_eq!(events.iter().filter(|e| e.0 == "silenced").count(), 1);
        let (_, list, _) = call(&pool, "GET", "/api/incidents?state=resolved", Some(&admin), None).await;
        assert_eq!(list["incidents"][0]["id"], incident.to_string());
        assert_eq!(list["incidents"][0]["silenced"], true);
        assert!(list["incidents"][0]["silenced_until"].is_null());
        let left: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM notification_outbox WHERE sent_at IS NULL",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(left, 0, "nothing stays pending");
    }

    #[sqlx::test]
    async fn an_incident_told_before_the_silence_gets_its_ending(pool: PgPool) {
        let a = place(&pool, "a").await;
        let (dispatcher, seen) = channel(&pool).await;
        let incident = open(&pool, a.server, "warning").await;
        settle(&dispatcher).await;
        assert_eq!(taken(&seen).len(), 1);

        let id = silence(&pool, Scope { customer: Some(a.customer), ..Scope::default() }, -1, 60).await;
        resolve(&pool, incident).await;
        settle(&dispatcher).await;
        assert!(taken(&seen).is_empty());
        end(&pool, id).await;
        settle(&dispatcher).await;
        assert_eq!(
            taken(&seen),
            ["Resolved: cpu_usage_percent on a-web (a / a-site) (during a silence)"]
        );
    }

    #[sqlx::test]
    async fn only_what_changed_is_told_at_the_end(pool: PgPool) {
        let a = place(&pool, "a").await;
        let (dispatcher, seen) = channel(&pool).await;
        // Told as a warning; worse during the silence.
        let worse = open(&pool, a.server, "warning").await;
        // Told, and nothing new during the silence.
        let b = place(&pool, "b").await;
        let same = open(&pool, b.server, "critical").await;
        settle(&dispatcher).await;
        assert_eq!(taken(&seen).len(), 2);

        let id = silence(&pool, Scope::default(), -1, 60).await;
        change(&pool, worse, "escalated", "critical").await;
        settle(&dispatcher).await;
        assert!(taken(&seen).is_empty());
        end(&pool, id).await;
        settle(&dispatcher).await;
        assert_eq!(
            taken(&seen),
            ["Still open after a silence. Escalated to critical: cpu_usage_percent is 97 on a-web (a / a-site)"]
        );
        let kinds: Vec<String> = timeline(&pool, same).await.into_iter().map(|e| e.0).collect();
        assert_eq!(kinds, ["opened"], "a quiet incident has nothing to say");
    }

    #[sqlx::test]
    async fn the_message_at_the_end_gives_the_state_at_the_end(pool: PgPool) {
        let a = place(&pool, "a").await;
        let (dispatcher, seen) = channel(&pool).await;
        let id = silence(&pool, Scope::default(), -1, 60).await;
        let incident = open(&pool, a.server, "critical").await;
        settle(&dispatcher).await;
        change(&pool, incident, "deescalated", "warning").await;
        settle(&dispatcher).await;
        end(&pool, id).await;
        settle(&dispatcher).await;
        assert_eq!(
            taken(&seen),
            ["Still open after a silence. Warning: cpu_usage_percent is 97 on a-web (a / a-site)"]
        );
    }

    #[sqlx::test]
    async fn overlapping_silences_hold_until_the_last_one_ends(pool: PgPool) {
        let a = place(&pool, "a").await;
        let (dispatcher, seen) = channel(&pool).await;
        let short = silence(&pool, Scope { server: Some(a.server), ..Scope::default() }, -1, 60).await;
        let long = silence(&pool, Scope { site: Some(a.site), ..Scope::default() }, -1, 120).await;
        open(&pool, a.server, "critical").await;
        settle(&dispatcher).await;
        end(&pool, short).await;
        settle(&dispatcher).await;
        assert!(taken(&seen).is_empty(), "the longer silence still covers it");
        end(&pool, long).await;
        settle(&dispatcher).await;
        assert_eq!(taken(&seen).len(), 1);
    }

    #[sqlx::test]
    async fn the_silence_acts_between_its_start_and_its_end_only(pool: PgPool) {
        let a = place(&pool, "a").await;
        let (dispatcher, seen) = channel(&pool).await;
        // Upcoming: holds nothing, and a past one neither.
        let upcoming = silence(&pool, Scope::default(), 30, 60).await;
        silence(&pool, Scope::default(), -60, -30).await;
        open(&pool, a.server, "critical").await;
        settle(&dispatcher).await;
        assert_eq!(taken(&seen).len(), 1);

        // Called off before it started: never holds, not even at its start.
        end(&pool, upcoming).await;
        sqlx::query("UPDATE silences SET starts_at = now() - interval '1 minute' WHERE id = $1")
            .bind(upcoming)
            .execute(&pool)
            .await
            .unwrap();
        open(&pool, place(&pool, "b").await.server, "critical").await;
        settle(&dispatcher).await;
        assert_eq!(taken(&seen).len(), 1);
    }

    #[sqlx::test]
    async fn a_new_event_waits_behind_its_incidents_release(pool: PgPool) {
        // The release is paced, so an incident can still be held when it
        // resolves after the silence: its resolution must not go first.
        let a = place(&pool, "a").await;
        let (dispatcher, seen) = channel(&pool).await;
        let id = silence(&pool, Scope::default(), -1, 60).await;
        let incident = open(&pool, a.server, "critical").await;
        settle(&dispatcher).await;
        end(&pool, id).await;
        // Resolved before the dispatcher looks again.
        resolve(&pool, incident).await;
        settle(&dispatcher).await;
        assert!(taken(&seen).is_empty(), "opened and resolved without anyone told");
    }

    #[sqlx::test]
    async fn a_silence_that_ends_on_its_own_sends_too_even_after_a_restart(pool: PgPool) {
        let a = place(&pool, "a").await;
        let (dispatcher, seen) = channel(&pool).await;
        let id = silence(&pool, Scope::default(), -1, 60).await;
        open(&pool, a.server, "critical").await;
        settle(&dispatcher).await;
        // A long silence, and the server down when it ended.
        sqlx::query(
            "UPDATE notification_outbox SET created_at = now() - interval '3 days',
                                            held_at = now() - interval '3 days'",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE silences SET starts_at = now() - interval '4 days',
                                 ends_at = now() - interval '1 hour' WHERE id = $1",
        )
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
        let restarted = Dispatcher::new(pool.clone(), "https://mf.example").without_pace();
        settle(&restarted).await;
        assert_eq!(taken(&seen).len(), 1, "older than a day, and still sent");
    }

    #[sqlx::test]
    async fn events_written_during_a_silence_are_held_if_the_server_only_sees_them_after(pool: PgPool) {
        let a = place(&pool, "a").await;
        let (dispatcher, seen) = channel(&pool).await;
        silence(&pool, Scope::default(), -120, -60).await;
        let incident = open(&pool, a.server, "critical").await;
        resolve(&pool, incident).await;
        sqlx::query("UPDATE notification_outbox SET created_at = now() - interval '90 minutes'")
            .execute(&pool)
            .await
            .unwrap();
        settle(&dispatcher).await;
        assert!(taken(&seen).is_empty(), "both happened during the silence");
    }

    #[sqlx::test]
    async fn a_silence_stays_inside_its_scope(pool: PgPool) {
        let a = place(&pool, "a").await;
        let b = place(&pool, "b").await;
        let (dispatcher, seen) = channel(&pool).await;
        silence(&pool, Scope { customer: Some(a.customer), ..Scope::default() }, -1, 60).await;
        open(&pool, a.server, "critical").await;
        open(&pool, b.server, "critical").await;
        settle(&dispatcher).await;
        assert_eq!(taken(&seen), ["Critical: cpu_usage_percent is 91 on b-web (b / b-site)"]);
    }

    #[sqlx::test]
    async fn offline_incidents_follow_the_same_rules_and_the_relay_still_covers(pool: PgPool) {
        let a = place(&pool, "a").await;
        let (dispatcher, seen) = channel(&pool).await;
        let id = silence(&pool, Scope { relay: Some(a.relay), ..Scope::default() }, -1, 60).await;

        // The silence opens no gap: the relay's incident still opens.
        let after = Duration::from_secs(300);
        let since = Utc::now() - TimeDelta::minutes(10);
        let opened = offline::apply(&pool, Peer::Relay(a.relay), &Action::Open { since }, after)
            .await
            .unwrap()
            .unwrap();
        settle(&dispatcher).await;
        assert!(taken(&seen).is_empty());
        // load() tells the rule the relay is not muted, so it keeps speaking
        // for its servers.
        let peers = offline::load(&pool).await.unwrap();
        let relay = peers.iter().find(|p| p.peer == Peer::Relay(a.relay)).unwrap();
        assert!(!relay.muted && relay.incident == Some(opened));

        end(&pool, id).await;
        settle(&dispatcher).await;
        assert_eq!(taken(&seen), ["Still open after a silence. Critical: relay a-gw (a / a-site) is offline"]);
    }

    #[sqlx::test]
    async fn events_of_the_whole_server_wait_only_for_everything(pool: PgPool) {
        let a = place(&pool, "a").await;
        let (dispatcher, seen) = channel(&pool).await;
        let backup = |at: DateTime<Utc>| {
            json!({"backup": {"id": "1", "error": "disk full", "last_success_at": at}})
        };
        let insert = "INSERT INTO notification_outbox (event, details) VALUES ('backup.failed', $1)";
        silence(&pool, Scope { customer: Some(a.customer), ..Scope::default() }, -1, 60).await;
        sqlx::query(insert).bind(backup(Utc::now())).execute(&pool).await.unwrap();
        settle(&dispatcher).await;
        assert_eq!(taken(&seen).len(), 1);

        let id = silence(&pool, Scope::default(), -1, 60).await;
        sqlx::query(insert).bind(backup(Utc::now())).execute(&pool).await.unwrap();
        settle(&dispatcher).await;
        assert!(taken(&seen).is_empty());
        end(&pool, id).await;
        settle(&dispatcher).await;
        assert_eq!(taken(&seen), ["During a silence: Backup of the server failed: disk full"]);
    }

    #[sqlx::test]
    async fn a_command_still_waiting_is_told_and_a_decided_one_is_not(pool: PgPool) {
        let a = place(&pool, "a").await;
        let (dispatcher, seen) = channel(&pool).await;
        let id = silence(&pool, Scope::default(), -1, 60).await;
        // The trigger writes the notification of a real pending command.
        let remediation: Uuid = sqlx::query_scalar(
            "INSERT INTO remediation_incidents (customer_id, server_id, metric,
                                                max_commands, max_duration_s)
             VALUES ($1, $2, 'cpu', 5, 600) RETURNING id",
        )
        .bind(a.customer)
        .bind(a.server)
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO remediation_incident_events
                 (incident_id, seq, kind, actor_kind, actor_id, payload, prev_hash, hash)
             VALUES ($1, 1, 'ai_proposal', 'ai', 'mock/m', '{\"command\": \"uptime\"}',
                     decode(repeat('00', 32), 'hex'), decode(repeat('00', 32), 'hex'))",
        )
        .bind(remediation)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO remediation_pending_commands
                 (incident_id, proposal_seq, command, diagnosis, reasoning, tier, rule, reason)
             VALUES ($1, 1, 'uptime', 'd', 'r', 'low_impact', '{}', 'r')",
        )
        .bind(remediation)
        .execute(&pool)
        .await
        .unwrap();
        // One decided or expired during the silence.
        sqlx::query(
            "INSERT INTO notification_outbox (event, customer_id, details)
             VALUES ('approval.waiting', $1, $2)",
        )
        .bind(a.customer)
        .bind(json!({"approval": {"id": Uuid::new_v4(), "command": "gone", "tier": "low_impact"}}))
        .execute(&pool)
        .await
        .unwrap();
        settle(&dispatcher).await;
        assert!(taken(&seen).is_empty());
        end(&pool, id).await;
        settle(&dispatcher).await;
        assert_eq!(
            taken(&seen),
            ["Still waiting after a silence. Approval needed on a-web (a / a-site): `uptime` (low impact)"]
        );
    }

    #[sqlx::test]
    async fn turning_offline_alerts_off_is_not_a_silence(pool: PgPool) {
        // The switch keeps resolving the incident, with its own message,
        // and a silence keeps that message back like any other.
        let a = place(&pool, "a").await;
        let (dispatcher, seen) = channel(&pool).await;
        let after = Duration::from_secs(300);
        let since = Utc::now() - TimeDelta::minutes(10);
        let peer = Peer::Server(a.server);
        let id = offline::apply(&pool, peer, &Action::Open { since }, after).await.unwrap().unwrap();
        settle(&dispatcher).await;
        assert_eq!(taken(&seen).len(), 1);
        let quiet = silence(&pool, Scope { server: Some(a.server), ..Scope::default() }, -1, 60).await;
        let resolve = Action::Resolve { id, at: Utc::now(), reason: "muted" };
        offline::apply(&pool, peer, &resolve, after).await.unwrap().unwrap();
        settle(&dispatcher).await;
        assert!(taken(&seen).is_empty());
        end(&pool, quiet).await;
        settle(&dispatcher).await;
        assert_eq!(
            taken(&seen),
            ["Resolved: offline alerts for a-web (a / a-site) were turned off (during a silence)"]
        );
    }

    #[sqlx::test]
    async fn a_silence_holds_back_from_every_channel_and_routing_still_applies(pool: PgPool) {
        let a = place(&pool, "a").await;
        let b = place(&pool, "b").await;
        let (dispatcher, site_a, _) = scoped(&pool, Coverage::Site(a.site)).await;
        let (_, customer_b, _) = scoped(&pool, Coverage::Customer(b.customer)).await;
        let (_, everything, _) = scoped(&pool, Coverage::Everything).await;
        let id = silence(&pool, Scope { server: Some(a.server), ..Scope::default() }, -1, 60).await;
        open(&pool, a.server, "critical").await;
        open(&pool, b.server, "critical").await;
        settle(&dispatcher).await;
        // a's incident is held from all channels, b's goes where it is routed.
        assert!(taken(&site_a).is_empty());
        assert_eq!(taken(&customer_b), ["Critical: cpu_usage_percent is 91 on b-web (b / b-site)"]);
        assert_eq!(taken(&everything).len(), 1);

        end(&pool, id).await;
        settle(&dispatcher).await;
        let still = "Still open after a silence. Critical: cpu_usage_percent is 91 on a-web (a / a-site)";
        assert_eq!(taken(&site_a), [still]);
        assert_eq!(taken(&everything), [still]);
        assert!(taken(&customer_b).is_empty(), "routing applies to what a silence released");
    }

    #[sqlx::test]
    async fn a_release_is_never_gathered_into_a_summary(pool: PgPool) {
        // Teams takes 5 messages a pass and gathers the rest (ADR-0077);
        // each still-open incident is owed a message of its own.
        let a = place(&pool, "a").await;
        let teams = chat(&pool, Destination::Teams, Vec::new()).await;
        let dispatcher = Dispatcher::new(pool.clone(), "https://mf.example")
            .with_retries(Vec::new())
            .without_pace();
        let id = silence(&pool, Scope::default(), -1, 60).await;
        for i in 0..12 {
            open_disk(&pool, a.server, &format!("/disk{i}")).await;
        }
        settle(&dispatcher).await;
        assert!(taken(&teams).is_empty());
        end(&pool, id).await;
        // A fresh burst meets the release in the same passes.
        let b = place(&pool, "b").await;
        for i in 0..8 {
            open_disk(&pool, b.server, &format!("/fresh{i}")).await;
        }
        // Released a few at a time, so no pass sends Teams more than its 5.
        let mut bodies = Vec::new();
        for _ in 0..10 {
            dispatcher.pass().await.unwrap();
            let pass = taken(&teams);
            assert!(pass.len() <= 5, "{} messages in one pass", pass.len());
            bodies.extend(pass);
        }
        for i in 0..12 {
            let named = format!("/disk{i} is 99 on a-web");
            let found: Vec<&String> = bodies.iter().filter(|b| b.contains(&named)).collect();
            assert_eq!(found.len(), 1, "{named} is owed one message: {bodies:#?}");
            assert!(found[0].contains("Still open after a silence"), "{}", found[0]);
            assert!(!found[0].contains("more notifications"), "{}", found[0]);
        }
        // The fresh ones may be gathered, as before; none is lost.
        let fresh = bodies.iter().filter(|b| b.contains("b-web")).count();
        assert!(fresh >= 1, "{bodies:#?}");
        let left: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM notification_outbox WHERE sent_at IS NULL",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(left, 0);
    }

    #[sqlx::test]
    async fn a_chat_channel_still_limiting_gets_the_release_at_the_next_pass(pool: PgPool) {
        let a = place(&pool, "a").await;
        // Three limited tries: the pass gives up on Discord (ADR-0077).
        let discord = chat(&pool, Destination::Discord, vec![429, 429, 429]).await;
        let (_, hook, _) = scoped(&pool, Coverage::Everything).await;
        let dispatcher = Dispatcher::new(pool.clone(), "https://mf.example")
            .with_retries(vec![Duration::ZERO, Duration::ZERO])
            .without_pace();
        let id = silence(&pool, Scope::default(), -1, 60).await;
        open_disk(&pool, a.server, "/var").await;
        open_disk(&pool, a.server, "/srv").await;
        settle(&dispatcher).await;
        end(&pool, id).await;
        dispatcher.pass().await.unwrap();
        dispatcher.pass().await.unwrap();
        assert_eq!(taken(&hook).len(), 2, "the webhook got both, once");
        let limited = taken(&discord);
        assert_eq!(limited.len(), 3, "three tries of the first, nothing more");
        settle(&dispatcher).await;
        let later = taken(&discord);
        for disk in ["/var", "/srv"] {
            let found = later.iter().filter(|b| b.contains(&format!("{disk} is 99"))).count();
            assert_eq!(found, 1, "{disk}: {later:#?}");
        }
        assert!(taken(&hook).is_empty(), "the copies are for Discord alone");
    }

    #[sqlx::test]
    async fn roles_decide_what_can_be_silenced_and_ended(pool: PgPool) {
        let a = place(&pool, "a").await;
        let b = place(&pool, "b").await;
        let viewer = login(&pool, "viewer", Role::Viewer, GrantScope::Site(a.site)).await;
        let operator = login(&pool, "operator", Role::Operator, GrantScope::Site(a.site)).await;
        let owner = login(&pool, "owner", Role::Operator, GrantScope::Customer(a.customer)).await;
        let admin = login(&pool, "admin-a", Role::Administrator, GrantScope::Customer(a.customer)).await;
        let root = login(&pool, "root", Role::Administrator, GrantScope::Global).await;

        // Read-only is 403, another customer's is 404.
        for scope in [("server", a.server), ("relay", a.relay), ("site", a.site)] {
            assert_eq!(post(&pool, &viewer, request(scope.0, Some(scope.1))).await, StatusCode::FORBIDDEN);
            assert_eq!(post(&pool, &operator, request(scope.0, Some(scope.1))).await, StatusCode::CREATED);
        }
        assert_eq!(post(&pool, &operator, request("server", Some(b.server))).await, StatusCode::NOT_FOUND);
        assert_eq!(post(&pool, &operator, request("site", Some(b.site))).await, StatusCode::NOT_FOUND);
        assert_eq!(post(&pool, &operator, request("server", Some(Uuid::new_v4()))).await, StatusCode::NOT_FOUND);

        // A whole customer needs a role on the whole customer.
        assert_eq!(post(&pool, &operator, request("customer", Some(a.customer))).await, StatusCode::FORBIDDEN);
        assert_eq!(post(&pool, &operator, request("customer", Some(b.customer))).await, StatusCode::NOT_FOUND);
        assert_eq!(post(&pool, &owner, request("customer", Some(a.customer))).await, StatusCode::CREATED);
        assert_eq!(post(&pool, &admin, request("customer", Some(b.customer))).await, StatusCode::NOT_FOUND);

        // Everything is for global administrators.
        for cookie in [&viewer, &operator, &owner, &admin] {
            assert_eq!(post(&pool, cookie, request("all", None)).await, StatusCode::FORBIDDEN);
        }
        assert_eq!(post(&pool, &root, request("all", None)).await, StatusCode::CREATED);

        // Who sees what: the viewer sees what covers its site, with a
        // say on none of it; the customer's administrator can end the
        // customer's but not everything.
        let silences = listed(&pool, &viewer, "").await;
        assert_eq!(silences.len(), 5, "{silences:?}");
        assert!(silences.iter().all(|s| s["can_end"] == false));
        let of = |list: &[Value], scope: &str| list.iter().find(|s| s["scope"] == scope).unwrap().clone();
        let list = listed(&pool, &operator, "").await;
        assert_eq!(of(&list, "server")["can_end"], true);
        assert_eq!(of(&list, "server")["created_by_username"], "operator");
        assert_eq!(of(&list, "server")["target_name"], "a-web");
        assert_eq!(of(&list, "site")["target_name"], "a-site");
        assert_eq!(of(&list, "customer")["target_name"], "a");
        assert_eq!(of(&list, "customer")["can_end"], false);
        assert_eq!(of(&list, "all")["can_end"], false);
        assert!(of(&list, "all")["target_name"].is_null());
        let list = listed(&pool, &admin, "").await;
        assert_eq!(of(&list, "customer")["can_end"], true);
        assert_eq!(of(&list, "all")["can_end"], false);

        let end_of = |silence: &Value| format!("{SILENCES}/{}/end", silence["id"].as_str().unwrap());
        let list = listed(&pool, &operator, "").await;
        let cases = [
            (&viewer, "server", StatusCode::FORBIDDEN),
            (&operator, "customer", StatusCode::FORBIDDEN),
            (&admin, "all", StatusCode::FORBIDDEN),
            (&operator, "server", StatusCode::NO_CONTENT),
            (&owner, "customer", StatusCode::NO_CONTENT),
            (&root, "all", StatusCode::NO_CONTENT),
        ];
        for (cookie, scope, status) in cases {
            let uri = end_of(&of(&list, scope));
            assert_eq!(call(&pool, "POST", &uri, Some(cookie), None).await.0, status, "{scope}");
        }
        let uri = end_of(&of(&list, "server"));
        assert_eq!(call(&pool, "POST", &uri, Some(&operator), None).await.0, StatusCode::CONFLICT);
        assert_eq!(listed(&pool, &operator, "").await.len(), 2, "relay and site remain");

        // Both actions are in the audit log, with what they did.
        assert_eq!(audited(&pool, "silence.create").await.len(), 5);
        let ended = audited(&pool, "silence.end").await;
        assert_eq!(ended.len(), 3);
        assert_eq!(ended[0]["scope"], "server");
        assert_eq!(ended[0]["started"], true);
        // The refused attempts recorded nothing, and ended_by says who.
        let by: Option<Uuid> = sqlx::query_scalar(
            "SELECT ended_by FROM silences WHERE server_id = $1",
        )
        .bind(a.server)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(by.is_some());
    }

    #[sqlx::test]
    async fn another_customers_silences_stay_hidden(pool: PgPool) {
        let a = place(&pool, "a").await;
        let b = place(&pool, "b").await;
        let other = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO sites (name, customer_id) VALUES ('other', $1) RETURNING id",
        )
        .bind(a.customer)
        .fetch_one(&pool)
        .await
        .unwrap();
        let admin = login(&pool, "admin-a", Role::Administrator, GrantScope::Customer(a.customer)).await;
        let site = login(&pool, "site-a", Role::Viewer, GrantScope::Site(a.site)).await;
        let scopes = [
            Scope { server: Some(b.server), ..Scope::default() },
            Scope { relay: Some(b.relay), ..Scope::default() },
            Scope { site: Some(b.site), ..Scope::default() },
            Scope { customer: Some(b.customer), ..Scope::default() },
        ];
        let mut ids = Vec::new();
        for scope in scopes {
            ids.push(silence(&pool, scope, -1, 60).await);
        }
        assert!(listed(&pool, &admin, "").await.is_empty());
        for id in ids {
            let uri = format!("{SILENCES}/{id}/end");
            assert_eq!(call(&pool, "POST", &uri, Some(&admin), None).await.0, StatusCode::NOT_FOUND);
        }
        // Nor does a site user see another site of its own customer.
        silence(&pool, Scope { site: Some(other), ..Scope::default() }, -1, 60).await;
        assert!(listed(&pool, &site, "").await.is_empty());
        assert_eq!(listed(&pool, &admin, "").await.len(), 1);

        // Asking for one that is not yours is 404 too.
        let uri = format!("{SILENCES}?server_id={}", b.server);
        assert_eq!(call(&pool, "GET", &uri, Some(&admin), None).await.0, StatusCode::NOT_FOUND);
    }

    #[sqlx::test]
    async fn a_page_lists_what_covers_it_and_upcoming_silences(pool: PgPool) {
        let a = place(&pool, "a").await;
        let root = login(&pool, "root", Role::Administrator, GrantScope::Global).await;
        silence(&pool, Scope { server: Some(a.server), ..Scope::default() }, -1, 60).await;
        silence(&pool, Scope { site: Some(a.site), ..Scope::default() }, 30, 60).await;
        silence(&pool, Scope { customer: Some(a.customer), ..Scope::default() }, -1, 60).await;
        silence(&pool, Scope::default(), -1, 60).await;
        silence(&pool, Scope { relay: Some(a.relay), ..Scope::default() }, -1, 60).await;
        silence(&pool, Scope::default(), -60, -1).await;

        assert_eq!(listed(&pool, &root, "").await.len(), 5);
        let server = listed(&pool, &root, &format!("?server_id={}", a.server)).await;
        let scopes: Vec<&str> = server.iter().map(|s| s["scope"].as_str().unwrap()).collect();
        assert_eq!(scopes.len(), 4);
        assert!(!scopes.contains(&"relay"));
        let upcoming = server.iter().find(|s| s["scope"] == "site").unwrap();
        assert_eq!(upcoming["active"], false);
        assert_eq!(listed(&pool, &root, &format!("?site_id={}", a.site)).await.len(), 3);
        assert_eq!(listed(&pool, &root, &format!("?relay_id={}", a.relay)).await.len(), 4);
        let both = format!("{SILENCES}?site_id={}&server_id={}", a.site, a.server);
        assert_eq!(call(&pool, "GET", &both, Some(&root), None).await.0, StatusCode::BAD_REQUEST);
        // An upcoming silence does not mark the server as silenced by itself.
        let uri = format!("/api/relays/{}", a.relay);
        let (_, relay, _) = call(&pool, "GET", &uri, Some(&root), None).await;
        assert!(!relay["silenced_until"].is_null());
    }

    #[sqlx::test]
    async fn a_silence_has_an_end_and_a_reason(pool: PgPool) {
        let root = login(&pool, "root", Role::Administrator, GrantScope::Global).await;
        let bad = |change: Value| {
            let mut body = request("all", None);
            body.as_object_mut().unwrap().extend(change.as_object().unwrap().clone());
            body
        };
        assert_eq!(post(&pool, &root, bad(json!({"reason": "  "}))).await, StatusCode::BAD_REQUEST);
        let year = Utc::now() + TimeDelta::days(365);
        assert_eq!(post(&pool, &root, bad(json!({"ends_at": year}))).await, StatusCode::BAD_REQUEST);
        let past = Utc::now() - TimeDelta::hours(1);
        assert_eq!(post(&pool, &root, bad(json!({"ends_at": past}))).await, StatusCode::BAD_REQUEST);
        let body = json!({"scope": "all", "reason": "x"});
        assert_eq!(post(&pool, &root, body).await, StatusCode::BAD_REQUEST, "no end");
        assert_eq!(post(&pool, &root, bad(json!({"target_id": Uuid::new_v4()}))).await, StatusCode::BAD_REQUEST);
        assert_eq!(post(&pool, &root, request("site", None)).await, StatusCode::BAD_REQUEST);

        // The database refuses it too.
        let forever = sqlx::query(
            "INSERT INTO silences (starts_at, ends_at, reason)
             VALUES (now(), now() + interval '31 days', 'x')",
        )
        .execute(&pool)
        .await;
        assert!(forever.is_err());
        let unended = sqlx::query("INSERT INTO silences (starts_at, reason) VALUES (now(), 'x')")
            .execute(&pool)
            .await;
        assert!(unended.is_err());
    }
}
