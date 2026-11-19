//! Dependencies (ADR-0152) against a real PostgreSQL: the schema refuses
//! what makes no sense and dependencies across customers, and the store
//! reads which parents are down, in doubt or recovering, so that holds come
//! out right. Ignored without `DATABASE_URL`.

use std::collections::HashSet;

use chrono::Utc;
use monitorflow_common::tls::PeerRole;
use monitorflow_server::dependencies::holds::Hold;
use monitorflow_server::dependencies::store::{self, Streams};
use monitorflow_server::dependencies::{Holds, Node};
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

async fn one(pool: &PgPool, sql: &'static str, binds: &[Uuid]) -> Uuid {
    let mut query = sqlx::query_scalar(sql);
    for bind in binds {
        query = query.bind(bind);
    }
    query.fetch_one(pool).await.unwrap()
}

async fn site(pool: &PgPool, name: &str) -> Uuid {
    let customer = sqlx::query_scalar("INSERT INTO customers (name) VALUES ($1) RETURNING id")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap();
    sqlx::query_scalar("INSERT INTO sites (name, customer_id) VALUES ($1, $2) RETURNING id")
        .bind(name)
        .bind::<Uuid>(customer)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn agent(pool: &PgPool, site: Uuid, name: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO servers (peer_id, site_id, hostname, os_family, last_seen_at)
         VALUES ($1, $2, $1, 'debian', now()) RETURNING id",
    )
    .bind(name)
    .bind(site)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn device(pool: &PgPool, site: Uuid, name: &str) -> Uuid {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO servers (peer_id, site_id, hostname, os_family, kind, address, package_updates)
         VALUES ($1, $2, $1, 'device', 'device', '192.0.2.1', false) RETURNING id",
    )
    .bind(name)
    .bind(site)
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO sensor_configs (server_id, kind, name, interval_ms, probe)
         VALUES ($1, 'ping', 'ping', 60000, 'central')",
    )
    .bind(id)
    .execute(pool)
    .await
    .unwrap();
    id
}

async fn depend(pool: &PgPool, child: Uuid, parent: Uuid) -> Result<Uuid, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO dependencies (server_id, parent_server_id) VALUES ($1, $2) RETURNING id",
    )
    .bind(child)
    .bind(parent)
    .fetch_one(pool)
    .await
}

async fn incident(
    pool: &PgPool,
    server: Uuid,
    metric: &str,
    severity: &str,
    resolved: bool,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO incidents (server_id, metric, instance, severity, state, opened_at, resolved_at,
                                last_value, last_value_at, peak_value, threshold)
         VALUES ($1, $2, '', $3, CASE WHEN $4 THEN 'resolved' ELSE 'open' END,
                 now() - interval '5 minutes', CASE WHEN $4 THEN now() - interval '1 minute' END,
                 0, now(), 0, '{}')
         RETURNING id",
    )
    .bind(server)
    .bind(metric)
    .bind(severity)
    .bind(resolved)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn holds(pool: &PgPool, online: &HashSet<(PeerRole, String)>) -> Holds {
    let mut conn = pool.acquire().await.unwrap();
    store::holds(&mut conn, Streams::Live(online))
        .await
        .unwrap()
}

