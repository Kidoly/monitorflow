//! Offline incidents (ADR-0057) against a real PostgreSQL: the rule opens
//! one incident per silent server or relay, none for servers behind a
//! down relay, resolves on reconnect and on mute, and the API lists them
//! and sets the mutes. Ignored without `DATABASE_URL`.

use std::collections::HashMap;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{DateTime, Duration, Utc};
use monitorflow_common::redact::Secret;
use monitorflow_common::tls::PeerRole;
use monitorflow_server::alerting::offline::{self, Action, OfflineSettings, Peer};
use monitorflow_server::alerting::AlertStore;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users;
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

/// Marks every test in the block as ignored when `DATABASE_URL` is unset.
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

const PASSWORD: &str = "correct horse battery";

struct Fleet {
    site: Uuid,
    relay: Uuid,
    /// Connects directly.
    direct: Uuid,
    /// Assigned to the relay.
    behind: Uuid,
}

/// One site, a relay, a direct server and one behind the relay, all last
/// seen an hour ago.
async fn fleet(pool: &PgPool) -> Fleet {
    let site: Uuid = sqlx::query_scalar("INSERT INTO sites (name) VALUES ('paris') RETURNING id")
        .fetch_one(pool)
        .await
        .unwrap();
    let relay: Uuid = sqlx::query_scalar(
        "INSERT INTO relays (peer_id, site_id, name, last_seen_at)
         VALUES ('relay-1', $1, 'relay-1', now() - interval '1 hour') RETURNING id",
    )
    .bind(site)
    .fetch_one(pool)
    .await
    .unwrap();
    let mut servers = Vec::new();
    for (peer, via) in [("direct", None), ("behind", Some(relay))] {
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO servers (peer_id, site_id, relay_id, hostname, os_family, last_seen_at)
             VALUES ($1, $2, $3, $1, 'debian', now() - interval '1 hour') RETURNING id",
        )
        .bind(peer)
        .bind(site)
        .bind(via)
        .fetch_one(pool)
        .await
        .unwrap();
        servers.push(id);
    }
    Fleet {
        site,
        relay,
        direct: servers[0],
        behind: servers[1],
    }
}

fn online(peers: &[(PeerRole, &str)]) -> HashMap<(PeerRole, String), DateTime<Utc>> {
    peers
        .iter()
        .map(|&(role, id)| ((role, id.to_owned()), Utc::now()))
        .collect()
}

/// A start long ago, so the server's own downtime does not hold anything.
fn started() -> DateTime<Utc> {
    Utc::now() - Duration::days(1)
}

async fn check(pool: &PgPool, up: &[(PeerRole, &str)]) -> Vec<(Peer, Action)> {
    offline::check(pool, &online(up), started(), &OfflineSettings::default())
        .await
        .unwrap()
}

