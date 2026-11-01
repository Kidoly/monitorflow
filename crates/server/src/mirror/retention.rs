//! Retention of snapshots (ADR-0074). Each mirror keeps its newest
//! [`KEEP_SNAPSHOTS`] published snapshots and whatever is in use: `current`,
//! snapshots pinned by a campaign not yet finished, the `current` of a
//! relay, and snapshots a server is or may go back to being pointed at.
//! The rest is removed every [`EVERY`], and `aptly db cleanup` then frees
//! the packages only those snapshots held.
//!
//! A snapshot is marked `pruning` before anything is deleted and `pruned`
//! once its files are gone, so a pass that stops halfway is finished by
//! the next one. Doubt keeps a snapshot: a source command that cannot be
//! read stops the pass.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::time::Duration;

use monitorflow_common::command::{CONFIGURE_SOURCES, REVERT_SOURCES};
use monitorflow_common::mirror::CURRENT;
use monitorflow_common::proto::v1::ConfigureSources;
use prost::Message;
use serde::Serialize;
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::model::{Family, Mirror, Snapshot, SnapshotState};
use super::runner::{Invocation, RunError};
use super::store::{self, RetentionLock};
use super::{apt, layout, removal, Layout, Mirrors};
use crate::api::audit::{self, Event};

/// Published snapshots each mirror keeps, in use or not: a week of daily
/// syncs.
pub const KEEP_SNAPSHOTS: usize = 7;
/// How often snapshots to remove are looked for.
pub const EVERY: Duration = Duration::from_secs(3600);

/// Every snapshot of a mirror fits in a pass.
const ALL: i64 = i64::MAX;

#[derive(Debug, thiserror::Error)]
pub enum RetentionError {
    #[error("database query failed: {0}")]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Run(#[from] RunError),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("cannot read a source command, so no snapshot is removed: {0}")]
    Payload(#[from] prost::DecodeError),
    #[error("aptly still lists a snapshot that was dropped: {0}")]
    Left(String),
}

/// Why a snapshot stays, the strongest reason when there are several.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kept {
    Current,
    Campaign,
    Server,
    Relay,
    Newest,
}

/// `(mirror name, snapshot name)` of what is in use beyond `current`.
#[derive(Debug, Default)]
pub struct InUse {
    pub campaigns: HashSet<(String, String)>,
    pub servers: HashSet<(String, String)>,
    pub relays: HashSet<(String, String)>,
}

impl InUse {
    pub async fn load(conn: &mut PgConnection) -> Result<Self, RetentionError> {
        let campaigns = sqlx::query_as(
            "SELECT m.name, s.name FROM campaign_snapshots cs
             JOIN campaigns c ON c.id = cs.campaign_id
             JOIN mirrors m ON m.id = cs.mirror_id
             JOIN mirror_snapshots s ON s.id = cs.snapshot_id
             WHERE c.state NOT IN ('stopped', 'completed', 'failed')",
        )
        .fetch_all(&mut *conn)
        .await?;
        let relays = sqlx::query_as(
            "SELECT rm.name, rm.current FROM relay_mirrors rm
             JOIN relays r ON r.id = rm.relay_id
             WHERE r.revoked_at IS NULL AND rm.current IS NOT NULL",
        )
        .fetch_all(&mut *conn)
        .await?;
        let mut servers = HashSet::new();
        for (_, spec) in source_specs(&mut *conn).await? {
            servers.extend(named_snapshots(&spec));
        }
        Ok(Self {
            campaigns: campaigns.into_iter().collect(),
            servers,
            relays: relays.into_iter().collect(),
        })
    }
}

