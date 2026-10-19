//! MonitorFlow relay library (ADR-0025). `main.rs` only wires these
//! modules together, so everything here is testable.
//!
//! Start-up order: load the relay identity or enroll with the configured
//! token, fetch the CRL from the upstream, open the relay's own stream,
//! then accept agents and forward each of their streams upstream until
//! SIGINT or SIGTERM, renewing the certificate before it expires. Agents
//! renew their certificates through the relay (ADR-0028).

pub mod control;
pub mod forward;
pub mod listener;
pub mod renew;
