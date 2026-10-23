//! Remediation incident endpoints against a real PostgreSQL (ADR-0042):
//! customer and site scoping, filters, pages, events and the chain
//! report. Incidents are written through `remediation::audit`, as the
//! loop will. Ignored without `DATABASE_URL`.

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users;
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
use monitorflow_server::remediation::audit::{self, Actor, NewEvent, NewIncident};
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery";

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
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, body, cookie)
}

/// A user with one role on `customer`, `site` or globally, logged in.
async fn session(
    pool: &PgPool,
    username: &str,
    role: Role,
    customer: Option<Uuid>,
    site: Option<Uuid>,
) -> String {
    let id = users::create_user(pool, username, &Secret::new(PASSWORD), role, "test")
        .await
        .unwrap();
    sqlx::query("UPDATE user_roles SET customer_id = $2, site_id = $3 WHERE user_id = $1")
        .bind(id)
        .bind(customer)
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

async fn get(pool: &PgPool, cookie: &str, uri: &str) -> (StatusCode, Value) {
    let request = Request::get(uri)
        .header("cookie", cookie)
        .body(Body::empty())
        .unwrap();
    let (status, body, _) = send(pool, request).await;
    (status, body)
}

struct Tenant {
    customer: Uuid,
    sites: Vec<Uuid>,
    servers: Vec<Uuid>,
}

/// A customer with two sites of one server each.
async fn tenant(pool: &PgPool, name: &str) -> Tenant {
    let customer: Uuid =
        sqlx::query_scalar("INSERT INTO customers (name) VALUES ($1) RETURNING id")
            .bind(name)
            .fetch_one(pool)
            .await
            .unwrap();
    let mut tenant = Tenant {
        customer,
        sites: Vec::new(),
        servers: Vec::new(),
    };
    for i in 0..2 {
        let site: Uuid = sqlx::query_scalar(
            "INSERT INTO sites (name, customer_id) VALUES ($1, $2) RETURNING id",
        )
        .bind(format!("{name}-{i}"))
        .bind(customer)
        .fetch_one(pool)
        .await
        .unwrap();
        let server: Uuid = sqlx::query_scalar(
            "INSERT INTO servers (peer_id, site_id, hostname, os_family)
             VALUES ($1, $2, $1, 'debian') RETURNING id",
        )
        .bind(format!("agent-{name}-{i}"))
        .bind(site)
        .fetch_one(pool)
        .await
        .unwrap();
        tenant.sites.push(site);
        tenant.servers.push(server);
    }
    tenant
}

async fn open(pool: &PgPool, server: Uuid, instance: &str) -> Uuid {
    let incident = NewIncident {
        server_id: server,
        alert_incident_id: None,
        metric: "monitorflow_disk_used_ratio".into(),
        instance: instance.into(),
        max_commands: 5,
        max_duration: Duration::from_secs(600),
    };
    audit::open(pool, &incident).await.unwrap()
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
    async fn incidents_are_scoped_to_customers_and_sites(pool: PgPool) {
        let acme = tenant(&pool, "acme").await;
        let globex = tenant(&pool, "globex").await;
        let a0 = open(&pool, acme.servers[0], "/").await;
        let a1 = open(&pool, acme.servers[1], "/").await;
        let g0 = open(&pool, globex.servers[0], "/").await;

        let customer_viewer = session(&pool, "acme-viewer", Role::Viewer, Some(acme.customer), None).await;
        let (status, body) = get(&pool, &customer_viewer, "/api/remediation/incidents").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(ids(&body), [json!(a1), json!(a0)]);
        let item = &body["incidents"][1];
        assert_eq!(item["customer_name"], "acme");
        assert_eq!(item["site_name"], "acme-0");
        assert_eq!(item["hostname"], "agent-acme-0");
        assert_eq!(item["state"], "open");
        assert_eq!(item["closed_at"], Value::Null);
        assert_eq!((item["max_commands"].clone(), item["max_duration_s"].clone()), (json!(5), json!(600)));
        assert_eq!(item["event_count"], 0);

        let site_viewer = session(&pool, "site-viewer", Role::Viewer, None, Some(acme.sites[1])).await;
        let (_, body) = get(&pool, &site_viewer, "/api/remediation/incidents").await;
        assert_eq!(ids(&body), [json!(a1)]);
        // Filters outside the scope match nothing rather than fail.
        for uri in [
            format!("/api/remediation/incidents?customer_id={}", globex.customer),
            format!("/api/remediation/incidents?server_id={}", acme.servers[0]),
        ] {
            let (status, body) = get(&pool, &site_viewer, &uri).await;
            assert_eq!((status, ids(&body).len()), (StatusCode::OK, 0), "{uri}");
        }
        for uri in [
            format!("/api/remediation/incidents/{a0}"),
            format!("/api/remediation/incidents/{g0}"),
            format!("/api/remediation/incidents/{}", Uuid::new_v4()),
            "/api/remediation/incidents/x".into(),
        ] {
            let (status, body) = get(&pool, &site_viewer, &uri).await;
            assert_eq!((status, body), (StatusCode::NOT_FOUND, json!({ "error": "not found" })), "{uri}");
        }

        let admin = session(&pool, "admin", Role::Administrator, None, None).await;
        let (_, body) = get(&pool, &admin, "/api/remediation/incidents").await;
        assert_eq!(ids(&body), [json!(g0), json!(a1), json!(a0)]);
        let (_, body) = get(&pool, &admin, &format!("/api/remediation/incidents?customer_id={}", globex.customer)).await;
        assert_eq!(ids(&body), [json!(g0)]);

        // No role, no data; no session, 401; no write route.
        let nobody = session(&pool, "nobody", Role::Viewer, None, None).await;
        sqlx::query("DELETE FROM user_roles WHERE user_id = (SELECT id FROM users WHERE username = 'nobody')")
            .execute(&pool)
            .await
            .unwrap();
        let (status, _) = get(&pool, &nobody, "/api/remediation/incidents").await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _, _) = send(&pool, Request::get("/api/remediation/incidents").body(Body::empty()).unwrap()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let request = Request::post("/api/remediation/incidents")
            .header("cookie", &admin)
            .header(CSRF_HEADER, "1")
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();
        let (status, _, _) = send(&pool, request).await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    }

    #[sqlx::test]
    async fn filters_and_pages_go_newest_first(pool: PgPool) {
        let acme = tenant(&pool, "acme").await;
        let mut all = Vec::new();
        for i in 0..5 {
            let id = open(&pool, acme.servers[0], &format!("/m{i}")).await;
            if i % 2 == 1 {
                sqlx::query("UPDATE remediation_incidents SET state = 'running' WHERE id = $1")
                    .bind(id)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
            all.push(id);
        }
        let admin = session(&pool, "admin", Role::Administrator, None, None).await;
        let mut seen = Vec::new();
        let mut uri = "/api/remediation/incidents?limit=2".to_owned();
        loop {
            let (status, body) = get(&pool, &admin, &uri).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            seen.extend(ids(&body));
            match body["next_cursor"].as_str() {
                Some(cursor) => uri = format!("/api/remediation/incidents?limit=2&cursor={cursor}"),
                None => break,
            }
        }
        // Opened one after the other, so newest first is creation order reversed.
        let newest_first: Vec<Value> = all.iter().rev().map(|id| json!(id)).collect();
        assert_eq!(seen, newest_first);

        let (_, body) = get(&pool, &admin, "/api/remediation/incidents?state=running").await;
        let mut running = ids(&body);
        running.sort_by_key(|v| v.to_string());
        let mut want = vec![json!(all[1]), json!(all[3])];
        want.sort_by_key(|v| v.to_string());
        assert_eq!(running, want);
        for bad in ["state=closed", "limit=0", "limit=501", "cursor=!!", "sort=asc"] {
            let (status, _) = get(&pool, &admin, &format!("/api/remediation/incidents?{bad}")).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
        }
    }

    #[sqlx::test]
    async fn detail_returns_events_and_flags_a_broken_chain(pool: PgPool) {
        let acme = tenant(&pool, "acme").await;
        let id = open(&pool, acme.servers[0], "/").await;
        let command = Uuid::new_v4();
        let user = Uuid::new_v4();
        for event in [
            NewEvent::context_sent("claude/test", json!({ "metric": "disk", "logs": "[REDACTED]" })),
            NewEvent::ai_proposal("claude/test", "disk full", "journal is large", Some("journalctl --disk-usage")),
            NewEvent::classification("journalctl --disk-usage", "read_only", Some("journalctl")),
            NewEvent::approval(Actor::User(user), "journalctl --disk-usage"),
            NewEvent::command_queued(command, "journalctl --disk-usage"),
            NewEvent::command_result("agent-acme-0", command, Some(0), "4.0G", "", Duration::from_millis(12)),
        ] {
            audit::append(&pool, id, event).await.unwrap();
        }
        let viewer = session(&pool, "viewer", Role::Viewer, None, Some(acme.sites[0])).await;
        let uri = format!("/api/remediation/incidents/{id}");
        let (status, body) = get(&pool, &viewer, &uri).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["id"], json!(id));
        assert_eq!(body["event_count"], 6);
        let events = body["events"].as_array().unwrap();
        let kinds: Vec<_> = events.iter().map(|e| e["kind"].as_str().unwrap()).collect();
        assert_eq!(kinds, ["context_sent", "ai_proposal", "classification", "approval", "command_queued", "command_result"]);
        assert_eq!(events[0]["seq"], 1);
        assert_eq!(events[0]["prev_hash"], "0".repeat(64));
        assert_eq!(events[1]["prev_hash"], events[0]["hash"]);
        assert_eq!(events[1]["actor_kind"], "ai");
        assert_eq!(events[1]["actor_id"], "claude/test");
        assert_eq!(events[3]["actor_id"], json!(user));
        assert_eq!(events[5]["payload"]["stdout"], "4.0G");
        assert_eq!(events[5]["payload"]["exit_code"], 0);
        assert_eq!(body["chain"], json!({
            "valid": true, "events": 6, "broken_at": null, "reason": null,
            "head_hash": events[5]["hash"],
        }));

        // A superuser rewrites the approver behind the triggers' back.
        let mut conn = pool.acquire().await.unwrap();
        sqlx::query("SET session_replication_role = replica").execute(&mut *conn).await.unwrap();
        sqlx::query("UPDATE remediation_incident_events SET actor_id = 'someone-else' WHERE incident_id = $1 AND seq = 4")
            .bind(id)
            .execute(&mut *conn)
            .await
            .unwrap();
        sqlx::query("RESET session_replication_role").execute(&mut *conn).await.unwrap();
        drop(conn);
        let (status, body) = get(&pool, &viewer, &uri).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["chain"]["valid"], false);
        assert_eq!(body["chain"]["broken_at"], 4);
        assert_eq!(body["chain"]["reason"], "hash");
    }
}
