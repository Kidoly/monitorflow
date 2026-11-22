//! Running AI remediation again on an open alert through the API
//! (ADR-0165) against a real PostgreSQL: who may, what refuses it, what
//! it records, and where it only diagnoses (ADR-0175). Ignored without `DATABASE_URL`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users;
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
use monitorflow_server::remediation::audit::{self, IncidentState, NewEvent, NewIncident};
use monitorflow_server::remediation::runner::store;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery";
const REFUSAL: &str = "shell commands are disabled on this agent (commands.shell); \
                       run the installer with MF_ALLOW_COMMANDS=1 to allow them";

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

fn settings(ai: bool) -> ApiSettings {
    let mut settings = ApiSettings::default();
    settings.ai.config.enabled = ai;
    settings
}

async fn send(
    pool: &PgPool,
    settings: ApiSettings,
    request: Request<Body>,
) -> (StatusCode, Value, Option<String>) {
    let app = api::router(ApiState::new(pool.clone(), Registry::default(), settings));
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

/// A user with one role on `site`, or globally, logged in.
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
    let (status, _, cookie) = send(pool, settings(false), request).await;
    assert_eq!(status, StatusCode::OK);
    cookie.unwrap()
}

async fn status(pool: &PgPool, ai: bool, cookie: &str, alert: Uuid) -> (StatusCode, Value) {
    let request = Request::get(format!("/api/remediation/alerts/{alert}"))
        .header("cookie", cookie)
        .body(Body::empty())
        .unwrap();
    let (status, body, _) = send(pool, settings(ai), request).await;
    (status, body)
}

async fn run(pool: &PgPool, ai: bool, cookie: &str, alert: Uuid) -> (StatusCode, Value) {
    let request = Request::post(format!("/api/remediation/alerts/{alert}/run"))
        .header("cookie", cookie)
        .header(CSRF_HEADER, "1")
        .body(Body::empty())
        .unwrap();
    let (status, body, _) = send(pool, settings(ai), request).await;
    (status, body)
}

struct Setup {
    site: Uuid,
    server: Uuid,
    alert: Uuid,
    /// The alert's first remediation, escalated on the agent's refusal.
    first: Uuid,
}

/// A customer, site and server with an open disk alert whose remediation
/// escalated because the agent refused its command, as the loop records
/// it.
async fn setup(pool: &PgPool, name: &str) -> Setup {
    let customer: Uuid =
        sqlx::query_scalar("INSERT INTO customers (name) VALUES ($1) RETURNING id")
            .bind(name)
            .fetch_one(pool)
            .await
            .unwrap();
    let site: Uuid =
        sqlx::query_scalar("INSERT INTO sites (name, customer_id) VALUES ($1, $2) RETURNING id")
            .bind(name)
            .bind(customer)
            .fetch_one(pool)
            .await
            .unwrap();
    let server: Uuid = sqlx::query_scalar(
        "INSERT INTO servers (peer_id, site_id, hostname, os_family, command_kinds)
         VALUES ($1, $2, $1, 'debian', '{\"shell\": \"accepted\"}') RETURNING id",
    )
    .bind(format!("agent-{name}"))
    .bind(site)
    .fetch_one(pool)
    .await
    .unwrap();
    let alert: Uuid = sqlx::query_scalar(
        "INSERT INTO incidents (server_id, metric, instance, severity, opened_at, last_value,
                                last_value_at, peak_value, threshold)
         VALUES ($1, 'monitorflow_disk_used_ratio', '/', 'critical', now(), 0.9, now(),
                 0.9, '{\"op\": \">\", \"critical\": 0.85}')
         RETURNING id",
    )
    .bind(server)
    .fetch_one(pool)
    .await
    .unwrap();
    let first = audit::open(
        pool,
        &NewIncident {
            server_id: server,
            alert_incident_id: Some(alert),
            metric: "monitorflow_disk_used_ratio".into(),
            instance: "/".into(),
            max_commands: 5,
            max_duration: std::time::Duration::from_secs(600),
        },
    )
    .await
    .unwrap();
    let mut tx = pool.begin().await.unwrap();
    store::transition(&mut tx, first, IncidentState::Open, IncidentState::Running)
        .await
        .unwrap();
    let command = Uuid::new_v4();
    for event in [
        NewEvent::command_queued(command, "df -h /"),
        NewEvent::command_result(
            "peer",
            command,
            None,
            "",
            "",
            Some(REFUSAL),
            Default::default(),
        ),
        NewEvent::closing(
            IncidentState::Running,
            IncidentState::Escalated,
            &format!("the command did not run to completion: {REFUSAL}"),
            "incomplete",
            None,
            None,
        ),
    ] {
        audit::append_in(&mut tx, first, event).await.unwrap();
    }
    store::set_state(&mut tx, first, IncidentState::Escalated)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    Setup {
        site,
        server,
        alert,
        first,
    }
}

