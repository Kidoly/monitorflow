//! Storage of automatic updates (ADR-0055): the server-wide settings, the
//! rollouts of each version and the servers they apply to.

use chrono::{DateTime, Utc};
use monitorflow_common::command::SELF_UPDATE;
use monitorflow_common::release::Version;
use serde::Serialize;
use serde_json::json;
use sqlx::{PgConnection, PgPool};

use super::policy::Settings;
use super::{Peer, ACTOR};
use crate::api::audit::{self, Event};

/// The settings as they apply, with `enabled` from the configuration
/// when it sets one, and when they last changed.
pub async fn settings(
    pool: &PgPool,
    enabled_override: Option<bool>,
) -> Result<(Settings, DateTime<Utc>), sqlx::Error> {
    let (enabled, paused, threshold, updated_at): (bool, bool, i32, DateTime<Utc>) =
        sqlx::query_as(
            "SELECT enabled, paused, failure_threshold, updated_at FROM auto_update_settings",
        )
        .fetch_one(pool)
        .await?;
    let settings = Settings {
        enabled: enabled_override.unwrap_or(enabled),
        enabled_locked: enabled_override.is_some(),
        paused,
        failure_threshold: threshold.max(1) as u32,
    };
    Ok((settings, updated_at))
}

/// Forgets automatic updates whose command expired before any peer ran
/// it, so they are tried again.
pub async fn release_expired(pool: &PgPool) -> Result<u64, sqlx::Error> {
    let agents = sqlx::query(
        "DELETE FROM agent_auto_updates a USING commands c
         WHERE c.id = a.command_id AND c.state = 'expired'",
    )
    .execute(pool)
    .await?
    .rows_affected();
    let relays = sqlx::query(
        "DELETE FROM relay_auto_updates a USING relay_commands c
         WHERE c.id = a.command_id AND c.state = 'expired'",
    )
    .execute(pool)
    .await?
    .rows_affected();
    Ok(agents + relays)
}

/// Every unrevoked agent or relay, sorted by name, with its automatic
/// update to `version`. Failures count from `counted_from`, else all of
/// them.
pub(crate) async fn peers(
    pool: &PgPool,
    kind: Kind,
    version: &Version,
    counted_from: Option<DateTime<Utc>>,
) -> Result<Vec<Peer>, sqlx::Error> {
    let sql = match kind {
        Kind::Agent => {
            "SELECT s.id, s.peer_id, s.site_id, t.agent_auto_update AS site_override,
                    s.agent_version AS version,
                    CASE WHEN a.server_id IS NOT NULL THEN coalesce(c.state, 'unknown') END AS tried,
                    a.queued_at >= coalesce($2, '-infinity'::timestamptz) AS counted,
                    EXISTS (SELECT 1 FROM commands b
                            WHERE b.server_id = s.id AND b.kind = $3
                              AND b.state IN ('queued', 'sent', 'running')) AS busy,
                    r.relay_version,
                    EXISTS (SELECT 1 FROM relay_commands rc
                            WHERE rc.relay_id = s.relay_id
                              AND rc.state IN ('queued', 'sent', 'running')) AS relay_busy
             FROM servers s
             JOIN sites t ON t.id = s.site_id
             LEFT JOIN relays r ON r.id = s.relay_id
             LEFT JOIN agent_auto_updates a ON a.server_id = s.id AND a.version = $1
             LEFT JOIN commands c ON c.id = a.command_id
             WHERE s.revoked_at IS NULL
             ORDER BY s.hostname, s.id"
        }
        Kind::Relay => {
            "SELECT r.id, r.peer_id, r.site_id, t.agent_auto_update AS site_override,
                    r.relay_version AS version,
                    CASE WHEN a.relay_id IS NOT NULL THEN coalesce(c.state, 'unknown') END AS tried,
                    a.queued_at >= coalesce($2, '-infinity'::timestamptz) AS counted,
                    EXISTS (SELECT 1 FROM relay_commands b
                            WHERE b.relay_id = r.id AND b.state IN ('queued', 'sent', 'running'))
                    OR EXISTS (SELECT 1 FROM commands b JOIN servers s ON s.id = b.server_id
                               WHERE s.relay_id = r.id AND b.kind = $3
                                 AND b.state IN ('queued', 'sent', 'running')) AS busy,
                    NULL::text AS relay_version,
                    false AS relay_busy
             FROM relays r
             JOIN sites t ON t.id = r.site_id
             LEFT JOIN relay_auto_updates a ON a.relay_id = r.id AND a.version = $1
             LEFT JOIN relay_commands c ON c.id = a.command_id
             WHERE r.revoked_at IS NULL
             ORDER BY r.peer_id, r.id"
        }
    };
    sqlx::query_as(sql)
        .bind(version.to_string())
        .bind(counted_from)
        .bind(SELF_UPDATE)
        .fetch_all(pool)
        .await
}

