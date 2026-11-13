//! On call (ADR-0081): who the dispatcher tells, against PostgreSQL and
//! webhook receivers on loopback, at hand-offs, in gaps, around a silence,
//! and the API's permissions. Ignored without `DATABASE_URL`.

use std::sync::{Arc, Mutex};

use axum::body::{Body, Bytes};
use axum::extract::Path;
use axum::http::{Request, StatusCode};
use chrono::{DateTime, NaiveTime, TimeDelta, Utc};
use monitorflow_common::redact::Secret;
use monitorflow_server::alerting::offline::{self, Action, Peer};
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
use monitorflow_server::notify::on_call::{self, Directory};
use monitorflow_server::notify::{self, Coverage, Destination, Dispatcher, Webhook};
use monitorflow_server::secrets::{self, seal, Column};
use serde_json::{json, Value};
use sqlx::PgPool;
use tokio::net::TcpListener;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery";
const SCHEDULES: &str = "/api/on-call/schedules";
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

async fn user(pool: &PgPool, name: &str, role: Role, scope: GrantScope) -> Uuid {
    users::create_user_with(pool, name, &Secret::new(PASSWORD), role, scope, "test")
        .await
        .unwrap()
}

async fn login(pool: &PgPool, name: &str) -> String {
    let body = json!({ "username": name, "password": PASSWORD });
    let (status, _, cookie) = call(pool, "POST", "/api/auth/login", None, Some(body)).await;
    assert_eq!(status, StatusCode::OK);
    cookie.unwrap()
}

/// A customer with a site and a server.
#[derive(Debug, Clone, Copy)]
struct Place {
    customer: Uuid,
    site: Uuid,
    server: Uuid,
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
    Place {
        customer,
        site,
        server,
    }
}

/// Webhook receivers on loopback: `/to/{name}` records `(name, text)`.
#[derive(Clone)]
struct Inbox {
    base: String,
    seen: Arc<Mutex<Vec<(String, String)>>>,
    /// Names that answer 429 once, as a chat service limiting.
    limited: Arc<Mutex<Vec<String>>>,
}

