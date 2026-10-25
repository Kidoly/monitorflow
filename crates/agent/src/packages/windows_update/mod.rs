//! Windows Update (ADR-0049). The agent's account cannot install updates,
//! so the `update-service` of this binary, the Windows service
//! `MonitorFlowUpdate` running as LocalSystem, makes every Windows Update
//! Agent call. The agent sends it typed requests on a named pipe that only
//! SYSTEM and the agent's service SID may open.
//!
//! This module is the part both sides share and that builds everywhere,
//! so Linux tests cover it: the pipe protocol and its framing, request
//! checks, which pending updates a request selects, how updates become
//! [`Package`]s, the pipe's DACL and the service SID it names. [`client`]
//! holds the agent's side; the COM calls and the pipe itself are in
//! `wua` and `pipe`, Windows only.

use monitorflow_common::command::valid_windows_update_name;
use monitorflow_common::proto::v1::{ApplyUpdates, Package};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// The pipe the update service listens on.
pub const PIPE_NAME: &str = r"\\.\pipe\MonitorFlowUpdate";
/// Name of the update service.
pub const SERVICE_NAME: &str = "MonitorFlowUpdate";
/// Name of the agent's service, whose SID may open the pipe.
pub const AGENT_SERVICE: &str = "MonitorFlowAgent";
/// Longest request read from the pipe.
pub const MAX_REQUEST_BYTES: usize = 64 * 1024;
/// Longest reply read from the pipe.
pub const MAX_REPLY_BYTES: usize = 16 * 1024 * 1024;
/// Most update names in one request.
pub const MAX_NAMES: usize = 1024;

/// Classification GUIDs of Windows Update, lower case, with their English
/// names: the names Windows gives are localized.
const CLASSIFICATIONS: [(&str, &str); 10] = [
    ("e6cf1350-c01b-414d-a61f-263d14d133b4", "Critical Updates"),
    ("e0789628-ce08-4437-be74-2495b842f43b", "Definition Updates"),
    ("ebfc1fc5-71a4-4f7b-9aca-3b9a503104a0", "Drivers"),
    ("b54e7d24-7add-428f-8b75-90a396fa584f", "Feature Packs"),
    (SECURITY_UPDATES, "Security Updates"),
    ("68c5b0a3-d1a6-4553-ae49-01d3a7827828", "Service Packs"),
    ("b4832bd8-e735-4761-8daf-37f882276dab", "Tools"),
    ("28bc880e-0592-4cbf-8f95-c79b17911d5f", "Update Rollups"),
    ("cd5ffd1e-e932-4e3a-bf74-18bf0b1bbd83", "Updates"),
    (UPGRADES, "Upgrades"),
];
const SECURITY_UPDATES: &str = "0fa1201d-4330-4fa8-8ae9-b877473b6441";
/// Feature updates to a new Windows version: only installed when named.
const UPGRADES: &str = "3689bdc8-b205-4af4-8d4a-a63924c5e9d5";

/// What to install, as in ADR-0036's update spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Every pending update but feature upgrades.
    All,
    /// Pending security updates, of `updates` when not empty.
    Security,
    /// The pending updates named, at least one.
    Named,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Security => "security",
            Self::Named => "named",
        }
    }
}