/// `(server, spec)` of what a server is or may go back to being pointed
/// at: its commands not ended, the one that last changed its sources, and
/// the one a campaign would put back (`pin::previous`). A failure after
/// those changes nothing here: it may have left the sources alone.
/// Removed servers do not count.
pub(super) async fn source_specs(
    conn: &mut PgConnection,
) -> Result<Vec<(Uuid, ConfigureSources)>, RetentionError> {
    let commands: Vec<(Uuid, String, Vec<u8>)> = sqlx::query_as(
        "WITH sources AS (
             SELECT c.* FROM commands c JOIN servers s ON s.id = c.server_id
             WHERE s.revoked_at IS NULL AND c.kind IN ($1, $2)
         )
         SELECT server_id, kind, payload FROM sources
         WHERE state IN ('queued', 'sent', 'running')
         UNION ALL
         (SELECT DISTINCT ON (server_id) server_id, kind, payload FROM sources
          WHERE state = 'succeeded'
          ORDER BY server_id, finished_at DESC NULLS LAST, issued_at DESC)
         UNION ALL
         (SELECT DISTINCT ON (server_id) server_id, kind, payload FROM sources
          WHERE state = 'succeeded' AND campaign_id IS NULL
          ORDER BY server_id, finished_at DESC NULLS LAST, issued_at DESC)",
    )
    .bind(CONFIGURE_SOURCES)
    .bind(REVERT_SOURCES)
    .fetch_all(conn)
    .await?;
    let mut specs = Vec::new();
    for (server, kind, payload) in commands {
        if kind != REVERT_SOURCES {
            specs.push((server, ConfigureSources::decode(&*payload)?));
        }
    }
    Ok(specs)
}

/// `(mirror, snapshot)` of each repository of `spec`. `current` follows
/// the mirror, so it names no snapshot.
pub fn named_snapshots(spec: &ConfigureSources) -> Vec<(String, String)> {
    let apt = spec.apt.iter().map(|r| (&r.mirror, &r.snapshot));
    let dnf = spec.dnf.iter().map(|r| (&r.mirror, &r.snapshot));
    apt.chain(dnf)
        .filter(|(_, snapshot)| *snapshot != CURRENT)
        .map(|(mirror, snapshot)| (mirror.clone(), snapshot.clone()))
        .collect()
}

/// What happens to the snapshots of one mirror.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub kept: HashMap<Uuid, Kept>,
    pub remove: Vec<Snapshot>,
}

/// `snapshots`, all those of `mirror` newest first, sorted out. `link` is
/// the snapshot the `current` link names: a crash between moving it and
/// recording the snapshot leaves it on one that is not published.
pub fn plan(mirror: &str, snapshots: &[Snapshot], link: Option<&str>, uses: &InUse) -> Plan {
    let latest = snapshots
        .iter()
        .filter(|s| s.state == SnapshotState::Published)
        .max_by_key(|s| s.published_at)
        .map(|s| s.id);
    let mut published = 0;
    let mut result = Plan::default();
    for snapshot in snapshots {
        let key = (mirror.to_owned(), snapshot.name.clone());
        if snapshot.state == SnapshotState::Published {
            published += 1;
        }
        let reason = if snapshot.state == SnapshotState::Pruning {
            None
        } else if Some(snapshot.id) == latest || Some(snapshot.name.as_str()) == link {
            Some(Kept::Current)
        } else if snapshot.state != SnapshotState::Published {
            None
        } else if uses.campaigns.contains(&key) {
            Some(Kept::Campaign)
        } else if uses.servers.contains(&key) {
            Some(Kept::Server)
        } else if uses.relays.contains(&key) {
            Some(Kept::Relay)
        } else if published <= KEEP_SNAPSHOTS {
            Some(Kept::Newest)
        } else {
            None
        };
        match (reason, snapshot.state) {
            (Some(reason), _) => {
                result.kept.insert(snapshot.id, reason);
            }
            // A job's, or stale: the mirror's next job closes it.
            (None, SnapshotState::Creating) => {}
            (None, _) => result.remove.push(snapshot.clone()),
        }
    }
    result
}

/// Why each snapshot of `mirror` stays, for its page.
pub async fn kept(
    pool: &PgPool,
    layout: Option<&Layout>,
    mirror: &Mirror,
) -> Result<HashMap<Uuid, Kept>, RetentionError> {
    let snapshots = store::snapshots(pool, mirror.id, ALL).await?;
    let uses = InUse::load(&mut *pool.acquire().await?).await?;
    let link = link(layout, mirror).await;
    Ok(plan(&mirror.name, &snapshots, link.as_deref(), &uses).kept)
}

