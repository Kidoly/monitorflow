//! Ticket statuses read back from the PSA (ADR-0164, ADR-0191): a ticket
//! closed or taken in HaloPSA acknowledges its incident, one deleted or
//! merged ends the link, and a 429 pauses every request. Against an
//! in-process stand-in for HaloPSA, never a real instance. Ignored without
//! `DATABASE_URL`.

mod fake_halo;

use std::sync::Arc;

use fake_halo::{FakeHalo, CLIENT_ID, CLIENT_SECRET, TECHNICIAN};
use monitorflow_server::psa::halopsa::Limiter;
use monitorflow_server::psa::poller::Poller;
use monitorflow_server::secrets::{self, Column};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

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

/// A connection acting as the agent 42, with an open incident and its
/// ticket `ticket`: `(connection, incident)`.
async fn setup(pool: &PgPool, halo: &FakeHalo, ticket: i64) -> (Uuid, Uuid) {
    let secret = secrets::seal(Column::PsaConnections, CLIENT_SECRET).unwrap();
    let connection: Uuid = sqlx::query_scalar(
        "INSERT INTO psa_connections (base_url, client_id, client_secret, agent_id)
         VALUES ($1, $2, $3, 42) RETURNING id",
    )
    .bind(&halo.url)
    .bind(CLIENT_ID)
    .bind(secret)
    .fetch_one(pool)
    .await
    .unwrap();
    let incident = incident(pool, connection, ticket).await;
    (connection, incident)
}

async fn incident(pool: &PgPool, connection: Uuid, ticket: i64) -> Uuid {
    let customer: Uuid =
        sqlx::query_scalar("INSERT INTO customers (name) VALUES ('acme' || $1) RETURNING id")
            .bind(ticket.to_string())
            .fetch_one(pool)
            .await
            .unwrap();
    let site: Uuid = sqlx::query_scalar(
        "INSERT INTO sites (name, customer_id) VALUES ('paris-' || $2, $1) RETURNING id",
    )
    .bind(customer)
    .bind(ticket.to_string())
    .fetch_one(pool)
    .await
    .unwrap();
    let server: Uuid = sqlx::query_scalar(
        "INSERT INTO servers (peer_id, site_id, hostname, os_family)
         VALUES ('web-' || $2, $1, 'web-' || $2, 'debian') RETURNING id",
    )
    .bind(site)
    .bind(ticket.to_string())
    .fetch_one(pool)
    .await
    .unwrap();
    let incident: Uuid = sqlx::query_scalar(
        "INSERT INTO incidents (server_id, metric, instance, severity, opened_at, last_value,
                                last_value_at, peak_value, threshold)
         VALUES ($1, 'disk_used_percent', '', 'critical', now(), 97, now(), 97, '{}') RETURNING id",
    )
    .bind(server)
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO psa_tickets (incident_id, connection_id, ticket_id, state)
         VALUES ($1, $2, $3, 'open')",
    )
    .bind(incident)
    .bind(connection)
    .bind(ticket)
    .execute(pool)
    .await
    .unwrap();
    incident
}

fn poller(pool: &PgPool) -> Poller {
    Poller::with_limiter(pool.clone(), Arc::new(Limiter::new(600)))
}

async fn acknowledgement(
    pool: &PgPool,
    incident: Uuid,
) -> Option<(String, String, Option<String>)> {
    sqlx::query_as(
        "SELECT username, via, note FROM incident_acknowledgements WHERE incident_id = $1",
    )
    .bind(incident)
    .fetch_optional(pool)
    .await
    .unwrap()
}

