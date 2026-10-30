//! First-run enrollment (ADR-0007), with the code shared with the relay in
//! [`monitorflow_common::client::enroll`].

use monitorflow_common::config::AgentConfig;

pub use monitorflow_common::client::enroll::*;

use crate::backoff::Backoff;
use crate::identity::AgentIdentity;
use crate::shutdown::Shutdown;

/// Loads the agent identity, enrolling first when there is none. The host
/// name goes with the request: an install key makes it the agent's ID
/// (ADR-0058).
pub async fn ensure_identity(
    config: &AgentConfig,
    backoff: Backoff,
    shutdown: Shutdown,
) -> Result<AgentIdentity, EnrollError> {
    let hostname = tokio::task::spawn_blocking(crate::os::hostname)
        .await
        .unwrap_or_default();
    let settings = EnrollSettings {
        hostname,
        ..EnrollSettings::agent(config)
    };
    monitorflow_common::client::enroll::ensure_identity(&settings, backoff, shutdown).await
}
