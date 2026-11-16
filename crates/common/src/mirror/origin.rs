//! Where an on-demand snapshot's packages come from (ADR-0142): `.origin`
//! in the snapshot's directory, hidden from the listener, written by the
//! server's sync. The listener reads it to fetch a package it does not
//! hold yet.

use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// File name in a snapshot's directory.
pub const ORIGIN: &str = ".origin";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    /// The upstream repository: a package's path in the package list is
    /// relative to it.
    pub upstream: String,
    /// The distribution's historical archives to try, in order, when the
    /// upstream no longer has a package; the same relative path below
    /// each. Empty when there is none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub archives: Vec<String>,
}

impl Origin {
    /// Writes the file atomically. Blocking.
    #[cfg(unix)]
    pub fn write(&self, path: &Path) -> io::Result<()> {
        let json = serde_json::to_vec(self).map_err(io::Error::other)?;
        super::write_atomically(path, &json)
    }

    /// The file at `path`; `None` when there is none. Blocking.
    pub fn read(path: &Path) -> io::Result<Option<Self>> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_origin_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(ORIGIN);
        assert_eq!(Origin::read(&path).unwrap(), None);
        let origin = Origin {
            upstream: "https://deb.debian.org/debian".into(),
            archives: vec![],
        };
        origin.write(&path).unwrap();
        assert_eq!(Origin::read(&path).unwrap(), Some(origin));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            r#"{"upstream":"https://deb.debian.org/debian"}"#
        );
        std::fs::write(&path, "{").unwrap();
        assert!(Origin::read(&path).is_err());
    }
}
