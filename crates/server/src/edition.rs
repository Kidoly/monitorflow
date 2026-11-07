//! Where the core meets the Enterprise edition (ADR-0100), whose code is
//! in `ee/` under its own license. Built with the `enterprise` feature,
//! on by default; without it the server is AGPL code only and refuses
//! what needs the Enterprise edition in plain words.

#[cfg(feature = "enterprise")]
pub use monitorflow_server_ee::{license, store, License, Status, Verifier};

/// Whether this build includes the Enterprise edition.
pub const ENTERPRISE: bool = cfg!(feature = "enterprise");

/// Said when something needs the Enterprise edition and this build has none.
pub const NOT_IN_THIS_BUILD: &str =
    "this build of MonitorFlow does not include the Enterprise edition";
