//! The relay's copy of the upstream's package mirrors (ADR-0034), served
//! on a plain HTTP listener with the server's layout
//! ([`monitorflow_common::mirror::serve`]): `apt/`, `dnf/` and `keys/`
//! under `mirror_dir`.
//!
//! [`meta`] describes the files of a snapshot, read by [`apt`] and [`dnf`]
//! from the repository metadata, decompressed by [`compress`]; [`sync`] copies a snapshot with
//! [`fetch`]; [`task`] runs the sync loop and [`retention`] bounds the
//! snapshots kept. [`report`] tells the server what the relay serves.

pub mod apt;
pub mod compress;
pub mod dnf;
pub mod fetch;
pub mod meta;
pub mod report;
pub mod retention;
pub mod sync;
pub mod task;

use std::io;
use std::path::Path;

use monitorflow_common::mirror::{Family, Served};

/// Creates the served directories under `root`. Blocking.
pub fn prepare(root: &Path) -> io::Result<Served> {
    let served = Served::under(root);
    for family in Family::ALL {
        std::fs::create_dir_all(served.root(family))?;
    }
    if let Some(keys) = served.public_key.parent() {
        std::fs::create_dir_all(keys)?;
    }
    Ok(served)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepare_creates_the_layout() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("mirrors");
        let served = prepare(&root).unwrap();
        prepare(&root).unwrap();
        assert!(root.join("apt").is_dir());
        assert!(root.join("dnf").is_dir());
        assert_eq!(served.public_key, root.join("keys/monitorflow-archive.asc"));
        assert!(root.join("keys").is_dir());
    }
}
