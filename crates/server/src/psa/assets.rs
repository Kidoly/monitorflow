//! Servers synced as PSA assets (ADR-0164, ADR-0187, ADR-0191): one way,
//! MonitorFlow to the PSA, for servers of matched sites and customers,
//! when the connection's asset sync is on.
//!
//! The leader looks every minute at servers never synced, and at those
//! synced over an hour ago, at most [`PER_PASS`] of them. A server without
//! a link is matched to an asset of its client and type by serial number,
//! then by host name in the type's key field; a match is only proposed,
//! for an administrator to confirm. Without a match the asset is created,
//! with an External Link back to the server. A linked server's asset is
//! updated only when what it would receive changed (host name, site, or
//! the maker, model and serial number from the inventory), so at most once
//! an hour. A removed server's asset is marked inactive when the
//! connection says so, and otherwise left as it is. MonitorFlow never
//! deletes PSA assets and never reads them into servers.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use ring::digest::{digest, SHA256};
use serde_json::Value;
use sqlx::PgPool;
use tokio::time::{interval, MissedTickBehavior};
use uuid::Uuid;

use super::halopsa::{AssetBody, HaloError, Limiter, RemoteAsset, INVENTORY_NUMBER};
use super::{Connection, Halo};
use crate::api::inventory::{system_facts, AssetFacts};

/// Time between two passes.
pub const POLL: Duration = Duration::from_secs(60);
/// Servers looked at per pass.
pub const PER_PASS: i64 = 20;

/// The PSA asset type of one system family and where each value goes.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow, serde::Serialize, utoipa::ToSchema)]
#[schema(as = PsaAssetType)]
pub struct AssetType {
    /// `linux` or `windows`.
    pub family: String,
    /// The PSA asset type.
    pub type_id: i64,
    pub type_name: String,
    /// Where the host name goes: `inventory_number` (the asset tag) or a
    /// field id of the type, as text; `null` for nowhere.
    pub hostname_field: Option<String>,
    /// Where the serial number goes.
    pub serial_field: Option<String>,
    /// Where the maker goes.
    pub manufacturer_field: Option<String>,
    /// Where the model goes.
    pub model_field: Option<String>,
}

/// The family of a server's `os_family`.
pub fn family(os_family: &str) -> &'static str {
    if os_family == "windows" {
        "windows"
    } else {
        "linux"
    }
}

/// What the asset of a server receives.
pub fn body(
    kind: &AssetType,
    facts: &AssetFacts,
    client: i64,
    site: Option<i64>,
    id: Option<i64>,
) -> AssetBody {
    let values = [
        (&kind.hostname_field, Some(&facts.hostname)),
        (&kind.serial_field, facts.serial.as_ref()),
        (&kind.manufacturer_field, facts.manufacturer.as_ref()),
        (&kind.model_field, facts.model.as_ref()),
    ];
    let mut fields: Vec<(String, String)> = Vec::new();
    for (target, value) in values {
        if let (Some(target), Some(value)) = (target, value) {
            if !value.is_empty() && !fields.iter().any(|(t, _)| t == target) {
                fields.push((target.clone(), value.clone()));
            }
        }
    }
    AssetBody {
        id,
        type_id: kind.type_id,
        client_id: client,
        site_id: site,
        fields,
        inactive: false,
        link: None,
    }
}

/// SHA-256, hex, of what an asset receives, without its id or link: what
/// tells whether anything changed since the last push.
pub fn fingerprint(body: &AssetBody) -> String {
    let mut plain = body.clone();
    plain.id = None;
    plain.link = None;
    digest(&SHA256, plain.json().to_string().as_bytes())
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The asset of `assets` that is this server's: same serial number, else
/// same host name in the key field or the host name's field, among those
/// not `taken` by another server.
pub fn find<'a>(
    assets: &'a [RemoteAsset],
    kind: &AssetType,
    facts: &AssetFacts,
    taken: &HashSet<i64>,
) -> Option<(&'a RemoteAsset, &'static str)> {
    let same =
        |a: Option<&str>, b: &str| a.is_some_and(|a| a.trim().eq_ignore_ascii_case(b.trim()));
    let free = || assets.iter().filter(|a| !taken.contains(&a.id));
    if let (Some(serial), Some(field)) = (facts.serial.as_deref(), kind.serial_field.as_deref()) {
        if let Some(asset) = free().find(|a| same(a.value(field), serial)) {
            return Some((asset, "serial"));
        }
    }
    let hostname = facts.hostname.as_str();
    if hostname.is_empty() {
        return None;
    }
    free()
        .find(|a| {
            same(a.key_field.as_deref(), hostname)
                || kind
                    .hostname_field
                    .as_deref()
                    .is_some_and(|f| same(a.value(f), hostname))
        })
        .map(|a| (a, "hostname"))
}

