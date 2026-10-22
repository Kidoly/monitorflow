//! Campaigns pinned to mirror snapshots (ADR-0038). Each server of such a
//! campaign gets three commands, one after the other:
//!
//! 1. `configure_sources` pointing it at the pinned snapshots of its OS
//!    family, queued inside a window like an update;
//! 2. once that succeeded, its `apply_updates`;
//! 3. once the update ended, or will not come now (the configuration
//!    failed, the campaign is paused, stopped or failed, or its window
//!    closed), a restore of its previous sources: the last
//!    `configure_sources` or `revert_sources` that succeeded outside
//!    campaigns, sent again, else `revert_sources`. Restores are queued
//!    whatever the campaign's state, never cancelled, and live
//!    [`RESTORE_TTL`].
//!
//! A server restored before its update starts again at 1. The runner also
//! writes `.pinned` in the served directory of each mirror, so the mirror
//! index lists the snapshots of unfinished campaigns and relays copy them.

use std::fmt;
use std::time::Duration;

use monitorflow_common::command::{CONFIGURE_SOURCES, REVERT_SOURCES};
use monitorflow_common::mirror::write_pinned;
use monitorflow_common::proto::v1::RevertSources;
use prost::Message;
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::model::SourcesMode;
use super::runner::record;
use super::store::CampaignRow;
use crate::api::commands::MAX_TTL_S;
use crate::commands::sources::{self, os_family, ArchiveKey, Selected};
use crate::commands::{CommandStore, NewCommand, State};
use crate::mirror::{store, Family, Layout, Mirrors};

/// Time to live of a restore: the longest a command may have, as putting
/// a server back is never wrong, even late.
pub const RESTORE_TTL: Duration = Duration::from_secs(MAX_TTL_S);

/// What pinned campaigns need from the server: the URL agents reach the
/// mirrors at and the mirror signing key.
#[derive(Clone)]
pub struct PinSources {
    pub public_url: Option<String>,
    pub mirrors: Mirrors,
}

impl fmt::Debug for PinSources {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PinSources")
            .field("public_url", &self.public_url)
            .finish_non_exhaustive()
    }
}

/// A server of the current wave without an update command, and where its
/// sources stand.
#[derive(Debug, Clone, sqlx::FromRow)]
pub(super) struct Pending {
    pub server_id: Uuid,
    pub peer_id: String,
    pub revoked: bool,
    pub site_id: Uuid,
    pub os_family: String,
    pub configure_state: Option<State>,
    pub configure_error: Option<String>,
    pub restore_command_id: Option<Uuid>,
    pub restore_state: Option<State>,
}

/// What a [`Pending`] server of a pinned campaign needs next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Next {
    Configure,
    Update,
    Wait,
    /// Restored before its update: configure again.
    Restart,
    /// Gets no update, for this reason.
    Fail(String),
}

impl Pending {
    pub(super) fn next(&self) -> Next {
        // A failed configuration ends the server's part, restored or not.
        if let Some(state) = self.configure_state {
            if state.is_final() && state != State::Succeeded {
                return Next::Fail(match &self.configure_error {
                    Some(error) => format!("configure_sources {}: {error}", state.as_str()),
                    None => format!("configure_sources {}", state.as_str()),
                });
            }
        }
        if self.restore_command_id.is_some() {
            return match self.restore_state {
                Some(State::Succeeded) => Next::Restart,
                Some(state) if state.is_final() => {
                    Next::Fail(format!("restoring sources {}", state.as_str()))
                }
                _ => Next::Wait,
            };
        }
        match self.configure_state {
            None => Next::Configure,
            Some(State::Succeeded) => Next::Update,
            Some(_) => Next::Wait,
        }
    }
}

/// Builds the `configure_sources` payloads of one campaign.
pub(super) struct Configurer {
    mode: SourcesMode,
    public_url: Option<String>,
    key: Result<ArchiveKey, String>,
    pinned: Vec<Selected>,
}