/// What a rollout updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "text", rename_all = "lowercase")]
pub enum Kind {
    Agent,
    Relay,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Relay => "relay",
        }
    }
}

impl std::str::FromStr for Kind {
    type Err = ();

    fn from_str(text: &str) -> Result<Self, ()> {
        match text {
            "agent" => Ok(Self::Agent),
            "relay" => Ok(Self::Relay),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "text", rename_all = "lowercase")]
pub enum RolloutState {
    Rolling,
    Halted,
}

/// The automatic rollout of one version to one kind of peer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, sqlx::FromRow)]
pub struct Rollout {
    pub kind: Kind,
    pub version: String,
    pub state: RolloutState,
    pub started_at: DateTime<Utc>,
    pub halted_at: Option<DateTime<Utc>>,
    pub reason: Option<String>,
    pub counted_from: DateTime<Utc>,
}

const COLUMNS: &str = "kind, version, state, started_at, halted_at, reason, counted_from";

impl Rollout {
    pub async fn load(
        pool: &PgPool,
        kind: Kind,
        version: &Version,
    ) -> Result<Option<Self>, sqlx::Error> {
        sqlx::query_as(&format!(
            "SELECT {COLUMNS} FROM auto_rollouts WHERE kind = $1 AND version = $2"
        ))
        .bind(kind)
        .bind(version.to_string())
        .fetch_optional(pool)
        .await
    }

    /// The newest rollouts first.
    pub async fn recent(pool: &PgPool, kind: Kind, limit: i64) -> Result<Vec<Self>, sqlx::Error> {
        sqlx::query_as(&format!(
            "SELECT {COLUMNS} FROM auto_rollouts WHERE kind = $1
             ORDER BY started_at DESC, version DESC LIMIT $2"
        ))
        .bind(kind)
        .bind(limit)
        .fetch_all(pool)
        .await
    }

    /// Records the start of the rollout of `version`, audited, unless it
    /// started already.
    pub async fn start(
        tx: &mut PgConnection,
        kind: Kind,
        version: &Version,
    ) -> Result<(), sqlx::Error> {
        let started = sqlx::query(
            "INSERT INTO auto_rollouts (kind, version) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        )
        .bind(kind)
        .bind(version.to_string())
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if started == 1 {
            tracing::info!(kind = kind.as_str(), %version, "automatic rollout started");
            audit::record(
                &mut *tx,
                Event {
                    actor_kind: ACTOR.0,
                    actor_id: ACTOR.1,
                    action: "auto_update.rollout_started",
                    target: Some(("release", &version.to_string())),
                    details: json!({ "kind": kind.as_str(), "version": version.to_string() }),
                },
            )
            .await?;
        }
        Ok(())
    }

    /// Halts the rollout of `version`, audited; `false` when it was
    /// halted already.
    pub async fn halt(
        pool: &PgPool,
        kind: Kind,
        version: &Version,
        reason: &str,
    ) -> Result<bool, sqlx::Error> {
        let mut tx = pool.begin().await?;
        let halted = sqlx::query(
            "INSERT INTO auto_rollouts (kind, version, state, halted_at, reason)
             VALUES ($1, $2, 'halted', now(), $3)
             ON CONFLICT (kind, version) DO UPDATE
                 SET state = 'halted', halted_at = now(), reason = EXCLUDED.reason
                 WHERE auto_rollouts.state <> 'halted'",
        )
        .bind(kind)
        .bind(version.to_string())
        .bind(reason)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if halted == 1 {
            audit::record(
                &mut *tx,
                Event {
                    actor_kind: ACTOR.0,
                    actor_id: ACTOR.1,
                    action: "auto_update.halted",
                    target: Some(("release", &version.to_string())),
                    details: json!({
                        "kind": kind.as_str(),
                        "version": version.to_string(),
                        "reason": reason,
                    }),
                },
            )
            .await?;
        }
        tx.commit().await?;
        Ok(halted == 1)
    }

    /// Resumes a halted rollout: failures count again from now, and sites
    /// whose canary failed get another one. `None` when it is not halted.
    pub async fn resume(
        tx: &mut PgConnection,
        kind: Kind,
        version: &str,
    ) -> Result<Option<Self>, sqlx::Error> {
        sqlx::query_as(&format!(
            "UPDATE auto_rollouts
             SET state = 'rolling', halted_at = NULL, reason = NULL, counted_from = now()
             WHERE kind = $1 AND version = $2 AND state = 'halted'
             RETURNING {COLUMNS}"
        ))
        .bind(kind)
        .bind(version)
        .fetch_optional(&mut *tx)
        .await
    }
}
