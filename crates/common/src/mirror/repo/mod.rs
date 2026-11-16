//! Reading apt and dnf repositories (ADR-0034), shared by the relay's
//! copies and the server's on-demand mirrors (ADR-0142).
//!
//! [`meta`] describes the files of a snapshot, read by [`apt`] and [`dnf`]
//! from the repository metadata, decompressed by [`compress`]; [`fetch`]
//! downloads them over HTTP, checking each one on the way to disk.

pub mod apt;
pub mod compress;
pub mod dnf;
pub mod fetch;
pub mod meta;