impl Configurer {
    pub(super) async fn new(
        conn: &mut PgConnection,
        row: &CampaignRow,
        mode: SourcesMode,
        context: Option<&PinSources>,
    ) -> Result<Self, sqlx::Error> {
        let rows: Vec<(Uuid, String)> = sqlx::query_as(
            "SELECT cs.mirror_id, s.name FROM campaign_snapshots cs
             JOIN mirror_snapshots s ON s.id = cs.snapshot_id
             WHERE cs.campaign_id = $1 AND s.state = 'published'",
        )
        .bind(row.id)
        .fetch_all(&mut *conn)
        .await?;
        let mut pinned = Vec::with_capacity(rows.len());
        for (mirror, snapshot) in rows {
            if let Some(mirror) = store::get(&mut *conn, mirror).await? {
                pinned.push(Selected { mirror, snapshot });
            }
        }
        pinned.sort_by(|a, b| a.mirror.name.cmp(&b.mirror.name));
        let key = match context {
            None => Err("mirrors are not enabled on this server".to_owned()),
            Some(c) => sources::archive_key(c.mirrors.runner(), c.mirrors.layout())
                .await
                .map_err(|e| format!("cannot read the mirror signing key: {e}")),
        };
        Ok(Self {
            mode,
            public_url: context.and_then(|c| c.public_url.clone()),
            key,
            pinned,
        })
    }

    /// The encoded spec for a server of `site` and `os_family`.
    pub(super) fn payload(&self, site: Uuid, os: &str) -> Result<Vec<u8>, String> {
        let key = self.key.as_ref().map_err(Clone::clone)?;
        let base = sources::base_url(self.public_url.as_deref(), site)
            .ok_or("mirror_public_url is not configured")?;
        let selected: Vec<Selected> = self
            .pinned
            .iter()
            .filter(|s| os_family(s.mirror.family) == os)
            .cloned()
            .collect();
        let spec = sources::build(&base, self.mode.into(), os, &selected, key)?;
        Ok(spec.encode_to_vec())
    }
}

/// Queues the restore of every server of `row` whose update ended or will
/// not come now; `updates_open` says whether the runner may queue updates
/// at this time. Returns the agents to wake.
pub(super) async fn restore(
    conn: &mut PgConnection,
    row: &CampaignRow,
    updates_open: bool,
) -> Result<Vec<String>, sqlx::Error> {
    let due: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT t.server_id, s.peer_id FROM campaign_targets t
         JOIN servers s ON s.id = t.server_id
         JOIN commands cfg ON cfg.id = t.sources_command_id
         LEFT JOIN commands u ON u.id = t.command_id
         WHERE t.campaign_id = $1 AND t.restore_command_id IS NULL AND s.revoked_at IS NULL
           AND cfg.sent_at IS NOT NULL
           AND cfg.state IN ('succeeded', 'failed', 'expired', 'cancelled')
           AND (u.state IN ('succeeded', 'failed', 'expired', 'cancelled')
                OR (t.command_id IS NULL
                    AND (cfg.state <> 'succeeded' OR t.error IS NOT NULL OR NOT $2)))
         ORDER BY s.hostname, s.peer_id",
    )
    .bind(row.id)
    .bind(updates_open)
    .fetch_all(&mut *conn)
    .await?;
    let mut peers = Vec::with_capacity(due.len());
    for (server_id, peer_id) in due {
        let (kind, payload) = previous(conn, server_id).await?;
        let new = NewCommand {
            server_id,
            kind,
            payload,
            ttl: RESTORE_TTL,
            requested_by: None,
            campaign_id: Some(row.id),
        };
        let command = CommandStore::enqueue_in(&mut *conn, &new).await?;
        sqlx::query(
            "UPDATE campaign_targets SET restore_command_id = $3
             WHERE campaign_id = $1 AND server_id = $2",
        )
        .bind(row.id)
        .bind(server_id)
        .bind(command.id)
        .execute(&mut *conn)
        .await?;
        peers.push(peer_id);
    }
    if !peers.is_empty() {
        let details = json!({ "commands": peers.len() });
        record(conn, row.id, "campaign.restore_sources", details).await?;
    }
    Ok(peers)
}

