//! Deleting a mirror (ADR-0075). [`request`] refuses while something uses
//! the mirror; otherwise it marks it in one transaction, so nothing new
//! can use it and its running job stops. [`finish`] then removes it under
//! the lock of retention, as `aptly db cleanup` needs every mirror still,
//! and is called again by each pass until it worked: every step can be
//! repeated. The rows go last.
//!
//! Relays learn from the `.removed` file of the family's directory, which
//! the index lists once the mirror is no longer published. It stays for
//! relays that are offline; publishing the name again overrides it.

use std::collections::HashSet;
use std::path::Path;

use monitorflow_common::mirror::{read_removed, remove_mirror_dir, remove_retired, write_removed};
use monitorflow_common::proto::v1::ConfigureSources;
use serde::Serialize;
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::model::{Family, Mirror};
use super::retention::{self, source_specs, stdout, RetentionError};
use super::{apt, dnf, layout, Layout, Mirrors};
use crate::api::audit::{self, Event};

/// Names a refusal spells out; the page has the full list.
const NAMED: usize = 5;

#[derive(Debug, thiserror::Error)]
pub enum RemovalError {
    #[error("no such mirror")]
    NotFound,
    /// Says what uses it, in a sentence for the page.
    #[error("{0}")]
    InUse(String),
    #[error(transparent)]
    Check(#[from] RetentionError),
    #[error("database query failed: {0}")]
    Database(#[from] sqlx::Error),
}

/// A campaign not finished, pinned to the mirror.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, sqlx::FromRow)]
pub struct UsingCampaign {
    pub id: Uuid,
    pub name: String,
}

/// A server pointed at the mirror, or that a campaign would put back on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, sqlx::FromRow)]
pub struct UsingServer {
    pub id: Uuid,
    pub hostname: String,
}

/// What keeps a mirror from being deleted. Relays are not here: they hold
/// copies, which go with it, and the servers behind them count as servers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Users {
    pub campaigns: Vec<UsingCampaign>,
    pub servers: Vec<UsingServer>,
}

impl Users {
    pub fn is_empty(&self) -> bool {
        self.campaigns.is_empty() && self.servers.is_empty()
    }

    /// "debian-12 is still used by 1 update campaign (march) and 2 servers
    /// (web-01, web-02)."
    fn sentence(&self, mirror: &str) -> String {
        let campaigns = self.campaigns.iter().map(|c| c.name.as_str());
        let servers = self.servers.iter().map(|s| s.hostname.as_str());
        let parts: Vec<String> = [
            count("update campaign", campaigns),
            count("server", servers),
        ]
        .into_iter()
        .flatten()
        .collect();
        format!("{mirror} is still used by {}", parts.join(" and "))
    }
}

/// "2 servers (web-01, web-02)", with at most [`NAMED`] names.
fn count<'a>(what: &str, names: impl ExactSizeIterator<Item = &'a str>) -> Option<String> {
    let total = names.len();
    let plural = if total == 1 { "" } else { "s" };
    let mut shown: Vec<&str> = names.take(NAMED).collect();
    let more = total.saturating_sub(shown.len());
    let more = format!("{more} more");
    if total > NAMED {
        shown.push(&more);
    }
    (total > 0).then(|| format!("{total} {what}{plural} ({})", shown.join(", ")))
}

/// Whether `spec` points at the mirror `name`, at any snapshot.
fn points_at(spec: &ConfigureSources, name: &str) -> bool {
    spec.apt.iter().any(|r| r.mirror == name) || spec.dnf.iter().any(|r| r.mirror == name)
}

