//! Schema of the threshold engine (ADR-0017): threshold rows, one open
//! incident per series, append-only incident events, revisions.

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
const INSUFFICIENT_PRIVILEGE: &str = "42501";

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

struct Threshold<'a> {
    owner: (Option<Uuid>, Option<Uuid>),
    metric: &'a str,
    instance: Option<&'a str>,
    comparison: &'a str,
    warning: Option<f64>,
    critical: Option<f64>,
    duration_s: i32,
    hysteresis: f64,
    enabled: bool,
}

impl Threshold<'_> {
    fn at_site(site_id: Uuid) -> Self {
        Threshold {
            owner: (Some(site_id), None),
            metric: "monitorflow_disk_used_ratio",
            instance: None,
            comparison: ">",
            warning: Some(0.8),
            critical: Some(0.9),
            duration_s: 60,
            hysteresis: 0.0,
            enabled: true,
        }
    }

    async fn insert(&self, pool: &PgPool) -> Result<Uuid, sqlx::Error> {
        sqlx::query_scalar(
            "INSERT INTO thresholds (site_id, server_id, metric, instance, comparison, warning,
                                     critical, duration_s, hysteresis, enabled)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING id",
        )
        .bind(self.owner.0)
        .bind(self.owner.1)
        .bind(self.metric)
        .bind(self.instance)
        .bind(self.comparison)
        .bind(self.warning)
        .bind(self.critical)
        .bind(self.duration_s)
        .bind(self.hysteresis)
        .bind(self.enabled)
        .fetch_one(pool)
        .await
    }
}

fn sqlstate<T: std::fmt::Debug>(result: Result<T, sqlx::Error>) -> String {
    let err = result.expect_err("statement should fail");
    err.as_database_error()
        .and_then(|e| e.code())
        .unwrap_or_else(|| panic!("not a database error: {err}"))
        .into_owned()
}

async fn open_incident(
    pool: &PgPool,
    server_id: Uuid,
    instance: &str,
) -> Result<Uuid, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO incidents (server_id, metric, instance, severity, opened_at, last_value,
                                last_value_at, peak_value, threshold)
         VALUES ($1, 'monitorflow_disk_used_ratio', $2, 'warning', now(), 0.9, now(), 0.9, '{}')
         RETURNING id",
    )
    .bind(server_id)
    .bind(instance)
    .fetch_one(pool)
    .await
}

