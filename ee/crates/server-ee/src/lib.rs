//! MonitorFlow Enterprise edition (ADR-0100), under the MonitorFlow
//! Enterprise License (`ee/LICENSE`), not the AGPL.
//!
//! The server reaches it through `crates/server/src/edition.rs` when built
//! with the `enterprise` feature.

pub mod admission;
pub mod license;
pub mod store;

pub use admission::{Added, Admission, Refusal};
pub use license::{License, LicenseError, Verifier};
pub use store::{Installed, State, Status};