/// A request on the pipe: `{"op": "install", "kind": "named", "updates":
/// ["KB5034441"]}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    /// Pending and installed updates, and whether a restart is pending.
    Scan,
    /// Whether a restart is pending.
    Status,
    Install {
        kind: Kind,
        /// KB numbers or update IDs.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        updates: Vec<String>,
    },
    /// Restart in one minute.
    Reboot,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RequestError {
    #[error("`{0}` installs take no update names")]
    UnexpectedNames(&'static str),
    #[error("`named` installs need at least one update name")]
    NoNames,
    #[error("more than {MAX_NAMES} update names")]
    TooManyNames,
    #[error("`{0}` is not a KB number or update ID")]
    InvalidName(String),
}

impl Request {
    /// The install an `apply_updates` command asks for; check it with
    /// [`Self::validate`].
    pub fn install(request: &ApplyUpdates) -> Self {
        let kind = if request.security_only {
            Kind::Security
        } else if request.packages.is_empty() {
            Kind::All
        } else {
            Kind::Named
        };
        Self::Install {
            kind,
            updates: request.packages.clone(),
        }
    }

    /// Names are KB numbers or update IDs, and only `named` and
    /// `security` take them.
    pub fn validate(&self) -> Result<(), RequestError> {
        let Self::Install { kind, updates } = self else {
            return Ok(());
        };
        match kind {
            Kind::All if !updates.is_empty() => {
                return Err(RequestError::UnexpectedNames(kind.as_str()))
            }
            Kind::Named if updates.is_empty() => return Err(RequestError::NoNames),
            _ => {}
        }
        if updates.len() > MAX_NAMES {
            return Err(RequestError::TooManyNames);
        }
        match updates.iter().find(|n| !valid_windows_update_name(n)) {
            Some(bad) => Err(RequestError::InvalidName(bad.escape_debug().to_string())),
            None => Ok(()),
        }
    }
}

/// An update as the service reports it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Update {
    /// `KB` and its first KB number, else its update ID.
    pub name: String,
    pub title: String,
    pub classification: String,
    pub severity: String,
    pub size_bytes: u64,
    pub revision: i64,
    pub installed: bool,
    /// Pending: installing it may need a restart. Installed: it waits for
    /// one.
    pub reboot_required: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scan {
    pub updates: Vec<Update>,
    pub reboot_required: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Status {
    pub reboot_required: bool,
}

/// What became of one update, from `OperationResultCode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    NotStarted,
    InProgress,
    Succeeded,
    SucceededWithErrors,
    Failed,
    Aborted,
}

impl Outcome {
    pub fn from_code(code: i32) -> Self {
        match code {
            1 => Self::InProgress,
            2 => Self::Succeeded,
            3 => Self::SucceededWithErrors,
            4 => Self::Failed,
            5 => Self::Aborted,
            _ => Self::NotStarted,
        }
    }

    pub fn succeeded(self) -> bool {
        matches!(self, Self::Succeeded | Self::SucceededWithErrors)
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::NotStarted => "not started",
            Self::InProgress => "in progress",
            Self::Succeeded => "succeeded",
            Self::SucceededWithErrors => "succeeded with errors",
            Self::Failed => "failed",
            Self::Aborted => "aborted",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallResult {
    pub name: String,
    pub title: String,
    pub result: Outcome,
    pub hresult: i32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Installed {
    pub results: Vec<InstallResult>,
    /// Names of the request that matched no pending update.
    pub not_pending: Vec<String>,
    pub reboot_required: bool,
}

/// A reply on the pipe: `{"updates": {…}}`, `{"error": "…"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Reply {
    Updates(Scan),
    Status(Status),
    Installed(Installed),
    Rebooting(()),
    Error(String),
}

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("a message is larger than {0} bytes")]
    TooLarge(usize),
    #[error("a message is not valid: {0}")]
    Json(String),
}

/// Writes `value` as one message: a 4-byte big-endian length, then JSON.
pub async fn write_frame<W, T>(writer: &mut W, value: &T) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let body = serde_json::to_vec(value).map_err(|e| FrameError::Json(e.to_string()))?;
    let len = u32::try_from(body.len()).map_err(|_| FrameError::TooLarge(u32::MAX as usize))?;
    writer.write_all(&len.to_be_bytes()).await?;
    writer.write_all(&body).await?;
    writer.flush().await?;
    Ok(())
}

/// Reads one message of at most `max` bytes.
pub async fn read_frame<R, T>(reader: &mut R, max: usize) -> Result<T, FrameError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut len = [0; 4];
    reader.read_exact(&mut len).await?;
    let len = u32::from_be_bytes(len) as usize;
    if len > max {
        return Err(FrameError::TooLarge(max));
    }
    let mut body = vec![0; len];
    reader.read_exact(&mut body).await?;
    serde_json::from_slice(&body).map_err(|e| FrameError::Json(e.to_string()))
}

/// An update as the service reads it from Windows Update, before it is
/// named or selected.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Found {
    pub title: String,
    /// KB article numbers, digits only.
    pub kb_ids: Vec<String>,
    pub update_id: String,
    pub revision: i64,
    /// The classification's GUID, any case; empty when none.
    pub classification_id: String,
    /// The classification's name, localized.
    pub classification_name: String,
    pub severity: String,
    pub size_bytes: u64,
    pub installed: bool,
    pub reboot_required: bool,
}

impl Found {
    pub fn name(&self) -> String {
        match self.kb_ids.first() {
            Some(kb) => format!("KB{kb}"),
            None => self.update_id.to_ascii_lowercase(),
        }
    }

    fn classification_is(&self, id: &str) -> bool {
        self.classification_id.eq_ignore_ascii_case(id)
    }

    /// The English name for a well-known classification, else Windows'.
    pub fn classification(&self) -> String {
        CLASSIFICATIONS
            .iter()
            .find(|(id, _)| self.classification_is(id))
            .map_or_else(|| self.classification_name.clone(), |(_, n)| n.to_string())
    }

    /// Whether `name`, a KB number or update ID, names this update.
    pub fn matches(&self, name: &str) -> bool {
        let kb = name
            .get(..2)
            .filter(|p| p.eq_ignore_ascii_case("kb"))
            .map(|_| &name[2..]);
        match kb {
            Some(number) => self.kb_ids.iter().any(|id| id == number),
            None => self.update_id.eq_ignore_ascii_case(name),
        }
    }

