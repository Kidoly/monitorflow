//! Installed plugins (ADR-0103): the publisher keys global administrators
//! trust, the plugins in `plugins` and where each may be used. The API
//! installs them ([`crate::api::plugins`]); channels and agents' checks
//! use them where they are enabled.

use monitorflow_plugin::keys::PublicKey;
use monitorflow_plugin::Manifest;
use serde::{Deserialize, Serialize};
use sqlx::{PgExecutor, Row};
use uuid::Uuid;

/// Who signed a plugin, in words.
pub const OFFICIAL_PUBLISHER: &str = "MonitorFlow";

/// The publisher keys of `plugin_publishers`. A row that is not a key is
/// skipped; the schema prevents it.
pub async fn trusted_keys(db: impl PgExecutor<'_>) -> Result<Vec<PublicKey>, sqlx::Error> {
    let keys: Vec<Vec<u8>> = sqlx::query_scalar("SELECT public_key FROM plugin_publishers")
        .fetch_all(db)
        .await?;
    Ok(keys
        .iter()
        .filter_map(|k| PublicKey::from_bytes(k).ok())
        .collect())
}

/// The name a signer shows as: MonitorFlow for a catalogue key, the
/// publisher's name, or `None` for a key nobody trusts (any more).
pub async fn publisher_name(
    db: impl PgExecutor<'_>,
    key: &PublicKey,
) -> Result<Option<String>, sqlx::Error> {
    if monitorflow_plugin::keys::official().contains(key) {
        return Ok(Some(OFFICIAL_PUBLISHER.into()));
    }
    sqlx::query_scalar("SELECT name FROM plugin_publishers WHERE public_key = $1")
        .bind(key.0.as_slice())
        .fetch_optional(db)
        .await
}

/// Where a plugin may be used: everything when both are `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Place {
    #[serde(default)]
    pub customer_id: Option<Uuid>,
    #[serde(default)]
    pub site_id: Option<Uuid>,
}

impl Place {
    pub const EVERYTHING: Self = Self {
        customer_id: None,
        site_id: None,
    };
}

/// An installed plugin, without its package.
#[derive(Debug, Clone)]
pub struct Installed {
    pub id: Uuid,
    pub manifest: Manifest,
    pub enabled: bool,
    pub sha256: Vec<u8>,
}

fn installed(row: &sqlx::postgres::PgRow) -> Result<Installed, sqlx::Error> {
    let manifest: serde_json::Value = row.try_get("manifest")?;
    let manifest = serde_json::from_value(manifest).map_err(|e| sqlx::Error::Decode(e.into()))?;
    Ok(Installed {
        id: row.try_get("id")?,
        manifest,
        enabled: row.try_get("enabled")?,
        sha256: row.try_get("sha256")?,
    })
}

/// The plugin `name` if it is turned on and may be used for what covers
/// `customer` and `site`, as a channel covers (ADR-0078): a site's checks
/// pass the site and its customer, a channel for a customer only the
/// customer, a channel for everything neither. A place of everything
/// allows them all, a customer's its sites too.
pub async fn usable(
    db: impl PgExecutor<'_>,
    name: &str,
    customer: Option<Uuid>,
    site: Option<Uuid>,
) -> Result<Option<Installed>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT p.id, p.manifest, p.enabled, p.sha256 FROM plugins p
         WHERE p.name = $1 AND p.enabled AND EXISTS (
             SELECT 1 FROM plugin_places pp WHERE pp.plugin_id = p.id AND (
                 (pp.customer_id IS NULL AND pp.site_id IS NULL)
                 OR pp.site_id = $3
                 OR pp.customer_id = COALESCE($2, (SELECT customer_id FROM sites WHERE id = $3))))",
    )
    .bind(name)
    .bind(customer)
    .bind(site)
    .fetch_optional(db)
    .await?;
    row.as_ref().map(installed).transpose()
}

/// The plugin `name`, turned on or not.
pub async fn by_name(
    db: impl PgExecutor<'_>,
    name: &str,
) -> Result<Option<Installed>, sqlx::Error> {
    let row = sqlx::query("SELECT id, manifest, enabled, sha256 FROM plugins WHERE name = $1")
        .bind(name)
        .fetch_optional(db)
        .await?;
    row.as_ref().map(installed).transpose()
}

/// How many channels and checks use the plugin `name`.
pub async fn uses(db: impl PgExecutor<'_>, name: &str) -> Result<(i64, i64), sqlx::Error> {
    sqlx::query_as(
        "SELECT
            (SELECT count(*) FROM notification_channels
             WHERE kind = 'plugin' AND settings ->> 'plugin' = $1),
            (SELECT count(*) FROM sensor_configs
             WHERE kind = 'plugin' AND params ->> 'plugin' = $1)",
    )
    .bind(name)
    .fetch_one(db)
    .await
}
