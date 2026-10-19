//! Incident and site endpoints against a real PostgreSQL: site scoping,
//! filters, keyset pagination and event timelines. Incidents are written
//! through the engine's store, as in production. Ignored without
//! `DATABASE_URL`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use monitorflow_common::redact::Secret;
use monitorflow_server::alerting::store::{IncidentStep, Step};
use monitorflow_server::alerting::{builtin_defaults, AlertStore, SeriesKey, Severity, Transition};
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users;
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery";
const DISK: &str = "monitorflow_disk_used_ratio";
const S: i64 = 1000;
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

fn app(pool: &PgPool) -> Router {
    api::router(ApiState::new(
        pool.clone(),
        Registry::default(),
        ApiSettings::default(),
    ))
}

async fn send(pool: &PgPool, request: Request<Body>) -> (StatusCode, Value, Option<String>) {
    let response = app(pool).oneshot(request).await.unwrap();
    let status = response.status();
    let cookie = response
        .headers()
        .get("set-cookie")
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned());
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, body, cookie)
}

/// A user with one role, on `site` or globally, logged in.
async fn session(pool: &PgPool, username: &str, role: Role, site: Option<Uuid>) -> String {
    let id = users::create_user(pool, username, &Secret::new(PASSWORD), role, "test")
        .await
        .unwrap();
    sqlx::query("UPDATE user_roles SET site_id = $2 WHERE user_id = $1")
        .bind(id)
        .bind(site)
        .execute(pool)
        .await
        .unwrap();
    let body = json!({ "username": username, "password": PASSWORD }).to_string();
    let request = Request::post("/api/auth/login")
        .header("content-type", "application/json")
        .header(CSRF_HEADER, "1")
        .body(Body::from(body))
        .unwrap();
    let (status, _, cookie) = send(pool, request).await;
    assert_eq!(status, StatusCode::OK);
    cookie.unwrap()
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
        .header("content-type", "application/json");
    let request = request
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let (status, body, _) = send(pool, request).await;
    (status, body)
}

struct Fleet {
    paris: Uuid,
    lyon: Uuid,
    paris_server: Uuid,
    lyon_server: Uuid,
}

async fn fleet(pool: &PgPool) -> Fleet {
    let mut ids = Vec::new();
    for name in ["paris", "lyon"] {
        let site: Uuid = sqlx::query_scalar("INSERT INTO sites (name) VALUES ($1) RETURNING id")
            .bind(name)
            .fetch_one(pool)
            .await
            .unwrap();
        let server: Uuid = sqlx::query_scalar(
            "INSERT INTO servers (peer_id, site_id, hostname, os_family)
             VALUES ($1, $2, $1, 'debian') RETURNING id",
        )
        .bind(format!("agent-{name}"))
        .bind(site)
        .fetch_one(pool)
        .await
        .unwrap();
        ids.push((site, server));
    }
    Fleet {
        paris: ids[0].0,
        paris_server: ids[0].1,
        lyon: ids[1].0,
        lyon_server: ids[1].1,
    }
}

/// A fixed time in the past, so expected timestamps are easy to write.
const T0: i64 = 1_790_000_000_000;

