//! Package mirrors (architecture §7, ADR-0030): full apt and dnf
//! repositories copied from upstream with `aptly` and `reposync` +
//! `createrepo_c`, frozen into signed snapshots that agents and update
//! campaigns point at.
//!
//! Mirrors are global, not per customer. [`model`] holds their rows and
//! naming rules, [`store`] keeps them in PostgreSQL with their snapshots
//! and runs, [`runner`] runs the tools; [`apt`], [`dnf`] and [`signing`]
//! build their command lines over the files of [`layout`], and [`jobs`]
//! syncs and snapshots mirrors with them, on request or from the
//! [`scheduler`]. [`http`] serves the result.

pub mod apt;
pub mod dnf;
pub mod http;
pub mod jobs;
pub mod layout;
pub mod model;
pub mod runner;
pub mod scheduler;
pub mod signing;
pub mod store;

pub use jobs::{Job, JobError, JobKind, Mirrors};
pub use layout::Layout;
pub use model::{
    Family, Mirror, MirrorSpec, Run, RunKind, RunState, Snapshot, SnapshotState, Trigger,
};
pub use runner::{CommandRunner, Invocation, Output, RunError, Runner, Tool};
pub use store::{MirrorLock, StoreError};
