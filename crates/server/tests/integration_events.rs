//! Events for integrations (ADR-0102): the triggers that write them from
//! incidents, servers, updates, campaigns and approvals, and the
//! deliveries each endpoint gets by what it wants and covers. Ignored
//! without `DATABASE_URL`.

use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

const SECRET: &str = "mfwh_test";
/// Every event, as `integration_events` checks them.
const ALL: [&str; 16] = [
    "incident.opened",
    "incident.escalated",
    "incident.acknowledged",
    "incident.resolved",
    "server.added",
    "server.removed",
    "server.offline",
    "server.online",
    "update.started",
    "update.succeeded",
    "update.failed",
    "campaign.started",
    "campaign.succeeded",
    "campaign.failed",
    "approval.waiting",
    "approval.decided",
];

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

async fn events(pool: &PgPool) -> Vec<(String, Option<Uuid>, String, Value)> {
    sqlx::query_as(
        "SELECT event, customer_id, subject, details FROM integration_events ORDER BY seq",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

/// An endpoint at `url` covering `customer` or `site` (or everything),
/// wanting `wanted` (or every event).
async fn endpoint(
    pool: &PgPool,
    url: &str,
    customer: Option<Uuid>,
    site: Option<Uuid>,
    wanted: Option<&[&str]>,
) -> Uuid {
    let wanted: Vec<String> = wanted
        .unwrap_or(&ALL)
        .iter()
        .map(|e| e.to_string())
        .collect();
    sqlx::query_scalar(
        "INSERT INTO integration_endpoints (name, url, secret, events, customer_id, site_id,
                                            allow_private)
         VALUES ('psa', $1, $2, $3, $4, $5, true) RETURNING id",
    )
    .bind(url)
    .bind(
        monitorflow_server::secrets::seal(
            monitorflow_server::secrets::Column::IntegrationEndpoints,
            SECRET,
        )
        .unwrap(),
    )
    .bind(&wanted)
    .bind(customer)
    .bind(site)
    .fetch_one(pool)
    .await
    .unwrap()
}

db_tests! {
    #[sqlx::test]
    async fn triggers_write_every_kind_of_event(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let (customer, site, server) = place(&pool, "acme").await;
        incident(
            &pool,
            server,
            "monitorflow_cpu_usage_percent",
            &[("opened", Some("warning"), None), ("escalated", Some("critical"), None),
              ("deescalated", Some("warning"), None), ("acknowledged", Some("warning"), None),
              ("resolved", None, None)],
        )
        .await;
        incident(
            &pool,
            server,
            "offline",
            &[("opened", Some("critical"), None), ("resolved", None, Some("back_online"))],
        )
        .await;
        sqlx::query("UPDATE servers SET revoked_at = now() WHERE id = $1")
            .bind(server)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE servers SET revoked_at = NULL WHERE id = $1")
            .bind(server)
            .execute(&pool)
            .await
            .unwrap();
        // An update outside a campaign starts and ends; other kinds and a
        // campaign's own commands are not updates of their own.
        for kind in ["apply_updates", "shell"] {
            sqlx::query(
                "INSERT INTO commands (server_id, kind, payload, expires_at)
                 VALUES ($1, $2, '', now() + interval '1 hour')",
            )
            .bind(server)
            .bind(kind)
            .execute(&pool)
            .await
            .unwrap();
        }
        sqlx::query("UPDATE commands SET state = 'running', started_at = now() WHERE kind = 'apply_updates'")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE commands SET state = 'cancelled', finished_at = now(), error = 'stopped'")
            .execute(&pool)
            .await
            .unwrap();
        let campaign: Uuid = sqlx::query_scalar(
            "INSERT INTO campaigns (customer_id, site_id, name) VALUES ($1, $2, 'May') RETURNING id",
        )
        .bind(customer)
        .bind(site)
        .fetch_one(&pool)
        .await
        .unwrap();
        for (state, finished) in [("running", false), ("paused", false), ("running", false), ("completed", true)] {
            sqlx::query(
                "UPDATE campaigns SET state = $2, started_at = now(),
                     finished_at = CASE WHEN $3 THEN now() END WHERE id = $1",
            )
            .bind(campaign)
            .bind(state)
            .bind(finished)
            .execute(&pool)
            .await
            .unwrap();
        }
        let remediation: Uuid = sqlx::query_scalar(
            "INSERT INTO remediation_incidents (customer_id, server_id, metric, max_commands, max_duration_s)
             VALUES ($1, $2, 'monitorflow_cpu_usage_percent', 5, 600) RETURNING id",
        )
        .bind(customer)
        .bind(server)
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO remediation_incident_events
                 (incident_id, seq, kind, actor_kind, actor_id, payload, prev_hash, hash)
             VALUES ($1, 1, 'ai_proposal', 'ai', 'test', '{}', decode(repeat('00', 32), 'hex'),
                     decode(repeat('00', 32), 'hex'))",
        )
        .bind(remediation)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO remediation_pending_commands
                 (incident_id, proposal_seq, command, diagnosis, reasoning, tier, rule, reason)
             VALUES ($1, 1, 'systemctl restart nginx', 'd', 'r', 'high_impact', '{}', 'unknown')",
        )
        .bind(remediation)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("UPDATE remediation_pending_commands SET state = 'rejected', decided_at = now()")
            .execute(&pool)
            .await
            .unwrap();

        let written = events(&pool).await;
        let kinds: Vec<&str> = written.iter().map(|(e, ..)| e.as_str()).collect();
        assert_eq!(
            kinds,
            [
                "server.added",
                "incident.opened", "incident.escalated", "incident.acknowledged",
                "incident.resolved",
                "incident.opened", "server.offline", "incident.resolved", "server.online",
                "server.removed", "server.added",
                "update.started", "update.failed",
                "campaign.started", "campaign.succeeded",
                "approval.waiting", "approval.decided",
            ]
        );
        assert!(written.iter().all(|(_, owner, ..)| *owner == Some(customer)));
        let (_, _, subject, details) = &written[1];
        assert!(subject.starts_with("incident:"), "{subject}");
        assert_eq!(details["server"]["name"], "acme-web");
        assert_eq!(details["site"]["name"], "acme-site");
        assert_eq!(details["customer"]["name"], "acme");
        assert_eq!(written[3].3["incident"]["acknowledged_by"]["name"], "alice");
        assert_eq!(written[6].2, format!("server:{server}"));
        assert_eq!(written[10].3["restored"], true);
        assert_eq!(written[0].3["restored"], false);
        assert_eq!(written[12].3["update"]["state"], "cancelled");
        assert_eq!(written[11].2, written[12].2);
        assert_eq!(written[14].3["campaign"]["name"], "May");
        assert_eq!(written[14].3["site"]["name"], "acme-site");
        assert_eq!(written[16].3["approval"]["state"], "rejected");
        assert_eq!(written[16].3["approval"]["command"], "systemctl restart nginx");
    }

    #[sqlx::test]
    async fn each_endpoint_gets_the_events_it_wants_and_covers(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let (acme, _, acme_server) = place(&pool, "acme").await;
        let (_, globex_site, globex_server) = place(&pool, "globex").await;
        let url = "https://psa.example/hook";
        let everything = endpoint(&pool, url, None, None, None).await;
        let acme_only = endpoint(&pool, url, Some(acme), None, None).await;
        let globex_site_only = endpoint(&pool, url, None, Some(globex_site), None).await;
        let servers_only = endpoint(&pool, url, None, None, Some(&["server.removed"])).await;
        let paused = endpoint(&pool, url, None, None, None).await;
        sqlx::query("UPDATE integration_endpoints SET state = 'paused' WHERE id = $1")
            .bind(paused)
            .execute(&pool)
            .await
            .unwrap();
        for server in [acme_server, globex_server] {
            incident(&pool, server, "monitorflow_cpu_usage_percent", &[("opened", Some("warning"), None)]).await;
            sqlx::query("UPDATE servers SET revoked_at = now() WHERE id = $1")
                .bind(server)
                .execute(&pool)
                .await
                .unwrap();
        }
        let count = |id: Uuid| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, i64>(
                    "SELECT count(*) FROM integration_deliveries WHERE endpoint_id = $1",
                )
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap()
            }
        };
        // Servers added before the endpoints existed are not sent.
        assert_eq!(count(everything).await, 4);
        assert_eq!(count(acme_only).await, 2);
        assert_eq!(count(globex_site_only).await, 2);
        assert_eq!(count(servers_only).await, 2);
        assert_eq!(count(paused).await, 0);
    }
}