db_tests! {
    #[sqlx::test]
    async fn an_approver_runs_it_again_once_and_the_chain_records_who(pool: PgPool) {
        let s = setup(&pool, "acme").await;
        let alice = session(&pool, "alice", Role::Approver, Some(s.site)).await;
        let (code, body) = status(&pool, true, &alice, s.alert).await;
        assert_eq!(code, StatusCode::OK, "{body}");
        assert_eq!(body["can_run"], true, "{body}");
        assert_eq!(body["blocked"], Value::Null);
        assert_eq!((body["attempts"].as_i64(), body["latest_state"].as_str()), (Some(1), Some("escalated")));

        let (code, body) = run(&pool, true, &alice, s.alert).await;
        assert_eq!(code, StatusCode::CREATED, "{body}");
        assert_eq!(body["attempt"], 2);
        assert_eq!(body["state"], "open");
        assert_eq!(body["previous_id"], json!(s.first));
        let id: Uuid = body["id"].as_str().unwrap().parse().unwrap();

        let events = audit::events(&pool, id).await.unwrap();
        assert_eq!(events.len(), 1);
        let note = &events[0];
        let alice_id: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE username = 'alice'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!((note.actor_kind.as_str(), note.actor_id.clone()), ("user", alice_id.to_string()));
        assert_eq!(note.payload["code"], "attempt");
        assert_eq!(note.payload["trigger"], "person");
        assert_eq!(note.payload["previous"]["code"], "incomplete");
        assert_eq!(note.payload["previous"]["state"], "escalated");
        assert!(audit::verify(&pool, id).await.unwrap().valid);
        let logged: (String, Value) = sqlx::query_as(
            "SELECT target_id, details FROM audit_log WHERE action = 'remediation.run'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(logged.0, id.to_string());
        assert_eq!(logged.1["attempt"], 2);

        // Not twice while the new one is in progress.
        let (code, body) = status(&pool, true, &alice, s.alert).await;
        assert_eq!((code, body["blocked"].as_str()), (StatusCode::OK, Some("in_progress")));
        let (code, body) = run(&pool, true, &alice, s.alert).await;
        assert_eq!(code, StatusCode::CONFLICT, "{body}");
        assert!(body["error"].as_str().unwrap().contains("already running"), "{body}");
    }

    #[sqlx::test]
    async fn it_is_refused_with_the_reason_before_anything_starts(pool: PgPool) {
        let s = setup(&pool, "acme").await;
        let alice = session(&pool, "alice", Role::Approver, Some(s.site)).await;

        // AI off.
        let (_, body) = status(&pool, false, &alice, s.alert).await;
        assert_eq!((body["can_run"].as_bool(), body["blocked"].as_str()), (Some(false), Some("ai_off")));
        let (code, body) = run(&pool, false, &alice, s.alert).await;
        assert_eq!(code, StatusCode::CONFLICT);
        assert!(body["error"].as_str().unwrap().contains("AI remediation is off"), "{body}");

        // A resolved alert has nothing left to fix.
        sqlx::query("UPDATE incidents SET state = 'resolved', resolved_at = now() WHERE id = $1")
            .bind(s.alert)
            .execute(&pool)
            .await
            .unwrap();
        let (_, body) = status(&pool, true, &alice, s.alert).await;
        assert_eq!(body["blocked"], "alert_resolved");
        let (code, _) = run(&pool, true, &alice, s.alert).await;
        assert_eq!(code, StatusCode::CONFLICT);

        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM remediation_incidents")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    /// Where no command can run, an attempt still starts: the AI
    /// diagnoses, and the status says so first (ADR-0175).
    #[sqlx::test]
    async fn where_no_command_can_run_it_runs_to_diagnose(pool: PgPool) {
        let s = setup(&pool, "acme").await;
        let alice = session(&pool, "alice", Role::Approver, Some(s.site)).await;
        let (_, body) = status(&pool, true, &alice, s.alert).await;
        assert_eq!((&body["diagnosis_only"], &body["refusal"]), (&Value::Null, &Value::Null), "{body}");

        sqlx::query("UPDATE servers SET command_kinds = '{\"shell\": \"off\"}' WHERE id = $1")
            .bind(s.server)
            .execute(&pool)
            .await
            .unwrap();
        let (_, body) = status(&pool, true, &alice, s.alert).await;
        assert_eq!((body["can_run"].as_bool(), &body["blocked"]), (Some(true), &Value::Null), "{body}");
        assert_eq!(body["refusal"], "commands_off");
        assert_eq!(body["diagnosis_only"], "commands_off");
        assert!(body["diagnosis_only_reason"].as_str().unwrap().contains("Commands tab"), "{body}");
        assert_eq!(body["no_attempt"], Value::Null);
        let (code, body) = run(&pool, true, &alice, s.alert).await;
        assert_eq!(code, StatusCode::CREATED, "{body}");

        // A device, and a host that stopped reporting.
        let device: Uuid = sqlx::query_scalar(
            "INSERT INTO servers (peer_id, site_id, hostname, os_family, kind, address, package_updates)
             VALUES ('device-sw1', $1, 'switch-1', 'device', 'device', '192.0.2.1', false)
             RETURNING id",
        )
        .bind(s.site)
        .fetch_one(&pool)
        .await
        .unwrap();
        for (server, metric, code) in [
            (device, "monitorflow_snmp_if_up", "device"),
            (s.server, "offline", "offline"),
        ] {
            let alert: Uuid = sqlx::query_scalar(
                "INSERT INTO incidents (server_id, metric, instance, severity, opened_at, last_value,
                                        last_value_at, peak_value, threshold)
                 VALUES ($1, $2, '', 'critical', now(), 0, now(), 0, '{}') RETURNING id",
            )
            .bind(server)
            .bind(metric)
            .fetch_one(&pool)
            .await
            .unwrap();
            let (_, body) = status(&pool, true, &alice, alert).await;
            assert_eq!((body["can_run"].as_bool(), body["diagnosis_only"].as_str()), (Some(true), Some(code)), "{body}");
            assert_eq!(body["no_attempt"], "soon", "{body}");
            let (status, body) = run(&pool, true, &alice, alert).await;
            assert_eq!(status, StatusCode::CREATED, "{body}");
        }
    }

    #[sqlx::test]
    async fn operators_read_it_and_others_do_not_see_it(pool: PgPool) {
        let s = setup(&pool, "acme").await;
        let other = setup(&pool, "globex").await;
        let bob = session(&pool, "bob", Role::Operator, Some(s.site)).await;
        let (code, body) = status(&pool, true, &bob, s.alert).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!((body["can_run"].as_bool(), body["blocked"].as_str()), (Some(false), Some("role")));
        let (code, _) = run(&pool, true, &bob, s.alert).await;
        assert_eq!(code, StatusCode::FORBIDDEN);

        // Another customer's alert is not found, for reading or running.
        let (code, _) = status(&pool, true, &bob, other.alert).await;
        assert_eq!(code, StatusCode::NOT_FOUND);
        let carol = session(&pool, "carol", Role::Administrator, Some(s.site)).await;
        let (code, _) = run(&pool, true, &carol, other.alert).await;
        assert_eq!(code, StatusCode::NOT_FOUND);
        let (code, _) = run(&pool, true, &carol, Uuid::new_v4()).await;
        assert_eq!(code, StatusCode::NOT_FOUND);

        // A global administrator may.
        let root = session(&pool, "root", Role::Administrator, None).await;
        let (code, body) = run(&pool, true, &root, other.alert).await;
        assert_eq!(code, StatusCode::CREATED, "{body}");
    }

    /// An alert that never had a remediation, like one older than an hour
    /// when AI was turned on, gets its first one on request.
    #[sqlx::test]
    async fn an_alert_without_any_remediation_gets_its_first(pool: PgPool) {
        let s = setup(&pool, "acme").await;
        let alert: Uuid = sqlx::query_scalar(
            "INSERT INTO incidents (server_id, metric, instance, severity, opened_at, last_value,
                                    last_value_at, peak_value, threshold)
             VALUES ($1, 'monitorflow_memory_used_ratio', '', 'warning', now() - interval '1 day',
                     0.9, now(), 0.9, '{\"op\": \">\", \"warning\": 0.85}')
             RETURNING id",
        )
        .bind(s.server)
        .fetch_one(&pool)
        .await
        .unwrap();
        let alice = session(&pool, "alice", Role::Approver, Some(s.site)).await;
        let (_, body) = status(&pool, true, &alice, alert).await;
        assert_eq!((body["attempts"].as_i64(), body["can_run"].as_bool()), (Some(0), Some(true)));
        // Too old for AI to look at it by itself: said, with the way out.
        assert_eq!(body["no_attempt"], "too_old", "{body}");
        assert!(body["no_attempt_reason"].as_str().unwrap().contains("start it by hand"), "{body}");
        let (_, body) = status(&pool, false, &alice, alert).await;
        assert_eq!(body["no_attempt"], "ai_off", "{body}");
        let (code, body) = run(&pool, true, &alice, alert).await;
        assert_eq!(code, StatusCode::CREATED, "{body}");
        assert_eq!((body["attempt"].as_i64(), &body["previous_id"]), (Some(1), &Value::Null));
    }
}
