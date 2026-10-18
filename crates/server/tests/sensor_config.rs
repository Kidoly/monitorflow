//! `sensor_configs`: schema checks, precedence of site and server rows over
//! the built-in defaults, and revisions (ADR-0016).

use monitorflow_server::sensor_config::{SensorConfigStore, Spec};
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

const CHECK_VIOLATION: &str = "23514";
const UNIQUE_VIOLATION: &str = "23505";

async fn site(pool: &PgPool, name: &str) -> Uuid {
    sqlx::query_scalar("INSERT INTO sites (name) VALUES ($1) RETURNING id")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn server(pool: &PgPool, peer_id: &str, site_id: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO servers (peer_id, site_id, hostname, os_family)
         VALUES ($1, $2, $1, 'debian') RETURNING id",
    )
    .bind(peer_id)
    .bind(site_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Inserts a row for a site (`site_id`) or a server (`server_id`).
async fn insert(
    pool: &PgPool,
    owner: (Option<Uuid>, Option<Uuid>),
    name: &str,
    instance: &str,
    interval_ms: i64,
    params: serde_json::Value,
    enabled: bool,
) -> Result<Uuid, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO sensor_configs (site_id, server_id, kind, name, instance, interval_ms, params, enabled)
         VALUES ($1, $2, $3, $3, $4, $5, $6, $7) RETURNING id",
    )
    .bind(owner.0)
    .bind(owner.1)
    .bind(name)
    .bind(instance)
    .bind(interval_ms)
    .bind(params)
    .bind(enabled)
    .fetch_one(pool)
    .await
}

fn sqlstate<T: std::fmt::Debug>(result: Result<T, sqlx::Error>) -> String {
    let err = result.expect_err("statement should fail");
    err.as_database_error()
        .and_then(|e| e.code())
        .unwrap_or_else(|| panic!("not a database error: {err}"))
        .into_owned()
}

fn keys(specs: &[Spec]) -> Vec<(&str, &str, u64)> {
    specs
        .iter()
        .map(|s| (s.name.as_str(), s.instance.as_str(), s.interval_ms))
        .collect()
}

async fn revision(store: &SensorConfigStore, peer_id: &str) -> u64 {
    store.load(peer_id).await.unwrap().unwrap().revision
}

