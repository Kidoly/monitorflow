//! Tickets from incidents (ADR-0164, ADR-0166, ADR-0191): the sync reads
//! incident events, opens one ticket for the PSA client of the incident's
//! site, customer or the default client, with the mapped priority and the
//! server's asset, holds it during a silence, raises its priority on
//! escalation and closes it at resolution; refusals, retries and lost
//! answers. Against an in-process stand-in for HaloPSA, never a real
//! instance. Ignored without `DATABASE_URL`.

mod fake_halo;

use std::sync::Arc;

use fake_halo::{FakeHalo, CLIENT_ID, CLIENT_SECRET};
use monitorflow_server::psa::halopsa::Limiter;
use monitorflow_server::psa::sync::Syncer;
use monitorflow_server::secrets::{self, Column};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

const WEB: &str = "https://mf.example.com";

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

/// A sync with its own rate limiter, so tests do not share one.
fn syncer(pool: &PgPool) -> Syncer {
    Syncer::with_limiter(pool.clone(), WEB, Arc::new(Limiter::new(600)))
}

/// A connection to `halo`, with a category; its id.
async fn connect(pool: &PgPool, halo: &FakeHalo, min_severity: &str) -> Uuid {
    let secret = secrets::seal(Column::PsaConnections, CLIENT_SECRET).unwrap();
    sqlx::query_scalar(
        "INSERT INTO psa_connections (base_url, client_id, client_secret, ticket_type_id,
                                      ticket_type_name, category, min_severity)
         VALUES ($1, $2, $3, 1, 'Incident', 'Hardware>Server', $4) RETURNING id",
    )
    .bind(&halo.url)
    .bind(CLIENT_ID)
    .bind(secret)
    .bind(min_severity)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// A customer, a site and a server on it: `(site, server)`.
async fn place(pool: &PgPool, name: &str) -> (Uuid, Uuid) {
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
    let server = sqlx::query_scalar(
        "INSERT INTO servers (peer_id, site_id, hostname, os_family)
         VALUES ($1 || '-web', $2, $1 || '-web', 'debian') RETURNING id",
    )
    .bind(name)
    .bind(site)
    .fetch_one(pool)
    .await
    .unwrap();
    (site, server)
}

async fn link(pool: &PgPool, connection: Uuid, site: Uuid, client: i64, halo_site: Option<i64>) {
    sqlx::query(
        "INSERT INTO psa_site_links (site_id, connection_id, client_id, client_name, halo_site_id,
                                     halo_site_name)
         VALUES ($1, $2, $3, 'client', $4, CASE WHEN $4 IS NULL THEN NULL ELSE 'site' END)",
    )
    .bind(site)
    .bind(connection)
    .bind(client)
    .bind(halo_site)
    .execute(pool)
    .await
    .unwrap();
}

async fn open_incident(pool: &PgPool, server: Uuid, severity: &str) -> Uuid {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO incidents (server_id, metric, instance, severity, opened_at, last_value,
                                last_value_at, peak_value, threshold)
         VALUES ($1, 'disk_used_percent', '/var' || (SELECT count(*) FROM incidents), $2, now(), 97, now(),
                 97, '{}')
         RETURNING id",
    )
    .bind(server)
    .bind(severity)
    .fetch_one(pool)
    .await
    .unwrap();
    event(pool, id, "opened", Some(severity)).await;
    id
}

async fn event(pool: &PgPool, incident: Uuid, kind: &str, severity: Option<&str>) {
    if kind == "resolved" {
        sqlx::query("UPDATE incidents SET state = 'resolved', resolved_at = now() WHERE id = $1")
            .bind(incident)
            .execute(pool)
            .await
            .unwrap();
    }
    sqlx::query(
        "INSERT INTO incident_events (incident_id, occurred_at, kind, severity, value, details)
         VALUES ($1, now(), $2, $3, 97, '{}')",
    )
    .bind(incident)
    .bind(kind)
    .bind(severity)
    .execute(pool)
    .await
    .unwrap();
}

