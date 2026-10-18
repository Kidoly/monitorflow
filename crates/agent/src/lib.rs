//! MonitorFlow agent library. `main.rs` only parses the command line and
//! wires these modules together, so everything here is testable.
//!
//! Start-up order: detect the OS (refuse unsupported families before a
//! token is spent), load the identity or enroll with the configured token,
//! then keep the stream to the upstream open until SIGINT or SIGTERM,
//! renewing the certificate before it expires.

pub mod backoff;
pub mod enroll;
pub mod identity;
pub mod os;
pub mod renew;
pub mod shutdown;
pub mod stream;
