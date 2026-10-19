//! The agent's identity on disk, with the code shared with the relay in
//! [`monitorflow_common::client::identity`].

use monitorflow_common::config::TlsPaths;
use monitorflow_common::tls::PeerRole;

pub use monitorflow_common::client::identity::*;

/// An identity whose certificate names an agent.
pub type AgentIdentity = StoredIdentity;

/// Loads the agent identity; see [`monitorflow_common::client::identity::load`].
pub fn load(paths: &TlsPaths) -> Result<Option<AgentIdentity>, IdentityError> {
    monitorflow_common::client::identity::load(paths, PeerRole::Agent)
}
