//! MonitorFlow agent library. `main.rs` only parses the command line and
//! wires these modules together, so everything here is testable.
//!
//! Start-up order: detect the OS (any Linux distribution starts; only the
//! package features depend on it), load the identity or enroll with the
//! configured token, then keep the stream to the upstream open until SIGINT or SIGTERM,
//! renewing the certificate before it expires and running the sensors.

pub mod ai_commands;
pub mod commands;
#[cfg(windows)]
pub mod data_folder;
#[cfg(unix)]
pub mod discovery;
pub mod enroll;
#[cfg(windows)]
pub mod eventlog;
pub mod hardware;
pub mod helper_trust;
pub mod identity;
pub mod init_config;
#[cfg(unix)]
pub mod journald;
pub mod os;
pub mod packages;
pub mod plugins;
pub mod remote;
pub mod remote_access;
pub mod renew;
pub mod self_update;
pub mod sensor_config;
pub mod sensors;
#[cfg(unix)]
pub mod snapshots;
pub mod stream;
pub mod upload;

pub use monitorflow_common::client::{backoff, shutdown};

/// Now, in Unix milliseconds; 0 before 1970.
pub fn unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}
