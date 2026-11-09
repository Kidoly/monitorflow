//! Plugins (ADR-0103): checks the agent runs and notification channels the
//! central server runs, written by others and loaded without changing
//! MonitorFlow.
//!
//! A plugin is one WebAssembly module with a [`manifest`], signed into a
//! [`package`] by a key the administrator trusts ([`keys`]). The [`runtime`]
//! runs it in wasmi, an interpreter, with no WASI: the module sees only its
//! input and the six `mf` imports of ABI version 1, and reaches the network
//! only through [`http`] requests to the hosts its manifest lists. Every
//! limit is in [`limits`] and none can be raised by a manifest.

pub mod http;
pub mod keys;
pub mod manifest;
pub mod package;
pub mod runtime;

pub use manifest::{Kind, Manifest, ManifestError, Setting, SettingType};
pub use package::{Package, PackageError, Verified};
pub use runtime::{Plugin, RunError, Runner};

/// The limits of ADR-0103, the same for every plugin.
pub mod limits {
    use std::time::Duration;

    /// Largest module.
    pub const MODULE_BYTES: usize = 2 * 1024 * 1024;
    /// Largest package file: the module in base64 and the rest.
    pub const PACKAGE_BYTES: usize = 3 * 1024 * 1024;
    /// Largest manifest.
    pub const MANIFEST_BYTES: usize = 16 * 1024;
    /// Linear memory of one instance.
    pub const MEMORY_BYTES: usize = 32 * 1024 * 1024;
    /// Table entries of one instance.
    pub const TABLE_ELEMENTS: usize = 10_000;
    /// wasmi fuel for one run, about a second of work.
    pub const FUEL: u64 = 2_000_000_000;
    /// HTTP requests in one run.
    pub const HTTP_REQUESTS: usize = 8;
    /// Body of one request.
    pub const HTTP_REQUEST_BYTES: usize = 64 * 1024;
    /// Body of one response; a longer one is an error.
    pub const HTTP_RESPONSE_BYTES: usize = 1024 * 1024;
    /// One request, connection included.
    pub const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
    /// The input JSON a plugin is given.
    pub const INPUT_BYTES: usize = 256 * 1024;
    /// The result JSON a plugin returns.
    pub const OUTPUT_BYTES: usize = 64 * 1024;
    /// Log lines of one run, together.
    pub const LOG_BYTES: usize = 4 * 1024;
    /// A channel's run; a check's is its timeout.
    pub const CHANNEL_TIMEOUT: Duration = Duration::from_secs(15);
    /// Runs at once in one process.
    pub const CONCURRENT_RUNS: usize = 4;
}