fn rfc3339(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Writes incidents the way the engine does.
struct Engine {
    store: AlertStore,
}

impl Engine {
    fn key(server: Uuid, instance: &str) -> SeriesKey {
        SeriesKey {
            server_id: server,
            metric: DISK.into(),
            instance: instance.into(),
        }
    }

    async fn apply(
        &self,
        key: &SeriesKey,
        at: i64,
        value: f64,
        incident: IncidentStep<'_>,
        transition: Transition,
    ) {
        let step = Step {
            key,
            at,
            value: Some(value),
            incident,
            transition: Some(transition),
            reason: None,
            state: None,
        };
        self.store.apply(&step).await.unwrap();
    }

    async fn open(&self, server: Uuid, instance: &str, at: i64, severity: Severity) -> Uuid {
        let rule = builtin_defaults()
            .into_iter()
            .find(|r| r.metric == DISK)
            .unwrap();
        let key = Self::key(server, instance);
        let step = Step {
            key: &key,
            at,
            value: Some(0.9),
            incident: IncidentStep::Open {
                severity,
                rule: &rule,
            },
            transition: Some(Transition::Opened(severity)),
            reason: None,
            state: None,
        };
        self.store.apply(&step).await.unwrap().unwrap().0
    }

    async fn resolve(&self, server: Uuid, instance: &str, id: Uuid, at: i64, from: Severity) {
        let key = Self::key(server, instance);
        self.apply(
            &key,
            at,
            0.5,
            IncidentStep::Resolve { id },
            Transition::Resolved { from },
        )
        .await;
    }
}

fn ids(body: &Value) -> Vec<Value> {
    body["incidents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].clone())
        .collect()
}

db_tests! {
    #[sqlx::test]
    async fn viewers_see_their_sites_only(pool: PgPool) {
        let f = fleet(&pool).await;
        let engine = Engine { store: AlertStore::new(pool.clone()) };
        let paris = engine.open(f.paris_server, "/", T0, Severity::Warning).await;
        let lyon = engine.open(f.lyon_server, "/", T0, Severity::Critical).await;
        // A second Paris server with no incident.
        sqlx::query("INSERT INTO servers (peer_id, site_id, hostname, os_family) VALUES ('agent-paris-2', $1, 'web-2', 'debian')")
            .bind(f.paris)
            .execute(&pool)
            .await
            .unwrap();
        let viewer = session(&pool, "paris-viewer", Role::Viewer, Some(f.paris)).await;

        let (status, body) = call(&pool, &viewer, "GET", "/api/incidents", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(ids(&body), [json!(paris)]);
        assert_eq!(body["next_cursor"], Value::Null);
        let incident = &body["incidents"][0];
        assert_eq!(incident["site_name"], "paris");
        assert_eq!(incident["hostname"], "agent-paris");
        assert_eq!(incident["threshold"]["critical"], 0.95);
        for uri in [
            format!("/api/incidents?site_id={}", f.lyon),
            format!("/api/incidents?server_id={}", f.lyon_server),
        ] {
            let (status, body) = call(&pool, &viewer, "GET", &uri, None).await;
            assert_eq!((status, ids(&body).len()), (StatusCode::OK, 0), "{uri}");
        }
        for uri in [format!("/api/incidents/{lyon}"), format!("/api/incidents/{}", Uuid::new_v4()), "/api/incidents/x".into()] {
            let (status, body) = call(&pool, &viewer, "GET", &uri, None).await;
            assert_eq!((status, body), (StatusCode::NOT_FOUND, json!({ "error": "not found" })), "{uri}");
        }
        let (status, _) = call(&pool, &viewer, "GET", &format!("/api/incidents/{paris}"), None).await;
        assert_eq!(status, StatusCode::OK);

        let (status, body) = call(&pool, &viewer, "GET", "/api/sites", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({ "sites": [{ "id": f.paris, "name": "paris", "server_count": 2,
            "customer_id": "00000000-0000-0000-0000-000000000001", "customer_name": "default" }] }));
        let admin = session(&pool, "admin", Role::Administrator, None).await;
        let (_, body) = call(&pool, &admin, "GET", "/api/sites", None).await;
        let names: Vec<_> = body["sites"].as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["lyon", "paris"]);
        let (_, body) = call(&pool, &admin, "GET", "/api/incidents", None).await;
        assert_eq!(ids(&body).len(), 2);

        // The fleet overview counts open incidents per server.
        let (_, body) = call(&pool, &viewer, "GET", "/api/servers", None).await;
        let counts: Vec<_> = body["servers"].as_array().unwrap().iter()
            .map(|s| (s["hostname"].as_str().unwrap(), s["open_incidents"].as_i64().unwrap()))
            .collect();
        assert_eq!(counts, [("agent-paris", 1), ("web-2", 0)]);
        engine.resolve(f.paris_server, "/", paris, T0 + 60 * S, Severity::Warning).await;
        let (_, body) = call(&pool, &viewer, "GET", "/api/servers", None).await;
        assert_eq!(body["servers"][0]["open_incidents"], 0);
    }

    #[sqlx::test]
    async fn filters_and_pages_put_open_incidents_first(pool: PgPool) {
        let f = fleet(&pool).await;
        let engine = Engine { store: AlertStore::new(pool.clone()) };
        let server = f.paris_server;
        // Opened at T0 + i minutes; even ones resolved 30 s later.
        let mut all = Vec::new();
        for i in 0..6 {
            let instance = format!("/m{i}");
            let severity = if i % 3 == 0 { Severity::Critical } else { Severity::Warning };
            let at = T0 + i * 60 * S;
            let id = engine.open(server, &instance, at, severity).await;
            if i % 2 == 0 {
                engine.resolve(server, &instance, id, at + 30 * S, severity).await;
            }
            all.push(id);
        }
        engine.open(f.lyon_server, "/", T0, Severity::Warning).await;
        let operator = session(&pool, "op", Role::Operator, Some(f.paris)).await;
        let get = |query: String| {
            let (pool, operator) = (pool.clone(), operator.clone());
            async move {
                let (status, body) = call(&pool, &operator, "GET", &format!("/api/incidents{query}"), None).await;
                assert_eq!(status, StatusCode::OK, "{query}: {body}");
                body
            }
        };
        let id = |i: usize| json!(all[i]);
        // Open (5, 3, 1) newest first, then resolved (4, 2, 0).
        assert_eq!(ids(&get(String::new()).await), [id(5), id(3), id(1), id(4), id(2), id(0)]);
        assert_eq!(ids(&get("?state=resolved".into()).await), [id(4), id(2), id(0)]);
        assert_eq!(ids(&get("?state=open&severity=warning".into()).await), [id(5), id(1)]);
        assert_eq!(ids(&get("?severity=critical".into()).await), [id(3), id(0)]);
        assert_eq!(ids(&get(format!("?server_id={server}&site_id={}", f.paris)).await).len(), 6);
        // Active during [T0 + 2 min, T0 + 3 min]: opened by its end and
        // not resolved before its start.
        let range = format!("?from={}&to={}", rfc3339(T0 + 120 * S), rfc3339(T0 + 180 * S));
        assert_eq!(ids(&get(range).await), [id(3), id(1), id(2)]);

        // Pages of 2 walk the same order without repeats.
        let mut walked = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let query = match &cursor { Some(c) => format!("?limit=2&cursor={c}"), None => "?limit=2".into() };
            let body = get(query).await;
            assert!(ids(&body).len() <= 2);
            walked.extend(ids(&body));
            match body["next_cursor"].as_str() { Some(c) => cursor = Some(c.to_owned()), None => break }
        }
        assert_eq!(walked, ids(&get(String::new()).await));
        let body = get("?state=open&limit=3".into()).await;
        assert_eq!(body["next_cursor"], Value::Null);

        for (query, expected) in [
            ("?limit=0", "limit must be between 1 and 500"),
            ("?limit=501", "limit must be between 1 and 500"),
            ("?cursor=nope", "invalid cursor"),
            ("?state=closed", "state"),
            ("?server_id=12", "server_id"),
            ("?from=yesterday", "from"),
            ("?order=asc", "order"),
        ] {
            let (status, body) = call(&pool, &operator, "GET", &format!("/api/incidents{query}"), None).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{query}");
            assert!(body["error"].as_str().unwrap().contains(expected), "{query}: {body}");
        }
        let query = format!("/api/incidents?from={}&to={}", rfc3339(T0 + S), rfc3339(T0));
        let (status, body) = call(&pool, &operator, "GET", &query, None).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::BAD_REQUEST, Some("from must not be after to")));
    }

    #[sqlx::test]
    async fn details_carry_the_timeline_in_order(pool: PgPool) {
        let f = fleet(&pool).await;
        let engine = Engine { store: AlertStore::new(pool.clone()) };
        let server = f.paris_server;
        let key = Engine::key(server, "/");
        let id = engine.open(server, "/", T0, Severity::Warning).await;
        let (w, c) = (Severity::Warning, Severity::Critical);
        engine.apply(&key, T0 + 60 * S, 0.97, IncidentStep::Update { id, severity: c, peak: 0.97 }, Transition::Escalated { from: w, to: c }).await;
        engine.apply(&key, T0 + 120 * S, 0.9, IncidentStep::Update { id, severity: w, peak: 0.97 }, Transition::Deescalated { from: c, to: w }).await;
        engine.resolve(server, "/", id, T0 + 180 * S, w).await;
        let viewer = session(&pool, "viewer", Role::Viewer, Some(f.paris)).await;

        let (status, body) = call(&pool, &viewer, "GET", &format!("/api/incidents/{id}"), None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["id"], json!(id));
        assert_eq!(body["state"], "resolved");
        assert_eq!(body["severity"], "warning");
        assert_eq!(body["peak_value"], 0.97);
        assert_eq!(body["opened_at"], rfc3339(T0));
        assert_eq!(body["resolved_at"], rfc3339(T0 + 180 * S));
        let events: Vec<_> = body["events"].as_array().unwrap().iter()
            .map(|e| (e["kind"].as_str().unwrap(), e["severity"].clone(), e["value"].clone(), e["occurred_at"].as_str().unwrap().to_owned()))
            .collect();
        assert_eq!(
            events,
            [
                ("opened", json!("warning"), json!(0.9), rfc3339(T0)),
                ("escalated", json!("critical"), json!(0.97), rfc3339(T0 + 60 * S)),
                ("deescalated", json!("warning"), json!(0.9), rfc3339(T0 + 120 * S)),
                ("resolved", Value::Null, json!(0.5), rfc3339(T0 + 180 * S)),
            ]
        );
        assert_eq!(body["events"][0]["details"], json!({}));
    }
}
