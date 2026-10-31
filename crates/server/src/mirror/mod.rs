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
//! [`scheduler`]. [`http`] serves the result; [`relays`] records the
//! copies relays serve (ADR-0037). [`tools`] reaches the tools in their
//! sidecar container and [`catalog`] lists the distributions an
//! administrator can add in one click (ADR-0067).

pub mod apt;
pub mod catalog;
pub mod dnf;
pub mod http;
pub mod jobs;
pub mod layout;
pub mod model;
pub mod relays;
pub mod runner;
pub mod scheduler;
pub mod signing;
pub mod store;
pub mod tools;

pub use jobs::{Job, JobError, JobKind, Mirrors};
pub use layout::Layout;
pub use model::{
    Family, Mirror, MirrorSpec, Run, RunKind, RunState, Snapshot, SnapshotState, Trigger,
};
pub use runner::{CommandRunner, Invocation, Output, RunError, Runner, Tool};
pub use store::{MirrorLock, StoreError};
pub use tools::ToolsRunner;
