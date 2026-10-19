//! Points the host's package manager at its MonitorFlow mirror and undoes
//! it (ADR-0032, architecture §7).

// Used by `apply` and `revert`, which the next change of ADR-0032 adds.
#[allow(dead_code)]
mod backup;
#[allow(dead_code)]
mod key;

use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Family {
    Apt,
    Dnf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Replace,
    Add,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid sources spec: {0}")]
    Invalid(String),
    #[error("signing key `{name}` refused: {reason}")]
    Key { name: String, reason: String },
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{}: {reason}", path.display())]
    Unsupported { path: PathBuf, reason: String },
    #[error("backup {}: {reason}", path.display())]
    Backup { path: PathBuf, reason: String },
    #[error("post-check failed, sources reverted: {0}")]
    PostCheck(String),
    #[error("{cause}; reverting failed too: {rollback}")]
    RollbackFailed {
        cause: Box<Error>,
        rollback: Box<Error>,
    },
}
