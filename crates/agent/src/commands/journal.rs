//! Commands this agent started or finished, kept on disk so a command
//! delivered again, after a reconnect or a restart, never runs twice
//! (ADR-0031).
//!
//! One file per command, `<command_id>.pb`, holding an [`Entry`]: written
//! before the handler starts, and again with the final result. Files are
//! written atomically and synced, so a crash leaves the previous state.
//! Entries are kept until [`RETENTION`] after the command's expiry, at most
//! [`MAX_ENTRIES`] of them.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use monitorflow_common::client::identity::write_atomic;
use monitorflow_common::proto::v1::CommandResult;
use prost::Message;

/// Directory under `data_dir`.
pub const DIR_NAME: &str = "commands";
/// How long an entry outlives its command's expiry. Until then a replay is
/// refused by the journal, after it by the expiry.
pub const RETENTION: Duration = Duration::from_secs(24 * 60 * 60);
pub const MAX_ENTRIES: usize = 1024;
const EXTENSION: &str = "pb";
const FILE_MODE: u32 = 0o600;

/// What the journal knows of one command.
#[derive(Clone, PartialEq, Message)]
pub struct Entry {
    #[prost(int64, tag = "1")]
    pub expires_at_unix_ms: i64,
    /// `None` while started and not finished.
    #[prost(message, optional, tag = "2")]
    pub result: Option<CommandResult>,
}

/// Command IDs come from the server (UUIDs) and name files: only these
/// characters are accepted, so an ID can never leave the directory.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

#[derive(Debug, Clone)]
pub struct Journal {
    dir: PathBuf,
}

impl Journal {
    /// The journal in `<data_dir>/commands`, created if missing.
    pub fn in_dir(data_dir: &Path) -> Self {
        Self {
            dir: data_dir.join(DIR_NAME),
        }
    }

    fn path(&self, id: &str) -> PathBuf {
        debug_assert!(valid_id(id));
        self.dir.join(format!("{id}.{EXTENSION}"))
    }

    /// The entry of `id`. An unreadable file reads as started and not
    /// finished: the safe answer is not to run it again.
    pub async fn get(&self, id: &str) -> io::Result<Option<Entry>> {
        match tokio::fs::read(self.path(id)).await {
            Ok(bytes) => Ok(Some(Entry::decode(bytes.as_slice()).unwrap_or_else(|_| {
                tracing::warn!(
                    command_id = id,
                    "unreadable journal entry; treating it as interrupted"
                );
                Entry {
                    expires_at_unix_ms: 0,
                    result: None,
                }
            }))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Records `entry` for `id`, synced to disk before returning.
    pub async fn put(&self, id: &str, entry: &Entry) -> io::Result<()> {
        let (path, bytes) = (self.path(id), entry.encode_to_vec());
        tokio::task::spawn_blocking(move || write_atomic(&path, &bytes, FILE_MODE))
            .await
            .map_err(io::Error::other)?
            .map_err(io::Error::other)
    }

    /// Deletes entries [`RETENTION`] past their expiry, then the ones
    /// expiring first beyond [`MAX_ENTRIES`]. Errors are logged.
    pub async fn prune(&self, now_unix_ms: i64) {
        let dir = self.dir.clone();
        let pruned = tokio::task::spawn_blocking(move || prune(&dir, now_unix_ms)).await;
        match pruned {
            Ok(Ok(0)) => {}
            Ok(Ok(count)) => tracing::debug!(count, "pruned the command journal"),
            Ok(Err(error)) => tracing::warn!(%error, "could not prune the command journal"),
            Err(error) => tracing::warn!(%error, "could not prune the command journal"),
        }
    }
}

fn prune(dir: &Path, now_unix_ms: i64) -> io::Result<usize> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let cutoff = now_unix_ms.saturating_sub(RETENTION.as_millis() as i64);
    let mut kept = Vec::new();
    let mut removed = 0;
    for file in entries {
        let path = file?.path();
        if path.extension().and_then(|e| e.to_str()) != Some(EXTENSION) {
            continue;
        }
        // Unreadable entries are kept until the count limit drops them.
        let expires = std::fs::read(&path)
            .ok()
            .and_then(|bytes| Entry::decode(bytes.as_slice()).ok())
            .map_or(i64::MAX, |e| e.expires_at_unix_ms);
        if expires < cutoff {
            std::fs::remove_file(&path)?;
            removed += 1;
        } else {
            kept.push((expires, path));
        }
    }
    if kept.len() > MAX_ENTRIES {
        kept.sort();
        for (_, path) in &kept[..kept.len() - MAX_ENTRIES] {
            std::fs::remove_file(path)?;
            removed += 1;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    fn finished(expires: i64) -> Entry {
        Entry {
            expires_at_unix_ms: expires,
            result: Some(CommandResult {
                command_id: "x".into(),
                exit_code: 3,
                ..CommandResult::default()
            }),
        }
    }

    #[test]
    fn ids_are_checked_before_naming_files() {
        assert!(valid_id("0b5c2f1e-6f0b-4c1d-9d55-1f0e6b0c2a11"));
        for bad in ["", "../x", "a/b", "a.pb", "a b", &"a".repeat(65)] {
            assert!(!valid_id(bad), "{bad}");
        }
    }

    #[tokio::test]
    async fn entries_round_trip_and_survive_reopening() {
        let data = TempDir::new().unwrap();
        let journal = Journal::in_dir(data.path());
        assert_eq!(journal.get("a").await.unwrap(), None);
        let started = Entry {
            expires_at_unix_ms: 5,
            result: None,
        };
        journal.put("a", &started).await.unwrap();
        assert_eq!(journal.get("a").await.unwrap(), Some(started));
        journal.put("a", &finished(5)).await.unwrap();
        let reopened = Journal::in_dir(data.path());
        assert_eq!(reopened.get("a").await.unwrap(), Some(finished(5)));

        // Garbage reads as interrupted.
        std::fs::write(data.path().join("commands/b.pb"), b"\xff\xff\xff").unwrap();
        assert_eq!(reopened.get("b").await.unwrap().unwrap().result, None);
    }

    #[tokio::test]
    async fn prune_drops_old_entries_then_the_earliest_beyond_the_limit() {
        let data = TempDir::new().unwrap();
        let journal = Journal::in_dir(data.path());
        let day = RETENTION.as_millis() as i64;
        let now = 10 * day;
        journal.put("old", &finished(now - day - 1)).await.unwrap();
        journal
            .put("recent", &finished(now - day + 1))
            .await
            .unwrap();
        journal.prune(now).await;
        assert_eq!(journal.get("old").await.unwrap(), None);
        assert!(journal.get("recent").await.unwrap().is_some());

        for i in 0..MAX_ENTRIES {
            journal
                .put(&format!("n{i}"), &finished(now + i as i64))
                .await
                .unwrap();
        }
        journal.prune(now).await;
        // "recent" expires first, so it goes.
        assert_eq!(journal.get("recent").await.unwrap(), None);
        assert!(journal.get("n0").await.unwrap().is_some());
        let count = std::fs::read_dir(data.path().join(DIR_NAME))
            .unwrap()
            .count();
        assert_eq!(count, MAX_ENTRIES);
    }
}