async fn link(layout: Option<&Layout>, mirror: &Mirror) -> Option<String> {
    let dir = layout?.mirror_dir(mirror);
    let target = tokio::fs::read_link(dir.join(CURRENT)).await.ok()?;
    target.to_str().map(str::to_owned)
}

/// The plan of every mirror that has something to remove.
async fn plans(
    conn: &mut PgConnection,
    layout: &Layout,
) -> Result<Vec<(Mirror, Vec<Snapshot>)>, RetentionError> {
    let uses = InUse::load(&mut *conn).await?;
    let mut plans = Vec::new();
    for mirror in store::list(&mut *conn).await? {
        // `removal` takes all of it.
        if mirror.deleting_at.is_some() {
            continue;
        }
        let snapshots = store::snapshots(&mut *conn, mirror.id, ALL).await?;
        let link = link(Some(layout), &mirror).await;
        let remove = plan(&mirror.name, &snapshots, link.as_deref(), &uses).remove;
        if !remove.is_empty() {
            plans.push((mirror, remove));
        }
    }
    Ok(plans)
}

/// Chooses what to remove and marks it `pruning`, in one transaction. The
/// row locks come first: a campaign pinning a snapshot right now commits
/// before [`InUse::load`] reads, or finds the snapshot no longer published.
async fn choose(mirrors: &Mirrors) -> Result<Vec<(Mirror, Vec<Snapshot>)>, RetentionError> {
    let mut tx = mirrors.pool().begin().await?;
    sqlx::query("SELECT 1 FROM mirror_snapshots WHERE state = 'published' FOR NO KEY UPDATE")
        .execute(&mut *tx)
        .await?;
    let chosen = plans(&mut tx, mirrors.layout()).await?;
    let ids: Vec<Uuid> = chosen
        .iter()
        .flat_map(|(_, remove)| remove.iter().map(|s| s.id))
        .collect();
    sqlx::query(
        "UPDATE mirror_snapshots SET state = 'pruning', published_at = NULL WHERE id = ANY($1)",
    )
    .bind(&ids)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(chosen)
}

pub(super) async fn stdout(
    mirrors: &Mirrors,
    invocation: Invocation,
) -> Result<String, RetentionError> {
    let output = mirrors.runner().run(&invocation).await?;
    Ok(output.check(&invocation)?.stdout)
}

/// Drops the aptly objects and the files of `snapshots`. A command fails
/// when its object is already gone, so [`free_aptly_pool`] checks instead.
async fn drop_files(
    mirrors: &Mirrors,
    mirror: &Mirror,
    snapshots: &[Snapshot],
) -> Result<(), RetentionError> {
    let dir = mirrors.layout().mirror_dir(mirror);
    for snapshot in snapshots {
        if mirror.family == Family::Apt {
            for command in apt::cleanup(mirrors.layout(), mirror, &snapshot.name) {
                if let Err(error) = mirrors.runner().run(&command).await {
                    tracing::warn!(%error, %command, "cannot run a snapshot cleanup");
                }
            }
        }
        let trees = [
            dir.join(&snapshot.name),
            dir.join(format!(".creating-{}", snapshot.name)),
        ];
        tokio::task::spawn_blocking(move || trees.iter().try_for_each(|t| layout::remove_tree(t)))
            .await
            .map_err(std::io::Error::other)??;
    }
    Ok(())
}

/// Checks that aptly let go of `dropped`, then deletes the package files
/// nothing refers to any more.
async fn free_aptly_pool(
    mirrors: &Mirrors,
    dropped: &[(Mirror, Vec<Snapshot>)],
) -> Result<(), RetentionError> {
    let layout = mirrors.layout();
    let snapshots = stdout(mirrors, apt::list_snapshots(layout)).await?;
    let published = stdout(mirrors, apt::list_published(layout)).await?;
    for (mirror, removed) in dropped {
        for snapshot in removed {
            let listed = snapshots
                .lines()
                .any(|line| apt::lists_snapshot(line.trim(), mirror, &snapshot.name))
                || published
                    .lines()
                    .any(|line| apt::lists_published(line.trim(), mirror, &snapshot.name));
            if listed {
                return Err(RetentionError::Left(format!(
                    "{}/{}",
                    mirror.name, snapshot.name
                )));
            }
        }
    }
    stdout(mirrors, apt::db_cleanup(layout)).await?;
    Ok(())
}

