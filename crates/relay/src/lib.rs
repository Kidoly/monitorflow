//! MonitorFlow relay library (ADR-0025). `main.rs` only wires these
//! modules together, so everything here is testable.
//!
//! [`listener`] accepts agents over mTLS with the CRL of the upstream, and
//! [`forward`] pipes each agent stream to a stream of its own upstream.

pub mod forward;
pub mod listener;
