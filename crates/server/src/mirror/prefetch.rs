//! Prefetching what servers are about to install (ADR-0142): the pending
//! updates agents report (`server_packages.upgradable_version`), fetched
//! into the on-demand store before anyone asks.
//!
//! - After each on-demand sync, the whole fleet's pending updates that the
//!   new `current` lists, so the next automatic updates come from disk.
//! - Every [`EVERY`] (and at start), for each campaign that is not final,
//!   its target servers' pending updates that its pinned snapshots list.
//!
//! A pending update is matched by name, version and architecture against
//! the package list, so no server needs mapping to a mirror: a version
//! names one distribution's package. Prefetches run [`AT_ONCE`] at a time,
//! leaving most of the shared downloads to clients' requests, and a
//! failure is logged and left to the first request.

use std::collections::HashSet;
use std::future::Future;
use std::time::Duration;

use monitorflow_common::mirror::repo::meta::Package;
use monitorflow_common::mirror::{read_current, Family};
use sqlx::PgPool;
use tokio::task::JoinSet;
use uuid::Uuid;

use super::model::{self, Mirror, Mode};
use super::Mirrors;

/// How often pinned campaigns are looked at.
pub const EVERY: Duration = Duration::from_secs(600);
/// Prefetches in flight at once, out of the shared downloads.
pub const AT_ONCE: usize = 2;

/// A pending update: name, version, architecture.
pub type Pending = (String, String, String);

/// What a prefetch did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Prefetched {
    /// Pending updates the snapshot lists.
    pub matched: usize,
    /// Of those, fetched or found in the store.
    pub stored: usize,
    pub failed: usize,
}

/// The pending updates of every server, or of `servers` only.
pub async fn pending(pool: &PgPool, servers: Option<&[Uuid]>) -> Result<Vec<Pending>, sqlx::Error> {
    sqlx::query_as(
        "SELECT DISTINCT name, upgradable_version, arch FROM server_packages
         WHERE upgradable_version IS NOT NULL AND ($1::uuid[] IS NULL OR server_id = ANY($1))",
    )
    .bind(servers)
    .fetch_all(pool)
    .await
}

/// The packages of `list` among `pending`, each once.
fn matching(
    list: &monitorflow_common::mirror::packages::PackageList,
    pending: &[Pending],
) -> Vec<Package> {
    let by_version = list.by_version();
    let mut seen = HashSet::new();
    pending
        .iter()
        .filter_map(|(name, version, arch)| {
            by_version.get(&(name.as_str(), version.as_str(), arch.as_str()))
        })
        .filter(|p| seen.insert(p.entry.path.clone()))
        .map(|p| (*p).clone())
        .collect()
}

/// Fetches into the store the packages of `pending` that the snapshot
/// `snapshot` of `mirror` lists, when it is on demand.
pub async fn snapshot(
    mirrors: &Mirrors,
    mirror: &Mirror,
    snapshot: &str,
    pending: &[Pending],
) -> Prefetched {
    let family = match mirror.family {
        model::Family::Apt => Family::Apt,
        model::Family::Dnf => Family::Dnf,
    };
    let dir = mirrors.layout().mirror_dir(mirror).join(snapshot);
    let ondemand = mirrors.ondemand().clone();
    let (o, d) = (ondemand.clone(), dir.clone());
    let list = match tokio::task::spawn_blocking(move || o.list(&d)).await {
        Ok(Ok(Some(list))) => list,
        Ok(Ok(None)) => return Prefetched::default(),
        Ok(Err(error)) => {
            tracing::warn!(mirror = %mirror.name, snapshot, %error, "cannot read a package list to prefetch");
            return Prefetched::default();
        }
        Err(_) => return Prefetched::default(),
    };
    let wanted = matching(&list, pending);
    let mut done = Prefetched {
        matched: wanted.len(),
        ..Prefetched::default()
    };
    let mut tasks = JoinSet::new();
    let mut queue = wanted.into_iter();
    loop {
        while tasks.len() < AT_ONCE {
            let Some(package) = queue.next() else { break };
            let (ondemand, dir, name, snapshot) = (
                ondemand.clone(),
                dir.clone(),
                mirror.name.clone(),
                snapshot.to_owned(),
            );
            tasks.spawn(async move {
                ondemand
                    .package(family, &name, &snapshot, &dir, &package.entry.path)
                    .await
            });
        }
        let Some(result) = tasks.join_next().await else {
            break;
        };
        match result {
            Ok(Ok(Some(_))) => done.stored += 1,
            _ => done.failed += 1,
        }
    }
    if done.matched > 0 {
        tracing::info!(mirror = %mirror.name, snapshot, matched = done.matched, stored = done.stored, failed = done.failed, "pending updates prefetched");
    }
    done
}

