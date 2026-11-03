//! Follow-up and acknowledgement (ADR-0082), against PostgreSQL and webhook
//! receivers on loopback: who is told next when nobody acknowledges, the
//! default, warnings, silences, hand-offs, restarts, grouping, the
//! resolution, the link in a personal message, and the API's permissions.
//! Ignored without `DATABASE_URL`.

use std::sync::{Arc, Mutex};

use axum::body::{Body, Bytes};
use axum::extract::Path;
use axum::http::{Request, StatusCode};
use chrono::{DateTime, TimeDelta, Utc};
use monitorflow_common::redact::Secret;
use monitorflow_server::alerting::offline::{self, Action, Peer};
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
use monitorflow_server::notify::{self, Coverage, Destination, Dispatcher, Webhook};
use serde_json::{json, Value};
use sqlx::PgPool;
use tokio::net::TcpListener;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery";
const AFTER: std::time::Duration = std::time::Duration::from_secs(300);

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
) -> (StatusCode, Value) {
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
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn user(pool: &PgPool, name: &str, role: Role, scope: GrantScope) -> Uuid {
    users::create_user_with(pool, name, &Secret::new(PASSWORD), role, scope, "test")
        .await
        .unwrap()
}

async fn login(pool: &PgPool, name: &str) -> String {
    let state = ApiState::new(pool.clone(), Registry::default(), ApiSettings::default());
    let body = json!({ "username": name, "password": PASSWORD }).to_string();
    let request = Request::post("/api/auth/login")
        .header("content-type", "application/json")
        .header(CSRF_HEADER, "1")
        .body(Body::from(body))
        .unwrap();
    let response = api::router(state).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

/// A customer with a site.
#[derive(Debug, Clone, Copy)]
struct Place {
    customer: Uuid,
    site: Uuid,
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
    Place { customer, site }
}

/// Webhook receivers on loopback: `/to/{name}` records `(name, body)`.
#[derive(Clone)]
struct Inbox {
    base: String,
    seen: Arc<Mutex<Vec<(String, Value)>>>,
}

impl Inbox {
    async fn start() -> Self {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let store = seen.clone();
        let app = axum::Router::new().route(
            "/to/{name}",
            axum::routing::post(move |Path(name): Path<String>, body: Bytes| {
                let store = store.clone();
                async move {
                    store
                        .lock()
                        .unwrap()
                        .push((name, serde_json::from_slice(&body).unwrap()));
                    StatusCode::OK
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/to/", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { base, seen }
    }

    /// `user`'s contact: a webhook to `/to/{name}`, vouched for so that
    /// loopback is allowed.
    async fn contact(&self, pool: &PgPool, user: Uuid, name: &str) {
        sqlx::query(
            "INSERT INTO user_contacts (user_id, kind, settings, public_only)
             VALUES ($1, 'webhook', jsonb_build_object('url', $2::text), false)",
        )
        .bind(user)
        .bind(format!("{}{name}", self.base))
        .execute(pool)
        .await
        .unwrap();
    }

    /// A webhook channel for `coverage` posting to `/to/{name}`.
    async fn channel(&self, pool: &PgPool, name: &str, coverage: Coverage) -> Uuid {
        let destination = Destination::Webhook(Webhook {
            url: format!("{}{name}", self.base),
        });
        notify::insert(pool, &destination, None, coverage, false)
            .await
            .unwrap()
    }

    /// `(name, text)` of what arrived, sorted, and clears the list.
    fn texts(&self) -> Vec<(String, String)> {
        let mut texts: Vec<(String, String)> = self
            .bodies()
            .into_iter()
            .map(|(name, body)| (name, body["text"].as_str().unwrap().to_owned()))
            .collect();
        texts.sort();
        texts
    }

    fn bodies(&self) -> Vec<(String, Value)> {
        std::mem::take(&mut *self.seen.lock().unwrap())
    }

    /// Who got something, sorted, and clears the list.
    fn who(&self) -> Vec<String> {
        self.texts().into_iter().map(|(name, _)| name).collect()
    }
}

/// A dispatcher without grouping's waits.
fn dispatcher(pool: &PgPool) -> Dispatcher {
    grouping(pool).without_waits()
}

/// A dispatcher with grouping's wait and window (ADR-0080).
fn grouping(pool: &PgPool) -> Dispatcher {
    Dispatcher::new(pool.clone(), "https://mf.example")
        .with_retries(Vec::new())
        .without_pace()
}

/// A server of `site` with an offline incident, opened the way the offline
/// rule does: critical.
async fn offline_server(pool: &PgPool, site: Uuid, name: &str) -> (Peer, Uuid) {
    let server = sqlx::query_scalar(
        "INSERT INTO servers (peer_id, site_id, hostname, os_family)
         VALUES ($1, $2, $1, 'debian') RETURNING id",
    )
    .bind(name)
    .bind(site)
    .fetch_one(pool)
    .await
    .unwrap();
    let peer = Peer::Server(server);
    let since = Utc::now() - TimeDelta::minutes(6);
    let incident = offline::apply(pool, peer, &Action::Open { since }, AFTER)
        .await
        .unwrap()
        .unwrap();
    (peer, incident)
}

async fn back_online(pool: &PgPool, peer: Peer, incident: Uuid) {
    let action = Action::Resolve {
        id: incident,
        at: Utc::now(),
        reason: "back_online",
    };
    offline::apply(pool, peer, &action, AFTER).await.unwrap();
}

/// Opens a disk incident of `severity` on a new server of `site`.
async fn disk(pool: &PgPool, site: Uuid, name: &str, severity: &str) -> Uuid {
    let server: Uuid = sqlx::query_scalar(
        "INSERT INTO servers (peer_id, site_id, hostname, os_family)
         VALUES ($1, $2, $1, 'debian') RETURNING id",
    )
    .bind(name)
    .bind(site)
    .fetch_one(pool)
    .await
    .unwrap();
    let id = sqlx::query_scalar(
        "INSERT INTO incidents (server_id, metric, instance, severity, opened_at, last_value,
                                last_value_at, peak_value, threshold)
         VALUES ($1, 'monitorflow_disk_used_percent', '/var', $2, now(), 91, now(), 91, '{}')
         RETURNING id",
    )
    .bind(server)
    .bind(severity)
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO incident_events (incident_id, occurred_at, kind, severity, value)
         VALUES ($1, now(), 'opened', $2, 91)",
    )
    .bind(id)
    .bind(severity)
    .execute(pool)
    .await
    .unwrap();
    id
}

/// The incident changes to `severity`, as the engine does.
async fn change(pool: &PgPool, incident: Uuid, kind: &str, severity: &str) {
    sqlx::query("UPDATE incidents SET severity = $2 WHERE id = $1")
        .bind(incident)
        .bind(severity)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO incident_events (incident_id, occurred_at, kind, severity, value)
         VALUES ($1, now(), $2, $3, 95)",
    )
    .bind(incident)
    .bind(kind)
    .bind(severity)
    .execute(pool)
    .await
    .unwrap();
}

async fn resolve(pool: &PgPool, incident: Uuid) {
    sqlx::query("UPDATE incidents SET state = 'resolved', resolved_at = now() WHERE id = $1")
        .bind(incident)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO incident_events (incident_id, occurred_at, kind) VALUES ($1, now(), 'resolved')")
        .bind(incident)
        .execute(pool)
        .await
        .unwrap();
}

/// `(step, next_at, stop_reason)` of the incident's follow-up.
async fn follow_up(
    pool: &PgPool,
    incident: Uuid,
) -> Option<(i32, Option<DateTime<Utc>>, Option<String>)> {
    sqlx::query_as(
        "SELECT step, next_at, stop_reason FROM incident_follow_ups WHERE incident_id = $1",
    )
    .bind(incident)
    .fetch_optional(pool)
    .await
    .unwrap()
}

/// Makes every running follow-up's next step due, as if its wait had
/// passed.
async fn due(pool: &PgPool) {
    sqlx::query(
        "UPDATE incident_follow_ups
         SET next_at = now() - interval '1 second', started_at = started_at - interval '15 minutes'
         WHERE next_at IS NOT NULL",
    )
    .execute(pool)
    .await
    .unwrap();
}

/// A daily schedule for `coverage`, its first member on call since 12
/// hours ago: no hand-off falls within the minutes a test looks ahead,
/// whatever the time of day.
async fn schedule(pool: &PgPool, coverage: Coverage, members: &[Uuid]) -> Uuid {
    schedule_since(pool, coverage, members, Utc::now() - TimeDelta::hours(12)).await
}

/// A daily schedule for `coverage` handing off at the time of day of
/// `since`, its first member having taken over at `since`.
async fn schedule_since(
    pool: &PgPool,
    coverage: Coverage,
    members: &[Uuid],
    since: DateTime<Utc>,
) -> Uuid {
    let (customer, site) = coverage.columns();
    let id = sqlx::query_scalar(
        "INSERT INTO on_call_schedules (name, customer_id, site_id, time_zone, rotation,
                                        handoff_time, anchor_date)
         VALUES ('Ops', $1, $2, 'UTC', 'daily', $3, $4) RETURNING id",
    )
    .bind(customer)
    .bind(site)
    .bind(since.time())
    .bind(since.date_naive())
    .fetch_one(pool)
    .await
    .unwrap();
    for (position, member) in members.iter().enumerate() {
        sqlx::query(
            "INSERT INTO on_call_members (schedule_id, position, user_id) VALUES ($1, $2, $3)",
        )
        .bind(id)
        .bind(i32::try_from(position).unwrap())
        .bind(member)
        .execute(pool)
        .await
        .unwrap();
    }
    id
}

/// The kinds of the incident's timeline, with their details.
async fn timeline(pool: &PgPool, incident: Uuid) -> Vec<(String, Value)> {
    sqlx::query_as("SELECT kind, details FROM incident_events WHERE incident_id = $1 ORDER BY id")
        .bind(incident)
        .fetch_all(pool)
        .await
        .unwrap()
}

/// The secret of an acknowledge link in a webhook body.
fn token_of(body: &Value) -> String {
    let url = body["acknowledge_url"]
        .as_str()
        .unwrap_or_else(|| panic!("no link in {body}"));
    let (page, token) = url.split_once('#').unwrap();
    assert_eq!(page, "https://mf.example/acknowledge/");
    token.to_owned()
}

db_tests! {
    #[sqlx::test]
    async fn by_default_the_administrators_hear_of_what_nobody_acknowledged(pool: PgPool) {
        let a = place(&pool, "acme").await;
        let alice = user(&pool, "alice", Role::Operator, GrantScope::Customer(a.customer)).await;
        let boss = user(&pool, "boss", Role::Administrator, GrantScope::Customer(a.customer)).await;
        let inbox = Inbox::start().await;
        inbox.contact(&pool, alice, "alice").await;
        inbox.contact(&pool, boss, "boss").await;
        inbox.channel(&pool, "team", Coverage::Customer(a.customer)).await;
        schedule(&pool, Coverage::Customer(a.customer), &[alice]).await;
        let (peer, incident) = offline_server(&pool, a.site, "web-1").await;
        let dispatcher = dispatcher(&pool);
        dispatcher.pass().await.unwrap();
        assert_eq!(inbox.who(), ["alice", "team"]);

        // The first message starts the wait: 15 minutes, kept in the
        // database.
        let (step, next_at, stopped) = follow_up(&pool, incident).await.unwrap();
        assert_eq!((step, stopped), (0, None));
        let wait = next_at.unwrap() - Utc::now();
        assert!(wait > TimeDelta::minutes(14) && wait <= TimeDelta::minutes(15), "{wait}");
        dispatcher.pass().await.unwrap();
        assert!(inbox.who().is_empty());

        // A dispatcher started after a restart takes the step when due.
        due(&pool).await;
        let restarted = self::dispatcher(&pool);
        restarted.pass().await.unwrap();
        let texts = inbox.texts();
        assert_eq!(texts.len(), 1, "{texts:?}");
        assert_eq!(texts[0].0, "boss");
        assert!(texts[0].1.starts_with("Not acknowledged after 15 minutes. "), "{}", texts[0].1);
        assert!(texts[0].1.ends_with("Critical: web-1 (acme / acme-site) is offline"), "{}", texts[0].1);
        // That was the default's only step: it stops, and says who it told.
        assert_eq!(follow_up(&pool, incident).await.unwrap(), (1, None, Some("finished".into())));
        let (kind, details) = timeline(&pool, incident).await.pop().unwrap();
        assert_eq!(kind, "followed_up");
        assert_eq!(details["told"], json!(["boss"]));
        due(&pool).await;
        restarted.pass().await.unwrap();
        assert!(inbox.who().is_empty());

        // The resolution reaches everyone who was told.
        back_online(&pool, peer, incident).await;
        restarted.pass().await.unwrap();
        let texts = inbox.texts();
        assert_eq!(texts.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["alice", "boss", "team"]);
        assert!(texts.iter().all(|(_, t)| t.contains("back online")), "{texts:?}");
    }

    #[sqlx::test]
    async fn warnings_are_not_followed_up_until_they_get_worse(pool: PgPool) {
        let a = place(&pool, "acme").await;
        let boss = user(&pool, "boss", Role::Administrator, GrantScope::Customer(a.customer)).await;
        let inbox = Inbox::start().await;
        inbox.contact(&pool, boss, "boss").await;
        let incident = disk(&pool, a.site, "db-1", "warning").await;
        let dispatcher = dispatcher(&pool);
        dispatcher.pass().await.unwrap();
        assert_eq!(follow_up(&pool, incident).await, None);

        change(&pool, incident, "escalated", "critical").await;
        dispatcher.pass().await.unwrap();
        assert_eq!(follow_up(&pool, incident).await.unwrap().0, 0);

        // Back to a warning: the next step does not go, and the follow-up
        // starts over if it gets worse again.
        change(&pool, incident, "deescalated", "warning").await;
        due(&pool).await;
        dispatcher.pass().await.unwrap();
        assert!(inbox.who().is_empty());
        assert_eq!(follow_up(&pool, incident).await.unwrap().2.as_deref(), Some("not_critical"));
        change(&pool, incident, "escalated", "critical").await;
        dispatcher.pass().await.unwrap();
        let (step, next_at, stopped) = follow_up(&pool, incident).await.unwrap();
        assert_eq!((step, stopped), (0, None));
        assert!(next_at.unwrap() > Utc::now() + TimeDelta::minutes(14));
    }

    #[sqlx::test]
    async fn acknowledging_stops_the_follow_up_and_says_who(pool: PgPool) {
        let a = place(&pool, "acme").await;
        let alice = user(&pool, "alice", Role::Operator, GrantScope::Customer(a.customer)).await;
        user(&pool, "victor", Role::Viewer, GrantScope::Customer(a.customer)).await;
        let boss = user(&pool, "boss", Role::Administrator, GrantScope::Customer(a.customer)).await;
        let inbox = Inbox::start().await;
        inbox.contact(&pool, boss, "boss").await;
        let incident = disk(&pool, a.site, "db-1", "critical").await;
        let dispatcher = dispatcher(&pool);
        dispatcher.pass().await.unwrap();

        // A viewer reads it but cannot say they are on it.
        let viewer = login(&pool, "victor").await;
        let (_, list) = call(&pool, "GET", "/api/incidents", Some(&viewer), None).await;
        assert_eq!(list["incidents"][0]["can_acknowledge"], false);
        let ask = json!({ "incident_ids": [incident] });
        let (status, _) = call(&pool, "POST", "/api/incidents/acknowledge", Some(&viewer), Some(ask.clone())).await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        let cookie = login(&pool, "alice").await;
        let (_, detail) = call(&pool, "GET", &format!("/api/incidents/{incident}"), Some(&cookie), None).await;
        assert_eq!(detail["can_acknowledge"], true);
        assert_eq!(detail["follow_up"]["state"], "waiting");
        assert_eq!(detail["follow_up"]["next"], json!(["boss"]));
        assert_eq!(detail["follow_up"]["policy"], "default");
        let (status, body) = call(&pool, "POST", "/api/incidents/acknowledge", Some(&cookie), Some(ask.clone())).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["acknowledged"], json!([incident]));
        // Someone was first: nothing changes.
        let (_, body) = call(&pool, "POST", "/api/incidents/acknowledge", Some(&cookie), Some(ask)).await;
        assert_eq!(body["acknowledged"], json!([]));

        let (_, detail) = call(&pool, "GET", &format!("/api/incidents/{incident}"), Some(&cookie), None).await;
        assert_eq!(detail["acknowledged_by"], "alice");
        assert!(detail["acknowledged_at"].is_string());
        assert_eq!(detail["acknowledged_via"], "page");
        assert_eq!(detail["can_acknowledge"], false);
        assert_eq!(detail["follow_up"]["state"], "stopped");
        assert_eq!(detail["follow_up"]["stop_reason"], "acknowledged");
        let kinds: Vec<String> = timeline(&pool, incident).await.into_iter().map(|(k, _)| k).collect();
        assert_eq!(kinds, ["opened", "acknowledged"]);
        let audited: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM audit_log WHERE action = 'incident.acknowledge' AND target_id = $1
               AND actor_id = $2 AND details ->> 'via' = 'page'",
        )
        .bind(incident.to_string())
        .bind(alice.to_string())
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(audited, 1);

        due(&pool).await;
        dispatcher.pass().await.unwrap();
        assert!(inbox.who().is_empty());
    }

    #[sqlx::test]
    async fn a_silence_puts_the_next_step_off_until_after_it(pool: PgPool) {
        let a = place(&pool, "acme").await;
        let alice = user(&pool, "alice", Role::Operator, GrantScope::Customer(a.customer)).await;
        let boss = user(&pool, "boss", Role::Administrator, GrantScope::Customer(a.customer)).await;
        let inbox = Inbox::start().await;
        inbox.contact(&pool, boss, "boss").await;
        let incident = disk(&pool, a.site, "db-1", "critical").await;
        let dispatcher = dispatcher(&pool);
        dispatcher.pass().await.unwrap();
        let end: DateTime<Utc> = sqlx::query_scalar(
            "INSERT INTO silences (customer_id, starts_at, ends_at, reason)
             VALUES ($1, now(), now() + interval '1 hour', 'x') RETURNING ends_at",
        )
        .bind(a.customer)
        .fetch_one(&pool)
        .await
        .unwrap();
        due(&pool).await;
        dispatcher.pass().await.unwrap();
        assert!(inbox.who().is_empty());
        // Due again once the silence is over and its own wait has passed.
        let (step, next_at, _) = follow_up(&pool, incident).await.unwrap();
        assert_eq!(step, 0);
        assert_eq!(next_at.unwrap(), end + TimeDelta::minutes(15));

        let cookie = login(&pool, "alice").await;
        let (_, detail) = call(&pool, "GET", &format!("/api/incidents/{incident}"), Some(&cookie), None).await;
        assert_eq!(detail["follow_up"]["state"], "silenced");
        let shown: DateTime<Utc> = detail["follow_up"]["next_at"].as_str().unwrap().parse().unwrap();
        assert_eq!(shown, end + TimeDelta::minutes(15));
        let _ = alice;

        sqlx::query("UPDATE silences SET ended_at = now()").execute(&pool).await.unwrap();
        due(&pool).await;
        dispatcher.pass().await.unwrap();
        assert_eq!(inbox.who(), ["boss"]);
    }

    #[sqlx::test]
    async fn each_step_tells_whoever_is_on_call_then(pool: PgPool) {
        let a = place(&pool, "acme").await;
        let alice = user(&pool, "alice", Role::Operator, GrantScope::Customer(a.customer)).await;
        let bob = user(&pool, "bob", Role::Operator, GrantScope::Customer(a.customer)).await;
        user(&pool, "boss", Role::Administrator, GrantScope::Customer(a.customer)).await;
        let inbox = Inbox::start().await;
        inbox.contact(&pool, alice, "alice").await;
        inbox.contact(&pool, bob, "bob").await;
        inbox.channel(&pool, "team", Coverage::Customer(a.customer)).await;
        // A channel only for follow-ups hears nothing else.
        let managers = inbox.channel(&pool, "managers", Coverage::Customer(a.customer)).await;
        notify::set_follow_up_only(&pool, managers, true).await.unwrap();
        let ops = schedule(&pool, Coverage::Customer(a.customer), &[alice, bob]).await;
        let cookie = login(&pool, "boss").await;
        let policy = json!({
            "customer_id": a.customer,
            "steps": [
                { "after_minutes": 5, "on_call": [ops] },
                { "after_minutes": 10, "channels": [managers] },
            ],
        });
        let (status, view) = call(&pool, "PUT", "/api/follow-up/policies", Some(&cookie), Some(policy)).await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!(view["steps"][1]["channels"][0]["name"], "a webhook");

        let incident = disk(&pool, a.site, "db-1", "critical").await;
        let dispatcher = dispatcher(&pool);
        dispatcher.pass().await.unwrap();
        assert_eq!(inbox.who(), ["alice", "team"]);
        let wait = follow_up(&pool, incident).await.unwrap().1.unwrap() - Utc::now();
        assert!(wait > TimeDelta::minutes(4) && wait <= TimeDelta::minutes(5), "{wait}");

        // Bob takes over before the first step: he is the one told.
        sqlx::query("UPDATE on_call_schedules SET anchor_position = 1 WHERE id = $1")
            .bind(ops)
            .execute(&pool)
            .await
            .unwrap();
        let (_, detail) = call(&pool, "GET", &format!("/api/incidents/{incident}"), Some(&cookie), None).await;
        assert_eq!(detail["follow_up"]["next"], json!(["bob"]));
        assert_eq!(detail["follow_up"]["policy"], "customer");
        due(&pool).await;
        dispatcher.pass().await.unwrap();
        assert_eq!(inbox.who(), ["bob"]);
        let (step, next_at, _) = follow_up(&pool, incident).await.unwrap();
        assert_eq!(step, 1);
        assert!(next_at.unwrap() - Utc::now() > TimeDelta::minutes(9));
        due(&pool).await;
        dispatcher.pass().await.unwrap();
        assert_eq!(inbox.who(), ["managers"]);
        assert_eq!(follow_up(&pool, incident).await.unwrap().2.as_deref(), Some("finished"));

        // Alice, on call when it opened, hears it resolve with the others,
        // though Bob is on call now.
        resolve(&pool, incident).await;
        dispatcher.pass().await.unwrap();
        assert_eq!(inbox.who(), ["alice", "bob", "managers", "team"]);
    }

    #[sqlx::test]
    async fn the_page_names_who_is_on_call_at_the_step_s_time(pool: PgPool) {
        let a = place(&pool, "acme").await;
        let alice = user(&pool, "alice", Role::Operator, GrantScope::Customer(a.customer)).await;
        let bob = user(&pool, "bob", Role::Operator, GrantScope::Customer(a.customer)).await;
        user(&pool, "boss", Role::Administrator, GrantScope::Customer(a.customer)).await;
        let inbox = Inbox::start().await;
        inbox.contact(&pool, alice, "alice").await;
        inbox.contact(&pool, bob, "bob").await;
        // Alice hands off to Bob in 3 minutes, before the step's 5.
        let handoff = Utc::now() + TimeDelta::minutes(3);
        let ops = schedule_since(&pool, Coverage::Customer(a.customer), &[alice, bob], handoff - TimeDelta::days(1)).await;
        let cookie = login(&pool, "boss").await;
        let policy = json!({
            "customer_id": a.customer,
            "steps": [{ "after_minutes": 5, "on_call": [ops] }],
        });
        let (status, view) = call(&pool, "PUT", "/api/follow-up/policies", Some(&cookie), Some(policy)).await;
        assert_eq!(status, StatusCode::OK, "{view}");

        let incident = disk(&pool, a.site, "db-1", "critical").await;
        let dispatcher = dispatcher(&pool);
        dispatcher.pass().await.unwrap();
        assert_eq!(inbox.who(), ["alice"]);
        let (_, detail) = call(&pool, "GET", &format!("/api/incidents/{incident}"), Some(&cookie), None).await;
        assert_eq!(detail["follow_up"]["next"], json!(["bob"]));

        // Sent early, before the hand-off, the step goes to Alice: whoever
        // is on call when it is sent.
        due(&pool).await;
        dispatcher.pass().await.unwrap();
        assert_eq!(inbox.who(), ["alice"]);
    }

    #[sqlx::test]
    async fn steps_due_together_go_as_one_message_within_the_window(pool: PgPool) {
        let a = place(&pool, "acme").await;
        let boss = user(&pool, "boss", Role::Administrator, GrantScope::Customer(a.customer)).await;
        let inbox = Inbox::start().await;
        sqlx::query(
            "INSERT INTO user_contacts (user_id, kind, settings, secret, public_only)
             VALUES ($1, 'slack', '{\"name\": \"me\", \"host\": \"hooks.slack.com\"}', $2, false)",
        )
        .bind(boss)
        .bind(format!("{}boss", inbox.base))
        .execute(&pool)
        .await
        .unwrap();
        let dispatcher = grouping(&pool);
        for n in 0..3 {
            offline_server(&pool, a.site, &format!("web-{n}")).await;
        }
        sqlx::query("UPDATE notification_outbox SET created_at = created_at - interval '10 seconds'")
            .execute(&pool)
            .await
            .unwrap();
        dispatcher.pass().await.unwrap();
        // The steps fall due within a minute of the customer's last
        // message: they wait for the window, then go as one.
        due(&pool).await;
        dispatcher.pass().await.unwrap();
        assert!(inbox.bodies().is_empty());
        sqlx::query("UPDATE notification_outbox SET sent_at = sent_at - interval '60 seconds'")
            .execute(&pool)
            .await
            .unwrap();
        dispatcher.pass().await.unwrap();
        let bodies = inbox.bodies();
        assert_eq!(bodies.len(), 1, "{bodies:?}");
        let text = bodies[0].1["text"].as_str().unwrap();
        assert!(text.starts_with("Not acknowledged after "), "{text}");
        assert!(text.ends_with("3 servers are offline at acme / acme-site"), "{text}");
        // One link acknowledges the three, on the card's second button.
        assert!(bodies[0].1.to_string().contains("|Acknowledge>"), "{}", bodies[0].1);
        let links: Vec<i32> = sqlx::query_scalar("SELECT cardinality(incident_ids) FROM acknowledge_links")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(links, [3]);
    }

    #[sqlx::test]
    async fn the_link_acknowledges_as_its_person_and_only_when_asked(pool: PgPool) {
        let a = place(&pool, "acme").await;
        let boss = user(&pool, "boss", Role::Administrator, GrantScope::Customer(a.customer)).await;
        let victor = user(&pool, "victor", Role::Viewer, GrantScope::Customer(a.customer)).await;
        let inbox = Inbox::start().await;
        inbox.contact(&pool, boss, "boss").await;
        inbox.contact(&pool, victor, "victor").await;
        schedule(&pool, Coverage::Customer(a.customer), &[victor]).await;
        let incident = disk(&pool, a.site, "db-1", "critical").await;
        let dispatcher = dispatcher(&pool);
        dispatcher.pass().await.unwrap();
        // A viewer on call is told, without a link: they cannot acknowledge.
        let bodies = inbox.bodies();
        assert_eq!(bodies.len(), 1);
        assert_eq!(bodies[0].0, "victor");
        assert!(bodies[0].1.get("acknowledge_url").is_none(), "{}", bodies[0].1);
        due(&pool).await;
        dispatcher.pass().await.unwrap();
        let bodies = inbox.bodies();
        let token = token_of(&bodies[0].1);
        let stored: i64 = sqlx::query_scalar("SELECT count(*) FROM acknowledge_links WHERE token_hash = sha256(convert_to($1, 'UTF8'))")
            .bind(&token)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(stored, 1, "only the hash of the secret is stored");

        // Fetching the link, as a mail scanner does, reaches no route: the
        // secret is in the fragment, and the API answers GET with 405.
        let (status, _) = call(&pool, "GET", "/api/acknowledge-link", None, None).await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
        let (status, _) = call(&pool, "GET", "/api/acknowledge-link/use", None, None).await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
        // The page shows what it acknowledges, without a session, and that
        // changes nothing.
        let ask = json!({ "token": token });
        let (status, view) = call(&pool, "POST", "/api/acknowledge-link", None, Some(ask.clone())).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(view["username"], "boss");
        assert_eq!(view["incidents"][0]["id"], json!(incident));
        assert_eq!(view["incidents"][0]["acknowledged_by"], Value::Null);
        assert!(view["incidents"][0]["text"].as_str().unwrap().contains("disk_used_percent /var"));
        let acknowledged: i64 = sqlx::query_scalar("SELECT count(*) FROM incident_acknowledgements")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(acknowledged, 0);

        // The click acknowledges, as its person, once.
        let (status, view) = call(&pool, "POST", "/api/acknowledge-link/use", None, Some(ask.clone())).await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!(view["incidents"][0]["acknowledged_by"], "boss");
        let (status, _) = call(&pool, "POST", "/api/acknowledge-link/use", None, Some(ask.clone())).await;
        assert_eq!(status, StatusCode::OK);
        let via: Vec<(String, String)> = sqlx::query_as("SELECT username, via FROM incident_acknowledgements")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(via, [("boss".to_owned(), "link".to_owned())]);
        let audited: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM audit_log WHERE action = 'incident.acknowledge'
               AND actor_id = $1 AND details ->> 'via' = 'link'",
        )
        .bind(boss.to_string())
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(audited, 1);

        // Expired, it shows and does nothing.
        sqlx::query("UPDATE acknowledge_links SET created_at = now() - interval '13 hours', expires_at = now() - interval '1 hour'")
            .execute(&pool)
            .await
            .unwrap();
        for path in ["/api/acknowledge-link", "/api/acknowledge-link/use"] {
            let (status, body) = call(&pool, "POST", path, None, Some(ask.clone())).await;
            assert_eq!(status, StatusCode::GONE, "{path}");
            assert!(body["error"].as_str().unwrap().contains("expired"), "{body}");
        }
        let (status, _) = call(&pool, "POST", "/api/acknowledge-link", None, Some(json!({ "token": "nope" }))).await;
        assert_eq!(status, StatusCode::GONE);
    }

    #[sqlx::test]
    async fn a_link_follows_its_person_s_roles_now(pool: PgPool) {
        let a = place(&pool, "acme").await;
        let boss = user(&pool, "boss", Role::Administrator, GrantScope::Customer(a.customer)).await;
        let inbox = Inbox::start().await;
        inbox.contact(&pool, boss, "boss").await;
        let incident = disk(&pool, a.site, "db-1", "critical").await;
        let dispatcher = dispatcher(&pool);
        dispatcher.pass().await.unwrap();
        due(&pool).await;
        dispatcher.pass().await.unwrap();
        let ask = json!({ "token": token_of(&inbox.bodies()[0].1) });
        // Boss became a viewer: the link shows the alert, acknowledges
        // nothing.
        sqlx::query("UPDATE user_roles SET role = 'viewer' WHERE user_id = $1")
            .bind(boss)
            .execute(&pool)
            .await
            .unwrap();
        let (status, _) = call(&pool, "POST", "/api/acknowledge-link/use", None, Some(ask.clone())).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        // No role at all: it no longer even shows it.
        sqlx::query("DELETE FROM user_roles WHERE user_id = $1").bind(boss).execute(&pool).await.unwrap();
        let (status, view) = call(&pool, "POST", "/api/acknowledge-link", None, Some(ask.clone())).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(view["incidents"], json!([]));
        // Disabled: the link is dead.
        sqlx::query("UPDATE users SET disabled_at = now() WHERE id = $1").bind(boss).execute(&pool).await.unwrap();
        let (status, _) = call(&pool, "POST", "/api/acknowledge-link", None, Some(ask)).await;
        assert_eq!(status, StatusCode::GONE);
        assert_eq!(follow_up(&pool, incident).await.unwrap().2.as_deref(), Some("finished"));
        let acknowledged: i64 = sqlx::query_scalar("SELECT count(*) FROM incident_acknowledgements")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(acknowledged, 0);
    }

    #[sqlx::test]
    async fn administrators_set_the_follow_up_of_what_they_administer(pool: PgPool) {
        let (a, b) = (place(&pool, "acme").await, place(&pool, "beta").await);
        user(&pool, "boss", Role::Administrator, GrantScope::Customer(a.customer)).await;
        user(&pool, "olga", Role::Operator, GrantScope::Customer(a.customer)).await;
        user(&pool, "root", Role::Administrator, GrantScope::Global).await;
        let stranger = user(&pool, "stranger", Role::Operator, GrantScope::Customer(b.customer)).await;
        let steps = json!([{ "after_minutes": 20, "administrators": true }]);
        let cookie = login(&pool, "boss").await;
        let put = |place: Value, steps: Value| {
            let mut body = place;
            body["steps"] = steps;
            body
        };

        let olga = login(&pool, "olga").await;
        for (method, uri) in [("GET", "/api/follow-up/policies"), ("PUT", "/api/follow-up/policies")] {
            let body = (method == "PUT").then(|| put(json!({ "customer_id": a.customer }), steps.clone()));
            assert_eq!(call(&pool, method, uri, Some(&olga), body).await.0, StatusCode::FORBIDDEN);
        }
        for place in [json!({}), json!({ "customer_id": b.customer }), json!({ "site_id": b.site })] {
            let (status, _) = call(&pool, "PUT", "/api/follow-up/policies", Some(&cookie), Some(put(place, steps.clone()))).await;
            assert_eq!(status, StatusCode::FORBIDDEN);
        }
        for (bad, words) in [
            (json!([{ "after_minutes": 0, "administrators": true }]), "between 1 and 1440"),
            (json!([{ "after_minutes": 5 }]), "tells nobody"),
            (json!([{ "after_minutes": 5, "users": [stranger] }]), "see all of this place"),
        ] {
            let (status, body) = call(&pool, "PUT", "/api/follow-up/policies", Some(&cookie), Some(put(json!({ "customer_id": a.customer }), bad))).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert!(body["error"].as_str().unwrap().contains(words), "{body}");
        }

        let (status, view) = call(&pool, "PUT", "/api/follow-up/policies", Some(&cookie), Some(put(json!({ "customer_id": a.customer }), steps.clone()))).await;
        assert_eq!(status, StatusCode::OK);
        let id = view["id"].as_str().unwrap().to_owned();
        // Saving again replaces it; an empty list tells nobody after the
        // first message.
        let (_, again) = call(&pool, "PUT", "/api/follow-up/policies", Some(&cookie), Some(put(json!({ "customer_id": a.customer }), json!([])))).await;
        assert_eq!(again["id"], view["id"]);
        assert_eq!(again["steps"], json!([]));
        let (_, list) = call(&pool, "GET", "/api/follow-up/policies", Some(&cookie), None).await;
        assert_eq!(list["policies"].as_array().unwrap().len(), 1);
        assert_eq!(list["default_steps"][0]["after_minutes"], 15);
        let root = login(&pool, "root").await;
        let (_, theirs) = call(&pool, "PUT", "/api/follow-up/policies", Some(&root), Some(put(json!({ "customer_id": b.customer }), steps))).await;
        let (_, list) = call(&pool, "GET", "/api/follow-up/policies", Some(&cookie), None).await;
        assert!(!list.to_string().contains(theirs["id"].as_str().unwrap()));
        let uri = format!("/api/follow-up/policies/{}", theirs["id"].as_str().unwrap());
        assert_eq!(call(&pool, "DELETE", &uri, Some(&cookie), None).await.0, StatusCode::NOT_FOUND);
        let uri = format!("/api/follow-up/policies/{id}");
        assert_eq!(call(&pool, "DELETE", &uri, Some(&cookie), None).await.0, StatusCode::NO_CONTENT);

        let actions: Vec<String> = sqlx::query_scalar(
            "SELECT action FROM audit_log WHERE action LIKE 'follow_up.%' ORDER BY id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            actions,
            ["follow_up.policy_create", "follow_up.policy_update", "follow_up.policy_create", "follow_up.policy_delete"]
        );
    }
}