async fn finish(pool: &PgPool, mirror: &Mirror, removed: &[Snapshot]) -> Result<(), sqlx::Error> {
    let ids: Vec<Uuid> = removed.iter().map(|s| s.id).collect();
    let names: Vec<&str> = removed.iter().map(|s| s.name.as_str()).collect();
    let mut tx = pool.begin().await?;
    sqlx::query("UPDATE mirror_snapshots SET state = 'pruned', error = NULL WHERE id = ANY($1)")
        .bind(&ids)
        .execute(&mut *tx)
        .await?;
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "system",
            actor_id: "retention",
            action: "mirror.prune",
            target: Some(("mirror", &mirror.id.to_string())),
            details: json!({ "name": mirror.name, "snapshots": names }),
        },
    )
    .await?;
    tx.commit().await?;
    tracing::info!(mirror = %mirror.name, snapshots = names.len(), "old mirror snapshots removed");
    Ok(())
}

/// The pass once it holds the lock: returns the snapshots removed.
async fn pass(mirrors: &Mirrors) -> Result<usize, RetentionError> {
    let chosen = choose(mirrors).await?;
    let mut apt_mirrors = Vec::new();
    let mut removed = 0;
    for (mirror, snapshots) in chosen {
        drop_files(mirrors, &mirror, &snapshots).await?;
        if mirror.family == Family::Apt {
            apt_mirrors.push((mirror, snapshots));
        } else {
            finish(mirrors.pool(), &mirror, &snapshots).await?;
            removed += snapshots.len();
        }
    }
    if !apt_mirrors.is_empty() {
        free_aptly_pool(mirrors, &apt_mirrors).await?;
        for (mirror, snapshots) in &apt_mirrors {
            finish(mirrors.pool(), mirror, snapshots).await?;
            removed += snapshots.len();
        }
    }
    Ok(removed)
}

/// Removes every snapshot nothing needs and frees its disk space, once
/// the running jobs ended, and finishes deleting mirrors (ADR-0075), under
/// the same lock. Returns how many snapshots it removed. On failure, what
/// is left stays `pruning` with the error, for its page and the next pass.
pub async fn prune(mirrors: &Mirrors) -> Result<usize, RetentionError> {
    let mut conn = mirrors.pool().acquire().await?;
    let planned = plans(&mut conn, mirrors.layout()).await;
    let deleting = store::deleting(&mut *conn).await?;
    drop(conn);
    // An unreadable command stops the pass, not a deletion: nothing used
    // that mirror when it was asked for.
    if deleting.is_empty() && planned?.is_empty() {
        return Ok(0);
    }
    let lock = RetentionLock::acquire(mirrors.pool()).await?;
    let result = pass(mirrors).await;
    if let Err(error) = removal::finish(mirrors).await {
        tracing::warn!(%error, "cannot finish deleting mirrors; will retry");
    }
    lock.release().await;
    if let Err(error) = &result {
        sqlx::query("UPDATE mirror_snapshots SET error = $1 WHERE state = 'pruning'")
            .bind(error.to_string())
            .execute(mirrors.pool())
            .await?;
    }
    result
}

