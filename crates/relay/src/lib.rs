//! MonitorFlow relay library (ADR-0025). `main.rs` only wires these
//! modules together, so everything here is testable.
//!
//! Start-up order: load the relay identity or enroll with the configured
//! token, fetch the CRL from the upstream, open the relay's own stream,
//! then accept agents and forward each of their streams upstream until
//! SIGINT or SIGTERM, renewing the certificate before it expires. Agents
//! renew their certificates through the relay (ADR-0028). Agents' batches
//! are kept on disk while the upstream is unreachable and sent in order
//! once it is back (ADR-0027). The relay serves a copy of the upstream's
//! package mirrors on a plain HTTP listener (ADR-0034). A relay enrolled
//! without names opens its own stream and waits for an administrator to
//! confirm the names it reported before anything else (ADR-0066).

pub mod buffer;
pub mod control;
pub mod forward;
pub mod listener;
pub mod mirror;
pub mod names;
pub mod renew;
pub mod self_update;
mod session;
