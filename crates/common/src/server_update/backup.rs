//! Backups of the compose deployment (ADR-0058): what the server and the
//! updater share about them. The updater takes them and reports them in
//! `status.json`; the server asks for one with [`BackupRequest`] and
//! serves the files from a read-only mount of the backups volume.

use chrono::{DateTime, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};

/// The server's request for a backup, in the inbox next to `request.json`.
pub const BACKUP_REQUEST_FILE: &str = "backup.json";
/// How many backups are kept, of every kind together.
pub const KEEP: usize = 7;

const PREFIX: &str = "monitorflow-backup-";
const SUFFIX: &str = ".tar.gz";
const STAMP: &str = "%Y%m%dT%H%M%SZ";

/// Why a backup was taken; part of its file name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackupKind {
    /// The automatic one, once a day.
    Daily,
    /// "Back up now".
    Manual,
    /// Before a server update.
    Update,
}

impl BackupKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Daily => "daily",
            Self::Manual => "manual",
            Self::Update => "update",
        }
    }
}

/// `monitorflow-backup-20261029T030000Z-daily.tar.gz`.
pub fn file_name(at: DateTime<Utc>, kind: BackupKind) -> String {
    format!("{PREFIX}{}-{}{SUFFIX}", at.format(STAMP), kind.as_str())
}

/// The time and kind in a name [`file_name`] made; `None` for any other
/// name, so a name from a request never reaches a path unchecked.
pub fn parse_file_name(name: &str) -> Option<(DateTime<Utc>, BackupKind)> {
    let rest = name.strip_prefix(PREFIX)?.strip_suffix(SUFFIX)?;
    let (stamp, kind) = rest.split_once('-')?;
    let kind = [BackupKind::Daily, BackupKind::Manual, BackupKind::Update]
        .into_iter()
        .find(|k| k.as_str() == kind)?;
    let at = NaiveDateTime::parse_from_str(stamp, STAMP).ok()?.and_utc();
    (file_name(at, kind) == name).then_some((at, kind))
}

/// `backup.json`: "Back up now". Nothing in it decides what is saved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupRequest {
    /// 1 to 64 of a-z, A-Z, 0-9 and `-`.
    pub id: String,
    pub requested_by: String,
    pub requested_at: DateTime<Utc>,
}

impl BackupRequest {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let request: Self = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        let id_ok = (1..=64).contains(&request.id.len())
            && request
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-');
        if !id_ok {
            return Err("invalid request id".into());
        }
        if request.requested_by.len() > 128 || request.requested_by.chars().any(char::is_control) {
            return Err("invalid requested_by".into());
        }
        Ok(request)
    }
}

/// The backups part of `status.json`. Missing in the status of an updater
/// older than backups.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Backups {
    /// The last backup, running or finished, of any kind.
    pub last: Option<BackupRun>,
    /// When the last one that succeeded finished.
    pub last_success_at: Option<DateTime<Utc>>,
    /// When the next automatic one is due.
    pub next_at: Option<DateTime<Utc>>,
    /// The files kept, newest first.
    pub files: Vec<BackupFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupRun {
    pub id: String,
    pub kind: BackupKind,
    /// Empty for automatic ones.
    pub requested_by: String,
    pub state: BackupState,
    pub error: Option<String>,
    /// What a backup that succeeded left out or noticed.
    #[serde(default)]
    pub warning: Option<String>,
    /// The file it wrote.
    pub file: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackupState {
    Running,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupFile {
    pub name: String,
    pub size: u64,
    pub created_at: DateTime<Utc>,
    pub kind: BackupKind,
}

/// `manifest.json`, the first file of every backup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// [`FORMAT`].
    pub format: u32,
    pub created_at: DateTime<Utc>,
    pub kind: BackupKind,
    /// The server that wrote the database, when the updater knew it: a
    /// restore refuses a server older than that.
    pub server_version: Option<String>,
    /// The latest migration applied.
    pub migration: i64,
    /// Whether `serverdata/` is in the archive: not when the updater's
    /// compose file predates backups and does not mount it.
    #[serde(default = "yes")]
    pub serverdata: bool,
}

fn yes() -> bool {
    true
}

/// The layout of a backup: `manifest.json`, `database.dump` (`pg_dump
/// -Fc`) and `serverdata/`.
pub const FORMAT: u32 = 1;
pub const MANIFEST: &str = "manifest.json";
pub const DATABASE: &str = "database.dump";
pub const SERVERDATA: &str = "serverdata";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip_and_nothing_else_parses() {
        let at = "2026-10-29T03:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let name = file_name(at, BackupKind::Update);
        assert_eq!(name, "monitorflow-backup-20261029T030000Z-update.tar.gz");
        assert_eq!(parse_file_name(&name), Some((at, BackupKind::Update)));
        for bad in [
            "monitorflow-backup-20261029T030000Z-weekly.tar.gz",
            "monitorflow-backup-20261329T030000Z-daily.tar.gz",
            "monitorflow-backup-20261029T030000Z-daily.tar.gz/../x",
            "../monitorflow-backup-20261029T030000Z-daily.tar.gz",
            "monitorflow-0.1.0-to-0.2.0-20261029T030000Z.dump",
            "monitorflow-backup-2026-10-29T030000Z-daily.tar.gz",
        ] {
            assert_eq!(parse_file_name(bad), None, "{bad}");
        }
    }

    #[test]
    fn requests_are_checked() {
        let ok =
            br#"{"id": "a-1", "requested_by": "admin", "requested_at": "2026-10-29T00:00:00Z"}"#;
        assert_eq!(BackupRequest::parse(ok).unwrap().id, "a-1");
        let extra = br#"{"id": "a", "requested_by": "x", "requested_at": "2026-10-29T00:00:00Z", "path": "/etc"}"#;
        assert!(BackupRequest::parse(extra).is_err());
        let bad_id =
            br#"{"id": "../a", "requested_by": "x", "requested_at": "2026-10-29T00:00:00Z"}"#;
        assert!(BackupRequest::parse(bad_id).is_err());
    }
}