/// What uses `mirror` now. Doubt refuses: a source command that cannot be
/// read is an error, as in retention.
pub async fn users(conn: &mut PgConnection, mirror: &Mirror) -> Result<Users, RetentionError> {
    let campaigns = sqlx::query_as(
        "SELECT c.id, c.name FROM campaign_snapshots cs
         JOIN campaigns c ON c.id = cs.campaign_id
         WHERE cs.mirror_id = $1 AND c.state NOT IN ('stopped', 'completed', 'failed')
         ORDER BY c.name, c.id",
    )
    .bind(mirror.id)
    .fetch_all(&mut *conn)
    .await?;
    let ids: HashSet<Uuid> = source_specs(&mut *conn)
        .await?
        .into_iter()
        .filter(|(_, spec)| points_at(spec, &mirror.name))
        .map(|(server, _)| server)
        .collect();
    let servers =
        sqlx::query_as("SELECT id, hostname FROM servers WHERE id = ANY($1) ORDER BY hostname, id")
            .bind(ids.into_iter().collect::<Vec<_>>())
            .fetch_all(&mut *conn)
            .await?;
    Ok(Users { campaigns, servers })
}

/// Marks the mirror `id` for deletion, audited as `mirror.delete` by
/// `actor` (`(kind, id)` as in the audit log), or refuses saying what uses
/// it. Asking again changes nothing: the next pass retries. The row locks
/// come first, as in retention: a campaign pinning a snapshot right now
/// commits before [`users`] reads, or finds it no longer published.
pub async fn request(pool: &PgPool, id: Uuid, actor: (&str, &str)) -> Result<Mirror, RemovalError> {
    let mut tx = pool.begin().await?;
    let mirror: Mirror = sqlx::query_as(&format!(
        "SELECT {} FROM mirrors WHERE id = $1 FOR UPDATE",
        super::store::MIRROR_COLUMNS
    ))
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(RemovalError::NotFound)?;
    if mirror.deleting_at.is_some() {
        return Ok(mirror);
    }
    sqlx::query(
        "SELECT 1 FROM mirror_snapshots WHERE mirror_id = $1 AND state = 'published'
         FOR NO KEY UPDATE",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    let users = users(&mut tx, &mirror).await?;
    if !users.is_empty() {
        return Err(RemovalError::InUse(users.sentence(&mirror.name)));
    }
    sqlx::query(
        "UPDATE mirror_snapshots SET state = 'pruning', published_at = NULL
         WHERE mirror_id = $1 AND state = 'published'",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    let mirror: Mirror = sqlx::query_as(&format!(
        "UPDATE mirrors SET deleting_at = now(), delete_error = NULL
         WHERE id = $1 RETURNING {}",
        super::store::MIRROR_COLUMNS
    ))
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    audit::record(
        &mut *tx,
        Event {
            actor_kind: actor.0,
            actor_id: actor.1,
            action: "mirror.delete",
            target: Some(("mirror", &mirror.id.to_string())),
            details: json!({ "name": mirror.name, "family": mirror.family }),
        },
    )
    .await?;
    tx.commit().await?;
    tracing::info!(mirror = %mirror.name, "mirror deletion requested");
    Ok(mirror)
}

/// Runs `f` on the blocking pool.
async fn blocking(f: impl FnOnce() -> std::io::Result<()> + Send + 'static) -> std::io::Result<()> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(std::io::Error::other)?
}

/// Adds `name` to the `.removed` file in `root`. Blocking.
fn tell_relays(root: &Path, name: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(root)?;
    let mut names = read_removed(root);
    names.push(name.to_owned());
    names.sort();
    names.dedup();
    write_removed(root, &names)
}

/// Whether the file or directory `entry` of dnf's cache is `mirror`'s:
/// `<repo>-<16 hex>/`, `<repo>.solv` or `<repo>-filenames.solvx`, where a
/// mirror named `rocky-9` must not match `mf-rocky-9-baseos`.
fn in_dnf_cache(entry: &str, mirror: &Mirror) -> bool {
    let Some(rest) = entry.strip_prefix(&dnf::repo_id(mirror)) else {
        return false;
    };
    let hash = rest
        .strip_prefix('-')
        .is_some_and(|h| h.len() == 16 && h.bytes().all(|b| b.is_ascii_hexdigit()));
    hash || rest == ".solv" || rest == "-filenames.solvx"
}

