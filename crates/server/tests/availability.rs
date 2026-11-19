//! Availability reports (ADR-0155) from what PostgreSQL holds: incidents
//! and their events, offline incidents of a relay, silences counted from
//! their creation, and business services. Ignored without `DATABASE_URL`.

use chrono::{DateTime, TimeDelta, Utc};
use monitorflow_server::availability::data::{load, Reach};
use monitorflow_server::availability::report::build;
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

/// Minute `m` of the day the tests look at.
fn t(m: i64) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
        + TimeDelta::minutes(m)
}

struct Place {
    customer: Uuid,
    site: Uuid,
    relay: Uuid,
    web: Uuid,
    db: Uuid,
}

async fn place(pool: &PgPool, name: &str) -> Place {
    let customer: Uuid =
        sqlx::query_scalar("INSERT INTO customers (name) VALUES ($1) RETURNING id")
            .bind(name)
            .fetch_one(pool)
            .await
            .unwrap();
    let site: Uuid =
        sqlx::query_scalar("INSERT INTO sites (name, customer_id) VALUES ($1, $2) RETURNING id")
            .bind(format!("{name}-site"))
            .bind(customer)
            .fetch_one(pool)
            .await
            .unwrap();
    let relay: Uuid = sqlx::query_scalar(
        "INSERT INTO relays (peer_id, site_id, name) VALUES ($1, $2, $1) RETURNING id",
    )
    .bind(format!("{name}-relay"))
    .bind(site)
    .fetch_one(pool)
    .await
    .unwrap();
    let mut ids = Vec::new();
    for (host, behind) in [("web", true), ("db", false)] {
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO servers (peer_id, site_id, hostname, os_family, relay_id, created_at)
             VALUES ($1, $2, $1, 'debian', $3, $4) RETURNING id",
        )
        .bind(format!("{name}-{host}"))
        .bind(site)
        .bind(behind.then_some(relay))
        .bind(t(-100_000))
        .fetch_one(pool)
        .await
        .unwrap();
        ids.push(id);
    }
    Place {
        customer,
        site,
        relay,
        web: ids[0],
        db: ids[1],
    }
}

/// An incident of `server` or `relay` with its events, each `(kind,
/// severity, minute)`.
async fn incident(
    pool: &PgPool,
    server: Option<Uuid>,
    relay: Option<Uuid>,
    metric: &str,
    events: &[(&str, Option<&str>, i64)],
) {
    let opened = events[0].2;
    let resolved = events.iter().find(|e| e.0 == "resolved").map(|e| t(e.2));
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO incidents (server_id, relay_id, metric, instance, severity, state, opened_at,
                                resolved_at, last_value, last_value_at, peak_value, threshold)
         VALUES ($1, $2, $3, '', $4, $5, $6, $7, 0, $6, 0, '{}') RETURNING id",
    )
    .bind(server)
    .bind(relay)
    .bind(metric)
    .bind(events[0].1.unwrap())
    .bind(if resolved.is_some() {
        "resolved"
    } else {
        "open"
    })
    .bind(t(opened))
    .bind(resolved)
    .fetch_one(pool)
    .await
    .unwrap();
    for (kind, severity, at) in events {
        sqlx::query(
            "INSERT INTO incident_events (incident_id, occurred_at, kind, severity)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(id)
        .bind(t(*at))
        .bind(kind)
        .bind(severity)
        .execute(pool)
        .await
        .unwrap();
    }
}

async fn silence(pool: &PgPool, server: Uuid, created: i64, start: i64, end: i64) {
    sqlx::query(
        "INSERT INTO silences (server_id, starts_at, ends_at, reason, created_at)
         VALUES ($1, $2, $3, 'Patches', $4)",
    )
    .bind(server)
    .bind(t(start))
    .bind(t(end))
    .bind(t(created))
    .execute(pool)
    .await
    .unwrap();
}

fn everything() -> Reach {
    Reach {
        sites: None,
        services_of: None,
    }
}

