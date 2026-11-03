//! Local snapshots of the system volumes on LVM or ZFS (ADR-0094), taken
//! before a change on hosts without a Proxmox VM mapping.
//!
//! The agent's user cannot read LVM, so everything goes through the root
//! helper in [`helper`], reached with fixed `sudo -n` lines: `inspect` for
//! the storage part of each package report, `take` and `delete` for the
//! commands, in [`handler`]. Linux only; Windows reports no storage.

pub mod handler;
pub mod helper;
pub mod parse;
#[cfg(test)]
mod tests;

pub use handler::{storage, SnapshotAction, SnapshotHandler};