/// The name an asset is shown by.
fn shown(asset: &RemoteAsset) -> Option<String> {
    asset
        .key_field
        .clone()
        .or_else(|| asset.inventory_number.clone())
        .map(|n| n.chars().take(500).collect())
}

#[derive(Debug, sqlx::FromRow)]
struct Candidate {
    server_id: Uuid,
    hostname: String,
    os_family: String,
    removed: bool,
    client_id: i64,
    halo_site_id: Option<i64>,
    system: Option<Value>,
    link_state: Option<String>,
    asset_id: Option<i64>,
    pushed_hash: Option<String>,
}

/// What to write on a server's link.
struct Saved<'a> {
    state: &'a str,
    asset_id: Option<i64>,
    matched_by: Option<&'a str>,
    asset_name: Option<String>,
    pushed_hash: Option<String>,
    deactivated: bool,
    error: Option<String>,
}

pub struct AssetSync {
    pool: PgPool,
    limiter: Arc<Limiter>,
    /// Base URL of the web interface, for the External Links.
    web_url: String,
}

impl AssetSync {
    pub fn new(pool: PgPool, web_url: impl Into<String>) -> Self {
        Self::with_limiter(pool, web_url, Limiter::shared())
    }

    pub fn with_limiter(pool: PgPool, web_url: impl Into<String>, limiter: Arc<Limiter>) -> Self {
        Self {
            pool,
            limiter,
            web_url: web_url.into().trim_end_matches('/').to_owned(),
        }
    }