db_tests! {
    #[sqlx::test]
    async fn numbers_come_from_incidents_and_silences(pool: PgPool) {
        let p = place(&pool, "acme").await;
        // Web: disk warning then critical 100-130; offline 200-260 with a
        // silence planned the day before for 250-400.
        incident(&pool, Some(p.web), None, "monitorflow_disk_used_ratio", &[
            ("opened", Some("warning"), 90),
            ("escalated", Some("critical"), 100),
            ("resolved", None, 130),
        ]).await;
        incident(&pool, Some(p.web), None, "offline", &[
            ("opened", Some("critical"), 200),
            ("resolved", None, 260),
        ]).await;
        silence(&pool, p.web, -1000, 250, 400).await;
        // The relay is offline 500-520: web is unreachable then.
        incident(&pool, None, Some(p.relay), "offline", &[
            ("opened", Some("critical"), 500),
            ("resolved", None, 520),
        ]).await;
        // Db: offline 600-700, and a silence made at 650 during it, which
        // counts only from then.
        incident(&pool, Some(p.db), None, "offline", &[
            ("opened", Some("critical"), 600),
            ("resolved", None, 700),
        ]).await;
        silence(&pool, p.db, 650, 590, 800).await;

        let mut db = pool.acquire().await.unwrap();
        let period = (t(0), t(1440));
        let data = load(&mut db, &everything(), period, t(5000)).await.unwrap();
        let report = build(&data, period, t(5000));
        let web = report.machines.iter().find(|m| m.id == p.web).unwrap();
        assert_eq!(web.numbers.down_seconds, (30.0 + 50.0 + 20.0) * 60.0);
        assert_eq!(web.numbers.maintenance_seconds, 150.0 * 60.0);
        assert_eq!(web.numbers.outages, 3);
        assert_eq!(web.outage_list[2].causes[0].kind, "unreachable");
        assert_eq!(web.outage_list[2].causes[0].via.as_deref(), Some("acme-relay"));
        let db_server = report.machines.iter().find(|m| m.id == p.db).unwrap();
        assert_eq!(db_server.numbers.down_seconds, 50.0 * 60.0);
        assert_eq!(db_server.numbers.maintenance_seconds, 150.0 * 60.0);
        assert_eq!(report.checks.len(), 1);
        assert_eq!(report.checks[0].name, "disk");
    }

    #[sqlx::test]
    async fn a_report_of_one_customer_reads_only_its_sites(pool: PgPool) {
        let acme = place(&pool, "acme").await;
        let other = place(&pool, "other").await;
        incident(&pool, Some(other.db), None, "offline", &[
            ("opened", Some("critical"), 10),
            ("resolved", None, 20),
        ]).await;
        sqlx::query(
            "INSERT INTO business_services (customer_id, name, rule, at_least, target)
             VALUES ($1, 'Email', 'at_least', 1, 99.9)",
        )
        .bind(acme.customer)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO business_services (customer_id, name) VALUES ($1, 'Other')",
        )
        .bind(other.customer)
        .execute(&pool)
        .await
        .unwrap();
        // A member of another customer, written past the API, is ignored.
        sqlx::query(
            "INSERT INTO business_service_members (service_id, server_id)
             SELECT id, $1 FROM business_services WHERE name = 'Email'
             UNION ALL SELECT id, $2 FROM business_services WHERE name = 'Email'",
        )
        .bind(acme.web)
        .bind(other.db)
        .execute(&pool)
        .await
        .unwrap();
        let reach = Reach {
            sites: Some(vec![acme.site]),
            services_of: Some(vec![acme.customer]),
        };
        let mut db = pool.acquire().await.unwrap();
        let period = (t(0), t(1440));
        let report = build(&load(&mut db, &reach, period, t(5000)).await.unwrap(), period, t(5000));
        let names: Vec<_> = report.machines.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["acme-db", "acme-web"]);
        assert_eq!(report.services.len(), 1);
        assert_eq!(report.services[0].members.len(), 1);
        assert_eq!(report.services[0].met, Some(true));
        assert_eq!(report.summary.outages, 0);
    }

    #[sqlx::test]
    async fn what_depends_on_a_parent_is_unreachable(pool: PgPool) {
        let p = place(&pool, "acme").await;
        // A switch of another site of the customer, which the report of
        // the first site does not cover, and db behind it.
        let other: Uuid = sqlx::query_scalar(
            "INSERT INTO sites (name, customer_id) VALUES ('acme-lyon', $1) RETURNING id",
        )
        .bind(p.customer)
        .fetch_one(&pool)
        .await
        .unwrap();
        let switch: Uuid = sqlx::query_scalar(
            "INSERT INTO servers (peer_id, site_id, hostname, os_family, kind, address, created_at,
                                  package_updates)
             VALUES ('sw-core', $1, 'sw-core', 'device', 'device', '192.0.2.1', $2, false)
             RETURNING id",
        )
        .bind(other)
        .bind(t(-100_000))
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO dependencies (server_id, parent_server_id) VALUES ($1, $2)")
            .bind(p.db)
            .bind(switch)
            .execute(&pool)
            .await
            .unwrap();
        incident(&pool, Some(switch), None, "monitorflow_ping_loss_ratio", &[
            ("opened", Some("critical"), 100),
            ("resolved", None, 125),
        ]).await;
        sqlx::query(
            "INSERT INTO sensor_configs (server_id, kind, name, interval_ms, params, probe)
             VALUES ($1, 'ping', 'ping', 60000, '{}', 'central')",
        )
        .bind(switch)
        .execute(&pool)
        .await
        .unwrap();
        let reach = Reach {
            sites: Some(vec![p.site]),
            services_of: Some(vec![]),
        };
        let mut db = pool.acquire().await.unwrap();
        let period = (t(0), t(1440));
        let report = build(&load(&mut db, &reach, period, t(5000)).await.unwrap(), period, t(5000));
        let names: Vec<_> = report.machines.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["acme-db", "acme-web"]);
        let db_server = report.machines.iter().find(|m| m.id == p.db).unwrap();
        assert_eq!(db_server.numbers.down_seconds, 25.0 * 60.0);
        assert_eq!(db_server.outage_list[0].causes[0].kind, "unreachable");
        // The switch is on a site this reader cannot read: not named, as
        // the dependencies API does not name it to them either.
        assert_eq!(db_server.outage_list[0].causes[0].via, None);
        let both = Reach {
            sites: Some(vec![p.site, other]),
            services_of: Some(vec![]),
        };
        let report = build(&load(&mut db, &both, period, t(5000)).await.unwrap(), period, t(5000));
        let db_server = report.machines.iter().find(|m| m.id == p.db).unwrap();
        assert_eq!(db_server.outage_list[0].causes[0].via.as_deref(), Some("sw-core"));
    }
}