/// The command putting `server`'s sources back as they were before any
/// campaign: its last source command that succeeded outside campaigns,
/// or a revert when that was a revert or there was none.
async fn previous(conn: &mut PgConnection, server: Uuid) -> Result<(String, Vec<u8>), sqlx::Error> {
    let last: Option<(String, Vec<u8>)> = sqlx::query_as(
        "SELECT kind, payload FROM commands
         WHERE server_id = $1 AND campaign_id IS NULL AND state = 'succeeded'
           AND kind IN ($2, $3)
         ORDER BY finished_at DESC NULLS LAST, issued_at DESC
         LIMIT 1",
    )
    .bind(server)
    .bind(CONFIGURE_SOURCES)
    .bind(REVERT_SOURCES)
    .fetch_optional(conn)
    .await?;
    Ok(match last {
        Some((kind, payload)) if kind == CONFIGURE_SOURCES => (kind, payload),
        _ => (REVERT_SOURCES.into(), RevertSources {}.encode_to_vec()),
    })
}

#[derive(Debug, thiserror::Error)]
pub enum PinError {
    #[error("database query failed: {0}")]
    Database(#[from] sqlx::Error),
    #[error("cannot write pinned snapshots: {0}")]
    Io(#[from] std::io::Error),
}

/// Writes `.pinned` in the served directory of every mirror that has one:
/// the snapshots that campaigns not yet finished pin.
pub async fn write_pins(pool: &PgPool, layout: &Layout) -> Result<(), PinError> {
    let mirrors: Vec<(Family, String, Vec<String>)> = sqlx::query_as(
        "SELECT m.family, m.name,
             COALESCE(array_agg(DISTINCT s.name) FILTER (WHERE c.id IS NOT NULL), '{}')
         FROM mirrors m
         LEFT JOIN campaign_snapshots cs ON cs.mirror_id = m.id
         LEFT JOIN campaigns c
             ON c.id = cs.campaign_id AND c.state NOT IN ('stopped', 'completed', 'failed')
         LEFT JOIN mirror_snapshots s ON s.id = cs.snapshot_id
         GROUP BY m.id, m.family, m.name",
    )
    .fetch_all(pool)
    .await?;
    let layout = layout.clone();
    tokio::task::spawn_blocking(move || {
        for (family, name, mut pinned) in mirrors {
            let dir = layout.served(family).join(name);
            if dir.is_dir() {
                pinned.sort();
                write_pinned(&dir, &pinned)?;
            }
        }
        Ok::<_, std::io::Error>(())
    })
    .await
    .map_err(std::io::Error::other)??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(configure: Option<State>, restore: Option<State>) -> Pending {
        Pending {
            server_id: Uuid::nil(),
            peer_id: "agent-1".into(),
            revoked: false,
            site_id: Uuid::nil(),
            os_family: "debian".into(),
            configure_state: configure,
            configure_error: Some("apt-get update failed".into()),
            restore_command_id: restore.map(|_| Uuid::nil()),
            restore_state: restore,
        }
    }

    #[test]
    fn servers_go_configure_then_update() {
        assert_eq!(pending(None, None).next(), Next::Configure);
        assert_eq!(pending(Some(State::Queued), None).next(), Next::Wait);
        assert_eq!(pending(Some(State::Running), None).next(), Next::Wait);
        assert_eq!(pending(Some(State::Succeeded), None).next(), Next::Update);
        assert_eq!(
            pending(Some(State::Failed), None).next(),
            Next::Fail("configure_sources failed: apt-get update failed".into())
        );
        // Restored before the update.
        let restored = Some(State::Succeeded);
        assert_eq!(pending(restored, Some(State::Sent)).next(), Next::Wait);
        assert_eq!(
            pending(restored, Some(State::Succeeded)).next(),
            Next::Restart
        );
        assert_eq!(
            pending(restored, Some(State::Expired)).next(),
            Next::Fail("restoring sources expired".into())
        );
        // Restored after a failed configuration: done, not configured again.
        assert!(matches!(
            pending(Some(State::Failed), Some(State::Succeeded)).next(),
            Next::Fail(_)
        ));
    }
}