async fn audit(pool: &PgPool) -> Vec<(String, String, Value)> {
    sqlx::query_as(
        "SELECT action, actor_kind, details FROM audit_log WHERE action LIKE 'psa.ticket.%' ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

db_tests! {
    #[sqlx::test]
    async fn a_ticket_closed_in_the_psa_acknowledges_its_incident(pool: PgPool) {
        secrets::install_for_tests();
        let halo = FakeHalo::start().await;
        let (_, incident) = setup(&pool, &halo, 5001).await;
        let poller = poller(&pool);

        // New and assigned to the application itself: nothing.
        halo.set_ticket(5001, json!({"status_id": 1, "agent_id": 42}));
        assert_eq!(poller.pass().await.unwrap(), 1);
        assert_eq!(acknowledgement(&pool, incident).await, None);
        let checked: (Option<i64>, bool) = sqlx::query_as(
            "SELECT psa_status_id, checked_at IS NOT NULL FROM psa_tickets WHERE incident_id = $1",
        )
        .bind(incident).fetch_one(&pool).await.unwrap();
        assert_eq!(checked, (Some(1), true));

        halo.set_ticket(5001, json!({"status_id": 9, "agent_id": TECHNICIAN.0}));
        poller.pass().await.unwrap();
        let (username, via, note) = acknowledgement(&pool, incident).await.unwrap();
        assert_eq!(username, "Alice Martin (HaloPSA)");
        assert_eq!(via, "psa");
        assert_eq!(note.as_deref(), Some("Closed in HaloPSA, ticket 5001."));
        // On the timeline, and the follow-up stops.
        let kinds: Vec<String> = sqlx::query_scalar(
            "SELECT kind FROM incident_events WHERE incident_id = $1 ORDER BY id",
        )
        .bind(incident).fetch_all(&pool).await.unwrap();
        assert!(kinds.contains(&"acknowledged".to_owned()), "{kinds:?}");
        // The incident stays open: only measurements resolve it.
        let state: String = sqlx::query_scalar("SELECT state FROM incidents WHERE id = $1")
            .bind(incident).fetch_one(&pool).await.unwrap();
        assert_eq!(state, "open");
        let audited = audit(&pool).await;
        assert_eq!(audited.len(), 1);
        assert_eq!((audited[0].0.as_str(), audited[0].1.as_str()), ("psa.ticket.acknowledge", "system"));
        assert_eq!(audited[0].2["reason"], "closed");

        // Read again: acknowledged once only.
        poller.pass().await.unwrap();
        assert_eq!(audit(&pool).await.len(), 1);
    }

    #[sqlx::test]
    async fn an_agent_taking_the_ticket_acknowledges_unless_turned_off(pool: PgPool) {
        secrets::install_for_tests();
        let halo = FakeHalo::start().await;
        let (connection, taken) = setup(&pool, &halo, 5001).await;
        let off = incident(&pool, connection, 5002).await;
        halo.set_ticket(5001, json!({"status_id": 2, "agent_id": TECHNICIAN.0}));
        poller(&pool).pass().await.unwrap();
        let (username, _, note) = acknowledgement(&pool, taken).await.unwrap();
        assert_eq!(username, "Alice Martin (HaloPSA)");
        assert_eq!(note.as_deref(), Some("Assigned in HaloPSA, ticket 5001."));

        sqlx::query("UPDATE psa_connections SET acknowledge_on_assign = false")
            .execute(&pool).await.unwrap();
        halo.set_ticket(5002, json!({"status_id": 2, "agent_id": TECHNICIAN.0}));
        poller(&pool).pass().await.unwrap();
        assert_eq!(acknowledgement(&pool, off).await, None);
    }

    #[sqlx::test]
    async fn a_deleted_or_merged_ticket_ends_the_link(pool: PgPool) {
        secrets::install_for_tests();
        let halo = FakeHalo::start().await;
        let (connection, deleted) = setup(&pool, &halo, 5001).await;
        let merged = incident(&pool, connection, 5002).await;
        let missing = incident(&pool, connection, 5003).await;
        halo.set_ticket(5001, json!({"deleted": true}));
        halo.set_ticket(5002, json!({"merged_into_id": 5100}));
        halo.set_ticket(5003, Value::Null);
        assert_eq!(poller(&pool).pass().await.unwrap(), 3);
        for (incident, reason) in [
            (deleted, "The ticket was deleted in HaloPSA."),
            (merged, "The ticket was merged into ticket 5100 in HaloPSA."),
            (missing, "The ticket was deleted in HaloPSA."),
        ] {
            let (state, error): (String, Option<String>) =
                sqlx::query_as("SELECT state, error FROM psa_tickets WHERE incident_id = $1")
                    .bind(incident).fetch_one(&pool).await.unwrap();
            assert_eq!((state.as_str(), error.as_deref()), ("ended", Some(reason)));
            assert_eq!(acknowledgement(&pool, incident).await, None);
        }
        assert_eq!(audit(&pool).await.len(), 3);
        // Ended links are not read again.
        assert_eq!(poller(&pool).pass().await.unwrap(), 0);
    }

    #[sqlx::test]
    async fn a_429_pauses_every_request_for_its_retry_after(pool: PgPool) {
        secrets::install_for_tests();
        let halo = FakeHalo::start().await;
        let (connection, _) = setup(&pool, &halo, 5001).await;
        incident(&pool, connection, 5002).await;
        let poller = poller(&pool);
        halo.rate_limit(Some(120));
        assert_eq!(poller.pass().await.unwrap(), 0);
        let asked = halo.requests().len();
        // Paused: the next pass sends nothing at all.
        halo.rate_limit(None);
        assert_eq!(poller.pass().await.unwrap(), 0);
        assert_eq!(halo.requests().len(), asked);
        let error: Option<String> = sqlx::query_scalar("SELECT last_error FROM psa_connections")
            .fetch_one(&pool).await.unwrap();
        assert!(error.unwrap().contains("700 requests in 5 minutes"));
        // Another limiter, as after the pause: it reads both.
        let fresh = Poller::with_limiter(pool.clone(), Arc::new(Limiter::new(600)));
        assert_eq!(fresh.pass().await.unwrap(), 2);
    }
}