db_tests! {
    #[sqlx::test]
    async fn the_schema_refuses_nonsense_and_other_customers(pool: PgPool) {
        let a = site(&pool, "acme").await;
        let b = site(&pool, "globex").await;
        let web = agent(&pool, a, "web-1").await;
        let switch = device(&pool, a, "sw-core").await;
        let theirs = device(&pool, b, "sw-other").await;
        depend(&pool, web, switch).await.unwrap();
        // Twice is once.
        assert!(depend(&pool, web, switch).await.is_err());
        // Never across customers, whoever writes it.
        let error = depend(&pool, web, theirs).await.unwrap_err();
        assert!(error.to_string().contains("never crosses customers"), "{error}");
        // Not on itself, but a check may depend on another of its server's.
        assert!(depend(&pool, web, web).await.is_err());
        sqlx::query(
            "INSERT INTO dependencies (server_id, check_name, parent_server_id, parent_check_name)
             VALUES ($1, 'https', $1, 'ping')",
        )
        .bind(switch)
        .execute(&pool)
        .await
        .unwrap();
        // A relay of another customer is refused too.
        let relay = one(
            &pool,
            "INSERT INTO relays (peer_id, site_id, name) VALUES ('relay-b', $1, 'relay-b') RETURNING id",
            &[b],
        )
        .await;
        let error = sqlx::query("INSERT INTO dependencies (server_id, parent_relay_id) VALUES ($1, $2)")
            .bind(web)
            .bind(relay)
            .execute(&pool)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("never crosses customers"), "{error}");
        // Removing the parent removes the dependency.
        sqlx::query("DELETE FROM servers WHERE id = $1").bind(switch).execute(&pool).await.unwrap();
        let left: i64 = sqlx::query_scalar("SELECT count(*) FROM dependencies")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(left, 0);
    }

    #[sqlx::test]
    async fn a_device_whose_ping_is_critical_holds_what_is_behind_it(pool: PgPool) {
        let a = site(&pool, "acme").await;
        let switch = device(&pool, a, "sw-core").await;
        let web = agent(&pool, a, "web-1").await;
        let printer = device(&pool, a, "printer").await;
        depend(&pool, web, switch).await.unwrap();
        depend(&pool, printer, switch).await.unwrap();
        let online = HashSet::from([(PeerRole::Agent, "web-1".to_owned())]);
        assert!(holds(&pool, &online).await.of(&Node::Server(web)).is_none());

        // A warning is not down.
        let warning = incident(&pool, switch, "monitorflow_ping_loss_ratio", "warning", false).await;
        assert!(holds(&pool, &online).await.of(&Node::Server(web)).is_none());
        // Pending critical: in doubt.
        sqlx::query(
            "INSERT INTO alert_states (server_id, metric, instance, critical_since, last_sample_at)
             VALUES ($1, 'monitorflow_ping_up', '', now(), now())",
        )
        .bind(switch)
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(holds(&pool, &online).await.of(&Node::Server(web)), Some(&Hold::Doubt));
        // Critical: down, and both children are unreachable because of it.
        let down = incident(&pool, switch, "monitorflow_ping_up", "critical", false).await;
        let now = holds(&pool, &online).await;
        for child in [web, printer] {
            assert_eq!(
                now.of(&Node::Server(child)),
                Some(&Hold::Unreachable { cause: Node::Server(switch), incident: down })
            );
        }
        assert_eq!(now.series(web, "offline", ""), now.of(&Node::Server(web)).cloned());
        assert_eq!(now.behind(&Node::Server(switch)).len(), 2);
        // Resolved a minute ago: settling for one more minute; the warning
        // never counted.
        sqlx::query("UPDATE incidents SET state = 'resolved', resolved_at = now() - interval '1 minute' WHERE id = ANY($1)")
            .bind([down, warning])
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM alert_states").execute(&pool).await.unwrap();
        let now = holds(&pool, &online).await;
        let Some(Hold::Settling { until }) = now.of(&Node::Server(web)) else {
            panic!("{:?}", now.of(&Node::Server(web)));
        };
        assert!(*until > Utc::now() && *until < Utc::now() + chrono::TimeDelta::seconds(70));
        assert!(now.heard(&Node::Server(web)).is_some());
    }

    #[sqlx::test]
    async fn agents_and_relays_are_in_doubt_without_a_stream_and_down_once_offline(pool: PgPool) {
        let a = site(&pool, "acme").await;
        let host = agent(&pool, a, "gw-host").await;
        let web = agent(&pool, a, "web-1").await;
        depend(&pool, web, host).await.unwrap();
        let relay = one(
            &pool,
            "INSERT INTO relays (peer_id, site_id, name) VALUES ('relay-1', $1, 'relay-1') RETURNING id",
            &[a],
        )
        .await;
        let db = agent(&pool, a, "db-1").await;
        sqlx::query("UPDATE servers SET relay_id = $1 WHERE id = $2")
            .bind(relay)
            .bind(db)
            .execute(&pool)
            .await
            .unwrap();
        // A device checked from the relay is behind it automatically.
        let nas = device(&pool, a, "nas").await;
        sqlx::query("UPDATE sensor_configs SET probe = 'relay', probe_relay_id = $1 WHERE server_id = $2")
            .bind(relay)
            .bind(nas)
            .execute(&pool)
            .await
            .unwrap();

        let all = HashSet::from([
            (PeerRole::Agent, "gw-host".to_owned()),
            (PeerRole::Relay, "relay-1".to_owned()),
        ]);
        let now = holds(&pool, &all).await;
        assert!(now.of(&Node::Server(web)).is_none());
        assert!(now.of(&Node::Server(db)).is_none());

        let none = HashSet::new();
        let now = holds(&pool, &none).await;
        assert_eq!(now.of(&Node::Server(web)), Some(&Hold::Doubt));
        assert_eq!(now.of(&Node::Server(db)), Some(&Hold::Doubt));
        assert_eq!(now.series(nas, "monitorflow_ping_up", ""), Some(Hold::Doubt));

        // Turned off: never in doubt, as it never opens an incident.
        sqlx::query("UPDATE servers SET offline_alerts = false WHERE id = $1")
            .bind(host)
            .execute(&pool)
            .await
            .unwrap();
        assert!(holds(&pool, &none).await.of(&Node::Server(web)).is_none());
        sqlx::query("UPDATE servers SET offline_alerts = true WHERE id = $1")
            .bind(host)
            .execute(&pool)
            .await
            .unwrap();

        let offline = incident(&pool, host, "offline", "critical", false).await;
        let relay_down: Uuid = sqlx::query_scalar(
            "INSERT INTO incidents (relay_id, metric, instance, severity, opened_at, last_value,
                                    last_value_at, peak_value, threshold)
             VALUES ($1, 'offline', '', 'critical', now(), 0, now(), 0, '{}') RETURNING id",
        )
        .bind(relay)
        .fetch_one(&pool)
        .await
        .unwrap();
        let now = holds(&pool, &none).await;
        assert_eq!(
            now.of(&Node::Server(web)),
            Some(&Hold::Unreachable { cause: Node::Server(host), incident: offline })
        );
        assert_eq!(
            now.of(&Node::Server(db)),
            Some(&Hold::Unreachable { cause: Node::Relay(relay), incident: relay_down })
        );
        assert!(now.series(nas, "monitorflow_ping_up", "").unwrap().unreachable().is_some());
        // An incident hidden by another never makes its server down.
        sqlx::query("UPDATE incidents SET hidden_by = $1 WHERE id = $2")
            .bind(relay_down)
            .bind(offline)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(holds(&pool, &none).await.of(&Node::Server(web)), Some(&Hold::Doubt));
        // A removed parent holds nothing.
        sqlx::query("UPDATE servers SET revoked_at = now() WHERE id = $1")
            .bind(host)
            .execute(&pool)
            .await
            .unwrap();
        assert!(holds(&pool, &none).await.of(&Node::Server(web)).is_none());
    }
}
