//! `sensor_configs` and the revision columns in PostgreSQL.

use std::sync::Arc;

use monitorflow_common::plugin_grant;
use monitorflow_common::proto::v1::{PluginGrant, PluginGrantSet};
use monitorflow_plugin::manifest::{CHECK_KIND, PLUGIN_PARAM};
use sqlx::PgPool;
use uuid::Uuid;

use super::{params, resolve, site_interval, Layer, Resolved, Row, Spec};
use crate::ca::CertificateAuthority;
use crate::secrets::{self, Column};

/// Most bytes of plugin packages in one configuration, under the stream's
/// 4 MiB per message; a plugin past it is not granted, and its checks show
/// as refused.
const MAX_GRANTED_BYTES: usize = 3 * 1024 * 1024;

/// Adds the opened password of a website check (ADR-0150) or an
/// application check (ADR-0183) to its `params`, as the check reads it.
/// One that does not open is left out, so the check runs without it and
/// its failure shows.
pub(crate) fn with_secret(
    kind: &str,
    params: &mut std::collections::BTreeMap<String, String>,
    secret: Option<String>,
) {
    if kind != monitorflow_webcheck::KIND && !monitorflow_apps::takes_secret(kind) {
        return;
    }
    match secrets::open_stored(Column::SensorConfigs, secret) {
        Some(Ok(secret)) => {
            params.insert(
                monitorflow_webcheck::SECRET_PARAM.into(),
                secret.expose().to_owned(),
            );
        }
        Some(Err(error)) => {
            tracing::warn!(%error, kind, "the password of a check does not open; sent without it")
        }
        None => {}
    }
}

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

    /// The signature of `grants` as the whole set `peer_id` may run now,
    /// an empty one included: without it nothing would tell an agent that
    /// its last plugin was turned off (ADR-0103).
    fn grant_set(&self, peer_id: &str, grants: &[PluginGrant]) -> Option<PluginGrantSet> {
        let ca = self.grants.as_ref()?;
        let issued_at_unix_ms = chrono::Utc::now().timestamp_millis();
        let expires_at_unix_ms = issued_at_unix_ms + plugin_grant::SET_LIFETIME_MS;
        let message =
            plugin_grant::set_signed_bytes(peer_id, issued_at_unix_ms, expires_at_unix_ms, grants);
        match ca.sign_command(&message) {
            Ok(signature) => Some(PluginGrantSet {
                issued_at_unix_ms,
                expires_at_unix_ms,
                signature,
            }),
            Err(error) => {
                tracing::warn!(agent_id = peer_id, %error, "cannot sign the plugins of a configuration");
                None
            }
        }
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
            bool,
            Option<String>,
        );
        // Checks a probe runs are not the agent's (ADR-0148); a device
        // runs none of the site's, nor the built-in ones.
        let lines: Vec<Line> = sqlx::query_as(
            "SELECT si.sensor_revision + s.sensor_revision, si.id, si.check_interval_ms,
                    c.server_id IS NOT NULL, c.kind, c.name, c.instance,
                    c.interval_ms, c.timeout_ms, c.params, c.enabled, s.kind = 'device',
                    c.secret
             FROM servers s
             JOIN sites si ON si.id = s.site_id
             LEFT JOIN sensor_configs c
                  ON c.probe IS NULL
                 AND ((c.site_id = s.site_id AND s.kind = 'agent') OR c.server_id = s.id)
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
        let device = first.11;
        let rows = lines
            .into_iter()
            .filter_map(|line| {
                let (
                    _,
                    _,
                    _,
                    server,
                    kind,
                    name,
                    instance,
                    interval,
                    timeout,
                    params_,
                    enabled,
                    _,
                    secret,
                ) = line;
                let kind = kind?;
                let mut params = params(params_?);
                with_secret(&kind, &mut params, secret);
                Some(Row {
                    layer: if server? { Layer::Server } else { Layer::Site },
                    spec: Spec {
                        kind,
                        name: name?,
                        instance: instance?,
                        interval_ms: non_negative(interval?),
                        timeout_ms: timeout.map(non_negative),
                        params,
                    },
                    enabled: enabled?,
                })
            })
            .collect();
        let mut sensors = resolve(rows, builtin_interval);
        if device {
            sensors.clear();
        }
        let plugins = self.grants(peer_id, site, &sensors).await?;
        let plugin_set = self.grant_set(peer_id, &plugins);
        Ok(Some(Resolved {
            revision,
            sensors,
            plugins,
            plugin_set,
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
        self.record_applied_many(&[(peer_id.to_owned(), revision)])
            .await
    }

    /// Records what each agent acknowledged, in one statement.
    pub async fn record_applied_many(&self, applied: &[(String, u64)]) -> Result<(), sqlx::Error> {
        let (ids, revisions): (Vec<&str>, Vec<i64>) = applied
            .iter()
            .map(|(id, revision)| (id.as_str(), i64::try_from(*revision).unwrap_or(i64::MAX)))
            .unzip();
        sqlx::query(
            "UPDATE servers s SET sensor_revision_applied = a.revision, sensor_applied_at = now()
             FROM unnest($1::text[], $2::bigint[]) AS a(peer_id, revision)
             WHERE s.peer_id = a.peer_id",
        )
        .bind(&ids)
        .bind(&revisions)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
