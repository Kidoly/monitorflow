//! Which agents a relay may speak for (ADR-0004), directly or through
//! chained relays (ADR-0028).

use monitorflow_common::tls::{PeerIdentity, PeerRole};
use sqlx::PgPool;
use tonic::async_trait;

use crate::db;

/// Decides whether a relay may forward envelopes on behalf of an agent.
/// The server runs [`DbRelayPolicy`].
#[async_trait]
pub trait RelayPolicy: Send + Sync + 'static {
    async fn may_speak_for(&self, relay_id: &str, agent_id: &str) -> bool;

    /// Whether a call forwarded along `relays`, from the connecting relay
    /// down to the one `origin` is attached to, may speak for `origin`, an
    /// agent or a relay. Without an override only one relay and an agent
    /// are allowed, by [`Self::may_speak_for`].
    async fn may_route(&self, relays: &[&str], origin: &PeerIdentity) -> bool {
        match (relays, origin.role()) {
            ([relay], PeerRole::Agent) => self.may_speak_for(relay, origin.id()).await,
            _ => false,
        }
    }
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

    async fn may_route(&self, relays: &[&str], origin: &PeerIdentity) -> bool {
        db::relay_route_allowed(&self.pool, relays, origin)
            .await
            .unwrap_or_else(|error| {
                tracing::warn!(?relays, %origin, %error, "relay policy lookup failed; denying");
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

    async fn may_route(&self, _relays: &[&str], _origin: &PeerIdentity) -> bool {
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

    #[tokio::test]
    async fn routes_default_to_one_relay_and_an_agent() {
        struct SameName;
        #[async_trait]
        impl RelayPolicy for SameName {
            async fn may_speak_for(&self, relay_id: &str, agent_id: &str) -> bool {
                relay_id == agent_id
            }
        }
        let agent = PeerIdentity::new(PeerRole::Agent, "x").unwrap();
        let relay = PeerIdentity::new(PeerRole::Relay, "x").unwrap();
        assert!(SameName.may_route(&["x"], &agent).await);
        assert!(!SameName.may_route(&["y"], &agent).await);
        assert!(!SameName.may_route(&["x", "x"], &agent).await);
        assert!(!SameName.may_route(&["x"], &relay).await);
        assert!(!SameName.may_route(&[], &agent).await);
        assert!(AllowAllRelayedAgents.may_route(&["a", "b"], &relay).await);
        assert!(!DenyRelayedAgents.may_route(&["a"], &agent).await);
    }
}