db_tests! {
    #[sqlx::test]
    async fn rows_follow_the_sensor_model(pool: PgPool) {
        let paris = site(&pool, "paris").await;
        let web = server(&pool, "web-01", paris).await;
        let at_site = (Some(paris), None);
        insert(&pool, at_site, "port", "22", 10_000, json!({"port": 22}), true).await.unwrap();
        // Exactly one owner.
        assert_eq!(sqlstate(insert(&pool, (None, None), "cpu", "", 60_000, json!({}), true).await), CHECK_VIOLATION);
        assert_eq!(sqlstate(insert(&pool, (Some(paris), Some(web)), "cpu", "", 60_000, json!({}), true).await), CHECK_VIOLATION);
        // ADR-0012 names and intervals.
        for (name, instance, interval) in [("CPU", "", 60_000), ("cpu", "a\nb", 60_000), ("cpu", "", 999), ("cpu", "", 86_400_001)] {
            assert_eq!(sqlstate(insert(&pool, at_site, name, instance, interval, json!({}), true).await), CHECK_VIOLATION, "{name} {instance:?} {interval}");
        }
        // Flat objects of scalars only.
        for params in [json!([1]), json!({"a": {"b": 1}}), json!({"a": [1]}), json!({"a": null})] {
            assert_eq!(sqlstate(insert(&pool, at_site, "cpu", "", 60_000, params.clone(), true).await), CHECK_VIOLATION, "{params}");
        }
        // A timeout longer than the interval.
        let long_timeout = sqlx::query(
            "INSERT INTO sensor_configs (site_id, kind, name, interval_ms, timeout_ms) VALUES ($1, 'cpu', 'cpu', 5000, 6000)",
        )
        .bind(paris)
        .execute(&pool)
        .await;
        assert_eq!(sqlstate(long_timeout), CHECK_VIOLATION);
        // (name, instance) is unique per owner, and may repeat across owners.
        assert_eq!(sqlstate(insert(&pool, at_site, "port", "22", 20_000, json!({}), true).await), UNIQUE_VIOLATION);
        insert(&pool, (None, Some(web)), "port", "22", 20_000, json!({}), true).await.unwrap();
    }

    #[sqlx::test]
    async fn server_rows_override_site_rows_which_override_defaults(pool: PgPool) {
        let paris = site(&pool, "paris").await;
        let lyon = site(&pool, "lyon").await;
        let web = server(&pool, "web-01", paris).await;
        server(&pool, "db-01", paris).await;
        server(&pool, "app-01", lyon).await;
        let store = SensorConfigStore::new(pool.clone());

        // Nothing configured: the built-in defaults.
        let defaults = store.load("web-01").await.unwrap().unwrap();
        assert_eq!(keys(&defaults.sensors), [("cpu", "", 60_000), ("disk", "", 60_000), ("memory", "", 60_000)]);
        assert!(store.load("nobody").await.unwrap().is_none());

        let (at_paris, at_web) = ((Some(paris), None), (None, Some(web)));
        insert(&pool, at_paris, "cpu", "", 30_000, json!({}), true).await.unwrap();
        insert(&pool, at_paris, "port", "22", 10_000, json!({"port": 22, "host": "localhost"}), true).await.unwrap();
        insert(&pool, at_paris, "port", "443", 10_000, json!({}), true).await.unwrap();
        insert(&pool, at_paris, "disk", "", 60_000, json!({}), false).await.unwrap();
        insert(&pool, at_web, "cpu", "", 5_000, json!({}), true).await.unwrap();
        insert(&pool, at_web, "port", "443", 10_000, json!({}), false).await.unwrap();
        insert(&pool, at_web, "service", "nginx", 15_000, json!({"unit": "nginx"}), true).await.unwrap();
        insert(&pool, (Some(lyon), None), "memory", "", 1_000, json!({}), false).await.unwrap();

        let web = store.load("web-01").await.unwrap().unwrap();
        assert_eq!(
            keys(&web.sensors),
            [("cpu", "", 5_000), ("memory", "", 60_000), ("port", "22", 10_000), ("service", "nginx", 15_000)]
        );
        let port = &web.sensors[2];
        assert_eq!((port.params["port"].as_str(), port.params["host"].as_str()), ("22", "localhost"));
        assert_eq!(web.sensors[3].params["unit"], "nginx");

        // Another server of the site sees the site rows only.
        let db = store.load("db-01").await.unwrap().unwrap();
        assert_eq!(
            keys(&db.sensors),
            [("cpu", "", 30_000), ("memory", "", 60_000), ("port", "22", 10_000), ("port", "443", 10_000)]
        );
        // Another site's rows stay there.
        let app = store.load("app-01").await.unwrap().unwrap();
        assert_eq!(keys(&app.sensors), [("cpu", "", 60_000), ("disk", "", 60_000)]);
    }

    #[sqlx::test]
    async fn every_change_of_either_layer_moves_the_revision(pool: PgPool) {
        let paris = site(&pool, "paris").await;
        let lyon = site(&pool, "lyon").await;
        let web = server(&pool, "web-01", paris).await;
        server(&pool, "db-01", paris).await;
        server(&pool, "app-01", lyon).await;
        let store = SensorConfigStore::new(pool.clone());
        assert_eq!(revision(&store, "web-01").await, 0);

        let mut last = 0;
        let mut moved = |now: u64| {
            assert!(now > last, "revision {now} not above {last}");
            last = now;
        };
        let row = insert(&pool, (Some(paris), None), "cpu", "", 30_000, json!({}), true).await.unwrap();
        moved(revision(&store, "web-01").await);
        insert(&pool, (None, Some(web)), "cpu", "", 5_000, json!({}), true).await.unwrap();
        moved(revision(&store, "web-01").await);
        sqlx::query("UPDATE sensor_configs SET interval_ms = 20000 WHERE id = $1").bind(row).execute(&pool).await.unwrap();
        moved(revision(&store, "web-01").await);
        sqlx::query("DELETE FROM sensor_configs WHERE id = $1").bind(row).execute(&pool).await.unwrap();
        moved(revision(&store, "web-01").await);

        // Site changes reach every server of the site, and only them.
        let mut revisions = store
            .revisions(&["web-01".into(), "db-01".into(), "app-01".into(), "nobody".into()])
            .await
            .unwrap();
        revisions.sort();
        assert_eq!(revisions, [("app-01".into(), 0), ("db-01".into(), 4), ("web-01".into(), last)]);

        // Deleting a server removes its rows without tripping the trigger.
        sqlx::query("DELETE FROM servers WHERE id = $1").bind(web).execute(&pool).await.unwrap();
        let left: i64 = sqlx::query_scalar("SELECT count(*) FROM sensor_configs").fetch_one(&pool).await.unwrap();
        assert_eq!(left, 0);
    }

    #[sqlx::test]
    async fn applied_revisions_are_recorded(pool: PgPool) {
        let paris = site(&pool, "paris").await;
        server(&pool, "web-01", paris).await;
        let store = SensorConfigStore::new(pool.clone());
        store.record_applied("web-01", 7).await.unwrap();
        let (applied, at): (Option<i64>, Option<chrono::DateTime<chrono::Utc>>) = sqlx::query_as(
            "SELECT sensor_revision_applied, sensor_applied_at FROM servers WHERE peer_id = 'web-01'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(applied, Some(7));
        assert!(at.is_some());
        // An unknown agent is not an error.
        store.record_applied("nobody", 1).await.unwrap();
    }
}
