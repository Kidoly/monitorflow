//! Automatic agent updates (ADR-0055, after ADR-0053's per-site policy).
//!
//! Every [`POLL`], unless paused, the server picks the release peers move
//! to ([`policy::target`]) and queues a `self_update` to it for online
//! agents of the sites where automatic updates apply: the server-wide
//! setting, or the site's override. [`policy`] decides how many per site:
//! a canary, then batches. A server is updated automatically at most once
//! per version (`agent_auto_updates`), so a release that rolls back is not
//! retried in a loop; one whose command expired unsent never ran and is
//! tried again. Once [`policy::Settings::failure_threshold`] updates to a
//! version failed, its rollout halts (`auto_rollouts`), is audited and
//! logged as an error, and waits for an administrator.

pub mod policy;
pub mod store;

use std::collections::BTreeMap;
use std::future::Future;
use std::str::FromStr;
use std::time::Duration;

use monitorflow_common::command::SELF_UPDATE;
use monitorflow_common::release::Version;
use monitorflow_common::tls::PeerRole;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use crate::agent_releases::Sources;
use crate::api::audit::{self, Event};
use crate::commands::{CommandStore, Commands, NewCommand};
use crate::grpc::Registry;
use crate::mirror::relays;
use policy::Tries;
use store::{Kind, Rollout, RolloutState};

pub const POLL: Duration = Duration::from_secs(5 * 60);
/// How long a queued automatic update may wait for its agent.
pub const TTL: Duration = Duration::from_secs(24 * 60 * 60);
pub(crate) const ACTOR: (&str, &str) = ("system", "auto-update");

/// The automatic updater; clones share the pool.
#[derive(Clone)]
pub struct AutoUpdate {
    pool: PgPool,
    sources: Sources,
    /// `auto_update.enabled` from the configuration, which wins.
    enabled_override: Option<bool>,
    /// Peers never move past the server's own version.
    server_version: Version,
    /// Only connected agents are picked; `None` takes every agent.
    registry: Option<Registry>,
}

/// One unrevoked server and its automatic update to the target, if any.
#[derive(Debug, sqlx::FromRow)]
pub(crate) struct Peer {
    pub id: Uuid,
    pub peer_id: String,
    pub site_id: Uuid,
    pub site_override: Option<bool>,
    pub agent_version: Option<String>,
    /// State of the command of this version's automatic update: `None`
    /// when not tried, `unknown` when the command is gone.
    pub tried: Option<String>,
    /// Whether that update counts towards the rollout's failures.
    pub counted: Option<bool>,
    /// Another `self_update` is in flight.
    pub busy: bool,
}

impl Peer {
    pub fn needs(&self, target: &Version) -> bool {
        self.agent_version
            .as_deref()
            .is_some_and(|v| policy::needs_update(v, target))
    }
}

/// The automatic updates of `peers`, per site.
pub(crate) fn tries(peers: &[Peer]) -> BTreeMap<Uuid, Tries> {
    let mut sites: BTreeMap<Uuid, Tries> = BTreeMap::new();
    for peer in peers {
        let tries = sites.entry(peer.site_id).or_default();
        match peer.tried.as_deref() {
            Some("queued" | "sent" | "running") => tries.pending += 1,
            Some("succeeded") => tries.succeeded += 1,
            Some("failed") if peer.counted == Some(true) => tries.failed += 1,
            _ => {}
        }
    }
    sites
}

impl AutoUpdate {
    pub fn new(pool: PgPool, sources: Sources) -> Self {
        Self {
            pool,
            sources,
            enabled_override: None,
            server_version: server_version(),
            registry: None,
        }
    }

    pub fn with_enabled_override(mut self, enabled: Option<bool>) -> Self {
        self.enabled_override = enabled;
        self
    }

    pub fn with_registry(mut self, registry: Registry) -> Self {
        self.registry = Some(registry);
        self
    }

    pub fn with_server_version(mut self, version: Version) -> Self {
        self.server_version = version;
        self
    }

    fn online(&self, peer_id: &str) -> bool {
        self.registry.as_ref().is_none_or(|registry| {
            registry
                .get(peer_id)
                .is_some_and(|p| p.role == PeerRole::Agent)
        })
    }

