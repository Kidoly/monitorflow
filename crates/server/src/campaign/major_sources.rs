//! Ubuntu and the RHEL family on the MonitorFlow mirrors in a major
//! upgrade campaign (ADR-0182). `do-release-upgrade` and leapp take the
//! next release from the distribution, and the mirror files name the old
//! release's mirror, so a server whose packages come from the mirrors gets,
//! one after the other:
//!
//! 1. `revert_sources`, its own sources back, before its snapshot;
//! 2. its snapshot and its `major_upgrade`, as any server;
//! 3. once its part ended, the mirrors back: those of the release it runs
//!    then, built as `use-mirror` builds them, after an upgrade done; its
//!    previous sources sent again (the last `configure_sources` that
//!    succeeded outside campaigns) when it did not upgrade. Where the
//!    mirrors do not carry the new release, it keeps its own sources and
//!    the audit log says why.
//!
//! The commands go in the columns of pinned campaigns (ADR-0038), which a
//! major upgrade campaign never has: `sources_command_id` holds the
//! revert, `restore_command_id` the mirrors back, so the tally waits for
//! the latter and the server's sources page sees a campaign holding them.
//! A server kept on its own sources has its revert as its restore too.
//! Debian's sources move to the new release's mirrors in the upgrade
//! itself and get none of this.

use monitorflow_common::command::CONFIGURE_SOURCES;
use monitorflow_common::major_upgrade::{self, Method};
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::model::MajorUpgradeSpec;
use super::pin::{previous, Next, Pending, PinSources, RESTORE_TTL};
use super::runner::record;
use super::store::CampaignRow;
use crate::api::package_sources;
use crate::commands::sources::{self, ArchiveKey};
use crate::commands::{CommandStore, NewCommand, State};

/// Whether servers of `spec` on the mirrors go back to their own sources
/// for the upgrade: Ubuntu and the RHEL family.
pub(super) fn needed(spec: &MajorUpgradeSpec) -> bool {
    major_upgrade::step(&spec.from, &spec.to).is_some_and(|u| u.method != Method::Apt)
}

/// The servers among `servers` whose packages come from the mirrors now.
pub(super) async fn on_mirror(
    conn: &mut PgConnection,
    servers: &[Uuid],
) -> Result<std::collections::HashSet<Uuid>, sqlx::Error> {
    package_sources::using_mirror(&mut *conn, servers).await
}

impl Pending {
    /// What a server of a campaign of [`needed`] needs before its
    /// snapshot, if anything: its own sources back when `on_mirror`, again
    /// after the mirrors came back while it waited (a pause), and nothing
    /// once they are back.
    pub(super) fn own_sources_first(&self, on_mirror: bool) -> Option<Next> {
        if self.restore_command_id.is_some() {
            return Some(match self.restore_state {
                Some(State::Succeeded) => Next::OwnSources,
                Some(state) if state.is_final() => {
                    Next::Fail(format!("putting the mirrors back {}", state.as_str()))
                }
                _ => Next::Wait,
            });
        }
        match self.configure_state {
            None if on_mirror => Some(Next::OwnSources),
            None | Some(State::Succeeded) => None,
            Some(state) if state.is_final() => Some(Next::Fail(match &self.configure_error {
                Some(error) => format!("putting its own sources back {}: {error}", state.as_str()),
                None => format!("putting its own sources back {}", state.as_str()),
            })),
            Some(_) => Some(Next::Wait),
        }
    }
}

/// A server whose own sources the campaign put back and whose part ended.
#[derive(Debug, sqlx::FromRow)]
struct Ended {
    server_id: Uuid,
    peer_id: String,
    os_family: String,
    os_version: Option<String>,
    sources_command_id: Uuid,
}

