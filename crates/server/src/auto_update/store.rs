//! Storage of automatic updates (ADR-0055): the server-wide settings, the
//! rollouts of each version and the servers they apply to.

use chrono::{DateTime, Utc};
use monitorflow_common::command::SELF_UPDATE;
use monitorflow_common::release::Version;
use serde::Serialize;
use serde_json::json;
use sqlx::{PgConnection, PgExecutor, PgPool};

use super::policy::Settings;
use super::{Peer, ACTOR};
use crate::api::audit::{self, Event};

/// The stored settings: the `auto_update_*` columns of `server_settings`
/// (ADR-0054), each `NULL` for its default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, sqlx::FromRow)]
pub struct Stored {
    #[sqlx(rename = "auto_update_enabled")]
    pub enabled: Option<bool>,
    #[sqlx(rename = "auto_update_paused")]
    pub paused: Option<bool>,
    #[sqlx(rename = "auto_update_failure_threshold")]
    pub failure_threshold: Option<i32>,
}

impl Stored {
    pub async fn load(db: impl PgExecutor<'_>) -> Result<Self, sqlx::Error> {
        Ok(sqlx::query_as(
            "SELECT auto_update_enabled, auto_update_paused, auto_update_failure_threshold
             FROM server_settings",
        )
        .fetch_optional(db)
        .await?
        .unwrap_or_default())
    }

    /// The settings as they apply, with `enabled` from the configuration
    /// when it sets one.
    pub fn effective(&self, enabled_override: Option<bool>) -> Settings {
        let defaults = Settings::default();
        Settings {
            enabled: enabled_override
                .or(self.enabled)
                .unwrap_or(defaults.enabled),
            enabled_locked: enabled_override.is_some(),
            paused: self.paused.unwrap_or(defaults.paused),
            failure_threshold: self
                .failure_threshold
                .map_or(defaults.failure_threshold, |t| t.max(1) as u32),
        }
    }
}

/// The settings as they apply.
pub async fn settings(
    pool: &PgPool,
    enabled_override: Option<bool>,
) -> Result<Settings, sqlx::Error> {
    Ok(Stored::load(pool).await?.effective(enabled_override))
}

/// Forgets automatic updates whose command expired before any agent ran
/// it, so they are tried again.
pub async fn release_expired(pool: &PgPool) -> Result<u64, sqlx::Error> {
    sqlx::query(
        "DELETE FROM agent_auto_updates a USING commands c
         WHERE c.id = a.command_id AND c.state = 'expired'",
    )
    .execute(pool)
    .await
    .map(|r| r.rows_affected())
}

/// Every unrevoked server, sorted by host name, with its automatic update
/// to `version`. Failures count from `counted_from`, else all of them.
pub(crate) async fn peers(
    pool: &PgPool,
    version: &Version,
    counted_from: Option<DateTime<Utc>>,
) -> Result<Vec<Peer>, sqlx::Error> {
    sqlx::query_as(
        "SELECT s.id, s.peer_id, s.site_id, t.agent_auto_update AS site_override,
                s.agent_version,
                CASE WHEN a.server_id IS NOT NULL THEN coalesce(c.state, 'unknown') END AS tried,
                a.queued_at >= coalesce($2, '-infinity'::timestamptz) AS counted,
                EXISTS (SELECT 1 FROM commands b
                        WHERE b.server_id = s.id AND b.kind = $3
                          AND b.state IN ('queued', 'sent', 'running')) AS busy
         FROM servers s
         JOIN sites t ON t.id = s.site_id
         LEFT JOIN agent_auto_updates a ON a.server_id = s.id AND a.version = $1
         LEFT JOIN commands c ON c.id = a.command_id
         WHERE s.revoked_at IS NULL
         ORDER BY s.hostname, s.id",
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_values_take_their_defaults_and_the_configuration_wins() {
        let defaults = Stored::default().effective(None);
        assert_eq!(defaults, Settings::default());
        assert!(defaults.enabled && !defaults.paused && !defaults.enabled_locked);
        let stored = Stored {
            enabled: Some(false),
            paused: Some(true),
            failure_threshold: Some(5),
        };
        let view = stored.effective(None);
        assert!(!view.enabled && view.paused);
        assert_eq!(view.failure_threshold, 5);
        let locked = stored.effective(Some(true));
        assert!(locked.enabled && locked.enabled_locked);
    }
}