async fn open_offline(pool: &PgPool) -> Vec<(Option<Uuid>, Option<Uuid>)> {
    sqlx::query_as(
        "SELECT server_id, relay_id FROM incidents
         WHERE metric = 'offline' AND state = 'open' ORDER BY relay_id NULLS FIRST",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn events(pool: &PgPool) -> Vec<(String, Value)> {
    sqlx::query_as(
        "SELECT e.kind, e.details FROM incident_events e
         JOIN incidents i ON i.id = e.incident_id ORDER BY e.id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn send(pool: &PgPool, request: Request<Body>) -> (StatusCode, Value, Option<String>) {
    let app = api::router(ApiState::new(
        pool.clone(),
        Registry::default(),
        ApiSettings::default(),
    ));
    let response = app.oneshot(request).await.unwrap();
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

async fn session(pool: &PgPool, username: &str, role: Role) -> String {
    users::create_user(pool, username, &Secret::new(PASSWORD), role, "test")
        .await
        .unwrap();
    let body = json!({ "username": username, "password": PASSWORD }).to_string();
    let request = Request::post("/api/auth/login")
        .header("content-type", "application/json")
        .header(CSRF_HEADER, "1")
        .body(Body::from(body))
        .unwrap();
    send(pool, request).await.2.unwrap()
}

async fn call(
    pool: &PgPool,
    cookie: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("cookie", cookie)
        .header(CSRF_HEADER, "1")
        .header("content-type", "application/json")
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let (status, body, _) = send(pool, request).await;
    (status, body)
}

db_tests! {
    #[sqlx::test]
    async fn opens_once_and_resolves_on_reconnect(pool: PgPool) {
        let fleet = fleet(&pool).await;
        // A start a minute ago holds everything: the server's own downtime.
        let held = offline::check(&pool, &online(&[]), Utc::now(), &OfflineSettings::default())
            .await
            .unwrap();
        assert!(held.is_empty());

        // The relay speaks for the server behind it.
        assert_eq!(check(&pool, &[]).await.len(), 2);
        assert_eq!(
            open_offline(&pool).await,
            [(Some(fleet.direct), None), (None, Some(fleet.relay))]
        );
        assert!(check(&pool, &[]).await.is_empty(), "no duplicate");

        // The relay is back but its agent is not: the agent's own incident.
        let up = [(PeerRole::Relay, "relay-1"), (PeerRole::Agent, "direct")];
        let changes = check(&pool, &up).await;
        assert_eq!(changes.len(), 3, "{changes:?}");
        assert_eq!(open_offline(&pool).await, [(Some(fleet.behind), None)]);
        let reasons: Vec<_> = events(&pool)
            .await
            .into_iter()
            .map(|(kind, details)| format!("{kind} {}", details["reason"].as_str().unwrap()))
            .collect();
        assert_eq!(reasons.iter().filter(|r| *r == "resolved back_online").count(), 2);
        assert_eq!(reasons.iter().filter(|r| *r == "opened no_heartbeat").count(), 3);

        // The threshold engine leaves them alone.
        let stored = AlertStore::new(pool.clone()).load_states().await.unwrap();
        assert!(stored.is_empty(), "{stored:?}");
    }

    #[sqlx::test]
    async fn muting_resolves_and_holds(pool: PgPool) {
        let fleet = fleet(&pool).await;
        check(&pool, &[]).await;
        let operator = session(&pool, "olive", Role::Operator).await;
        let viewer = session(&pool, "victor", Role::Viewer).await;

        // The API lists the relay's incident under its name.
        let (status, list) = call(&pool, &viewer, "GET", "/api/incidents?state=open", None).await;
        assert_eq!(status, StatusCode::OK);
        let relay = list["incidents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["relay_id"] == json!(fleet.relay))
            .unwrap();
        assert_eq!(
            (&relay["hostname"], &relay["server_id"], &relay["metric"], &relay["severity"]),
            (&json!("relay-1"), &Value::Null, &json!("offline"), &json!("critical"))
        );
        assert_eq!(relay["site_id"], json!(fleet.site));

        let mute = Some(json!({ "enabled": false }));
        let uri = format!("/api/sites/{}/offline-alerts", fleet.site);
        let (status, _) = call(&pool, &viewer, "PUT", &uri, mute.clone()).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, body) = call(&pool, &operator, "PUT", &uri, mute.clone()).await;
        assert_eq!((status, body), (StatusCode::OK, json!({ "enabled": false })));
        let uri = format!("/api/servers/{}/offline-alerts", fleet.direct);
        let (status, _) = call(&pool, &operator, "PUT", &uri, mute).await;
        assert_eq!(status, StatusCode::OK);
        let (_, server) = call(&pool, &viewer, "GET", &format!("/api/servers/{}", fleet.direct), None).await;
        assert_eq!((&server["offline_alerts"], &server["site_offline_alerts"]), (&json!(false), &json!(false)));

        assert_eq!(check(&pool, &[]).await.len(), 2);
        assert!(open_offline(&pool).await.is_empty());
        let audited: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM audit_log WHERE action LIKE '%.offline_alerts'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(audited, 2);

        // The site back on: the relay, not the muted server, opens again.
        let uri = format!("/api/sites/{}/offline-alerts", fleet.site);
        call(&pool, &operator, "PUT", &uri, Some(json!({ "enabled": true }))).await;
        check(&pool, &[]).await;
        assert_eq!(open_offline(&pool).await, [(None, Some(fleet.relay))]);

        // A revoked relay resolves, and its servers speak for themselves.
        sqlx::query("UPDATE relays SET revoked_at = now()")
            .execute(&pool)
            .await
            .unwrap();
        check(&pool, &[]).await;
        assert_eq!(open_offline(&pool).await, [(Some(fleet.behind), None)]);
    }
}
