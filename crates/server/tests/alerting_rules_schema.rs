//! Schema of richer alert rules (ADR-0153): each kind has exactly its
//! fields, limits run the kind's way, and thresholds, incidents and alert
//! states are keyed by kind too.

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

const CHECK_VIOLATION: &str = "23514";
const UNIQUE_VIOLATION: &str = "23505";

async fn server(pool: &PgPool) -> Uuid {
    let site: Uuid = sqlx::query_scalar("INSERT INTO sites (name) VALUES ('paris') RETURNING id")
        .fetch_one(pool)
        .await
        .unwrap();
    sqlx::query_scalar(
        "INSERT INTO servers (peer_id, site_id, hostname, os_family)
         VALUES ('web-1', $1, 'web-1', 'debian') RETURNING id",
    )
    .bind(site)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Inserts a row of `server` from JSON; the error code on failure.
async fn insert(pool: &PgPool, server: Uuid, row: Value) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO thresholds (server_id, metric, instance, comparison, warning, critical,
                                 duration_s, kind, function, window_s, offset_s, target,
                                 conditions)
         SELECT $1, coalesce(r.metric, 'monitorflow_disk_used_ratio'), r.instance,
                coalesce(r.comparison, '>'), r.warning, r.critical, coalesce(r.duration_s, 0),
                coalesce(r.kind, 'value'), r.function, r.window_s, r.offset_s, r.target,
                coalesce(r.conditions, '[]')
         FROM jsonb_to_record($2) AS r(metric text, instance text, comparison text,
              warning float8, critical float8, duration_s int, kind text, function text,
              window_s int, offset_s int, target float8, conditions jsonb)",
    )
    .bind(server)
    .bind(row)
    .execute(pool)
    .await
    .map(|_| ())
    .map_err(|e| {
        e.as_database_error()
            .and_then(|d| d.code())
            .map(|c| c.into_owned())
            .unwrap_or_default()
    })
}

db_tests! {
    #[sqlx::test(migrations = "./migrations")]
    async fn each_kind_takes_exactly_its_fields(pool: PgPool) {
        let web = server(&pool).await;
        let ok = [
            json!({"warning": 0.8}),
            json!({"metric": "monitorflow_cpu_load1", "kind": "window", "function": "rate",
                   "window_s": 60, "warning": 1}),
            json!({"metric": "monitorflow_cpu_load1", "instance": "", "kind": "compare",
                   "function": "ratio", "window_s": 300, "offset_s": 604800, "warning": 2}),
            json!({"kind": "forecast", "comparison": ">=", "window_s": 86400, "target": 1,
                   "warning": 259200, "critical": 86400}),
            json!({"kind": "nodata", "warning": 300, "critical": 900}),
        ];
        for row in ok {
            insert(&pool, web, row.clone()).await.unwrap_or_else(|e| panic!("{row}: {e}"));
        }
        let refused = [
            // A plain threshold has no window.
            json!({"instance": "/a", "warning": 0.8, "window_s": 600}),
            // A window needs its function, one of the window ones.
            json!({"instance": "/b", "kind": "window", "window_s": 600, "warning": 1}),
            json!({"instance": "/c", "kind": "window", "function": "ratio", "window_s": 600,
                   "warning": 1}),
            json!({"instance": "/d", "kind": "window", "function": "avg", "window_s": 90000,
                   "warning": 1}),
            // A comparison needs one day or one week.
            json!({"instance": "/e", "kind": "compare", "function": "ratio", "window_s": 300,
                   "offset_s": 3600, "warning": 2}),
            // A forecast needs its target and at least an hour.
            json!({"instance": "/f", "kind": "forecast", "window_s": 86400, "warning": 1}),
            json!({"instance": "/g", "kind": "forecast", "window_s": 600, "target": 1,
                   "warning": 1}),
            // Critical is less time left than warning.
            json!({"instance": "/h", "kind": "forecast", "window_s": 3600, "target": 1,
                   "warning": 3600, "critical": 7200}),
            // No data: a longer silence for critical, no duration, no conditions.
            json!({"instance": "/i", "kind": "nodata", "warning": 900, "critical": 300}),
            json!({"instance": "/j", "kind": "nodata", "warning": 300, "duration_s": 60}),
            json!({"instance": "/k", "kind": "nodata", "warning": 300,
                   "conditions": [{"metric": "monitorflow_cpu_load1", "comparison": ">", "value": 1}]}),
            // At most two conditions.
            json!({"instance": "/l", "warning": 0.8, "conditions": [{}, {}, {}]}),
            json!({"instance": "/m", "kind": "median", "warning": 0.8}),
        ];
        for row in refused {
            assert_eq!(insert(&pool, web, row.clone()).await, Err(CHECK_VIOLATION.into()), "{row}");
        }
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn one_series_has_one_row_and_one_open_incident_per_kind(pool: PgPool) {
        let web = server(&pool).await;
        let forecast = json!({"kind": "forecast", "comparison": ">=", "window_s": 86400,
                              "target": 1, "warning": 259200});
        insert(&pool, web, json!({"warning": 0.8})).await.unwrap();
        insert(&pool, web, forecast.clone()).await.unwrap();
        assert_eq!(insert(&pool, web, forecast).await, Err(UNIQUE_VIOLATION.into()));
        let open = |kind: &'static str| {
            sqlx::query(
                "INSERT INTO incidents (server_id, metric, instance, severity, opened_at,
                                        last_value, last_value_at, peak_value, threshold, kind)
                 VALUES ($1, 'monitorflow_disk_used_ratio', '/', 'warning', now(), 1, now(), 1,
                         '{}', $2)",
            )
            .bind(web)
            .bind(kind)
        };
        open("value").execute(&pool).await.unwrap();
        open("forecast").execute(&pool).await.unwrap();
        let again = open("forecast").execute(&pool).await.unwrap_err();
        assert_eq!(again.as_database_error().unwrap().code().unwrap(), UNIQUE_VIOLATION);
        let state = |kind: &'static str| {
            sqlx::query(
                "INSERT INTO alert_states (server_id, metric, instance, severity, last_sample_at,
                                           kind)
                 VALUES ($1, 'monitorflow_disk_used_ratio', '/', 'warning', now(), $2)",
            )
            .bind(web)
            .bind(kind)
        };
        state("value").execute(&pool).await.unwrap();
        state("nodata").execute(&pool).await.unwrap();
        assert!(state("nodata").execute(&pool).await.is_err());
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn notifications_say_the_kind_and_the_rule(pool: PgPool) {
        let web = server(&pool).await;
        let incident: Uuid = sqlx::query_scalar(
            "INSERT INTO incidents (server_id, metric, instance, severity, opened_at, last_value,
                                    last_value_at, peak_value, threshold, kind)
             VALUES ($1, 'monitorflow_disk_used_ratio', '/', 'warning', now(), 7200, now(),
                     7200, '{\"target\": 1}', 'forecast')
             RETURNING id",
        )
        .bind(web)
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO incident_events (incident_id, occurred_at, kind, severity, value)
             VALUES ($1, now(), 'opened', 'warning', 7200)",
        )
        .bind(incident)
        .execute(&pool)
        .await
        .unwrap();
        let details: Value = sqlx::query_scalar(
            "SELECT details -> 'incident' FROM notification_outbox ORDER BY id DESC LIMIT 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(details["kind"], "forecast");
        assert_eq!(details["threshold"]["target"], 1);
    }
}
