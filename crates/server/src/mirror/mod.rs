//! Package mirrors (architecture §7, ADR-0030): full apt and dnf
//! repositories copied from upstream with `aptly` and `reposync` +
//! `createrepo_c`, frozen into signed snapshots that agents and update
//! campaigns point at.
//!
//! Mirrors are global, not per customer. [`model`] holds their rows and
//! naming rules.

pub mod model;

pub use model::{
    Family, Mirror, MirrorSpec, Run, RunKind, RunState, Snapshot, SnapshotState, Trigger,
};
