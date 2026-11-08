//! Delivery of integration events (ADR-0102): signed requests to a
//! receiver on loopback, retries, order per subject, two instances,
//! disabling after a day of failures, private addresses, replay and
//! pruning. Ignored without `DATABASE_URL`.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::http::{HeaderMap, StatusCode};
use chrono::{TimeDelta, Utc};
use monitorflow_server::integrations::{self, Deliverer};
use monitorflow_server::notify::send::{signature, SIGNATURE_HEADER, TIMESTAMP_HEADER};
use serde_json::Value;
use sqlx::PgPool;
use tokio::net::TcpListener;
use uuid::Uuid;

const SECRET: &str = "mfwh_test";
const WEB: &str = "https://mf.example";

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

/// A customer with a site and a server: `(customer, site, server)`.
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
    (customer, site, server)
}

/// Opens an incident of `metric` on `server` with `kinds` as its events,
/// each `(kind, severity, reason)`.
async fn incident(
    pool: &PgPool,
    server: Uuid,
    metric: &str,
    kinds: &[(&str, Option<&str>, Option<&str>)],
) -> Uuid {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO incidents (server_id, metric, instance, severity, opened_at, last_value,
                                last_value_at, peak_value, threshold)
         VALUES ($1, $2, '', 'warning', now(), 91, now(), 91, '{}')
         RETURNING id",
    )
    .bind(server)
    .bind(metric)
    .fetch_one(pool)
    .await
    .unwrap();
    for (kind, severity, reason) in kinds {
        let details = match reason {
            Some(reason) => serde_json::json!({ "reason": reason }),
            None if *kind == "acknowledged" => {
                serde_json::json!({ "user_id": Uuid::nil(), "username": "alice" })
            }
            None => serde_json::json!({}),
        };
        sqlx::query(
            "INSERT INTO incident_events (incident_id, occurred_at, kind, severity, value, details)
             VALUES ($1, now(), $2, $3, 91, $4)",
        )
        .bind(id)
        .bind(kind)
        .bind(severity)
        .bind(details)
        .execute(pool)
        .await
        .unwrap();
    }
    id
}

/// An endpoint at `url` covering `customer` or `site` (or everything),
/// wanting `wanted` (or every event); private addresses allowed, as the
/// receivers are on loopback.
async fn endpoint(
    pool: &PgPool,
    url: &str,
    customer: Option<Uuid>,
    site: Option<Uuid>,
    wanted: Option<&[&str]>,
) -> Uuid {
    let wanted: Vec<String> = wanted
        .map(|w| w.iter().map(|e| e.to_string()).collect())
        .unwrap_or_else(|| integrations::EVENTS.iter().map(|e| e.to_string()).collect());
    sqlx::query_scalar(
        "INSERT INTO integration_endpoints (name, url, secret, events, customer_id, site_id,
                                            allow_private)
         VALUES ('psa', $1, $2, $3, $4, $5, true) RETURNING id",
    )
    .bind(url)
    .bind(SECRET)
    .bind(&wanted)
    .bind(customer)
    .bind(site)
    .fetch_one(pool)
    .await
    .unwrap()
}

type Seen = Arc<Mutex<Vec<(HeaderMap, Bytes)>>>;