impl Inbox {
    async fn start() -> Self {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let limited: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let (store, limits) = (seen.clone(), limited.clone());
        let app = axum::Router::new().route(
            "/to/{name}",
            axum::routing::post(move |Path(name): Path<String>, body: Bytes| {
                let (store, limits) = (store.clone(), limits.clone());
                async move {
                    let mut limits = limits.lock().unwrap();
                    if let Some(at) = limits.iter().position(|n| *n == name) {
                        limits.remove(at);
                        let mut response = axum::response::IntoResponse::into_response(
                            StatusCode::TOO_MANY_REQUESTS,
                        );
                        response
                            .headers_mut()
                            .insert("retry-after", "0".parse().unwrap());
                        return response;
                    }
                    let body: Value = serde_json::from_slice(&body).unwrap();
                    let text = body["text"].as_str().unwrap().to_owned();
                    store.lock().unwrap().push((name, text));
                    axum::response::IntoResponse::into_response(StatusCode::OK)
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/to/", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            base,
            seen,
            limited,
        }
    }

    /// `user`'s contact: a Slack destination posting to `/to/{name}`.
    async fn slack(&self, pool: &PgPool, user: Uuid, name: &str) {
        sqlx::query(
            "INSERT INTO user_contacts (user_id, kind, settings, secret, public_only)
             VALUES ($1, 'slack', '{\"name\": \"me\", \"host\": \"hooks.slack.com\"}', $2, false)",
        )
        .bind(user)
        .bind(seal(Column::UserContacts, &format!("{}{name}", self.base)).unwrap())
        .execute(pool)
        .await
        .unwrap();
    }

    /// Who got a message, in order, and clears the list.
    fn who(&self) -> Vec<String> {
        let mut seen: Vec<String> = std::mem::take(&mut *self.seen.lock().unwrap())
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        seen.sort();
        seen
    }

    fn texts(&self) -> Vec<(String, String)> {
        std::mem::take(&mut *self.seen.lock().unwrap())
    }

    /// A channel for `coverage` posting to `/to/{name}`.
    async fn channel(&self, pool: &PgPool, name: &str, coverage: Coverage) {
        let destination = Destination::Webhook(Webhook {
            url: format!("{}{name}", self.base),
        });
        notify::insert(pool, &destination, None, coverage, false)
            .await
            .unwrap();
    }

    /// `user`'s contact: a webhook to `/to/{name}`, as a global
    /// administrator would vouch for it, so loopback is allowed.
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
}

/// A dispatcher that sends at once: grouping's waits are tested apart.
fn dispatcher(pool: &PgPool) -> Dispatcher {
    grouping(pool).without_waits()
}

/// A dispatcher with grouping's 10 s wait and one-minute window (ADR-0080).
fn grouping(pool: &PgPool) -> Dispatcher {
    Dispatcher::new(pool.clone(), "https://mf.example")
        .with_retries(Vec::new())
        .without_pace()
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

/// A server of `site` with an offline incident, opened the way the offline
/// rule does.
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

/// A daily schedule for `coverage` handing off at the time of day of
/// `since` UTC, its first member having taken over at `since`.
async fn schedule(
    pool: &PgPool,
    coverage: Coverage,
    since: DateTime<Utc>,
    members: &[Uuid],
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

/// 12 hours ago: a daily schedule starting then has its first member on
/// call now, and no hand-off near, whatever the time of day.
fn earlier() -> DateTime<Utc> {
    Utc::now() - TimeDelta::hours(12)
}

/// Opens a critical incident on a new disk of `server`.
async fn open(pool: &PgPool, server: Uuid) -> Uuid {
    let id = sqlx::query_scalar(
        "INSERT INTO incidents (server_id, metric, instance, severity, opened_at, last_value,
                                last_value_at, peak_value, threshold)
         VALUES ($1, 'monitorflow_disk_used_percent', $2, 'critical', now(), 91, now(), 91, '{}')
         RETURNING id",
    )
    .bind(server)
    .bind(Uuid::new_v4().to_string())
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO incident_events (incident_id, occurred_at, kind, severity, value)
         VALUES ($1, now(), 'opened', 'critical', 91)",
    )
    .bind(id)
    .execute(pool)
    .await
    .unwrap();
    id
}

db_tests! {
    #[sqlx::test]
    async fn a_site_going_dark_reaches_the_person_on_call_as_one_message(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let a = place(&pool, "acme").await;
        let alice = user(&pool, "alice", Role::Operator, GrantScope::Customer(a.customer)).await;
        let bob = user(&pool, "bob", Role::Operator, GrantScope::Site(a.site)).await;
        let inbox = Inbox::start().await;
        inbox.slack(&pool, alice, "alice").await;
        inbox.contact(&pool, bob, "bob").await;
        schedule(&pool, Coverage::Customer(a.customer), earlier(), &[alice]).await;
        schedule(&pool, Coverage::Site(a.site), earlier(), &[bob]).await;
        let dispatcher = grouping(&pool);
        let mut down = Vec::new();
        for n in 0..30 {
            down.push(offline_server(&pool, a.site, &format!("web-{n}")).await);
        }
        // The same wait as a channel: nothing in the first ten seconds.
        assert_eq!(dispatcher.pass().await.unwrap(), 0);
        assert!(inbox.texts().is_empty());
        wait(&pool, 10).await;
        assert_eq!(dispatcher.pass().await.unwrap(), 30);
        let texts = inbox.texts();
        let alice_got: Vec<&String> = texts.iter().filter(|(n, _)| n == "alice").map(|(_, t)| t).collect();
        assert_eq!(alice_got.len(), 1, "{texts:?}");
        assert!(alice_got[0].contains("30 servers are offline"), "{}", alice_got[0]);
        // A personal webhook keeps one request per event, as webhooks do.
        assert_eq!(texts.iter().filter(|(n, _)| n == "bob").count(), 30);

        // Their return waits for the window, then is one message too.
        for (peer, incident) in down {
            back_online(&pool, peer, incident).await;
        }
        wait(&pool, 30).await;
        assert_eq!(dispatcher.pass().await.unwrap(), 0);
        wait(&pool, 30).await;
        dispatcher.pass().await.unwrap();
        let texts = inbox.texts();
        let alice_got: Vec<&String> = texts.iter().filter(|(n, _)| n == "alice").map(|(_, t)| t).collect();
        assert_eq!(alice_got.len(), 1, "{texts:?}");
        assert!(alice_got[0].contains("back online"), "{}", alice_got[0]);
    }

    #[sqlx::test]
    async fn a_hand_off_within_the_window_sends_the_group_to_the_new_person(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let a = place(&pool, "acme").await;
        let alice = user(&pool, "alice", Role::Operator, GrantScope::Customer(a.customer)).await;
        let bob = user(&pool, "bob", Role::Operator, GrantScope::Customer(a.customer)).await;
        let inbox = Inbox::start().await;
        inbox.slack(&pool, alice, "alice").await;
        inbox.slack(&pool, bob, "bob").await;
        let id = schedule(&pool, Coverage::Customer(a.customer), earlier(), &[alice, bob]).await;
        let dispatcher = grouping(&pool);
        for n in 0..5 {
            offline_server(&pool, a.site, &format!("web-{n}")).await;
        }
        assert_eq!(dispatcher.pass().await.unwrap(), 0);
        // Bob takes over while the group waits: he is on call when it goes.
        sqlx::query("UPDATE on_call_schedules SET anchor_position = 1 WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        wait(&pool, 10).await;
        dispatcher.pass().await.unwrap();
        let texts = inbox.texts();
        assert_eq!(texts.len(), 1, "{texts:?}");
        assert_eq!(texts[0].0, "bob");
        assert!(texts[0].1.contains("5 servers are offline"), "{}", texts[0].1);
    }

    #[sqlx::test]
    async fn what_a_silence_owes_the_person_on_call_survives_a_rate_limit(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let a = place(&pool, "acme").await;
        let alice = user(&pool, "alice", Role::Operator, GrantScope::Customer(a.customer)).await;
        let inbox = Inbox::start().await;
        inbox.slack(&pool, alice, "alice").await;
        inbox.channel(&pool, "team", Coverage::Customer(a.customer)).await;
        schedule(&pool, Coverage::Customer(a.customer), earlier(), &[alice]).await;
        let silence: Uuid = sqlx::query_scalar(
            "INSERT INTO silences (customer_id, starts_at, ends_at, reason)
             VALUES ($1, now() - interval '1 minute', now() + interval '1 hour', 'x') RETURNING id",
        )
        .bind(a.customer)
        .fetch_one(&pool)
        .await
        .unwrap();
        for _ in 0..3 {
            open(&pool, a.server).await;
        }
        let dispatcher = dispatcher(&pool);
        dispatcher.pass().await.unwrap();
        assert!(inbox.texts().is_empty(), "held by the silence");
        sqlx::query("UPDATE silences SET ended_at = now() WHERE id = $1")
            .bind(silence)
            .execute(&pool)
            .await
            .unwrap();
        // Alice's Slack limits once: what she was owed is copied for her
        // alone, and the next pass sends it.
        inbox.limited.lock().unwrap().push("alice".into());
        dispatcher.pass().await.unwrap();
        dispatcher.pass().await.unwrap();
        let copies: Vec<(Option<Uuid>, Option<Uuid>)> = sqlx::query_as(
            "SELECT channel_id, user_id FROM notification_outbox
             WHERE channel_id IS NOT NULL OR user_id IS NOT NULL",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert!(!copies.is_empty() && copies.iter().all(|c| *c == (None, Some(alice))), "{copies:?}");
        for _ in 0..2 {
            dispatcher.pass().await.unwrap();
        }
        let texts = inbox.texts();
        let to = |who: &str| texts.iter().filter(|(n, _)| n == who).map(|(_, t)| t.clone()).collect::<Vec<_>>();
        // The webhook channel keeps one request per event; Alice gets the
        // group once, after its copy, and nothing twice.
        assert_eq!(to("team").len(), 3, "{texts:?}");
        let alice_got = to("alice");
        assert_eq!(alice_got.len(), 1, "{texts:?}");
        assert!(alice_got[0].contains("after a silence"), "{}", alice_got[0]);
    }

    #[sqlx::test]
    async fn the_person_on_call_hears_of_their_place_and_the_channels_still_do(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let (a, b) = (place(&pool, "acme").await, place(&pool, "beta").await);
        let alice = user(&pool, "alice", Role::Operator, GrantScope::Customer(a.customer)).await;
        let bob = user(&pool, "bob", Role::Operator, GrantScope::Customer(a.customer)).await;
        let carol = user(&pool, "carol", Role::Viewer, GrantScope::Site(b.site)).await;
        let inbox = Inbox::start().await;
        for (id, name) in [(alice, "alice"), (bob, "bob"), (carol, "carol")] {
            inbox.contact(&pool, id, name).await;
        }
        inbox.channel(&pool, "team", Coverage::Everything).await;
        schedule(&pool, Coverage::Customer(a.customer), earlier(), &[alice, bob]).await;
        schedule(&pool, Coverage::Site(b.site), earlier(), &[carol]).await;

        open(&pool, a.server).await;
        open(&pool, b.server).await;
        dispatcher(&pool).pass().await.unwrap();
        assert_eq!(inbox.who(), ["alice", "carol", "team", "team"]);

        // The outcome is the contact's status.
        let sent: bool = sqlx::query_scalar(
            "SELECT last_sent_at IS NOT NULL AND last_error IS NULL FROM user_contacts WHERE user_id = $1",
        )
        .bind(alice)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(sent);

        // Tomorrow's hand-off makes it Bob's.
        sqlx::query("UPDATE on_call_schedules SET anchor_date = anchor_date - 1 WHERE customer_id = $1")
            .bind(a.customer)
            .execute(&pool)
            .await
            .unwrap();
        open(&pool, a.server).await;
        dispatcher(&pool).pass().await.unwrap();
        assert_eq!(inbox.who(), ["bob", "team"]);
    }

    #[sqlx::test]
    async fn a_hand_off_during_a_pass_goes_to_one_person(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let a = place(&pool, "acme").await;
        let alice = user(&pool, "alice", Role::Operator, GrantScope::Customer(a.customer)).await;
        let bob = user(&pool, "bob", Role::Operator, GrantScope::Customer(a.customer)).await;
        let inbox = Inbox::start().await;
        inbox.contact(&pool, alice, "alice").await;
        inbox.contact(&pool, bob, "bob").await;
        let since: DateTime<Utc> = "2026-11-02T09:00:00Z".parse().unwrap();
        schedule(&pool, Coverage::Customer(a.customer), since, &[alice, bob]).await;
        let event = notify::Event {
            id: 1,
            event: "incident.opened".into(),
            customer_id: Some(a.customer),
            site_id: Some(a.site),
            details: json!({}),
            created_at: Utc::now(),
        };
        let handoff: DateTime<Utc> = "2026-11-03T09:00:00Z".parse().unwrap();
        let mut conn = pool.acquire().await.unwrap();
        let schedules = on_call::schedules(&mut conn, handoff - TimeDelta::days(1), None).await.unwrap();
        let directory = Directory::load(&mut conn).await.unwrap();
        // The pass looks once, at the moment it sends: the instant of the
        // hand-off is the next person's, the moment before the last one's.
        let to = |now| {
            on_call::deliveries(&schedules, &directory, &[], std::slice::from_ref(&event), &Default::default(), &Default::default(), now)
                .into_iter()
                .map(|(channel, _)| channel.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(to(handoff - TimeDelta::milliseconds(1)), [alice]);
        assert_eq!(to(handoff), [bob]);
    }

    #[sqlx::test]
    async fn gaps_go_to_the_nearest_administrators(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let a = place(&pool, "acme").await;
        let bob = user(&pool, "bob", Role::Operator, GrantScope::Customer(a.customer)).await;
        let boss = user(&pool, "boss", Role::Administrator, GrantScope::Customer(a.customer)).await;
        let root = user(&pool, "root", Role::Administrator, GrantScope::Global).await;
        let inbox = Inbox::start().await;
        inbox.contact(&pool, boss, "boss").await;
        inbox.contact(&pool, root, "root").await;
        inbox.channel(&pool, "team", Coverage::Customer(a.customer)).await;
        schedule(&pool, Coverage::Site(a.site), earlier(), &[bob]).await;
        let pass = || async {
            open(&pool, a.server).await;
            dispatcher(&pool).pass().await.unwrap();
            inbox.who()
        };

        // Bob has no contact: his customer's administrator is told.
        assert_eq!(pass().await, ["boss", "team"]);
        inbox.contact(&pool, bob, "bob").await;
        assert_eq!(pass().await, ["bob", "team"]);

        // Bob loses his role while on call.
        sqlx::query("DELETE FROM user_roles WHERE user_id = $1").bind(bob).execute(&pool).await.unwrap();
        assert_eq!(pass().await, ["boss", "team"]);
        sqlx::query("INSERT INTO user_roles (user_id, role, site_id) VALUES ($1, 'viewer', $2)")
            .bind(bob)
            .bind(a.site)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(pass().await, ["bob", "team"]);

        // Disabled: a gap again. Without the customer's administrator,
        // the global one.
        sqlx::query("UPDATE users SET disabled_at = now() WHERE id = $1").bind(bob).execute(&pool).await.unwrap();
        sqlx::query("DELETE FROM user_contacts WHERE user_id = $1").bind(boss).execute(&pool).await.unwrap();
        assert_eq!(pass().await, ["root", "team"]);

        // Removed while on call: the turn stays, empty, and is a gap.
        sqlx::query("DELETE FROM users WHERE id = $1").bind(bob).execute(&pool).await.unwrap();
        let members: Vec<Option<Uuid>> = sqlx::query_scalar("SELECT user_id FROM on_call_members")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(members, [None]);
        assert_eq!(pass().await, ["root", "team"]);

        // Nobody can be told in person: the channel still is.
        sqlx::query("DELETE FROM user_contacts").execute(&pool).await.unwrap();
        assert_eq!(pass().await, ["team"]);
    }

    #[sqlx::test]
    async fn a_silence_holds_personal_messages_for_whoever_is_on_call_at_its_end(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let a = place(&pool, "acme").await;
        let alice = user(&pool, "alice", Role::Operator, GrantScope::Customer(a.customer)).await;
        let bob = user(&pool, "bob", Role::Operator, GrantScope::Customer(a.customer)).await;
        let inbox = Inbox::start().await;
        inbox.contact(&pool, alice, "alice").await;
        inbox.contact(&pool, bob, "bob").await;
        let id = schedule(&pool, Coverage::Customer(a.customer), earlier(), &[alice, bob]).await;
        let silence: Uuid = sqlx::query_scalar(
            "INSERT INTO silences (customer_id, starts_at, ends_at, reason)
             VALUES ($1, now() - interval '1 minute', now() + interval '1 hour', 'x') RETURNING id",
        )
        .bind(a.customer)
        .fetch_one(&pool)
        .await
        .unwrap();
        open(&pool, a.server).await;
        let dispatcher = dispatcher(&pool);
        dispatcher.pass().await.unwrap();
        assert!(inbox.who().is_empty());

        // Bob took over during the silence; he hears of it at the end.
        sqlx::query("UPDATE on_call_schedules SET anchor_position = 1 WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE silences SET ended_at = now() WHERE id = $1")
            .bind(silence)
            .execute(&pool)
            .await
            .unwrap();
        for _ in 0..3 {
            dispatcher.pass().await.unwrap();
        }
        let texts = inbox.texts();
        assert_eq!(texts.len(), 1, "{texts:?}");
        assert_eq!(texts[0].0, "bob");
        assert!(texts[0].1.contains("after a silence"), "{}", texts[0].1);
    }

    #[sqlx::test]
    async fn overrides_put_someone_else_on_and_the_newest_wins(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let (a, b) = (place(&pool, "acme").await, place(&pool, "beta").await);
        user(&pool, "boss", Role::Administrator, GrantScope::Customer(a.customer)).await;
        let alice = user(&pool, "alice", Role::Operator, GrantScope::Customer(a.customer)).await;
        let bob = user(&pool, "bob", Role::Viewer, GrantScope::Site(a.site)).await;
        let carol = user(&pool, "carol", Role::Operator, GrantScope::Customer(b.customer)).await;
        let cookie = login(&pool, "boss").await;
        let request = json!({
            "name": "Paris on call", "site_id": a.site, "time_zone": "Europe/Paris",
            "rotation": "weekly", "handoff_time": "09:00", "handoff_day": 1,
            "members": [alice],
        });
        let (status, body, _) = call(&pool, "POST", SCHEDULES, Some(&cookie), Some(request)).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body["now"]["username"], "alice");
        assert_eq!(body["handoff_day"], 1);
        let id = body["id"].as_str().unwrap().to_owned();
        let overrides = format!("{SCHEDULES}/{id}/overrides");
        let now = Utc::now();
        let cover = |who: Uuid, from: i64, to: i64| json!({
            "user_id": who,
            "starts_at": now + TimeDelta::hours(from),
            "ends_at": now + TimeDelta::hours(to),
        });

        // Someone of another customer cannot cover; an end before the start
        // is refused.
        for bad in [cover(carol, 0, 48), cover(bob, 10, 5), cover(bob, 0, 24 * 91)] {
            let (status, body, _) = call(&pool, "POST", &overrides, Some(&cookie), Some(bad)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        }
        // Bob covers Alice for two days, then Alice takes back the first
        // hours of it.
        let (status, body, _) = call(&pool, "POST", &overrides, Some(&cookie), Some(cover(bob, -1, 48))).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body["now"]["username"], "bob");
        assert_eq!(body["next"]["username"], "alice");
        let first = body["overrides"][0]["id"].as_str().unwrap().to_owned();
        let (_, body, _) = call(&pool, "POST", &overrides, Some(&cookie), Some(cover(alice, 0, 5))).await;
        assert_eq!(body["now"]["username"], "alice");
        assert_eq!(body["next"]["username"], "bob");
        assert_eq!(body["overrides"].as_array().unwrap().len(), 2);
        let turns: Vec<&str> = body["turns"].as_array().unwrap()[..3]
            .iter()
            .map(|t| t["username"].as_str().unwrap())
            .collect();
        assert_eq!(turns, ["alice", "bob", "alice"]);

        // Removing an override; removing the user who holds the other.
        let uri = format!("/api/on-call/overrides/{first}");
        assert_eq!(call(&pool, "DELETE", &uri, Some(&cookie), None).await.0, StatusCode::NO_CONTENT);
        assert_eq!(call(&pool, "DELETE", &uri, Some(&cookie), None).await.0, StatusCode::NOT_FOUND);
        sqlx::query("DELETE FROM users WHERE id = $1").bind(alice).execute(&pool).await.unwrap();
        let (_, body, _) = call(&pool, "GET", SCHEDULES, Some(&cookie), None).await;
        let schedule = &body["schedules"][0];
        assert_eq!(schedule["overrides"], json!([]));
        assert_eq!(schedule["members"][0]["problem"], "removed");
        assert_eq!(schedule["now"]["problem"], "removed");
        // The boss can be told during that gap.
        assert_eq!(schedule["fallback"], json!([]), "the boss has no contact yet");
    }

    #[sqlx::test]
    async fn editing_a_schedule_keeps_who_is_on_call_now(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let a = place(&pool, "acme").await;
        user(&pool, "boss", Role::Administrator, GrantScope::Customer(a.customer)).await;
        let mut people = Vec::new();
        for name in ["alice", "bob", "carol", "dave"] {
            people.push(user(&pool, name, Role::Viewer, GrantScope::Customer(a.customer)).await);
        }
        let [alice, bob, carol, dave] = people[..] else { unreachable!() };
        let cookie = login(&pool, "boss").await;
        let request = |members: Vec<Uuid>, rotation: &str, time: &str| json!({
            "name": "Ops", "customer_id": a.customer, "time_zone": "America/New_York",
            "rotation": rotation, "handoff_time": time, "handoff_day": 3, "members": members,
        });
        let (status, body, _) = call(&pool, "POST", SCHEDULES, Some(&cookie), Some(request(vec![alice, bob, carol], "daily", "08:00"))).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let uri = format!("{SCHEDULES}/{}", body["id"].as_str().unwrap());
        sqlx::query("UPDATE on_call_schedules SET anchor_position = 1").execute(&pool).await.unwrap();
        let now_on = |body: &Value| body["now"]["user_id"].as_str().unwrap().parse::<Uuid>().unwrap();

        // Reordered, another day, another time: Bob stays on.
        let (status, body, _) = call(&pool, "PUT", &uri, Some(&cookie), Some(request(vec![dave, carol, bob], "weekly", "17:30"))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(now_on(&body), bob);
        assert_eq!(body["handoff_time"], "17:30");
        assert_eq!(body["handoff_day"], 3);
        // Bob taken off: the one after him in the list takes over.
        let (_, body, _) = call(&pool, "PUT", &uri, Some(&cookie), Some(request(vec![dave, carol], "weekly", "17:30"))).await;
        assert_eq!(now_on(&body), dave);
        // Mistakes are sentences.
        for (bad, words) in [
            (request(vec![], "daily", "08:00"), "at least one"),
            (request(vec![dave, dave], "daily", "08:00"), "one turn"),
            (request(vec![dave], "monthly", "08:00"), "daily or weekly"),
            (request(vec![dave], "daily", "8h"), "HH:MM"),
            (json!({"name": "x", "time_zone": "Mars/Base", "rotation": "daily", "handoff_time": "08:00", "members": [dave], "customer_id": a.customer}), "time zone"),
        ] {
            let (status, body, _) = call(&pool, "PUT", &uri, Some(&cookie), Some(bad)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert!(body.to_string().contains(words), "{body}");
        }
        let audited: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action LIKE 'on_call.%'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(audited, 3);
    }

    #[sqlx::test]
    async fn administrators_manage_their_own_schedules_and_members_only_look(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let (a, b) = (place(&pool, "acme").await, place(&pool, "beta").await);
        user(&pool, "boss", Role::Administrator, GrantScope::Customer(a.customer)).await;
        user(&pool, "root", Role::Administrator, GrantScope::Global).await;
        let alice = user(&pool, "alice", Role::Operator, GrantScope::Customer(a.customer)).await;
        let staff = user(&pool, "staff", Role::Operator, GrantScope::Global).await;
        let carol = user(&pool, "carol", Role::Operator, GrantScope::Customer(b.customer)).await;
        user(&pool, "eve", Role::Operator, GrantScope::Customer(a.customer)).await;
        let theirs = schedule(&pool, Coverage::Customer(b.customer), earlier(), &[carol]).await;
        let boss = login(&pool, "boss").await;
        let ask = |members: Vec<Uuid>, customer: Uuid| json!({
            "name": "Ops", "customer_id": customer, "time_zone": "UTC",
            "rotation": "daily", "handoff_time": "08:00", "members": members,
        });

        // Only A's people; a global user is the MSP's to put on.
        let (_, body, _) = call(&pool, "GET", &format!("/api/on-call/people?customer_id={}", a.customer), Some(&boss), None).await;
        let names: Vec<&str> = body["people"].as_array().unwrap().iter().map(|p| p["username"].as_str().unwrap()).collect();
        assert_eq!(names, ["alice", "boss", "eve"]);
        for members in [vec![carol], vec![staff]] {
            let (status, _, _) = call(&pool, "POST", SCHEDULES, Some(&boss), Some(ask(members, a.customer))).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
        }
        let (status, _, _) = call(&pool, "POST", SCHEDULES, Some(&boss), Some(ask(vec![alice], b.customer))).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, mine, _) = call(&pool, "POST", SCHEDULES, Some(&boss), Some(ask(vec![alice], a.customer))).await;
        assert_eq!(status, StatusCode::CREATED);
        let (_, body, _) = call(&pool, "GET", SCHEDULES, Some(&boss), None).await;
        assert_eq!(body["schedules"].as_array().unwrap().len(), 1);
        assert!(!body.to_string().contains(&theirs.to_string()));

        // A global administrator may put a global user on A's schedule.
        let root = login(&pool, "root").await;
        let uri = format!("{SCHEDULES}/{}", mine["id"].as_str().unwrap());
        let (status, _, _) = call(&pool, "PUT", &uri, Some(&root), Some(ask(vec![alice, staff], a.customer))).await;
        assert_eq!(status, StatusCode::OK);
        // The customer administrator can still edit it with them on.
        let (status, _, _) = call(&pool, "PUT", &uri, Some(&boss), Some(ask(vec![staff, alice], a.customer))).await;
        assert_eq!(status, StatusCode::OK);

        // Alice sees the schedule she is on, without managing it; Eve, not
        // on it, sees nothing, and neither can change anything.
        let alice_cookie = login(&pool, "alice").await;
        let (_, body, _) = call(&pool, "GET", SCHEDULES, Some(&alice_cookie), None).await;
        assert_eq!(body["schedules"][0]["can_manage"], false);
        assert_eq!(body["schedules"][0]["fallback"], json!([]));
        let eve = login(&pool, "eve").await;
        let (_, body, _) = call(&pool, "GET", SCHEDULES, Some(&eve), None).await;
        assert_eq!(body["schedules"], json!([]));
        for cookie in [&alice_cookie, &eve] {
            assert_eq!(call(&pool, "DELETE", &uri, Some(cookie), None).await.0, StatusCode::FORBIDDEN);
        }
        assert_eq!(call(&pool, "DELETE", &uri, Some(&boss), None).await.0, StatusCode::NO_CONTENT);
    }

    #[sqlx::test]
    async fn a_user_keeps_their_own_contact(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let a = place(&pool, "acme").await;
        let alice = user(&pool, "alice", Role::Viewer, GrantScope::Customer(a.customer)).await;
        let cookie = login(&pool, "alice").await;
        let uri = "/api/on-call/contact";
        let (_, body, _) = call(&pool, "GET", uri, Some(&cookie), None).await;
        assert_eq!(body["email"], Value::Null);
        let slack = "https://hooks.slack.com/services/T0/B0/SECRETPART";
        let put = |body: Value| call(&pool, "PUT", uri, Some(&cookie), Some(body));
        let (status, body, _) = put(json!({
            "email": " alice@example.com ", "kind": "slack", "settings": {"name": "me"}, "secret": slack,
        }))
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["email"], "alice@example.com");
        assert_eq!(body["has_secret"], true);
        assert!(!body.to_string().contains("SECRETPART"));
        // Kept without typing it again, for the same service only.
        let (status, body, _) = put(json!({"email": "alice@example.com", "kind": "slack", "settings": {"name": "dm"}})).await;
        assert_eq!((status, body["has_secret"].clone()), (StatusCode::OK, json!(true)));
        let (status, body, _) = put(json!({"kind": "discord", "settings": {}})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.to_string().contains("again"), "{body}");
        let (status, _, _) = put(json!({"email": "not an address"})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        // Chosen by someone other than a global administrator: public only.
        let row: (bool, String) = sqlx::query_as("SELECT public_only, secret FROM user_contacts WHERE user_id = $1")
            .bind(alice)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(row.1.starts_with("mfs1:"), "stored sealed: {}", row.1);
        let opened = secrets::open(Column::UserContacts, &row.1).unwrap();
        assert_eq!((row.0, opened.expose()), (true, slack));
        let audit: String = sqlx::query_scalar("SELECT details::text FROM audit_log WHERE action = 'user.contact_update' ORDER BY id DESC LIMIT 1")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(audit.contains("hooks.slack.com") && !audit.contains("SECRETPART"), "{audit}");
        // A test with no email channel says so; clearing removes it all.
        put(json!({"email": "alice@example.com"})).await;
        let (_, body, _) = call(&pool, "POST", "/api/on-call/contact/test", Some(&cookie), None).await;
        assert_eq!(body["results"][0]["kind"], "email");
        assert!(body["results"][0]["error"].as_str().unwrap().contains("No email channel"));
        let (status, body, _) = put(json!({})).await;
        assert_eq!((status, body["email"].clone(), body["kind"].clone()), (StatusCode::OK, Value::Null, Value::Null));
        let (status, _, _) = call(&pool, "POST", "/api/on-call/contact/test", Some(&cookie), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
}

#[test]
fn the_rotation_is_read_in_the_schedules_own_zone() {
    // A schedule in New York hands off at 09:00 there, whatever the
    // server's zone: 14:00 UTC in winter, 13:00 in summer.
    let rota = on_call::Rota {
        time_zone: chrono_tz::America::New_York,
        rotation: on_call::Rotation::Daily,
        handoff: NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
        anchor: "2027-03-12".parse().unwrap(),
        anchor_position: 0,
        members: vec![None],
    };
    assert_eq!(
        rota.handoff(0),
        "2027-03-12T14:00:00Z".parse::<DateTime<Utc>>().unwrap()
    );
    assert_eq!(
        rota.handoff(2),
        "2027-03-14T13:00:00Z".parse::<DateTime<Utc>>().unwrap()
    );
}
