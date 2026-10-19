//! First-run enrollment (ADR-0007), with the code shared with the relay in
//! [`monitorflow_common::client::enroll`].

use monitorflow_common::config::AgentConfig;

pub use monitorflow_common::client::enroll::*;

use crate::backoff::Backoff;
use crate::identity::AgentIdentity;
use crate::shutdown::Shutdown;

/// Loads the agent identity, enrolling first when there is none.
pub async fn ensure_identity(
    config: &AgentConfig,
    backoff: Backoff,
    shutdown: Shutdown,
) -> Result<AgentIdentity, EnrollError> {
    monitorflow_common::client::enroll::ensure_identity(
        &EnrollSettings::agent(config),
        backoff,
        shutdown,
    )
    .await
}