/// Removes the working tree of a dnf mirror and its part of dnf's cache.
/// The package files go with the last link to them. Blocking.
fn remove_dnf_work(layout: &Layout, mirror: &Mirror) -> std::io::Result<()> {
    layout::remove_tree(&layout.dnf_work(mirror))?;
    let cache = layout.dnf_cache();
    let entries = match std::fs::read_dir(&cache) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        other => other?,
    };
    for entry in entries {
        let entry = entry?;
        if !entry
            .file_name()
            .to_str()
            .is_some_and(|n| in_dnf_cache(n, mirror))
        {
            continue;
        }
        if entry.file_type()?.is_dir() {
            layout::remove_tree(&entry.path())?;
        } else {
            std::fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

/// Drops every aptly object of `mirror`. A command fails when its object
/// is already gone, so [`clean_aptly`] checks instead.
async fn drop_aptly(mirrors: &Mirrors, mirror: &Mirror) -> Result<(), RetentionError> {
    let layout = mirrors.layout();
    let mut commands = Vec::new();
    let published = stdout(mirrors, apt::list_published(layout)).await?;
    for line in published.lines() {
        if let Some((prefix, distribution)) = apt::published_of(line.trim(), mirror) {
            commands.push(apt::drop_published(layout, distribution, prefix));
        }
    }
    let snapshots = stdout(mirrors, apt::list_snapshots(layout)).await?;
    for line in snapshots.lines().map(str::trim) {
        if apt::lists_object(line, mirror) {
            commands.push(apt::drop_snapshot(layout, line));
        }
    }
    let aptly_mirrors = stdout(mirrors, apt::list(layout)).await?;
    for line in aptly_mirrors.lines().map(str::trim) {
        if apt::lists_object(line, mirror) {
            commands.push(apt::drop_mirror(layout, line));
        }
    }
    for command in commands {
        if let Err(error) = mirrors.runner().run(&command).await {
            tracing::warn!(%error, %command, "cannot run a mirror cleanup");
        }
    }
    Ok(())
}

/// Checks that aptly let go of `dropped`, then deletes the package files
/// nothing refers to any more.
async fn clean_aptly(mirrors: &Mirrors, dropped: &[Mirror]) -> Result<(), RetentionError> {
    let layout = mirrors.layout();
    let published = stdout(mirrors, apt::list_published(layout)).await?;
    let snapshots = stdout(mirrors, apt::list_snapshots(layout)).await?;
    let aptly_mirrors = stdout(mirrors, apt::list(layout)).await?;
    for mirror in dropped {
        let left = published
            .lines()
            .any(|line| apt::published_of(line.trim(), mirror).is_some())
            || snapshots
                .lines()
                .chain(aptly_mirrors.lines())
                .any(|line| apt::lists_object(line.trim(), mirror));
        if left {
            return Err(RetentionError::Left(mirror.name.clone()));
        }
    }
    stdout(mirrors, apt::db_cleanup(layout)).await?;
    Ok(())
}

/// Takes `mirror` out of service: relays are told, its aptly objects
/// dropped, its files gone. An apt mirror's packages wait for
/// [`clean_aptly`].
async fn take_down(mirrors: &Mirrors, mirror: &Mirror) -> Result<(), RetentionError> {
    let layout = mirrors.layout().clone();
    let root = layout.served(mirror.family);
    let name = mirror.name.clone();
    let told = root.clone();
    blocking(move || tell_relays(&told, &name)).await?;
    if mirror.family == Family::Apt {
        drop_aptly(mirrors, mirror).await?;
    }
    let (name, gone) = (mirror.name.clone(), mirror.clone());
    blocking(move || {
        remove_mirror_dir(&root, &name)?;
        remove_retired(&root)?;
        if gone.family == Family::Dnf {
            remove_dnf_work(&layout, &gone)?;
        }
        Ok(())
    })
    .await?;
    Ok(())
}

/// Removes the rows of `mirror`; a campaign that pinned it keeps the names.
async fn forget(pool: &PgPool, mirror: &Mirror) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM mirror_runs WHERE mirror_id = $1")
        .bind(mirror.id)
        .execute(&mut *tx)
        .await?;
    let snapshots = sqlx::query("DELETE FROM mirror_snapshots WHERE mirror_id = $1")
        .bind(mirror.id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    sqlx::query("DELETE FROM mirrors WHERE id = $1")
        .bind(mirror.id)
        .execute(&mut *tx)
        .await?;
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "system",
            actor_id: "retention",
            action: "mirror.deleted",
            target: Some(("mirror", &mirror.id.to_string())),
            details: json!({ "name": mirror.name, "family": mirror.family, "snapshots": snapshots }),
        },
    )
    .await?;
    tx.commit().await?;
    tracing::info!(mirror = %mirror.name, "mirror deleted");
    Ok(())
}

