//! `sensor_configs` and the revision columns in PostgreSQL.

use std::sync::Arc;

use monitorflow_common::plugin_grant;
use monitorflow_common::proto::v1::PluginGrant;
use monitorflow_plugin::manifest::{CHECK_KIND, PLUGIN_PARAM};
use sqlx::PgPool;
use uuid::Uuid;

use super::{params, resolve, site_interval, Layer, Resolved, Row, Spec};
use crate::ca::CertificateAuthority;

/// Most bytes of plugin packages in one configuration, under the stream's
/// 4 MiB per message; a plugin past it is not granted, and its checks show
/// as refused.
const MAX_GRANTED_BYTES: usize = 3 * 1024 * 1024;

/// Reads configurations and records what agents applied.
#[derive(Clone)]
pub struct SensorConfigStore {
    pool: PgPool,
    /// Signs the grants of check plugins; none are sent without it.
    grants: Option<Arc<CertificateAuthority>>,
}

impl std::fmt::Debug for SensorConfigStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SensorConfigStore")
            .field("grants", &self.grants.is_some())
            .finish_non_exhaustive()
    }
}

/// Revisions, intervals and timeouts are never negative in the schema.
fn non_negative(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

impl SensorConfigStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool, grants: None }
    }

    /// Sends agents the check plugins their sensors use, granted with the
    /// command key of `ca` (ADR-0103).
    pub fn with_plugin_grants(mut self, ca: Arc<CertificateAuthority>) -> Self {
        self.grants = Some(ca);
        self
    }

    /// Grants to `peer_id` of the check plugins `sensors` name that are
    /// turned on for `site`, in name order, as many as fit.
    async fn grants(
        &self,
        peer_id: &str,
        site: Uuid,
        sensors: &[Spec],
    ) -> Result<Vec<PluginGrant>, sqlx::Error> {
        let Some(ca) = &self.grants else {
            return Ok(Vec::new());
        };
        let mut names: Vec<&str> = sensors
            .iter()
            .filter(|s| s.kind == CHECK_KIND)
            .filter_map(|s| s.params.get(PLUGIN_PARAM).map(String::as_str))
            .collect();
        if names.is_empty() {
            return Ok(Vec::new());
        }
        names.sort_unstable();
        names.dedup();
        let packages: Vec<(String, Vec<u8>)> = sqlx::query_as(
            "SELECT p.name, p.package FROM plugins p
             WHERE p.name = ANY($1) AND p.kind = 'check' AND p.enabled AND EXISTS (
                 SELECT 1 FROM plugin_places pp WHERE pp.plugin_id = p.id AND (
                     (pp.customer_id IS NULL AND pp.site_id IS NULL)
                     OR pp.site_id = $2
                     OR pp.customer_id = (SELECT customer_id FROM sites WHERE id = $2)))
             ORDER BY p.name",
        )
        .bind(&names)
        .bind(site)
        .fetch_all(&self.pool)
        .await?;
        let mut grants = Vec::new();
        let mut total = 0;
        for (name, package) in packages {
            total += package.len();
            if total > MAX_GRANTED_BYTES {
                tracing::warn!(agent_id = peer_id, plugin = %name, "plugin packages over the size of one configuration; not granted");
                continue;
            }
            let message =
                plugin_grant::signed_bytes(peer_id, &name, &plugin_grant::sha256(&package));
            match ca.sign_command(&message) {
                Ok(signature) => grants.push(PluginGrant {
                    name,
                    package,
                    signature,
                }),
                Err(error) => {
                    tracing::warn!(agent_id = peer_id, plugin = %name, %error, "cannot grant a plugin")
                }
            }
        }
        Ok(grants)
    }

    /// The effective configuration of the agent `peer_id`, or `None`
    /// without a `servers` row. One statement, so the revision and the
    /// rows come from the same snapshot.
    pub async fn load(&self, peer_id: &str) -> Result<Option<Resolved>, sqlx::Error> {
        type Line = (
            i64,
            Uuid,
            Option<i32>,
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
            "SELECT si.sensor_revision + s.sensor_revision, si.id, si.check_interval_ms,
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
        let site = first.1;
        let builtin_interval = site_interval(first.2.map(i64::from));
        let rows = lines
            .into_iter()
            .filter_map(|line| {
                let (_, _, _, server, kind, name, instance, interval, timeout, params_, enabled) =
                    line;
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
        let sensors = resolve(rows, builtin_interval);
        let plugins = self.grants(peer_id, site, &sensors).await?;
        Ok(Some(Resolved {
            revision,
            sensors,
            plugins,
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
