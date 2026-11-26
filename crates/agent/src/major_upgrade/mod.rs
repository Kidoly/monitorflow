//! Major distribution upgrades (ADR-0182): Debian through apt, Ubuntu
//! through `do-release-upgrade`, the RHEL family through leapp.
//!
//! - [`sources`] points apt's sources at the next release, both formats;
//! - [`repos`] reads the RHEL family's repositories before leapp;
//! - [`checks`] are the checks before, shared by the report and the helper;
//! - [`state`] is the state file root writes and everyone reads;
//! - [`helper`] is the root side: `major-upgrade` and its unit's `run`;
//! - [`handler`] is the agent's side: the command, the report, the watch.

pub mod checks;
pub mod handler;
pub mod helper;
pub mod repos;
pub mod sources;
pub mod state;
#[cfg(test)]
mod tests;

pub use handler::MajorUpgradeHandler;
