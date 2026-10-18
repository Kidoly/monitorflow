//! Which agents a relay may speak for (ADR-0004).

use tonic::async_trait;

/// Decides whether a relay may forward envelopes on behalf of an agent.
///
/// The real check, "the agent is assigned to the relay's site", needs the
/// database and comes with it; until then the server runs [`DenyRelayedAgents`].
#[async_trait]
pub trait RelayPolicy: Send + Sync + 'static {
    async fn may_speak_for(&self, relay_id: &str, agent_id: &str) -> bool;
}

/// Lets relays speak only for themselves. The production default until the
/// site assignment exists in the database.
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