/// A receiver on loopback that records each request and answers the
/// status in the returned cell.
async fn receiver() -> (String, Seen, Arc<AtomicU16>) {
    let seen: Seen = Arc::default();
    let status = Arc::new(AtomicU16::new(204));
    let (store, answer) = (seen.clone(), status.clone());
    let app = axum::Router::new().route(
        "/hook",
        axum::routing::post(move |headers: HeaderMap, body: Bytes| {
            let (store, answer) = (store.clone(), answer.clone());
            async move {
                store.lock().unwrap().push((headers, body));
                StatusCode::from_u16(answer.load(Ordering::SeqCst)).unwrap()
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/hook", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, seen, status)
}

fn bodies(seen: &Seen) -> Vec<Value> {
    seen.lock()
        .unwrap()
        .iter()
        .map(|(_, body)| serde_json::from_slice(body).unwrap())
        .collect()
}

/// Makes every pending delivery due now, as if its wait were over.
async fn due_now(pool: &PgPool) {
    sqlx::query("UPDATE integration_deliveries SET next_at = now() WHERE state = 'pending'")
        .execute(pool)
        .await
        .unwrap();
}

async fn deliveries(pool: &PgPool) -> Vec<(String, i32, Option<i32>, Option<String>)> {
    sqlx::query_as(
        "SELECT state, attempts, last_status, last_error FROM integration_deliveries ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

db_tests! {
    #[sqlx::test]
    async fn deliveries_are_signed_and_retried_until_they_succeed(pool: PgPool) {
        let (_, _, server) = place(&pool, "acme").await;
        let (url, seen, status) = receiver().await;
        let id = endpoint(&pool, &url, None, None, None).await;
        incident(&pool, server, "monitorflow_cpu_usage_percent", &[("opened", Some("warning"), None)]).await;
        let deliverer = Deliverer::new(pool.clone(), WEB);

        status.store(500, Ordering::SeqCst);
        assert_eq!(deliverer.pass().await.unwrap(), 1);
        let [(state, attempts, code, error)] = deliveries(&pool).await.try_into().unwrap();
        assert_eq!((state.as_str(), attempts, code), ("pending", 1, Some(500)));
        assert!(error.unwrap().contains("500"));
        // The next try waits.
        assert_eq!(deliverer.pass().await.unwrap(), 0);
        let failing: Option<chrono::DateTime<Utc>> =
            sqlx::query_scalar("SELECT failing_since FROM integration_endpoints WHERE id = $1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(failing.is_some());

        status.store(200, Ordering::SeqCst);
        due_now(&pool).await;
        assert_eq!(deliverer.pass().await.unwrap(), 1);
        let [(state, attempts, code, error)] = deliveries(&pool).await.try_into().unwrap();
        assert_eq!((state.as_str(), attempts, code, error), ("delivered", 2, Some(200), None));
        let failing: Option<chrono::DateTime<Utc>> =
            sqlx::query_scalar("SELECT failing_since FROM integration_endpoints WHERE id = $1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(failing, None);

        // Both tries carried the same event ID, each signed over its own
        // timestamp.
        let requests = seen.lock().unwrap().clone();
        assert_eq!(requests.len(), 2);
        let ids: HashSet<&str> = requests
            .iter()
            .map(|(h, _)| h["x-monitorflow-delivery"].to_str().unwrap())
            .collect();
        assert_eq!(ids.len(), 1);
        for (headers, body) in &requests {
            let timestamp: i64 = headers[TIMESTAMP_HEADER].to_str().unwrap().parse().unwrap();
            assert_eq!(
                headers[SIGNATURE_HEADER].to_str().unwrap(),
                signature(SECRET, timestamp, body)
            );
            assert_eq!(headers["x-monitorflow-event"], "incident.opened");
            assert!(headers.get(integrations::REPLAY_HEADER).is_none());
        }
        let body = &bodies(&seen)[1];
        assert_eq!(body["version"], 1);
        assert_eq!(body["id"], *ids.iter().next().unwrap());
        assert_eq!(body["server"]["name"], "acme-web");
        assert!(body["url"].as_str().unwrap().starts_with("https://mf.example/incidents/view/?id="));
    }

    #[sqlx::test]
    async fn a_subject_waits_for_its_earlier_events_but_others_do_not(pool: PgPool) {
        let (_, _, server) = place(&pool, "acme").await;
        let (url, seen, status) = receiver().await;
        endpoint(&pool, &url, None, None, Some(&["incident.opened", "incident.resolved"])).await;
        incident(&pool, server, "monitorflow_cpu_usage_percent",
                 &[("opened", Some("warning"), None), ("resolved", None, None)]).await;
        incident(&pool, server, "monitorflow_memory_used_percent", &[("opened", Some("warning"), None)]).await;
        let deliverer = Deliverer::new(pool.clone(), WEB);

        status.store(503, Ordering::SeqCst);
        // The first incident's opening and the second incident go; the
        // first's resolution waits behind its opening.
        assert_eq!(deliverer.pass().await.unwrap(), 2);
        due_now(&pool).await;
        status.store(204, Ordering::SeqCst);
        assert_eq!(deliverer.pass().await.unwrap(), 2);
        assert_eq!(deliverer.pass().await.unwrap(), 1);
        let sent: Vec<(String, String)> = bodies(&seen)
            .iter()
            .skip(2)
            .map(|b| (b["event"].as_str().unwrap().to_owned(), b["incident"]["metric"].as_str().unwrap().to_owned()))
            .collect();
        let resolved = sent.iter().position(|(e, _)| e == "incident.resolved").unwrap();
        let opened = sent
            .iter()
            .position(|(e, m)| e == "incident.opened" && m == "monitorflow_cpu_usage_percent")
            .unwrap();
        assert!(opened < resolved, "{sent:?}");
    }

    #[sqlx::test]
    async fn two_instances_never_send_one_delivery_at_once(pool: PgPool) {
        let (_, _, server) = place(&pool, "acme").await;
        let (url, seen, _) = receiver().await;
        endpoint(&pool, &url, None, None, None).await;
        for n in 0..30 {
            incident(&pool, server, &format!("monitorflow_m{n}"), &[("opened", Some("warning"), None)]).await;
        }
        let a = Deliverer::new(pool.clone(), WEB);
        let b = Deliverer::new(pool.clone(), WEB);
        loop {
            let (x, y) = tokio::join!(a.pass(), b.pass());
            if x.unwrap() + y.unwrap() == 0 {
                break;
            }
        }
        let ids: Vec<String> = bodies(&seen).iter().map(|b| b["id"].as_str().unwrap().to_owned()).collect();
        assert_eq!(ids.len(), 30);
        assert_eq!(ids.iter().collect::<HashSet<_>>().len(), 30);
    }

    #[sqlx::test]
    async fn a_claim_left_by_a_dead_instance_is_sent_after_its_lease(pool: PgPool) {
        let (_, _, server) = place(&pool, "acme").await;
        let (url, seen, _) = receiver().await;
        endpoint(&pool, &url, None, None, None).await;
        incident(&pool, server, "monitorflow_cpu_usage_percent", &[("opened", Some("warning"), None)]).await;
        // What a claim leaves when its instance dies before sending.
        sqlx::query("UPDATE integration_deliveries SET attempts = 1, next_at = now() + interval '60 seconds'")
            .execute(&pool)
            .await
            .unwrap();
        let deliverer = Deliverer::new(pool.clone(), WEB);
        assert_eq!(deliverer.pass().await.unwrap(), 0);
        due_now(&pool).await;
        assert_eq!(deliverer.pass().await.unwrap(), 1);
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert_eq!(deliveries(&pool).await[0].1, 2);
    }

    #[sqlx::test]
    async fn a_day_of_failures_gives_up_and_disables_the_endpoint(pool: PgPool) {
        let (customer, _, server) = place(&pool, "acme").await;
        let (url, _, status) = receiver().await;
        let id = endpoint(&pool, &url, Some(customer), None, None).await;
        incident(&pool, server, "monitorflow_cpu_usage_percent", &[("opened", Some("warning"), None)]).await;
        status.store(500, Ordering::SeqCst);
        sqlx::raw_sql(
            "UPDATE integration_deliveries SET created_at = now() - interval '25 hours';
             UPDATE integration_endpoints SET failing_since = now() - interval '25 hours'",
        )
        .execute(&pool)
        .await
        .unwrap();
        let deliverer = Deliverer::new(pool.clone(), WEB);
        assert_eq!(deliverer.pass().await.unwrap(), 1);
        assert_eq!(deliveries(&pool).await[0].0, "failed");
        let state: String = sqlx::query_scalar("SELECT state FROM integration_endpoints WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(state, "disabled");
        let notices: Vec<(Option<Uuid>, Value)> = sqlx::query_as(
            "SELECT customer_id, details FROM notification_outbox WHERE event = 'integration.disabled'",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        let [(owner, details)] = notices.try_into().unwrap();
        assert_eq!(owner, Some(customer));
        assert_eq!(details["integration"]["host"], "127.0.0.1");
        assert!(details["integration"]["error"].as_str().unwrap().contains("500"));
        // A disabled endpoint gets nothing new.
        incident(&pool, server, "monitorflow_memory_used_percent", &[("opened", Some("warning"), None)]).await;
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM integration_deliveries")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    #[sqlx::test]
    async fn private_addresses_need_to_be_allowed(pool: PgPool) {
        let (_, _, server) = place(&pool, "acme").await;
        let (url, seen, _) = receiver().await;
        let id = endpoint(&pool, &url, None, None, None).await;
        sqlx::query("UPDATE integration_endpoints SET allow_private = false WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        incident(&pool, server, "monitorflow_cpu_usage_percent", &[("opened", Some("warning"), None)]).await;
        let deliverer = Deliverer::new(pool.clone(), WEB);
        deliverer.pass().await.unwrap();
        let [(state, _, code, error)] = deliveries(&pool).await.try_into().unwrap();
        assert_eq!((state.as_str(), code), ("pending", None));
        assert!(error.unwrap().contains("127.0.0.1 is a private address"));
        assert!(seen.lock().unwrap().is_empty());
    }

    #[sqlx::test]
    async fn events_are_replayed_with_their_id_and_old_ones_pruned(pool: PgPool) {
        let (_, _, server) = place(&pool, "acme").await;
        let (url, seen, _) = receiver().await;
        let id = endpoint(&pool, &url, None, None, Some(&["incident.opened"])).await;
        incident(&pool, server, "monitorflow_cpu_usage_percent", &[("opened", Some("warning"), None)]).await;
        let deliverer = Deliverer::new(pool.clone(), WEB);
        deliverer.pass().await.unwrap();
        let event: Uuid = sqlx::query_scalar("SELECT id FROM integration_events WHERE event = 'incident.opened'")
            .fetch_one(&pool)
            .await
            .unwrap();

        assert!(integrations::replay_event(&pool, id, event).await.unwrap());
        // Not an event this endpoint wants.
        let added: Uuid = sqlx::query_scalar("SELECT id FROM integration_events WHERE event = 'server.added'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(!integrations::replay_event(&pool, id, added).await.unwrap());
        let now = Utc::now();
        let queued = integrations::replay_range(&pool, id, now - TimeDelta::hours(1), now + TimeDelta::minutes(1))
            .await
            .unwrap();
        assert_eq!(queued, 1);
        while deliverer.pass().await.unwrap() > 0 {}
        let requests = seen.lock().unwrap().clone();
        assert_eq!(requests.len(), 3);
        for (headers, _) in &requests[1..] {
            assert_eq!(headers[integrations::REPLAY_HEADER], "1");
            assert_eq!(headers["x-monitorflow-delivery"].to_str().unwrap(), event.to_string());
        }

        sqlx::query("UPDATE integration_events SET created_at = now() - interval '31 days'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(deliverer.prune().await.unwrap(), 2);
        let left: i64 = sqlx::query_scalar("SELECT count(*) FROM integration_deliveries")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(left, 0);
    }
}
