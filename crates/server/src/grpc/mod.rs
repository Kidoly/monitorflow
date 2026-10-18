//! gRPC ingress: the `AgentService::Stream` endpoint and the registry of
//! connected agents and relays.

mod policy;
mod registry;

pub use policy::{AllowAllRelayedAgents, DenyRelayedAgents, RelayPolicy};
pub use registry::{CloseReason, Counters, PeerInfo, Registration, Registry, RegistryError};