async fn failed(pool: &PgPool, ids: &[Uuid], error: &RetentionError) -> Result<(), sqlx::Error> {
    tracing::warn!(%error, ?ids, "cannot delete a mirror; will retry");
    sqlx::query("UPDATE mirrors SET delete_error = $1 WHERE id = ANY($2)")
        .bind(error.to_string())
        .bind(ids)
        .execute(pool)
        .await?;
    Ok(())
}

/// Removes every mirror being deleted and returns how many. Needs a
/// [`super::store::RetentionLock`]. A failure stays on the mirror's row,
/// for its page, and the next pass starts it over.
pub(super) async fn finish(mirrors: &Mirrors) -> Result<usize, RetentionError> {
    let pool = mirrors.pool();
    let deleting = super::store::deleting(pool).await?;
    if deleting.is_empty() {
        return Ok(0);
    }
    let layout = mirrors.layout().clone();
    blocking(move || layout.prepare()).await?;
    let mut done = Vec::new();
    for mirror in deleting {
        match take_down(mirrors, &mirror).await {
            Ok(()) => done.push(mirror),
            Err(error) => failed(pool, &[mirror.id], &error).await?,
        }
    }
    let (apt_done, dnf_done): (Vec<_>, Vec<_>) =
        done.into_iter().partition(|m| m.family == Family::Apt);
    let mut removed = 0;
    for mirror in &dnf_done {
        forget(pool, mirror).await?;
        removed += 1;
    }
    if !apt_done.is_empty() {
        match clean_aptly(mirrors, &apt_done).await {
            Ok(()) => {
                for mirror in &apt_done {
                    forget(pool, mirror).await?;
                    removed += 1;
                }
            }
            Err(error) => {
                let ids: Vec<Uuid> = apt_done.iter().map(|m| m.id).collect();
                failed(pool, &ids, &error).await?;
            }
        }
    }
    Ok(removed)
}

/// Finishes in the background what [`request`] started, without holding up
/// the request: the lock waits for every running job.
pub fn start(mirrors: Mirrors) {
    tokio::spawn(async move {
        if let Err(error) = retention::prune(&mirrors).await {
            tracing::warn!(%error, "cannot remove old mirror snapshots; will retry");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mirror::apt::tests::mirror;

    #[test]
    fn a_refusal_says_what_uses_the_mirror() {
        let server = |hostname: &str| UsingServer {
            id: Uuid::nil(),
            hostname: hostname.into(),
        };
        let mut users = Users {
            campaigns: vec![UsingCampaign {
                id: Uuid::nil(),
                name: "march".into(),
            }],
            servers: vec![server("web-01"), server("web-02")],
        };
        assert_eq!(
            users.sentence("debian-12"),
            "debian-12 is still used by 1 update campaign (march) and 2 servers (web-01, web-02)"
        );
        users.campaigns.clear();
        users.servers = (0..7).map(|i| server(&format!("s{i}"))).collect();
        assert_eq!(
            users.sentence("m"),
            "m is still used by 7 servers (s0, s1, s2, s3, s4, 2 more)"
        );
    }

    #[test]
    fn the_dnf_cache_of_a_mirror_is_told_from_a_longer_name() {
        let m = Mirror {
            name: "rocky-9".into(),
            ..mirror(Family::Dnf)
        };
        assert!(in_dnf_cache("mf-rocky-9-0123456789abcdef", &m));
        assert!(in_dnf_cache("mf-rocky-9.solv", &m));
        assert!(in_dnf_cache("mf-rocky-9-filenames.solvx", &m));
        assert!(!in_dnf_cache("mf-rocky-9-baseos-0123456789abcdef", &m));
        assert!(!in_dnf_cache("mf-rocky-9-baseos.solv", &m));
        assert!(!in_dnf_cache("packages.db", &m));
    }
}
