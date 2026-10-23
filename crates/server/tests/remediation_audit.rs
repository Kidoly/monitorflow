//! Remediation incident history against a real PostgreSQL (ADR-0042):
//! sequence numbers under concurrency, the append-only triggers, and the
//! hash chain catching rows changed behind the triggers' back. Ignored
//! without `DATABASE_URL`.

use std::time::Duration;

use monitorflow_server::remediation::audit::{
    self, Actor, AuditError, EventKind, IncidentState, NewEvent, NewIncident, GENESIS,
};
use monitorflow_server::remediation::risk::{classify, RuleSet};
use serde_json::json;
use sqlx::PgPool;
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

const DISK: &str = "monitorflow_disk_used_ratio";

/// A server on a new site of a new customer, with an open alert on it.
async fn server(pool: &PgPool, name: &str) -> (Uuid, Uuid, Uuid) {
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
        "INSERT INTO servers (peer_id, site_id, hostname, os_family)
         VALUES ($1, $2, $1, 'debian') RETURNING id",
    )
    .bind(format!("agent-{name}"))
    .bind(site)
    .fetch_one(pool)
    .await
    .unwrap();
    let alert: Uuid = sqlx::query_scalar(
        "INSERT INTO incidents (server_id, metric, instance, severity, opened_at, last_value,
                                last_value_at, peak_value, threshold)
         VALUES ($1, $2, '/', 'critical', now(), 0.97, now(), 0.97, '{}')
         RETURNING id",
    )
    .bind(server)
    .bind(DISK)
    .fetch_one(pool)
    .await
    .unwrap();
    (customer, server, alert)
}

fn new_incident(server: Uuid, alert: Uuid) -> NewIncident {
    NewIncident {
        server_id: server,
        alert_incident_id: Some(alert),
        metric: DISK.into(),
        instance: "/".into(),
        max_commands: 10,
        max_duration: Duration::from_secs(1800),
    }
}

async fn incident(pool: &PgPool, name: &str) -> Uuid {
    let (_, server, alert) = server(pool, name).await;
    audit::open(pool, &new_incident(server, alert))
        .await
        .unwrap()
}

