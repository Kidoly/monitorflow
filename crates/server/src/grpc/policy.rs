//! Which agents a relay may speak for (ADR-0004).

use sqlx::PgPool;
use tonic::async_trait;

use crate::db;

/// Decides whether a relay may forward envelopes on behalf of an agent.
/// The server runs [`DbRelayPolicy`].
#[async_trait]
pub trait RelayPolicy: Send + Sync + 'static {
    async fn may_speak_for(&self, relay_id: &str, agent_id: &str) -> bool;
}

/// The production policy: the agent is assigned to the relay's site and
/// neither is revoked ([`db::relay_may_speak_for`]). A database error
/// denies.
#[derive(Debug, Clone)]
pub struct DbRelayPolicy {
    pool: PgPool,
}

impl DbRelayPolicy {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl RelayPolicy for DbRelayPolicy {
    async fn may_speak_for(&self, relay_id: &str, agent_id: &str) -> bool {
        db::relay_may_speak_for(&self.pool, relay_id, agent_id)
            .await
            .unwrap_or_else(|error| {
                tracing::warn!(relay_id, agent_id, %error, "relay policy lookup failed; denying");
                false
            })
    }
}

/// Lets relays speak only for themselves.
#[derive(Debug, Clone, Copy, Default)]
pub struct DenyRelayedAgents;

#[async_trait]
impl RelayPolicy for DenyRelayedAgents {
    async fn may_speak_for(&self, _relay_id: &str, _agent_id: &str) -> bool {
        false
    }
}

/// Lets any relay speak for any agent. **Tests only**: in production this
/// would let a compromised relay impersonate every agent.
#[derive(Debug, Clone, Copy, Default)]
pub struct AllowAllRelayedAgents;

#[async_trait]
impl RelayPolicy for AllowAllRelayedAgents {
    async fn may_speak_for(&self, _relay_id: &str, _agent_id: &str) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn policies_answer_as_named() {
        assert!(!DenyRelayedAgents.may_speak_for("relay-1", "agent-1").await);
        assert!(
            AllowAllRelayedAgents
                .may_speak_for("relay-1", "agent-1")
                .await
        );
    }
}