/// Runs [`prune`] every `every` until `shutdown` resolves. A pass stopped
/// by it is finished by the next one.
pub async fn run(mirrors: Mirrors, every: Duration, shutdown: impl Future<Output = ()>) {
    let mut interval = tokio::time::interval(every);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            _ = interval.tick() => {}
        }
        tokio::select! {
            _ = &mut shutdown => break,
            result = prune(&mirrors) => {
                if let Err(error) = result {
                    tracing::warn!(%error, "cannot remove old mirror snapshots; will retry");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration as Days, Utc};

    use monitorflow_common::proto::v1::configure_sources::{AptRepository, DnfRepository};

    use super::*;

    /// `count` published snapshots `s0`, the newest, to `s<count - 1>`.
    fn snapshots(count: usize) -> Vec<Snapshot> {
        (0..count)
            .map(|i| {
                let at = Utc::now() - Days::days(i as i64);
                Snapshot {
                    id: Uuid::new_v4(),
                    mirror_id: Uuid::nil(),
                    name: format!("s{i}"),
                    state: SnapshotState::Published,
                    size_bytes: None,
                    error: None,
                    created_at: at,
                    published_at: Some(at),
                }
            })
            .collect()
    }

    fn removed(plan: &Plan) -> Vec<&str> {
        plan.remove.iter().map(|s| s.name.as_str()).collect()
    }

    fn pair(snapshot: &str) -> (String, String) {
        ("m".to_owned(), snapshot.to_owned())
    }

    #[test]
    fn the_newest_are_kept_and_older_ones_go() {
        let all = snapshots(KEEP_SNAPSHOTS + 2);
        let plan = plan("m", &all, Some("s0"), &InUse::default());
        assert_eq!(removed(&plan), ["s7", "s8"]);
        assert_eq!(plan.kept[&all[0].id], Kept::Current);
        assert_eq!(plan.kept[&all[6].id], Kept::Newest);
        assert_eq!(plan.kept.len(), KEEP_SNAPSHOTS);
    }

    #[test]
    fn anything_in_use_stays_however_old() {
        let all = snapshots(KEEP_SNAPSHOTS + 4);
        let uses = InUse {
            campaigns: [pair("s7")].into(),
            servers: [pair("s8"), pair("s7")].into(),
            relays: [pair("s9"), ("other".to_owned(), "s10".to_owned())].into(),
        };
        let plan = plan("m", &all, Some("s0"), &uses);
        assert_eq!(removed(&plan), ["s10"]);
        assert_eq!(plan.kept[&all[7].id], Kept::Campaign);
        assert_eq!(plan.kept[&all[8].id], Kept::Server);
        assert_eq!(plan.kept[&all[9].id], Kept::Relay);
    }

    #[test]
    fn the_current_link_wins_over_dates_and_states() {
        // An older snapshot published last is what `current` names.
        let mut all = snapshots(KEEP_SNAPSHOTS + 2);
        all[8].published_at = Some(Utc::now() + Days::days(1));
        let found = plan("m", &all, None, &InUse::default());
        assert_eq!(removed(&found), ["s7"]);
        assert_eq!(found.kept[&all[8].id], Kept::Current);

        // A crash left the link on a failed snapshot.
        let mut all = snapshots(2);
        all[1].state = SnapshotState::Failed;
        all[1].published_at = None;
        let found = plan("m", &all, Some("s1"), &InUse::default());
        assert!(removed(&found).is_empty());
        assert_eq!(found.kept[&all[1].id], Kept::Current);
    }

    #[test]
    fn failed_ones_go_at_once_and_creating_ones_stay() {
        let mut all = snapshots(4);
        all[1].state = SnapshotState::Failed;
        all[2].state = SnapshotState::Creating;
        all[3].state = SnapshotState::Pruning;
        for snapshot in &mut all[1..] {
            snapshot.published_at = None;
        }
        let plan = plan("m", &all, Some("s0"), &InUse::default());
        assert_eq!(removed(&plan), ["s1", "s3"]);
        assert_eq!(plan.kept.len(), 1);
    }

    #[test]
    fn a_source_command_names_its_snapshots_but_not_current() {
        let spec = ConfigureSources {
            apt: vec![
                AptRepository {
                    mirror: "a".into(),
                    snapshot: "s1".into(),
                    ..AptRepository::default()
                },
                AptRepository {
                    mirror: "b".into(),
                    snapshot: CURRENT.into(),
                    ..AptRepository::default()
                },
            ],
            dnf: vec![DnfRepository {
                mirror: "c".into(),
                snapshot: "s2".into(),
                ..DnfRepository::default()
            }],
            ..ConfigureSources::default()
        };
        assert_eq!(
            named_snapshots(&spec),
            [
                ("a".to_owned(), "s1".to_owned()),
                ("c".to_owned(), "s2".to_owned())
            ]
        );
    }
}
