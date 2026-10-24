//! The approval queue against a real
//! PostgreSQL (ADR-0045): listing, approving, editing, rejecting, double
//! decisions, deadlines, roles and scoping. Parked incidents are written
//! the way the loop writes them. Ignored without `DATABASE_URL`.

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users;
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
use monitorflow_server::remediation::audit::{self, IncidentState, NewEvent, NewIncident};
use monitorflow_server::remediation::risk::{classify, RuleSet};
use monitorflow_server::remediation::runner::store;
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

async fn user_id(pool: &PgPool, username: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM users WHERE username = $1")
        .bind(username)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn get(pool: &PgPool, cookie: &str, uri: &str) -> (StatusCode, Value) {
    let request = Request::get(uri)
        .header("cookie", cookie)
        .body(Body::empty())
        .unwrap();
    let (status, body, _) = send(pool, request).await;
    (status, body)
}

async fn post(pool: &PgPool, cookie: &str, uri: &str, body: Value) -> (StatusCode, Value) {
    let request = Request::post(uri)
        .header("cookie", cookie)
        .header(CSRF_HEADER, "1")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
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

/// An incident on `server` parked on `command`, as the loop parks it.
/// Returns the incident and the pending command.
async fn park(pool: &PgPool, server: Uuid, command: &str, max_duration: Duration) -> (Uuid, Uuid) {
    let incident = audit::open(
        pool,
        &NewIncident {
            server_id: server,
            alert_incident_id: None,
            metric: "monitorflow_disk_used_ratio".into(),
            instance: "/var".into(),
            max_commands: 5,
            max_duration,
        },
    )
    .await
    .unwrap();
    let mut tx = pool.begin().await.unwrap();
    store::transition(
        &mut tx,
        incident,
        IncidentState::Open,
        IncidentState::Running,
    )
    .await
    .unwrap();
    audit::append_in(
        &mut tx,
        incident,
        NewEvent::context_sent(
            "mock/m",
            json!({ "rendered": "BIG CONTEXT", "redactions": {} }),
        ),
    )
    .await
    .unwrap();
    let proposal = audit::append_in(
        &mut tx,
        incident,
        NewEvent::ai_proposal("mock/m", "journal too big", "vacuum it", Some(command)),
    )
    .await
    .unwrap();
    let decision = classify(command, &RuleSet::default());
    audit::append_in(
        &mut tx,
        incident,
        NewEvent::classification(command, &decision),
    )
    .await
    .unwrap();
    store::insert_pending(
        &mut tx,
        incident,
        proposal.seq,
        "journal too big",
        command,
        "vacuum it",
        &decision,
    )
    .await
    .unwrap();
    store::transition(
        &mut tx,
        incident,
        IncidentState::Running,
        IncidentState::AwaitingApproval,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let pending =
        sqlx::query_scalar("SELECT id FROM remediation_pending_commands WHERE incident_id = $1")
            .bind(incident)
            .fetch_one(pool)
            .await
            .unwrap();
    (incident, pending)
}

const LOW: &str = "systemctl restart nginx";
const HIGH: &str = "apt-get autoremove -y";
const HOUR: Duration = Duration::from_secs(3600);

fn approve_uri(pending: Uuid) -> String {
    format!("/api/remediation/approvals/{pending}/approve")
}

fn reject_uri(pending: Uuid) -> String {
    format!("/api/remediation/approvals/{pending}/reject")
}

async fn kinds(pool: &PgPool, incident: Uuid) -> Vec<String> {
    audit::events(pool, incident)
        .await
        .unwrap()
        .iter()
        .map(|e| {
            serde_json::to_value(e.kind)
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}

async fn states(pool: &PgPool, incident: Uuid) -> (String, String) {
    sqlx::query_as(
        "SELECT i.state, p.state FROM remediation_incidents i
         JOIN remediation_pending_commands p ON p.incident_id = i.id WHERE i.id = $1",
    )
    .bind(incident)
    .fetch_one(pool)
    .await
    .unwrap()
}

db_tests! {
    #[sqlx::test]
    async fn the_queue_lists_what_each_user_may_see_and_decide(pool: PgPool) {
        let acme = tenant(&pool, "acme").await;
        let globex = tenant(&pool, "globex").await;
        let (low_incident, low) = park(&pool, acme.servers[0], LOW, HOUR).await;
        let (_, high) = park(&pool, acme.servers[1], HIGH, Duration::from_secs(600)).await;
        let (_, other) = park(&pool, globex.servers[0], LOW, HOUR).await;

        let approver = session(&pool, "approver", Role::Approver, Some(acme.customer), None).await;
        let (status, body) = get(&pool, &approver, "/api/remediation/approvals").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let items = body["approvals"].as_array().unwrap();
        // Nearest deadline first.
        let ids: Vec<&Value> = items.iter().map(|i| &i["id"]).collect();
        assert_eq!(ids, [&json!(high), &json!(low)]);
        let item = &items[1];
        assert_eq!(item["incident_id"], json!(low_incident));
        assert_eq!((item["hostname"].as_str(), item["site_name"].as_str()), (Some("agent-acme-0"), Some("acme-0")));
        assert_eq!(item["customer_name"], "acme");
        assert_eq!((item["metric"].as_str(), item["instance"].as_str()), (Some("monitorflow_disk_used_ratio"), Some("/var")));
        assert_eq!(item["command"], LOW);
        assert_eq!((item["diagnosis"].as_str(), item["reasoning"].as_str()), (Some("journal too big"), Some("vacuum it")));
        assert_eq!(item["tier"], "low_impact");
        assert_eq!(item["rule"]["source"], "builtin");
        assert!(item["reason"].as_str().is_some_and(|r| !r.is_empty()));
        assert_eq!((item["commands_run"].clone(), item["max_commands"].clone()), (json!(0), json!(5)));
        let opened: chrono::DateTime<chrono::Utc> = serde_json::from_value(item["opened_at"].clone()).unwrap();
        let deadline: chrono::DateTime<chrono::Utc> = serde_json::from_value(item["deadline"].clone()).unwrap();
        assert_eq!((deadline - opened).num_seconds(), 3600);
        assert_eq!((item["can_approve"].clone(), item["can_reject"].clone()), (json!(true), json!(true)));
        assert_eq!(item["snapshots"], json!({ "available": false, "reason": "disabled" }));
        // High impact: an approver may only reject.
        assert_eq!((items[0]["can_approve"].clone(), items[0]["can_reject"].clone()), (json!(false), json!(true)));

        let viewer = session(&pool, "viewer", Role::Viewer, None, Some(acme.sites[0])).await;
        let (_, body) = get(&pool, &viewer, "/api/remediation/approvals").await;
        let items = body["approvals"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!((items[0]["can_approve"].clone(), items[0]["can_reject"].clone()), (json!(false), json!(false)));

        let admin = session(&pool, "admin", Role::Administrator, None, None).await;
        let (_, body) = get(&pool, &admin, &format!("/api/remediation/approvals?customer_id={}", globex.customer)).await;
        assert_eq!(body["approvals"][0]["id"], json!(other));
        assert_eq!(body["approvals"].as_array().unwrap().len(), 1);
        let (_, body) = get(&pool, &admin, &format!("/api/remediation/approvals?server_id={}", acme.servers[1])).await;
        assert_eq!(body["approvals"][0]["can_approve"], true);
        let (status, _) = get(&pool, &admin, "/api/remediation/approvals?state=pending").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[sqlx::test]
    async fn an_approval_resumes_the_incident_once(pool: PgPool) {
        let acme = tenant(&pool, "acme").await;
        let (incident, pending) = park(&pool, acme.servers[0], LOW, HOUR).await;
        let approver = session(&pool, "approver", Role::Approver, None, Some(acme.sites[0])).await;
        let (status, body) = post(&pool, &approver, &approve_uri(pending), json!({})).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({
            "id": pending, "incident_id": incident, "state": "approved", "command": LOW, "edited": false,
        }));
        let events = audit::events(&pool, incident).await.unwrap();
        let tail: Vec<_> = events.iter().rev().take(2).collect();
        let me = user_id(&pool, "approver").await;
        assert_eq!(serde_json::to_value(tail[1].kind).unwrap(), "approval");
        assert_eq!(tail[1].payload, json!({ "command": LOW }));
        assert_eq!((tail[1].actor_kind.as_str(), tail[1].actor_id.clone()), ("user", me.to_string()));
        assert_eq!(tail[0].payload, json!({ "from": "awaiting_approval", "to": "running" }));
        assert_eq!(tail[0].actor_id, me.to_string());
        assert_eq!(states(&pool, incident).await, ("running".into(), "approved".into()));
        let decided_by: Option<Uuid> = sqlx::query_scalar(
            "SELECT decided_by FROM remediation_pending_commands WHERE id = $1",
        )
        .bind(pending)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(decided_by, Some(me));
        assert!(audit::verify(&pool, incident).await.unwrap().valid);
        let audited: (String, String, Value) = sqlx::query_as(
            "SELECT action, target_id, details FROM audit_log WHERE action LIKE 'remediation.%'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(audited.0, "remediation.approve");
        assert_eq!(audited.1, incident.to_string());
        assert_eq!(audited.2, json!({ "pending_command_id": pending, "tier": "low_impact", "edited": false }));

        // Second decisions conflict and write nothing.
        let before = kinds(&pool, incident).await;
        let (status, body) = post(&pool, &approver, &approve_uri(pending), json!({})).await;
        assert_eq!((status, body), (StatusCode::CONFLICT, json!({ "error": "the command is no longer pending" })));
        let (status, _) = post(&pool, &approver, &reject_uri(pending), json!({ "reason": "late" })).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(kinds(&pool, incident).await, before);
        // And the queue no longer lists it.
        let (_, body) = get(&pool, &approver, "/api/remediation/approvals").await;
        assert_eq!(body["approvals"], json!([]));
    }

    #[sqlx::test]
    async fn concurrent_decisions_let_one_through(pool: PgPool) {
        let acme = tenant(&pool, "acme").await;
        let (incident, pending) = park(&pool, acme.servers[0], LOW, HOUR).await;
        let a = session(&pool, "a", Role::Approver, None, None).await;
        let b = session(&pool, "b", Role::Approver, None, None).await;
        let (approve, reject) = (approve_uri(pending), reject_uri(pending));
        let (first, second) = tokio::join!(
            post(&pool, &a, &approve, json!({})),
            post(&pool, &b, &reject, json!({ "reason": "no" })),
        );
        let mut statuses = [first.0, second.0];
        statuses.sort();
        assert_eq!(statuses, [StatusCode::OK, StatusCode::CONFLICT]);
        let kinds = kinds(&pool, incident).await;
        let decisions = kinds.iter().filter(|k| *k == "approval" || *k == "rejection").count();
        assert_eq!(decisions, 1);
        assert!(audit::verify(&pool, incident).await.unwrap().valid);
    }

    #[sqlx::test]
    async fn edits_are_recorded_and_classified_again(pool: PgPool) {
        let acme = tenant(&pool, "acme").await;
        let (incident, pending) = park(&pool, acme.servers[0], LOW, HOUR).await;
        let approver = session(&pool, "approver", Role::Approver, Some(acme.customer), None).await;
        for (command, status) in [
            ("", StatusCode::BAD_REQUEST),
            ("rm -rf --no-preserve-root /", StatusCode::BAD_REQUEST),
            // Editing into a high-impact command needs an administrator.
            (HIGH, StatusCode::FORBIDDEN),
        ] {
            let (got, body) = post(&pool, &approver, &approve_uri(pending), json!({ "command": command })).await;
            assert_eq!(got, status, "{command}: {body}");
        }
        let (_, body) = post(&pool, &approver, &approve_uri(pending), json!({ "command": "x".repeat(4097) })).await;
        assert_eq!(body["error"], "command must be at most 4096 bytes");
        let (status, body) = post(&pool, &approver, &approve_uri(pending), json!({ "command": "rm -rf --no-preserve-root /" })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"].as_str().unwrap().starts_with("the edited command is forbidden"), "{body}");
        assert_eq!(states(&pool, incident).await, ("awaiting_approval".into(), "pending".into()));

        let edited = "systemctl reload nginx";
        let (status, body) = post(&pool, &approver, &approve_uri(pending), json!({ "command": edited })).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!((body["command"].as_str(), body["edited"].as_bool()), (Some(edited), Some(true)));
        let events = audit::events(&pool, incident).await.unwrap();
        let n = events.len();
        assert_eq!(kinds(&pool, incident).await[n - 3..], ["edit", "approval", "state_change"]);
        assert_eq!(events[n - 3].payload, json!({ "original": LOW, "edited": edited }));
        assert_eq!(events[n - 2].payload, json!({ "command": edited }));

        // An unchanged "edit" is a plain approval.
        let (_, same) = park(&pool, acme.servers[0], LOW, HOUR).await;
        let (_, body) = post(&pool, &approver, &approve_uri(same), json!({ "command": LOW })).await;
        assert_eq!(body["edited"], false);
    }

    #[sqlx::test]
    async fn a_rejection_needs_a_reason_and_resumes_the_incident(pool: PgPool) {
        let acme = tenant(&pool, "acme").await;
        let (incident, pending) = park(&pool, acme.servers[0], HIGH, HOUR).await;
        // Any approver may reject, whatever the tier.
        let approver = session(&pool, "approver", Role::Approver, Some(acme.customer), None).await;
        for body in [json!({}), json!({ "reason": "  " }), json!({ "reason": "x".repeat(1001) })] {
            let (status, _) = post(&pool, &approver, &reject_uri(pending), body.clone()).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        }
        let (status, body) = post(&pool, &approver, &reject_uri(pending), json!({ "reason": " not during business hours " })).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!((body["state"].as_str(), body["command"].as_str()), (Some("rejected"), Some(HIGH)));
        let events = audit::events(&pool, incident).await.unwrap();
        let rejection = &events[events.len() - 2];
        assert_eq!(serde_json::to_value(rejection.kind).unwrap(), "rejection");
        assert_eq!(rejection.payload, json!({ "command": HIGH, "reason": "not during business hours" }));
        assert_eq!(states(&pool, incident).await, ("running".into(), "rejected".into()));
        let action: String = sqlx::query_scalar("SELECT action FROM audit_log WHERE action LIKE 'remediation.%'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(action, "remediation.reject");
    }

    #[sqlx::test]
    async fn roles_and_sites_decide_who_decides(pool: PgPool) {
        let acme = tenant(&pool, "acme").await;
        let globex = tenant(&pool, "globex").await;
        let (_, low) = park(&pool, acme.servers[0], LOW, HOUR).await;
        let (high_incident, high) = park(&pool, acme.servers[0], HIGH, HOUR).await;

        let viewer = session(&pool, "viewer", Role::Viewer, None, None).await;
        let operator = session(&pool, "operator", Role::Operator, Some(acme.customer), None).await;
        let approver = session(&pool, "approver", Role::Approver, None, Some(acme.sites[0])).await;
        let elsewhere = session(&pool, "elsewhere", Role::Administrator, None, Some(acme.sites[1])).await;
        let foreign = session(&pool, "foreign", Role::Administrator, Some(globex.customer), None).await;
        let site_admin = session(&pool, "site-admin", Role::Administrator, None, Some(acme.sites[0])).await;

        for (who, cookie) in [("viewer", &viewer), ("operator", &operator)] {
            let (status, _) = post(&pool, cookie, &approve_uri(low), json!({})).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{who}");
            let (status, _) = post(&pool, cookie, &reject_uri(low), json!({ "reason": "no" })).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{who}");
        }
        let (status, _) = post(&pool, &approver, &approve_uri(high), json!({})).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        // An administrator of another site reads the customer's other site
        // only through its own grant: here it does not read site 0 at all.
        for cookie in [&elsewhere, &foreign] {
            let (status, body) = post(&pool, cookie, &approve_uri(high), json!({})).await;
            assert_eq!((status, body), (StatusCode::NOT_FOUND, json!({ "error": "not found" })));
        }
        for uri in [approve_uri(Uuid::new_v4()), "/api/remediation/approvals/x/approve".into()] {
            let (status, _) = post(&pool, &site_admin, &uri, json!({})).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
        }
        assert_eq!(states(&pool, high_incident).await.0, "awaiting_approval");
        let (status, body) = post(&pool, &site_admin, &approve_uri(high), json!({})).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (status, _) = post(&pool, &approver, &approve_uri(low), json!({})).await;
        assert_eq!(status, StatusCode::OK);
        // Without the CSRF header, nothing is decided.
        let request = Request::post(approve_uri(low))
            .header("cookie", &approver)
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();
        let (status, _, _) = send(&pool, request).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[sqlx::test]
    async fn decisions_after_the_deadline_or_the_incident_conflict(pool: PgPool) {
        let acme = tenant(&pool, "acme").await;
        let admin = session(&pool, "admin", Role::Administrator, None, None).await;
        let (late_incident, late) = park(&pool, acme.servers[0], LOW, Duration::from_secs(1)).await;
        tokio::time::sleep(Duration::from_millis(1100)).await;
        let (status, body) = post(&pool, &admin, &approve_uri(late), json!({})).await;
        assert_eq!((status, body), (StatusCode::CONFLICT, json!({ "error": "the incident reached its duration limit" })));
        assert_eq!(states(&pool, late_incident).await, ("awaiting_approval".into(), "pending".into()));

        // The incident closed (its alert resolved) before the decision.
        let (closed_incident, closed) = park(&pool, acme.servers[0], LOW, HOUR).await;
        let mut tx = pool.begin().await.unwrap();
        store::close_pending(&mut tx, closed_incident, "cancelled").await.unwrap();
        store::transition(&mut tx, closed_incident, IncidentState::AwaitingApproval, IncidentState::Resolved)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let (status, _) = post(&pool, &admin, &reject_uri(closed), json!({ "reason": "no" })).await;
        assert_eq!(status, StatusCode::CONFLICT);
    }
}