/// Queues the mirrors back for every server of `row` whose own sources it
/// put back and whose part ended: failed, done, or no upgrade coming now
/// (the campaign paused, stopped or failed). Returns the agents to wake.
pub(super) async fn mirrors_back(
    conn: &mut PgConnection,
    pool: &PgPool,
    row: &CampaignRow,
    spec: &MajorUpgradeSpec,
    context: Option<&PinSources>,
) -> Result<Vec<String>, sqlx::Error> {
    if !needed(spec) {
        return Ok(Vec::new());
    }
    let due: Vec<Ended> = sqlx::query_as(
        "SELECT t.server_id, s.peer_id, s.os_family, s.os_version, t.sources_command_id
         FROM campaign_targets t
         JOIN servers s ON s.id = t.server_id
         JOIN commands cfg ON cfg.id = t.sources_command_id
         LEFT JOIN commands u ON u.id = t.command_id
         LEFT JOIN server_major_upgrades m ON m.server_id = t.server_id
         WHERE t.campaign_id = $1 AND t.restore_command_id IS NULL AND s.revoked_at IS NULL
           AND cfg.sent_at IS NOT NULL
           AND cfg.state IN ('succeeded', 'failed', 'expired', 'cancelled')
           AND (t.error IS NOT NULL OR t.upgraded_at IS NOT NULL
                OR u.state IN ('failed', 'expired', 'cancelled')
                OR (t.command_id IS NULL AND (cfg.state <> 'succeeded' OR NOT $2))
                OR (NOT $2 AND u.state = 'succeeded'
                    AND m.report -> 'progress' ->> 'command_id' = u.id::text
                    AND m.report -> 'progress' ->> 'phase' IN ('done', 'failed')))
         ORDER BY s.hostname, s.peer_id",
    )
    .bind(row.id)
    .bind(row.state.is_active())
    .fetch_all(&mut *conn)
    .await?;
    let mut key: Option<Result<ArchiveKey, String>> = None;
    let mut peers = Vec::new();
    for ended in due {
        let os_version = ended.os_version.clone().unwrap_or_default();
        let upgraded = major_upgrade::runs(&os_version, &spec.to);
        let command = if upgraded {
            let key = match &key {
                Some(key) => key,
                None => key.insert(match context {
                    None => Err("mirrors are not enabled on this server".to_owned()),
                    Some(c) => sources::archive_key(c.mirrors.runner(), c.mirrors.layout())
                        .await
                        .map_err(|e| format!("cannot read the mirror signing key: {e}")),
                }),
            };
            let built = match key {
                Ok(key) => {
                    let public_url = context.and_then(|c| c.live.mirror_public_url());
                    package_sources::mirror_spec(
                        pool,
                        public_url.as_deref(),
                        key,
                        ended.server_id,
                        &ended.os_family,
                        &os_version,
                    )
                    .await?
                }
                Err(why) => Err(why.clone()),
            };
            built.map(|payload| (CONFIGURE_SOURCES.to_owned(), payload))
        } else {
            Ok(previous(&mut *conn, ended.server_id).await?)
        };
        let restore = match command {
            Ok((kind, payload)) => {
                let new = NewCommand {
                    server_id: ended.server_id,
                    kind,
                    payload,
                    ttl: RESTORE_TTL,
                    requested_by: None,
                    campaign_id: Some(row.id),
                };
                let command = CommandStore::enqueue_in(&mut *conn, &new).await?;
                peers.push(ended.peer_id);
                command.id
            }
            Err(reason) => {
                let details = json!({
                    "server_id": ended.server_id,
                    "release": os_version,
                    "reason": reason,
                });
                record(conn, row.id, "campaign.sources_kept_own", details).await?;
                ended.sources_command_id
            }
        };
        sqlx::query(
            "UPDATE campaign_targets SET restore_command_id = $3
             WHERE campaign_id = $1 AND server_id = $2",
        )
        .bind(row.id)
        .bind(ended.server_id)
        .bind(restore)
        .execute(&mut *conn)
        .await?;
    }
    if !peers.is_empty() {
        let details = json!({ "commands": peers.len() });
        record(conn, row.id, "campaign.restore_sources", details).await?;
    }
    Ok(peers)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(configure: Option<State>, restore: Option<State>) -> Pending {
        Pending {
            server_id: Uuid::nil(),
            peer_id: "agent-1".into(),
            revoked: false,
            os_family: "debian".into(),
            package_updates: true,
            configure_state: configure,
            configure_error: None,
            restore_command_id: restore.map(|_| Uuid::nil()),
            restore_state: restore,
            snapshot_command_id: None,
            snapshot_state: None,
            snapshot_error: None,
            proxmox_mapped: false,
            storage: None,
            reports_third_party: false,
            vm_snapshot_state: None,
            vm_snapshot_at: None,
            vm_snapshot_error: None,
            software_state: None,
            msi_known: false,
        }
    }

    fn spec(from: &str, to: &str) -> MajorUpgradeSpec {
        MajorUpgradeSpec {
            from: from.into(),
            to: to.into(),
            accept_limited_rollback: false,
            parallel: 1,
        }
    }

    #[test]
    fn only_ubuntu_and_the_rhel_family_go_back_to_their_own_sources() {
        assert!(!needed(&spec("debian 12", "debian 13")));
        assert!(needed(&spec("ubuntu 22.04", "ubuntu 24.04")));
        assert!(needed(&spec("almalinux 9", "almalinux 10")));
    }

    #[test]
    fn own_sources_come_back_first_then_the_upgrade() {
        // Not on the mirrors: straight to the snapshot.
        assert_eq!(pending(None, None).own_sources_first(false), None);
        assert_eq!(
            pending(None, None).own_sources_first(true),
            Some(Next::OwnSources)
        );
        assert_eq!(
            pending(Some(State::Sent), None).own_sources_first(true),
            Some(Next::Wait)
        );
        assert_eq!(
            pending(Some(State::Succeeded), None).own_sources_first(false),
            None,
            "back on its own sources: the snapshot next"
        );
        let mut failed = pending(Some(State::Failed), None);
        failed.configure_error = Some("no backup".into());
        assert_eq!(
            failed.own_sources_first(false),
            Some(Next::Fail(
                "putting its own sources back failed: no backup".into()
            ))
        );
        // The mirrors came back during a pause: their own sources again.
        assert_eq!(
            pending(Some(State::Succeeded), Some(State::Succeeded)).own_sources_first(false),
            Some(Next::OwnSources)
        );
        assert_eq!(
            pending(Some(State::Succeeded), Some(State::Running)).own_sources_first(false),
            Some(Next::Wait)
        );
        assert_eq!(
            pending(Some(State::Succeeded), Some(State::Expired)).own_sources_first(false),
            Some(Next::Fail("putting the mirrors back expired".into()))
        );
    }
}
