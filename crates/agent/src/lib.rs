//! MonitorFlow agent library. `main.rs` only parses the command line and
//! wires these modules together, so everything here is testable.
//!
//! Start-up order: detect the OS (refuse unsupported families before a
//! token is spent), load the identity or enroll with the configured token,
//! then keep the stream to the upstream open until SIGINT or SIGTERM,
//! renewing the certificate before it expires and running the sensors.

pub mod enroll;
pub mod identity;
pub mod os;
pub mod packages;
pub mod renew;
pub mod sensor_config;
pub mod sensors;
pub mod stream;
pub mod upload;

pub use monitorflow_common::client::{backoff, shutdown};