    /// Syncs every `poll` until `stop` resolves.
    pub async fn run(self, poll: Duration, stop: impl Future<Output = ()>) {
        tokio::pin!(stop);
        let mut tick = interval(poll);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = &mut stop => return,
                _ = tick.tick() => {}
            }
            if let Err(error) = self.pass().await {
                tracing::warn!(%error, "could not sync PSA assets; will retry");
            }
        }
    }

    /// One pass; how many servers were looked at.
    pub async fn pass(&self) -> Result<usize, sqlx::Error> {
        let Some(connection) = super::load(&self.pool).await? else {
            return Ok(0);
        };
        if !connection.enabled || !connection.assets.asset_sync {
            return Ok(0);
        }
        let Ok(credentials) = connection.credentials() else {
            return Ok(0);
        };
        let types: Vec<AssetType> = sqlx::query_as(
            "SELECT family, type_id, type_name, hostname_field, serial_field, manufacturer_field,
                    model_field
             FROM psa_asset_types WHERE connection_id = $1",
        )
        .bind(connection.id)
        .fetch_all(&self.pool)
        .await?;
        if types.is_empty() {
            return Ok(0);
        }
        let families: Vec<String> = types.iter().map(|t| t.family.clone()).collect();
        let types: HashMap<String, AssetType> =
            types.into_iter().map(|t| (t.family.clone(), t)).collect();
        let candidates: Vec<Candidate> = sqlx::query_as(
            "SELECT s.id AS server_id, s.hostname, s.os_family, s.revoked_at IS NOT NULL AS removed,
                    coalesce(sl.client_id, cl.client_id) AS client_id,
                    CASE WHEN sl.client_id IS NOT NULL THEN sl.halo_site_id END AS halo_site_id,
                    (SELECT it.fields FROM server_inventory_items it
                     WHERE it.server_id = s.id AND it.kind = 'system' LIMIT 1) AS system,
                    al.state AS link_state, al.asset_id, al.pushed_hash
             FROM servers s
             JOIN sites si ON si.id = s.site_id
             LEFT JOIN psa_site_links sl ON sl.site_id = s.site_id AND sl.connection_id = $1
             LEFT JOIN psa_customer_links cl
                    ON cl.customer_id = si.customer_id AND cl.connection_id = $1
             LEFT JOIN psa_asset_links al ON al.server_id = s.id
             WHERE coalesce(sl.client_id, cl.client_id) IS NOT NULL
               AND (CASE WHEN s.os_family = 'windows' THEN 'windows' ELSE 'linux' END) = ANY($2)
               AND CASE
                   WHEN al.server_id IS NULL THEN s.revoked_at IS NULL
                   WHEN al.state = 'proposed' THEN false
                   WHEN s.revoked_at IS NOT NULL
                       THEN $3 AND al.state = 'linked' AND NOT al.deactivated
                   ELSE al.pushed_at IS NULL OR al.pushed_at < now() - interval '1 hour'
               END
             ORDER BY al.pushed_at NULLS FIRST, s.id
             LIMIT $4",
        )
        .bind(connection.id)
        .bind(&families)
        .bind(connection.assets.asset_inactive_on_removal)
        .bind(PER_PASS)
        .fetch_all(&self.pool)
        .await?;
        if candidates.is_empty() {
            return Ok(0);
        }
        let halo = Halo::new(credentials).with_limiter(self.limiter.clone());
        let mut taken: HashSet<i64> = sqlx::query_scalar::<_, i64>(
            "SELECT asset_id FROM psa_asset_links WHERE connection_id = $1 AND asset_id IS NOT NULL",
        )
        .bind(connection.id)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .collect();
        let mut listed: HashMap<(i64, i64), Vec<RemoteAsset>> = HashMap::new();
        let mut seen = 0;
        let mut worked = false;
        for candidate in &candidates {
            let Some(kind) = types.get(family(&candidate.os_family)) else {
                continue;
            };
            seen += 1;
            match self
                .one(&connection, &halo, kind, candidate, &mut listed, &mut taken)
                .await?
            {
                Ok(()) => worked = true,
                Err(error) if error.retryable() => {
                    super::record_failure(&self.pool, connection.id, &error.message).await?;
                    break;
                }
                Err(error) => {
                    super::record_failure(&self.pool, connection.id, &error.message).await?;
                    self.save(
                        &connection,
                        candidate.server_id,
                        Saved {
                            state: "failed",
                            asset_id: candidate.asset_id,
                            matched_by: None,
                            asset_name: None,
                            pushed_hash: None,
                            deactivated: false,
                            error: Some(error.message),
                        },
                    )
                    .await?;
                }
            }
        }
        if worked {
            super::record_success(&self.pool, connection.id, None).await?;
        }
        Ok(seen)
    }

    /// Syncs one server; the PSA's refusal as the inner error.
    async fn one(
        &self,
        connection: &Connection,
        halo: &Halo,
        kind: &AssetType,
        candidate: &Candidate,
        listed: &mut HashMap<(i64, i64), Vec<RemoteAsset>>,
        taken: &mut HashSet<i64>,
    ) -> Result<Result<(), HaloError>, sqlx::Error> {
        let facts = system_facts(candidate.hostname.clone(), candidate.system.as_ref());
        let link = (
            candidate.server_id.to_string(),
            candidate.hostname.clone(),
            format!("{}/servers/view/?id={}", self.web_url, candidate.server_id),
        );
        let fresh = body(
            kind,
            &facts,
            candidate.client_id,
            candidate.halo_site_id,
            None,
        );
        let hash = fingerprint(&fresh);
        let linked = matches!(candidate.link_state.as_deref(), Some("linked" | "failed"))
            && candidate.asset_id.is_some();
        if linked {
            let id = candidate.asset_id;
            if candidate.removed {
                let mut gone = body(
                    kind,
                    &facts,
                    candidate.client_id,
                    candidate.halo_site_id,
                    id,
                );
                gone.fields.clear();
                gone.inactive = true;
                if let Err(error) = halo.upsert_asset(&gone).await {
                    return Ok(Err(error));
                }
                self.save(
                    connection,
                    candidate.server_id,
                    Saved {
                        state: "linked",
                        asset_id: id,
                        matched_by: None,
                        asset_name: None,
                        pushed_hash: candidate.pushed_hash.clone(),
                        deactivated: true,
                        error: None,
                    },
                )
                .await?;
                return Ok(Ok(()));
            }
            if candidate.pushed_hash.as_deref() != Some(hash.as_str()) {
                let mut update = fresh.clone();
                update.id = id;
                // The first push after a confirmation links back.
                if candidate.pushed_hash.is_none() {
                    update.link = Some(link);
                }
                if let Err(error) = halo.upsert_asset(&update).await {
                    return Ok(Err(error));
                }
            }
            self.save(
                connection,
                candidate.server_id,
                Saved {
                    state: "linked",
                    asset_id: id,
                    matched_by: None,
                    asset_name: None,
                    pushed_hash: Some(hash),
                    deactivated: false,
                    error: None,
                },
            )
            .await?;
            return Ok(Ok(()));
        }
        if candidate.link_state.as_deref() != Some("create") {
            let key = (candidate.client_id, kind.type_id);
            if let std::collections::hash_map::Entry::Vacant(entry) = listed.entry(key) {
                match halo.assets(candidate.client_id, kind.type_id).await {
                    Ok(assets) => {
                        entry.insert(assets);
                    }
                    Err(error) => return Ok(Err(error)),
                }
            }
            if let Some((asset, by)) = find(&listed[&key], kind, &facts, taken) {
                taken.insert(asset.id);
                self.save(
                    connection,
                    candidate.server_id,
                    Saved {
                        state: "proposed",
                        asset_id: Some(asset.id),
                        matched_by: Some(by),
                        asset_name: shown(asset),
                        pushed_hash: None,
                        deactivated: false,
                        error: None,
                    },
                )
                .await?;
                return Ok(Ok(()));
            }
        }
        let mut create = fresh;
        create.link = Some(link);
        let id = match halo.upsert_asset(&create).await {
            Ok(id) => id,
            Err(error) => return Ok(Err(error)),
        };
        taken.insert(id);
        let name = create
            .fields
            .iter()
            .find(|(t, _)| t == INVENTORY_NUMBER)
            .map(|(_, v)| v.clone())
            .or(Some(candidate.hostname.clone()));
        self.save(
            connection,
            candidate.server_id,
            Saved {
                state: "linked",
                asset_id: Some(id),
                matched_by: Some("created"),
                asset_name: name,
                pushed_hash: Some(hash),
                deactivated: false,
                error: None,
            },
        )
        .await?;
        tracing::info!(server = %candidate.server_id, asset = id, "created a PSA asset for a server");
        Ok(Ok(()))
    }

    async fn save(
        &self,
        connection: &Connection,
        server: Uuid,
        saved: Saved<'_>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO psa_asset_links (server_id, connection_id, asset_id, state, matched_by,
                                          asset_name, pushed_hash, pushed_at, deactivated, error)
             VALUES ($1, $2, $3, $4, $5, $6, $7, now(), $8, $9)
             ON CONFLICT (server_id) DO UPDATE SET connection_id = $2, asset_id = $3, state = $4,
                 matched_by = coalesce($5, psa_asset_links.matched_by),
                 asset_name = coalesce($6, psa_asset_links.asset_name),
                 pushed_hash = CASE WHEN $4 = 'failed' THEN psa_asset_links.pushed_hash ELSE $7 END,
                 pushed_at = now(), deactivated = $8, error = $9,
                 updated_at = now()",
        )
        .bind(server)
        .bind(connection.id)
        .bind(saved.asset_id)
        .bind(saved.state)
        .bind(saved.matched_by)
        .bind(saved.asset_name)
        .bind(saved.pushed_hash)
        .bind(saved.deactivated)
        .bind(saved.error)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind() -> AssetType {
        AssetType {
            family: "linux".into(),
            type_id: 3,
            type_name: "Server".into(),
            hostname_field: Some(INVENTORY_NUMBER.into()),
            serial_field: Some("12".into()),
            manufacturer_field: Some("13".into()),
            model_field: None,
        }
    }

    fn facts() -> AssetFacts {
        AssetFacts {
            hostname: "web-01".into(),
            manufacturer: Some("Dell Inc.".into()),
            model: Some("R650".into()),
            serial: Some("SN123".into()),
        }
    }

    fn asset(id: i64, key: &str, serial: &str) -> RemoteAsset {
        RemoteAsset {
            id,
            inventory_number: None,
            key_field: Some(key.into()),
            fields: vec![(12, serial.into())],
        }
    }

    #[test]
    fn an_asset_receives_the_chosen_fields_only() {
        let body = body(&kind(), &facts(), 10, Some(100), None);
        assert_eq!(
            body.fields,
            [
                (INVENTORY_NUMBER.to_owned(), "web-01".to_owned()),
                ("12".to_owned(), "SN123".to_owned()),
                ("13".to_owned(), "Dell Inc.".to_owned()),
            ]
        );
        assert_eq!(family("windows"), "windows");
        assert_eq!(family("debian"), "linux");
    }

    #[test]
    fn the_fingerprint_changes_with_what_is_sent_only() {
        let a = body(&kind(), &facts(), 10, Some(100), None);
        let mut same = a.clone();
        same.id = Some(7);
        same.link = Some(("x".into(), "y".into(), "z".into()));
        assert_eq!(fingerprint(&a), fingerprint(&same));
        let mut moved = facts();
        moved.hostname = "web-02".into();
        assert_ne!(
            fingerprint(&a),
            fingerprint(&body(&kind(), &moved, 10, Some(100), None))
        );
        assert_ne!(
            fingerprint(&a),
            fingerprint(&body(&kind(), &facts(), 10, Some(101), None))
        );
    }

    #[test]
    fn the_serial_number_comes_before_the_host_name() {
        let assets = [asset(1, "WEB-01", "other"), asset(2, "old-name", "sn123")];
        let none = HashSet::new();
        let (found, by) = find(&assets, &kind(), &facts(), &none).unwrap();
        assert_eq!((found.id, by), (2, "serial"));
        // Taken by another server: the host name is next.
        let (found, by) = find(&assets, &kind(), &facts(), &HashSet::from([2])).unwrap();
        assert_eq!((found.id, by), (1, "hostname"));
        assert!(find(&assets, &kind(), &facts(), &HashSet::from([1, 2])).is_none());
        let mut unknown = facts();
        unknown.hostname = "db-01".into();
        unknown.serial = None;
        assert!(find(&assets, &kind(), &unknown, &none).is_none());
    }
}