/// After a sync of an on-demand mirror: the fleet's pending updates that
/// its `current` lists.
pub async fn after_sync(mirrors: &Mirrors, mirror: &Mirror) -> Prefetched {
    if mirror.mode != Mode::OnDemand {
        return Prefetched::default();
    }
    let dir = mirrors.layout().mirror_dir(mirror);
    let Some(current) = tokio::task::spawn_blocking(move || read_current(&dir))
        .await
        .ok()
        .flatten()
    else {
        return Prefetched::default();
    };
    match pending(mirrors.pool(), None).await {
        Ok(pending) => snapshot(mirrors, mirror, &current, &pending).await,
        Err(error) => {
            tracing::warn!(%error, "cannot read pending updates to prefetch");
            Prefetched::default()
        }
    }
}

/// Every campaign that is not final, with what it pins: its target
/// servers' pending updates that its pinned snapshots list.
pub async fn campaigns(mirrors: &Mirrors) -> Result<Prefetched, sqlx::Error> {
    let pins: Vec<(Uuid, Uuid, String)> = sqlx::query_as(
        "SELECT c.id, cs.mirror_id, s.name FROM campaigns c
         JOIN campaign_snapshots cs ON cs.campaign_id = c.id
         JOIN mirror_snapshots s ON s.id = cs.snapshot_id AND s.state = 'published'
         WHERE c.state NOT IN ('stopped', 'completed', 'failed')
         ORDER BY c.created_at",
    )
    .fetch_all(mirrors.pool())
    .await?;
    let mut total = Prefetched::default();
    for (campaign, mirror_id, snapshot_name) in pins {
        let Some(mirror) = super::store::get(mirrors.pool(), mirror_id).await? else {
            continue;
        };
        let servers: Vec<Uuid> =
            sqlx::query_scalar("SELECT server_id FROM campaign_targets WHERE campaign_id = $1")
                .bind(campaign)
                .fetch_all(mirrors.pool())
                .await?;
        let pending = pending(mirrors.pool(), Some(&servers)).await?;
        let done = snapshot(mirrors, &mirror, &snapshot_name, &pending).await;
        total.matched += done.matched;
        total.stored += done.stored;
        total.failed += done.failed;
    }
    Ok(total)
}

/// Prefetches for pinned campaigns every [`EVERY`] until `shutdown`.
pub async fn run(mirrors: Mirrors, shutdown: impl Future<Output = ()>) {
    tokio::pin!(shutdown);
    loop {
        if let Err(error) = campaigns(&mirrors).await {
            tracing::warn!(%error, "cannot prefetch the packages of pinned campaigns");
        }
        tokio::select! {
            () = &mut shutdown => return,
            () = tokio::time::sleep(EVERY) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use monitorflow_common::mirror::packages::PackageList;
    use monitorflow_common::mirror::repo::meta::{Algorithm, Checksum, Entry};

    use super::*;

    fn package(name: &str, version: &str, arch: &str) -> Package {
        Package {
            entry: Entry {
                path: format!("pool/{name}_{version}_{arch}.deb"),
                size: 1,
                checksum: Checksum::new(Algorithm::Sha256, &"a".repeat(64)).unwrap(),
            },
            name: name.into(),
            version: version.into(),
            arch: arch.into(),
        }
    }

    #[test]
    fn pending_updates_match_by_name_version_and_architecture() {
        let list = PackageList::new(vec![
            package("bash", "5.2-2", "amd64"),
            package("bash", "5.2-1", "amd64"),
            package("tzdata", "2026a-1", "all"),
        ])
        .unwrap();
        let pending: Vec<Pending> = [
            ("bash", "5.2-2", "amd64"),
            ("bash", "5.2-2", "amd64"),
            ("tzdata", "2026a-1", "all"),
            ("bash", "5.3-1", "amd64"),
            ("curl", "8.0-1", "amd64"),
        ]
        .iter()
        .map(|(n, v, a)| (n.to_string(), v.to_string(), a.to_string()))
        .collect();
        let found: Vec<String> = matching(&list, &pending)
            .into_iter()
            .map(|p| p.entry.path)
            .collect();
        assert_eq!(
            found,
            ["pool/bash_5.2-2_amd64.deb", "pool/tzdata_2026a-1_all.deb"]
        );
    }
}