db_tests! {
    #[sqlx::test]
    async fn threshold_rows_are_checked(pool: PgPool) {
        let paris = site(&pool, "paris").await;
        let web = server(&pool, "web-01", paris).await;
        Threshold::at_site(paris).insert(&pool).await.unwrap();
        let bad = [
            Threshold { owner: (None, None), ..Threshold::at_site(paris) },
            Threshold { owner: (Some(paris), Some(web)), ..Threshold::at_site(paris) },
            Threshold { metric: "disk_used_ratio", ..Threshold::at_site(paris) },
            Threshold { metric: "monitorflow_Disk", ..Threshold::at_site(paris) },
            Threshold { instance: Some("a\nb"), ..Threshold::at_site(paris) },
            Threshold { comparison: "=", ..Threshold::at_site(paris) },
            Threshold { warning: None, critical: None, ..Threshold::at_site(paris) },
            Threshold { warning: Some(f64::NAN), ..Threshold::at_site(paris) },
            Threshold { critical: Some(f64::INFINITY), ..Threshold::at_site(paris) },
            Threshold { duration_s: -1, ..Threshold::at_site(paris) },
            Threshold { duration_s: 86_401, ..Threshold::at_site(paris) },
            Threshold { hysteresis: -0.1, ..Threshold::at_site(paris) },
            // Critical must be at least as bad as warning.
            Threshold { warning: Some(0.9), critical: Some(0.8), ..Threshold::at_site(paris) },
            Threshold { comparison: "<", ..Threshold::at_site(paris) },
        ];
        for (i, threshold) in bad.iter().enumerate() {
            let threshold = Threshold { instance: threshold.instance.or(Some("/x")), ..*threshold };
            assert_eq!(sqlstate(threshold.insert(&pool).await), CHECK_VIOLATION, "case {i}");
        }
        // A disabled row needs no value; one value is enough otherwise.
        Threshold { instance: Some("/boot"), warning: None, critical: None, enabled: false, ..Threshold::at_site(paris) }
            .insert(&pool).await.unwrap();
        Threshold { owner: (None, Some(web)), critical: None, ..Threshold::at_site(paris) }
            .insert(&pool).await.unwrap();
        // (metric, instance) is unique per owner, NULL instance included.
        assert_eq!(sqlstate(Threshold::at_site(paris).insert(&pool).await), UNIQUE_VIOLATION);
    }

    #[sqlx::test]
    async fn changes_of_either_layer_move_the_revision(pool: PgPool) {
        let paris = site(&pool, "paris").await;
        let web = server(&pool, "web-01", paris).await;
        let revision = || async {
            sqlx::query_scalar::<_, i64>(
                "SELECT si.threshold_revision + s.threshold_revision
                 FROM servers s JOIN sites si ON si.id = s.site_id WHERE s.id = $1",
            )
            .bind(web)
            .fetch_one(&pool)
            .await
            .unwrap()
        };
        assert_eq!(revision().await, 0);
        let row = Threshold::at_site(paris).insert(&pool).await.unwrap();
        assert_eq!(revision().await, 1);
        Threshold { owner: (None, Some(web)), ..Threshold::at_site(paris) }.insert(&pool).await.unwrap();
        assert_eq!(revision().await, 2);
        sqlx::query("UPDATE thresholds SET warning = 0.7 WHERE id = $1").bind(row).execute(&pool).await.unwrap();
        // An update counts the old and the new owner, here the same one.
        assert_eq!(revision().await, 4);
        sqlx::query("DELETE FROM thresholds WHERE id = $1").bind(row).execute(&pool).await.unwrap();
        assert_eq!(revision().await, 5);
    }

    #[sqlx::test]
    async fn one_open_incident_per_series(pool: PgPool) {
        let paris = site(&pool, "paris").await;
        let web = server(&pool, "web-01", paris).await;
        let first = open_incident(&pool, web, "/").await.unwrap();
        assert_eq!(sqlstate(open_incident(&pool, web, "/").await), UNIQUE_VIOLATION);
        open_incident(&pool, web, "/var").await.unwrap();
        // Resolved needs resolved_at, and frees the series.
        let half_resolved = sqlx::query("UPDATE incidents SET state = 'resolved' WHERE id = $1")
            .bind(first)
            .execute(&pool)
            .await;
        assert_eq!(sqlstate(half_resolved), CHECK_VIOLATION);
        sqlx::query("UPDATE incidents SET state = 'resolved', resolved_at = now() WHERE id = $1")
            .bind(first)
            .execute(&pool)
            .await
            .unwrap();
        open_incident(&pool, web, "/").await.unwrap();
    }

    #[sqlx::test]
    async fn incident_events_are_never_updated(pool: PgPool) {
        let paris = site(&pool, "paris").await;
        let web = server(&pool, "web-01", paris).await;
        let incident = open_incident(&pool, web, "/").await.unwrap();
        let insert = |kind: &'static str, severity: Option<&'static str>| {
            sqlx::query(
                "INSERT INTO incident_events (incident_id, occurred_at, kind, severity, value)
                 VALUES ($1, now(), $2, $3, 0.9)",
            )
            .bind(incident)
            .bind(kind)
            .bind(severity)
            .execute(&pool)
        };
        insert("opened", Some("warning")).await.unwrap();
        assert_eq!(sqlstate(insert("resolved", Some("warning")).await), CHECK_VIOLATION);
        assert_eq!(sqlstate(insert("opened", None).await), CHECK_VIOLATION);
        assert_eq!(sqlstate(insert("stale", Some("warning")).await), CHECK_VIOLATION);
        let update = sqlx::query("UPDATE incident_events SET value = 0").execute(&pool).await;
        assert_eq!(sqlstate(update), INSUFFICIENT_PRIVILEGE);
        let truncate = sqlx::query("TRUNCATE incident_events").execute(&pool).await;
        assert_eq!(sqlstate(truncate), INSUFFICIENT_PRIVILEGE);
        // Deleting the server takes its incidents and their events along.
        sqlx::query("DELETE FROM servers WHERE id = $1").bind(web).execute(&pool).await.unwrap();
        let left: i64 = sqlx::query_scalar("SELECT count(*) FROM incident_events").fetch_one(&pool).await.unwrap();
        assert_eq!(left, 0);
    }

    #[sqlx::test]
    async fn alert_states_exist_only_when_not_ok(pool: PgPool) {
        let paris = site(&pool, "paris").await;
        let web = server(&pool, "web-01", paris).await;
        let insert = |severity: Option<&'static str>, pending: bool| {
            sqlx::query(
                "INSERT INTO alert_states (server_id, metric, instance, severity, warning_since, last_sample_at)
                 VALUES ($1, 'monitorflow_disk_used_ratio', '/', $2, CASE WHEN $3 THEN now() END, now())",
            )
            .bind(web)
            .bind(severity)
            .bind(pending)
            .execute(&pool)
        };
        assert_eq!(sqlstate(insert(None, false).await), CHECK_VIOLATION);
        assert_eq!(sqlstate(insert(Some("info"), false).await), CHECK_VIOLATION);
        insert(None, true).await.unwrap();
        assert_eq!(sqlstate(insert(Some("warning"), false).await), UNIQUE_VIOLATION);
    }
}