    pub fn to_update(&self) -> Update {
        Update {
            name: self.name(),
            title: self.title.clone(),
            classification: self.classification(),
            severity: self.severity.clone(),
            size_bytes: self.size_bytes,
            revision: self.revision,
            installed: self.installed,
            reboot_required: self.reboot_required,
        }
    }
}

/// Which of `found` an install of `kind` and `names` takes, by index, and
/// the names that match no pending update.
pub fn select(kind: Kind, names: &[String], found: &[Found]) -> (Vec<usize>, Vec<String>) {
    let named = |f: &Found| names.iter().any(|n| f.matches(n));
    let picked = found
        .iter()
        .enumerate()
        .filter(|(_, f)| !f.installed)
        .filter(|(_, f)| match kind {
            Kind::All => !f.classification_is(UPGRADES),
            Kind::Security => {
                f.classification_is(SECURITY_UPDATES) && (names.is_empty() || named(f))
            }
            Kind::Named => named(f),
        })
        .map(|(i, _)| i)
        .collect();
    let not_pending = names
        .iter()
        .filter(|n| !found.iter().any(|f| !f.installed && f.matches(n)))
        .cloned()
        .collect();
    (picked, not_pending)
}

/// The report's packages for `updates`, one per name: updates sharing a KB
/// number are one entry, pending when any of them is, with their sizes
/// added up (ADR-0049).
pub fn to_packages(updates: &[Update]) -> Vec<Package> {
    let mut names: Vec<&str> = Vec::new();
    for update in updates {
        if !names.contains(&update.name.as_str()) {
            names.push(&update.name);
        }
    }
    names
        .into_iter()
        .map(|name| {
            let same: Vec<&Update> = updates.iter().filter(|u| u.name == name).collect();
            let pending = same.iter().any(|u| !u.installed);
            let kept: Vec<&Update> = same
                .into_iter()
                .filter(|u| u.installed != pending)
                .collect();
            let first = kept[0];
            let revision = first.revision.to_string();
            let (version, upgradable_version) = if pending {
                (String::new(), revision)
            } else {
                (revision, String::new())
            };
            Package {
                name: name.to_owned(),
                version,
                arch: String::new(),
                upgradable_version,
                title: first.title.clone(),
                classification: first.classification.clone(),
                severity: first.severity.clone(),
                size_bytes: kept.iter().map(|u| u.size_bytes).sum(),
                reboot_required: kept.iter().any(|u| u.reboot_required),
            }
        })
        .collect()
}

/// The command's output for an install: one line per update, then the
/// names not pending, and the error when any update did not succeed.
pub fn describe(installed: &Installed) -> (String, Option<String>) {
    let mut out = String::new();
    for r in &installed.results {
        if r.result.succeeded() {
            out.push_str(&format!("{} {}: {}\n", r.name, r.result.as_str(), r.title));
        } else {
            out.push_str(&format!(
                "{} {} ({:#010x}): {}\n",
                r.name,
                r.result.as_str(),
                r.hresult,
                r.title
            ));
        }
    }
    for name in &installed.not_pending {
        out.push_str(&format!("{name} is not pending\n"));
    }
    if installed.results.is_empty() {
        out.push_str("nothing to install\n");
    }
    if installed.reboot_required {
        out.push_str("a restart is required\n");
    }
    let failed: Vec<String> = installed
        .results
        .iter()
        .filter(|r| !r.result.succeeded())
        .map(|r| format!("{} ({:#010x})", r.name, r.hresult))
        .collect();
    let error = (!failed.is_empty()).then(|| {
        format!(
            "{} of {} updates did not install: {}",
            failed.len(),
            installed.results.len(),
            failed.join(", ")
        )
    });
    (out, error)
}

/// The SID of a service's virtual account, `NT SERVICE\<name>`:
/// `S-1-5-80-` and the SHA-1 of the upper-case name in UTF-16LE, as five
/// little-endian numbers.
pub fn service_sid(name: &str) -> String {
    let utf16: Vec<u8> = name
        .to_uppercase()
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let digest = ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, &utf16);
    let parts: Vec<String> = digest
        .as_ref()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| u32::from_le_bytes(*c).to_string())
        .collect();
    format!("S-1-5-80-{}", parts.join("-"))
}

/// The pipe's protected DACL: full access for SYSTEM, read and write for
/// `client`, nothing for anyone else.
pub fn pipe_sddl(client: &str) -> String {
    format!("D:P(A;;GA;;;SY)(A;;GRGW;;;{client})")
}

#[cfg(test)]
mod tests;