async fn sql_verify(pool: &PgPool, id: Uuid) -> Option<i64> {
    sqlx::query_scalar("SELECT remediation_verify_chain($1)")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

fn sqlstate(result: Result<impl std::fmt::Debug, sqlx::Error>) -> String {
    match result {
        Err(sqlx::Error::Database(e)) => e.code().unwrap().into_owned(),
        other => panic!("expected a database error, got {other:?}"),
    }
}

const INSUFFICIENT_PRIVILEGE: &str = "42501";

/// A story of every kind of event.
fn story(user: Uuid, command: Uuid) -> Vec<NewEvent> {
    vec![
        NewEvent::context_sent("claude/test", json!({ "logs": "[REDACTED]" })),
        NewEvent::ai_proposal(
            "claude/test",
            "disk full",
            "logs grow",
            Some("du -sh /var/log"),
        ),
        NewEvent::classification(
            "du -sh /var/log",
            &classify("du -sh /var/log", &RuleSet::default()),
        ),
        NewEvent::approval(Actor::System, "du -sh /var/log"),
        NewEvent::command_queued(command, "du -sh /var/log"),
        NewEvent::command_result(
            "agent-a",
            command,
            Some(0),
            "4G /var/log",
            "",
            None,
            Duration::from_millis(80),
        ),
        NewEvent::ai_proposal(
            "claude/test",
            "old logs",
            "rotate",
            Some("journalctl --vacuum-size=1G"),
        ),
        NewEvent::edit(
            user,
            "journalctl --vacuum-size=1G",
            "journalctl --vacuum-size=2G",
        ),
        NewEvent::rejection(user, "journalctl --vacuum-size=2G", Some("not now")),
        NewEvent::snapshot("none", None, "no snapshot support"),
        NewEvent::escalation(Actor::System, "approval rejected"),
        NewEvent::state_change(
            Actor::System,
            IncidentState::Running,
            IncidentState::Escalated,
        ),
        NewEvent::note(Actor::User(user), "handled by hand"),
    ]
}

db_tests! {
    #[sqlx::test]
    async fn appends_number_and_chain_every_event(pool: PgPool) {
        let id = incident(&pool, "a").await;
        let user = Uuid::new_v4();
        let mut prev = GENESIS.to_vec();
        for (event, seq) in story(user, Uuid::new_v4()).into_iter().zip(1..) {
            let kind = event.kind;
            let stored = audit::append(&pool, id, event).await.unwrap();
            assert_eq!((stored.seq, stored.kind), (seq, kind));
            assert_eq!(stored.prev_hash, prev);
            assert_ne!(stored.hash, GENESIS.to_vec());
            prev = stored.hash;
        }
        let events = audit::events(&pool, id).await.unwrap();
        assert_eq!(events.len(), 13);
        assert_eq!(events[7].kind, EventKind::Edit);
        assert_eq!((events[7].actor_kind.as_str(), events[7].actor_id.clone()), ("user", user.to_string()));
        assert_eq!(events[5].actor_id, "agent-a");
        assert_eq!(events[5].payload["exit_code"], 0);
        let report = audit::verify(&pool, id).await.unwrap();
        assert!(report.valid, "{report:?}");
        assert_eq!(report.events, 13);
        assert_eq!(report.head_hash, Some(audit::hex(&prev)));
        assert_eq!(sql_verify(&pool, id).await, None);

        // The incident row moves; its identity does not.
        sqlx::query("UPDATE remediation_incidents SET state = 'escalated', closed_at = now() WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        assert!(audit::verify(&pool, id).await.unwrap().valid);
        assert!(matches!(
            audit::append(&pool, Uuid::new_v4(), NewEvent::note(Actor::System, "x")).await,
            Err(AuditError::NotFound)
        ));
    }

    #[sqlx::test]
    async fn concurrent_appends_stay_ordered(pool: PgPool) {
        let first = incident(&pool, "a").await;
        let second = incident(&pool, "b").await;
        let mut tasks = Vec::new();
        for i in 0..40 {
            let pool = pool.clone();
            let id = if i % 2 == 0 { first } else { second };
            tasks.push(tokio::spawn(async move {
                audit::append(&pool, id, NewEvent::note(Actor::System, &format!("note {i}")))
                    .await
                    .unwrap()
                    .seq
            }));
        }
        let mut seqs = Vec::new();
        for task in tasks {
            seqs.push(task.await.unwrap());
        }
        seqs.sort_unstable();
        seqs.dedup();
        assert_eq!(seqs, (1..=20).collect::<Vec<i64>>());
        for id in [first, second] {
            let events = audit::events(&pool, id).await.unwrap();
            assert_eq!(events.iter().map(|e| e.seq).collect::<Vec<_>>(), (1..=20).collect::<Vec<i64>>());
            assert!(audit::verify(&pool, id).await.unwrap().valid);
            assert_eq!(sql_verify(&pool, id).await, None);
        }
    }

    #[sqlx::test]
    async fn triggers_refuse_changes_and_out_of_order_rows(pool: PgPool) {
        let id = incident(&pool, "a").await;
        for i in 0..3 {
            audit::append(&pool, id, NewEvent::note(Actor::System, &format!("{i}"))).await.unwrap();
        }
        for sql in [
            "UPDATE remediation_incident_events SET payload = '{\"text\": \"x\"}' WHERE incident_id = $1",
            "DELETE FROM remediation_incident_events WHERE incident_id = $1",
            "DELETE FROM remediation_incidents WHERE id = $1",
            "UPDATE remediation_incidents SET server_id = gen_random_uuid() WHERE id = $1",
            "UPDATE remediation_incidents SET max_commands = 1000 WHERE id = $1",
            "UPDATE remediation_incidents SET head_seq = 1 WHERE id = $1",
        ] {
            let result = sqlx::query(sql).bind(id).execute(&pool).await;
            assert_eq!(sqlstate(result), INSUFFICIENT_PRIVILEGE, "{sql}");
        }
        for table in ["remediation_incident_events", "remediation_incidents"] {
            let result = sqlx::query(&format!("TRUNCATE {table} CASCADE")).execute(&pool).await;
            assert_eq!(sqlstate(result), INSUFFICIENT_PRIVILEGE, "{table}");
        }
        // A row that does not follow the head is refused, whatever its hash.
        let insert = "INSERT INTO remediation_incident_events
                          (incident_id, seq, kind, actor_kind, actor_id, payload, prev_hash, hash)
                      SELECT $1, $2, 'note', 'system', 'server', '{}', $3, $3";
        let head: Vec<u8> = sqlx::query_scalar("SELECT head_hash FROM remediation_incidents WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        for (seq, prev) in [(5_i64, head.clone()), (4, GENESIS.to_vec()), (2, head.clone())] {
            let result = sqlx::query(insert).bind(id).bind(seq).bind(&prev).execute(&pool).await;
            assert_eq!(sqlstate(result), "40001", "seq {seq}");
        }
        // The right one goes in, with the hash the trigger computed.
        sqlx::query(insert).bind(id).bind(4_i64).bind(&head).execute(&pool).await.unwrap();
        let report = audit::verify(&pool, id).await.unwrap();
        assert!(report.valid, "{report:?}");
        assert_eq!(report.events, 4);
    }

    #[sqlx::test]
    async fn incidents_belong_to_their_servers_customer(pool: PgPool) {
        let (customer_a, server_a, alert_a) = server(&pool, "a").await;
        let (_, server_b, alert_b) = server(&pool, "b").await;
        let insert = "INSERT INTO remediation_incidents
                          (customer_id, server_id, alert_incident_id, metric, max_commands, max_duration_s)
                      VALUES ($1, $2, $3, 'm', 1, 1)";
        let result = sqlx::query(insert).bind(customer_a).bind(server_b).bind(None::<Uuid>).execute(&pool).await;
        assert_eq!(sqlstate(result), "23514");
        let result = sqlx::query(insert).bind(customer_a).bind(server_a).bind(alert_b).execute(&pool).await;
        assert_eq!(sqlstate(result), "23514");

        let id = audit::open(&pool, &new_incident(server_a, alert_a)).await.unwrap();
        let (customer, state): (Uuid, IncidentState) =
            sqlx::query_as("SELECT customer_id, state FROM remediation_incidents WHERE id = $1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!((customer, state), (customer_a, IncidentState::Open));
        // One active remediation per alert.
        assert!(audit::open(&pool, &new_incident(server_a, alert_a)).await.is_err());
        assert!(matches!(
            audit::open(&pool, &new_incident(Uuid::new_v4(), alert_a)).await,
            Err(AuditError::NotFound)
        ));
        let mut zero = new_incident(server_a, alert_a);
        zero.max_commands = 0;
        assert!(matches!(audit::open(&pool, &zero).await, Err(AuditError::Invalid(_))));
    }

    #[sqlx::test]
    async fn the_chain_shows_rows_changed_behind_the_triggers(pool: PgPool) {
        let id = incident(&pool, "a").await;
        for event in story(Uuid::new_v4(), Uuid::new_v4()).into_iter().take(5) {
            audit::append(&pool, id, event).await.unwrap();
        }
        // A superuser can switch user triggers off for its session.
        let mut conn = pool.acquire().await.unwrap();
        sqlx::query("SET session_replication_role = replica").execute(&mut *conn).await.unwrap();

        sqlx::query(
            "UPDATE remediation_incident_events SET payload = jsonb_set(payload, '{command}', '\"rm -rf /\"')
             WHERE incident_id = $1 AND seq = 2",
        )
        .bind(id)
        .execute(&mut *conn)
        .await
        .unwrap();
        let report = audit::verify(&pool, id).await.unwrap();
        assert_eq!((report.valid, report.broken_at, report.reason), (false, Some(2), Some("hash")));
        assert_eq!(sql_verify(&pool, id).await, Some(2));

        // Recomputing that row's hash moves the break to the next row.
        sqlx::query(
            "UPDATE remediation_incident_events e SET hash = remediation_event_hash(e)
             WHERE incident_id = $1 AND seq = 2",
        )
        .bind(id)
        .execute(&mut *conn)
        .await
        .unwrap();
        let report = audit::verify(&pool, id).await.unwrap();
        assert_eq!((report.broken_at, report.reason), (Some(3), Some("prev_hash")));
        assert_eq!(sql_verify(&pool, id).await, Some(3));

        // A removed tail shows against the incident's head.
        let other = incident(&pool, "b").await;
        for i in 0..3 {
            audit::append(&pool, other, NewEvent::note(Actor::System, &format!("{i}"))).await.unwrap();
        }
        sqlx::query("DELETE FROM remediation_incident_events WHERE incident_id = $1 AND seq = 3")
            .bind(other)
            .execute(&mut *conn)
            .await
            .unwrap();
        let report = audit::verify(&pool, other).await.unwrap();
        assert_eq!((report.broken_at, report.reason), (Some(3), Some("head")));
        assert_eq!(sql_verify(&pool, other).await, Some(3));

        // A removed middle row is a gap.
        sqlx::query("DELETE FROM remediation_incident_events WHERE incident_id = $1 AND seq = 1")
            .bind(other)
            .execute(&mut *conn)
            .await
            .unwrap();
        let report = audit::verify(&pool, other).await.unwrap();
        assert_eq!((report.broken_at, report.reason), (Some(1), Some("gap")));
        assert_eq!(sql_verify(&pool, other).await, Some(1));
        sqlx::query("RESET session_replication_role").execute(&mut *conn).await.unwrap();
    }
}