async fn ticket(pool: &PgPool, incident: Uuid) -> Option<(String, Option<i64>, Option<String>)> {
    sqlx::query_as("SELECT state, ticket_id, error FROM psa_tickets WHERE incident_id = $1")
        .bind(incident)
        .fetch_optional(pool)
        .await
        .unwrap()
}

async fn pending(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM psa_deliveries WHERE state = 'pending'")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Makes every pending delivery due now, as if its retry wait had passed.
async fn due_now(pool: &PgPool) {
    sqlx::query("UPDATE psa_deliveries SET next_at = now() WHERE state = 'pending'")
        .execute(pool)
        .await
        .unwrap();
}

db_tests! {
    #[sqlx::test]
    async fn an_incident_opens_one_ticket_and_its_resolution_closes_it(pool: PgPool) {
        secrets::install_for_tests();
        let halo = FakeHalo::start().await;
        let connection = connect(&pool, &halo, "critical").await;
        let (site, server) = place(&pool, "paris").await;
        link(&pool, connection, site, 10, Some(100)).await;
        let sync = syncer(&pool);

        let incident = open_incident(&pool, server, "critical").await;
        assert_eq!(sync.pass().await.unwrap(), 1);
        let created = halo.tickets();
        assert_eq!(created.len(), 1);
        let sent = &created[0];
        assert_eq!(sent["client_id"], 10);
        assert_eq!(sent["site_id"], 100);
        assert_eq!(sent["tickettype_id"], 1);
        assert_eq!(sent["category_1"], "Hardware>Server");
        assert_eq!(sent["summary"], "Critical: disk_used_percent (/var0) on paris-web");
        let details = sent["details"].as_str().unwrap();
        assert!(details.contains(&format!("{WEB}/incidents/view/?id={incident}")), "{details}");
        assert_eq!(ticket(&pool, incident).await, Some(("open".into(), Some(5001), None)));

        // A replayed event opens no second ticket.
        sqlx::query(
            "INSERT INTO psa_deliveries (event_seq, subject)
             SELECT seq, subject FROM integration_events WHERE event = 'incident.opened'",
        )
        .execute(&pool)
        .await
        .unwrap();
        sync.pass().await.unwrap();
        assert_eq!(halo.tickets().len(), 1);

        event(&pool, incident, "resolved", None).await;
        sync.pass().await.unwrap();
        let actions = halo.actions();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0]["ticket_id"], 5001);
        assert_eq!(actions[0]["hiddenfromuser"], true);
        let updates: Vec<Value> = halo.tickets().into_iter().filter(|t| t.get("id").is_some()).collect();
        assert_eq!(updates, [json!({"id": 5001, "status_id": 9})]);
        assert_eq!(ticket(&pool, incident).await.unwrap().0, "closed");
        assert_eq!(pending(&pool).await, 0);
    }

    #[sqlx::test]
    async fn severity_matches_and_the_off_switch_decide(pool: PgPool) {
        secrets::install_for_tests();
        let halo = FakeHalo::start().await;
        let connection = connect(&pool, &halo, "critical").await;
        let (site, server) = place(&pool, "paris").await;
        link(&pool, connection, site, 10, None).await;
        let (_, unmatched) = place(&pool, "lyon").await;
        let sync = syncer(&pool);

        // A warning opens nothing, until it becomes critical.
        let warning = open_incident(&pool, server, "warning").await;
        sync.pass().await.unwrap();
        assert!(halo.tickets().is_empty());
        assert_eq!(ticket(&pool, warning).await, None);
        event(&pool, warning, "escalated", Some("critical")).await;
        sync.pass().await.unwrap();
        assert_eq!(halo.tickets().len(), 1);
        assert!(halo.tickets()[0].get("site_id").is_none());

        // A site without a match: skipped, and said why.
        let elsewhere = open_incident(&pool, unmatched, "critical").await;
        sync.pass().await.unwrap();
        let (state, _, error) = ticket(&pool, elsewhere).await.unwrap();
        assert_eq!(state, "skipped");
        assert_eq!(
            error.unwrap(),
            "The site lyon is not matched to a HaloPSA client, and no default client is set."
        );

        // Turned off: no deliveries at all.
        sqlx::query("UPDATE psa_connections SET enabled = false").execute(&pool).await.unwrap();
        let off = open_incident(&pool, server, "critical").await;
        assert_eq!(sync.pass().await.unwrap(), 0);
        assert_eq!(ticket(&pool, off).await, None);
        assert_eq!(halo.tickets().len(), 1);
    }

    #[sqlx::test]
    async fn a_refusal_is_kept_and_not_retried(pool: PgPool) {
        secrets::install_for_tests();
        let halo = FakeHalo::start().await;
        halo.require_category();
        let connection = connect(&pool, &halo, "critical").await;
        sqlx::query("UPDATE psa_connections SET category = NULL").execute(&pool).await.unwrap();
        let (site, server) = place(&pool, "paris").await;
        link(&pool, connection, site, 10, None).await;
        let sync = syncer(&pool);
        let incident = open_incident(&pool, server, "critical").await;
        sync.pass().await.unwrap();
        let (state, id, error) = ticket(&pool, incident).await.unwrap();
        assert_eq!((state.as_str(), id), ("failed", None));
        assert!(error.unwrap().contains("Veuillez renseigner la catégorie"));
        assert_eq!(pending(&pool).await, 0);
        let last: Option<String> = sqlx::query_scalar("SELECT last_error FROM psa_connections")
            .fetch_one(&pool).await.unwrap();
        assert!(last.unwrap().contains("400"));
        // Its resolution has no ticket to close.
        event(&pool, incident, "resolved", None).await;
        sync.pass().await.unwrap();
        assert!(halo.actions().is_empty());
    }

    #[sqlx::test]
    async fn a_failure_is_retried_and_a_lost_answer_finds_its_ticket(pool: PgPool) {
        secrets::install_for_tests();
        let halo = FakeHalo::start().await;
        let connection = connect(&pool, &halo, "critical").await;
        let (site, server) = place(&pool, "paris").await;
        link(&pool, connection, site, 10, None).await;
        let sync = syncer(&pool);

        halo.refuse_tickets(503, "Service Unavailable");
        let incident = open_incident(&pool, server, "critical").await;
        sync.pass().await.unwrap();
        let (state, _, error) = ticket(&pool, incident).await.unwrap();
        assert_eq!(state, "creating");
        assert!(error.unwrap().contains("503"));
        assert_eq!(pending(&pool).await, 1);
        // Not due yet: nothing happens.
        halo.accept_tickets();
        sync.pass().await.unwrap();
        assert!(halo.tickets().is_empty());

        // The resolution waits behind the opening.
        event(&pool, incident, "resolved", None).await;
        due_now(&pool).await;
        sync.pass().await.unwrap();
        assert_eq!(halo.tickets().len(), 1);
        assert_eq!(ticket(&pool, incident).await.unwrap().0, "open");
        sync.pass().await.unwrap();
        assert_eq!(ticket(&pool, incident).await.unwrap().0, "closed");

        // Created, but the answer was lost: the retry finds it by the
        // incident's id instead of creating another.
        let lost = open_incident(&pool, server, "critical").await;
        sqlx::query("UPDATE psa_deliveries SET next_at = now() + interval '1 hour' WHERE state = 'pending'")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO psa_tickets (incident_id, connection_id, state) VALUES ($1, $2, 'creating')")
            .bind(lost).bind(connection).execute(&pool).await.unwrap();
        halo.inner.tickets.lock().unwrap().push(json!({
            "summary": "Critical", "details": format!("MonitorFlow incident: {lost}"), "client_id": 10,
        }));
        let before = halo.tickets().len();
        due_now(&pool).await;
        sync.pass().await.unwrap();
        assert_eq!(halo.tickets().len(), before);
        assert_eq!(ticket(&pool, lost).await, Some(("open".into(), Some(5000 + before as i64), None)));
        assert!(halo.requests().iter().any(|r| r.starts_with("GET /api/Tickets?") && r.contains(&lost.to_string())));
    }

    #[sqlx::test]
    async fn resolving_can_add_a_note_only(pool: PgPool) {
        secrets::install_for_tests();
        let halo = FakeHalo::start().await;
        let connection = connect(&pool, &halo, "warning").await;
        sqlx::query("UPDATE psa_connections SET resolved_status_id = NULL, resolved_status_name = NULL")
            .execute(&pool).await.unwrap();
        let (site, server) = place(&pool, "paris").await;
        link(&pool, connection, site, 10, None).await;
        let sync = syncer(&pool);
        let incident = open_incident(&pool, server, "warning").await;
        sync.pass().await.unwrap();
        event(&pool, incident, "resolved", None).await;
        sync.pass().await.unwrap();
        assert_eq!(halo.actions().len(), 1);
        assert!(halo.tickets().iter().all(|t| t.get("status_id").is_none()));
        assert_eq!(ticket(&pool, incident).await.unwrap().0, "closed");
    }

    #[sqlx::test]
    async fn a_customer_or_the_default_client_takes_unmatched_sites(pool: PgPool) {
        secrets::install_for_tests();
        let halo = FakeHalo::start().await;
        let connection = connect(&pool, &halo, "warning").await;
        sqlx::query(
            "UPDATE psa_connections SET warning_priority_id = 3, warning_priority_name = 'Medium',
                 critical_priority_id = 1, critical_priority_name = 'Critical'",
        )
        .execute(&pool).await.unwrap();
        let (site, server) = place(&pool, "paris").await;
        let customer: Uuid = sqlx::query_scalar("SELECT customer_id FROM sites WHERE id = $1")
            .bind(site).fetch_one(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO psa_customer_links (customer_id, connection_id, client_id, client_name)
             VALUES ($1, $2, 11, 'Example Logistics')",
        )
        .bind(customer).bind(connection).execute(&pool).await.unwrap();
        let (_, elsewhere) = place(&pool, "lyon").await;
        let sync = syncer(&pool);

        // The customer's client, without a site; the warning priority.
        open_incident(&pool, server, "warning").await;
        sync.pass().await.unwrap();
        let sent = halo.tickets();
        assert_eq!(sent[0]["client_id"], 11);
        assert!(sent[0].get("site_id").is_none());
        assert_eq!(sent[0]["priority_id"], 3);

        // No match at all: the default client and site.
        sqlx::query(
            "UPDATE psa_connections SET default_client_id = 12, default_client_name = 'Sample Corp',
                 default_site_id = 120, default_site_name = 'Main'",
        )
        .execute(&pool).await.unwrap();
        let incident = open_incident(&pool, elsewhere, "warning").await;
        sync.pass().await.unwrap();
        let sent = halo.tickets();
        assert_eq!((sent[1]["client_id"].as_i64(), sent[1]["site_id"].as_i64()), (Some(12), Some(120)));

        // Escalated: a note and the critical priority.
        event(&pool, incident, "escalated", Some("critical")).await;
        sync.pass().await.unwrap();
        assert_eq!(halo.actions().len(), 1);
        assert!(halo.actions()[0]["note"].as_str().unwrap().contains("critical"));
        let updates: Vec<Value> = halo.tickets().into_iter().filter(|t| t.get("id").is_some()).collect();
        assert_eq!(updates, [json!({"id": 5002, "priority_id": 1})]);
    }

    #[sqlx::test]
    async fn a_silence_holds_the_ticket_until_it_ends(pool: PgPool) {
        secrets::install_for_tests();
        let halo = FakeHalo::start().await;
        let connection = connect(&pool, &halo, "critical").await;
        let (site, server) = place(&pool, "paris").await;
        link(&pool, connection, site, 10, None).await;
        let sync = syncer(&pool);
        let silence: Uuid = sqlx::query_scalar(
            "INSERT INTO silences (site_id, starts_at, ends_at, reason)
             VALUES ($1, now() - interval '1 minute', now() + interval '1 hour', 'maintenance')
             RETURNING id",
        )
        .bind(site).fetch_one(&pool).await.unwrap();

        let still_open = open_incident(&pool, server, "critical").await;
        let gone = open_incident(&pool, server, "critical").await;
        sync.pass().await.unwrap();
        assert!(halo.tickets().is_empty());
        let (state, _, error) = ticket(&pool, still_open).await.unwrap();
        assert_eq!(state, "held");
        assert!(error.unwrap().starts_with("Held by a silence until "));
        // Held, not failing: no attempt counted, due after the silence.
        let (attempts, later): (i32, bool) = sqlx::query_as(
            "SELECT attempts, next_at > now() + interval '50 minutes' FROM psa_deliveries
             WHERE state = 'pending' ORDER BY id LIMIT 1",
        )
        .fetch_one(&pool).await.unwrap();
        assert_eq!((attempts, later), (0, true));

        // One resolves during the silence; then the silence ends.
        event(&pool, gone, "resolved", None).await;
        sqlx::query("UPDATE silences SET ended_at = now() WHERE id = $1")
            .bind(silence).execute(&pool).await.unwrap();
        due_now(&pool).await;
        sync.pass().await.unwrap();
        sync.pass().await.unwrap();
        assert_eq!(halo.tickets().len(), 1, "only the incident still open has a ticket");
        assert_eq!(ticket(&pool, still_open).await.unwrap().0, "open");
        let (state, _, error) = ticket(&pool, gone).await.unwrap();
        assert_eq!(state, "skipped");
        assert_eq!(error.unwrap(), "The incident opened and resolved during a silence.");
        assert!(halo.actions().is_empty());
        assert_eq!(pending(&pool).await, 0);
    }

    #[sqlx::test]
    async fn a_ticket_names_the_server_s_asset_when_the_psa_takes_it(pool: PgPool) {
        secrets::install_for_tests();
        let halo = FakeHalo::start().await;
        let connection = connect(&pool, &halo, "critical").await;
        let (site, server) = place(&pool, "paris").await;
        link(&pool, connection, site, 10, None).await;
        sqlx::query(
            "INSERT INTO psa_asset_links (server_id, connection_id, asset_id, state, matched_by)
             VALUES ($1, $2, 7001, 'linked', 'created')",
        )
        .bind(server).bind(connection).execute(&pool).await.unwrap();
        let sync = syncer(&pool);
        open_incident(&pool, server, "critical").await;
        sync.pass().await.unwrap();
        assert_eq!(halo.tickets()[0]["assets"], json!([{"id": 7001}]));

        // An instance that refuses it gets the ticket without.
        halo.refuse_assets_on_tickets();
        let second = open_incident(&pool, server, "critical").await;
        sync.pass().await.unwrap();
        assert_eq!(halo.tickets().len(), 2);
        assert!(halo.tickets()[1].get("assets").is_none());
        assert_eq!(ticket(&pool, second).await.unwrap().0, "open");
    }

    #[sqlx::test]
    async fn a_ticket_closed_in_the_psa_is_not_moved_at_resolution(pool: PgPool) {
        secrets::install_for_tests();
        let halo = FakeHalo::start().await;
        let connection = connect(&pool, &halo, "critical").await;
        sqlx::query("UPDATE psa_connections SET resolved_status_id = 20, resolved_status_name = 'Resolved'")
            .execute(&pool).await.unwrap();
        let (site, server) = place(&pool, "paris").await;
        link(&pool, connection, site, 10, None).await;
        let sync = syncer(&pool);
        let incident = open_incident(&pool, server, "critical").await;
        sync.pass().await.unwrap();
        // The poller saw it closed (9) in the PSA.
        sqlx::query("UPDATE psa_tickets SET psa_status_id = 9 WHERE incident_id = $1")
            .bind(incident).execute(&pool).await.unwrap();
        event(&pool, incident, "resolved", None).await;
        sync.pass().await.unwrap();
        assert_eq!(halo.actions().len(), 1);
        assert!(halo.tickets().iter().all(|t| t.get("status_id").is_none()));
        assert_eq!(ticket(&pool, incident).await.unwrap().0, "closed");
    }
}
