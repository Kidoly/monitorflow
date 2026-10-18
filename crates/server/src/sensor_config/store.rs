//! `sensor_configs` and the revision columns in PostgreSQL.

use sqlx::PgPool;

use super::{params, resolve, Layer, Resolved, Row, Spec};

/// Reads configurations and records what agents applied.
#[derive(Debug, Clone)]
pub struct SensorConfigStore {
    pool: PgPool,
}

/// Revisions, intervals and timeouts are never negative in the schema.
fn non_negative(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

impl SensorConfigStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// The effective configuration of the agent `peer_id`, or `None`
    /// without a `servers` row. One statement, so the revision and the
    /// rows come from the same snapshot.
    pub async fn load(&self, peer_id: &str) -> Result<Option<Resolved>, sqlx::Error> {
        type Line = (
            i64,
            Option<bool>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<i64>,
            Option<i64>,
            Option<serde_json::Value>,
            Option<bool>,
        );
        let lines: Vec<Line> = sqlx::query_as(
            "SELECT si.sensor_revision + s.sensor_revision,
                    c.server_id IS NOT NULL, c.kind, c.name, c.instance,
                    c.interval_ms, c.timeout_ms, c.params, c.enabled
             FROM servers s
             JOIN sites si ON si.id = s.site_id
             LEFT JOIN sensor_configs c ON c.site_id = s.site_id OR c.server_id = s.id
             WHERE s.peer_id = $1",
        )
        .bind(peer_id)
        .fetch_all(&self.pool)
        .await?;
        let Some(first) = lines.first() else {
            return Ok(None);
        };
        let revision = non_negative(first.0);
        let rows = lines
            .into_iter()
            .filter_map(|line| {
                let (_, server, kind, name, instance, interval, timeout, params_, enabled) = line;
                Some(Row {
                    layer: if server? { Layer::Server } else { Layer::Site },
                    spec: Spec {
                        kind: kind?,
                        name: name?,
                        instance: instance?,
                        interval_ms: non_negative(interval?),
                        timeout_ms: timeout.map(non_negative),
                        params: params(params_?),
                    },
                    enabled: enabled?,
                })
            })
            .collect();
        Ok(Some(Resolved {
            revision,
            sensors: resolve(rows),
        }))
    }

    /// Current revisions of the agents in `peer_ids` that have a row.
    pub async fn revisions(&self, peer_ids: &[String]) -> Result<Vec<(String, u64)>, sqlx::Error> {
        let rows: Vec<(String, i64)> = sqlx::query_as(
            "SELECT s.peer_id, si.sensor_revision + s.sensor_revision
             FROM servers s JOIN sites si ON si.id = s.site_id
             WHERE s.peer_id = ANY($1)",
        )
        .bind(peer_ids)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(id, rev)| (id, non_negative(rev)))
            .collect())
    }

    /// Records the revision the agent `peer_id` acknowledged.
    pub async fn record_applied(&self, peer_id: &str, revision: u64) -> Result<(), sqlx::Error> {
        sqlx::query(
            "UPDATE servers SET sensor_revision_applied = $2, sensor_applied_at = now()
             WHERE peer_id = $1",
        )
        .bind(peer_id)
        .bind(i64::try_from(revision).unwrap_or(i64::MAX))
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