    /// Queues the agent updates due now; returns the peers that got one.
    pub async fn tick(&self) -> Result<Vec<String>, sqlx::Error> {
        let settings = store::settings(&self.pool, self.enabled_override).await?.0;
        if settings.paused {
            return Ok(Vec::new());
        }
        store::release_expired(&self.pool).await?;
        let held = self.sources.held().await;
        let Some(target) = policy::target(&held, &self.server_version).cloned() else {
            return Ok(Vec::new());
        };
        let rollout = Rollout::load(&self.pool, Kind::Agent, &target).await?;
        if rollout
            .as_ref()
            .is_some_and(|r| r.state == RolloutState::Halted)
        {
            return Ok(Vec::new());
        }
        let counted_from = rollout.as_ref().map(|r| r.counted_from);
        let peers = store::peers(&self.pool, &target, counted_from).await?;
        let sites = tries(&peers);
        let failed: usize = sites.values().map(|t| t.failed).sum();
        if policy::halts(failed, settings.failure_threshold) {
            let reason = format!(
                "{failed} automatic updates to {target} failed (threshold {})",
                settings.failure_threshold
            );
            if Rollout::halt(&self.pool, Kind::Agent, &target, &reason).await? {
                tracing::error!(version = %target, failed, "automatic agent rollout halted; resume it in the web interface once the cause is fixed");
            }
            return Ok(Vec::new());
        }

        let mut by_site: BTreeMap<Uuid, Vec<&Peer>> = BTreeMap::new();
        for peer in &peers {
            by_site.entry(peer.site_id).or_default().push(peer);
        }
        let mut queued = Vec::new();
        for (site, members) in by_site {
            if !settings.site_enabled(members[0].site_override) {
                continue;
            }
            let allowance = sites
                .get(&site)
                .copied()
                .unwrap_or_default()
                .allowance(members.len());
            let due = members
                .into_iter()
                .filter(|p| p.tried.is_none() && !p.busy && p.needs(&target))
                .filter(|p| self.online(&p.peer_id))
                .take(allowance);
            for peer in due {
                if self.queue(peer, &target).await? {
                    queued.push(peer.peer_id.clone());
                }
            }
        }
        Ok(queued)
    }

    /// Queues one update, claiming the server and version first; `false`
    /// when another tick claimed it.
    async fn queue(&self, peer: &Peer, target: &Version) -> Result<bool, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        Rollout::start(&mut tx, Kind::Agent, target).await?;
        let claimed = sqlx::query(
            "INSERT INTO agent_auto_updates (server_id, version) VALUES ($1, $2)
             ON CONFLICT DO NOTHING",
        )
        .bind(peer.id)
        .bind(target.to_string())
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if claimed == 0 {
            return Ok(false);
        }
        let relay = relays::url_for_server(&mut tx, peer.id).await?;
        let new = NewCommand {
            server_id: peer.id,
            kind: SELF_UPDATE.into(),
            payload: self.sources.payload(target, relay.as_deref(), false, 0),
            ttl: TTL,
            requested_by: None,
            campaign_id: None,
        };
        let command = CommandStore::enqueue_in(&mut *tx, &new).await?;
        sqlx::query(
            "UPDATE agent_auto_updates SET command_id = $3 WHERE server_id = $1 AND version = $2",
        )
        .bind(peer.id)
        .bind(target.to_string())
        .bind(command.id)
        .execute(&mut *tx)
        .await?;
        audit::record(
            &mut *tx,
            Event {
                actor_kind: ACTOR.0,
                actor_id: ACTOR.1,
                action: "agent.auto_update",
                target: Some(("command", &command.id.to_string())),
                details: json!({
                    "server_id": peer.id,
                    "peer_id": peer.peer_id,
                    "from": peer.agent_version,
                    "to": target.to_string(),
                }),
            },
        )
        .await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Runs [`Self::tick`] every `poll` until `shutdown`, waking the
    /// dispatcher for the agents that got an update.
    pub async fn run(self, commands: Commands, poll: Duration, shutdown: impl Future<Output = ()>) {
        let mut interval = tokio::time::interval(poll);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                _ = &mut shutdown => break,
                _ = interval.tick() => {}
            }
            match self.tick().await {
                Ok(peers) if peers.is_empty() => {}
                Ok(peers) => {
                    tracing::info!(agents = peers.len(), "automatic agent updates queued");
                    peers.iter().for_each(|peer| commands.dispatch(peer));
                }
                Err(error) => {
                    tracing::warn!(%error, "cannot queue automatic agent updates; will retry")
                }
            }
        }
    }
}

/// This server's version, the ceiling of automatic updates.
pub fn server_version() -> Version {
    Version::from_str(monitorflow_common::VERSION).expect("the build version is a version")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(site: u128, tried: Option<&str>, counted: bool) -> Peer {
        Peer {
            id: Uuid::new_v4(),
            peer_id: "p".into(),
            site_id: Uuid::from_u128(site),
            site_override: None,
            agent_version: Some("0.1.0".into()),
            tried: tried.map(str::to_owned),
            counted: Some(counted),
            busy: false,
        }
    }

    #[test]
    fn tries_count_per_site_and_failures_only_since_the_count_started() {
        let peers = [
            peer(1, Some("running"), true),
            peer(1, Some("succeeded"), true),
            peer(1, Some("failed"), true),
            peer(1, Some("failed"), false),
            peer(1, Some("cancelled"), true),
            peer(1, None, true),
            peer(2, Some("queued"), true),
        ];
        let sites = tries(&peers);
        assert_eq!(
            sites[&Uuid::from_u128(1)],
            Tries {
                pending: 1,
                succeeded: 1,
                failed: 1
            }
        );
        assert_eq!(sites[&Uuid::from_u128(2)].pending, 1);
    }

    #[test]
    fn peers_need_an_update_only_from_an_older_version() {
        let target: Version = "0.2.0".parse().unwrap();
        let mut p = peer(1, None, true);
        assert!(p.needs(&target));
        p.agent_version = Some("0.2.0".into());
        assert!(!p.needs(&target));
        p.agent_version = None;
        assert!(!p.needs(&target));
    }

    #[test]
    fn the_server_version_parses() {
        assert_eq!(server_version().to_string(), monitorflow_common::VERSION);
    }
}
