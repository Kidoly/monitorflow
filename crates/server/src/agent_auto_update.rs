//! The per-site agent auto-update policy (ADR-0053), off by default. In
//! sites with `agent_auto_update`, every [`POLL`] the server queues a
//! `self_update` to the newest release it holds for each unrevoked Linux
//! server whose agent is older and has no update in flight. A server gets
//! at most one automatic update per version (`agent_auto_updates`), so a
//! release that rolls back is not tried again; an administrator can still
//! update it by hand.

use std::future::Future;
use std::str::FromStr;
use std::time::Duration;

use monitorflow_common::command::SELF_UPDATE;
use monitorflow_common::release::Version;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use crate::agent_releases::{self, Sources};
use crate::api::audit::{self, Event};
use crate::commands::{CommandStore, Commands, NewCommand};

pub const POLL: Duration = Duration::from_secs(10 * 60);
/// How long a queued automatic update may wait for its agent.
pub const TTL: Duration = Duration::from_secs(24 * 60 * 60);
const ACTOR: (&str, &str) = ("system", "agent-auto-update");

#[derive(sqlx::FromRow)]
struct Candidate {
    id: Uuid,
    peer_id: String,
    agent_version: String,
}

/// Queues the updates due now; returns the peers that got one.
pub async fn tick(pool: &PgPool, sources: &Sources) -> Result<Vec<String>, sqlx::Error> {
    let cache = sources.cache.clone();
    let versions = tokio::task::spawn_blocking(move || cache.versions())
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();
    let Some(latest) = agent_releases::latest(&versions).cloned() else {
        return Ok(Vec::new());
    };
    let candidates: Vec<Candidate> = sqlx::query_as(
        "SELECT s.id, s.peer_id, s.agent_version
         FROM servers s JOIN sites t ON t.id = s.site_id
         WHERE t.agent_auto_update AND s.revoked_at IS NULL AND s.os_family <> 'windows'
           AND s.agent_version IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM agent_auto_updates a
                           WHERE a.server_id = s.id AND a.version = $1)
           AND NOT EXISTS (SELECT 1 FROM commands c
                           WHERE c.server_id = s.id AND c.kind = $2
                             AND c.state IN ('queued', 'sent', 'running'))
         ORDER BY s.hostname",
    )
    .bind(latest.to_string())
    .bind(SELF_UPDATE)
    .fetch_all(pool)
    .await?;
    let payload = sources.payload(&latest, false, 0);
    let mut queued = Vec::new();
    for server in candidates {
        // Agents that are newer, or report something that is not a version
        // (a development build), are left alone.
        let older = Version::from_str(&server.agent_version).is_ok_and(|v| v < latest);
        if !older {
            continue;
        }
        let mut tx = pool.begin().await?;
        let claimed = sqlx::query(
            "INSERT INTO agent_auto_updates (server_id, version) VALUES ($1, $2)
             ON CONFLICT DO NOTHING",
        )
        .bind(server.id)
        .bind(latest.to_string())
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if claimed == 0 {
            continue;
        }
        let new = NewCommand {
            server_id: server.id,
            kind: SELF_UPDATE.into(),
            payload: payload.clone(),
            ttl: TTL,
            requested_by: None,
            campaign_id: None,
        };
        let command = CommandStore::enqueue_in(&mut *tx, &new).await?;
        sqlx::query(
            "UPDATE agent_auto_updates SET command_id = $3 WHERE server_id = $1 AND version = $2",
        )
        .bind(server.id)
        .bind(latest.to_string())
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
                    "server_id": server.id,
                    "peer_id": server.peer_id,
                    "from": server.agent_version,
                    "to": latest.to_string(),
                }),
            },
        )
        .await?;
        tx.commit().await?;
        queued.push(server.peer_id);
    }
    Ok(queued)
}

/// Runs [`tick`] every `poll` until `shutdown`, waking the dispatcher for
/// the agents that got an update.
pub async fn run(
    pool: PgPool,
    sources: Sources,
    commands: Commands,
    poll: Duration,
    shutdown: impl Future<Output = ()>,
) {
    let mut interval = tokio::time::interval(poll);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            _ = interval.tick() => {}
        }
        match tick(&pool, &sources).await {
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
