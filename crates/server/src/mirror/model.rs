//! Rows of `mirrors`, `mirror_snapshots` and `mirror_runs`, and the rules
//! a new mirror or snapshot name must follow (ADR-0030). The schema checks
//! the same rules; these give a readable error first.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

/// Longest mirror, snapshot, component or architecture name.
pub const MAX_NAME_LEN: usize = 64;

/// Shortest automatic sync interval, in seconds.
pub const MIN_SYNC_INTERVAL_S: i32 = 900;

/// Name of the symlink to the newest published snapshot.
pub const CURRENT: &str = "current";

/// Package family of a mirror, which decides the tools and the URL prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "text", rename_all = "lowercase")]
pub enum Family {
    Apt,
    Dnf,
}

impl Family {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Apt => "apt",
            Self::Dnf => "dnf",
        }
    }
}

impl fmt::Display for Family {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Family {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "apt" => Ok(Self::Apt),
            "dnf" => Ok(Self::Dnf),
            _ => Err(format!("unknown family `{s}`, expected apt or dnf")),
        }
    }
}

/// A mirror to create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorSpec {
    pub name: String,
    pub family: Family,
    pub upstream_url: String,
    /// apt suite; empty for dnf.
    pub distribution: String,
    /// apt components, at least one; none for dnf.
    pub components: Vec<String>,
    /// Empty: every architecture of the upstream.
    pub architectures: Vec<String>,
    /// `None`: synced only on request.
    pub sync_interval_s: Option<i32>,
    /// Publish a snapshot at the end of every successful sync.
    pub auto_snapshot: bool,
}

/// Why a mirror or snapshot was refused; safe to show.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct Invalid(pub String);

fn invalid(message: impl Into<String>) -> Invalid {
    Invalid(message.into())
}

/// `[a-z0-9]` then up to 63 of `[a-z0-9._-]`: a URL segment, and `:`-free
/// so aptly object names `mf:<mirror>:<component>` stay unambiguous.
pub fn validate_mirror_name(name: &str) -> Result<(), Invalid> {
    let mut chars = name.chars();
    let first_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    let rest_ok = chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c));
    (first_ok && rest_ok && name.len() <= MAX_NAME_LEN)
        .then_some(())
        .ok_or_else(|| {
            invalid("mirror names are 1 to 64 of a-z, 0-9, `.`, `_` and `-`, starting with a letter or digit")
        })
}

/// 1 to 64 of `[A-Za-z0-9._-]`, not starting with a dot (hidden from the
/// HTTP listener) and not [`CURRENT`].
pub fn validate_snapshot_name(name: &str) -> Result<(), Invalid> {
    let valid = (1..=MAX_NAME_LEN).contains(&name.len())
        && !name.starts_with('.')
        && name != CURRENT
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c));
    valid.then_some(()).ok_or_else(|| {
        invalid("snapshot names are 1 to 64 of A-Z, a-z, 0-9, `.`, `_` and `-`, not starting with a dot, and not `current`")
    })
}

/// Default snapshot name: the UTC time, like `20261023T090000Z`.
pub fn snapshot_name_at(at: DateTime<Utc>) -> String {
    at.format("%Y%m%dT%H%M%SZ").to_string()
}

/// Suites, components and architectures end up in tool arguments and
/// paths: 1 to 64 of `[A-Za-z0-9._-]`, not starting with `-` or `.`.
fn validate_word(what: &str, value: &str) -> Result<(), Invalid> {
    let valid = (1..=MAX_NAME_LEN).contains(&value.len())
        && !value.starts_with(['-', '.'])
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c));
    valid
        .then_some(())
        .ok_or_else(|| invalid(format!("invalid {what} `{}`", value.escape_debug())))
}

/// An http or https URL with a host and without userinfo, query or
/// fragment: credentials would be stored and logged.
pub fn validate_upstream_url(url: &str) -> Result<(), Invalid> {
    let parsed = reqwest::Url::parse(url).map_err(|_| invalid("upstream_url is not a URL"))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(invalid("upstream_url must be an http or https URL"));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(invalid("upstream_url must not contain credentials"));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() || url.contains('@') {
        return Err(invalid(
            "upstream_url must not have a query, a fragment or `@`",
        ));
    }
    if url.len() > 2048 || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(invalid("upstream_url is too long or has spaces"));
    }
    Ok(())
}

impl MirrorSpec {
    pub fn validate(&self) -> Result<(), Invalid> {
        validate_mirror_name(&self.name)?;
        validate_upstream_url(&self.upstream_url)?;
        match self.family {
            Family::Apt => {
                validate_word("distribution", &self.distribution)?;
                if self.components.is_empty() {
                    return Err(invalid("an apt mirror needs at least one component"));
                }
            }
            Family::Dnf => {
                if !self.distribution.is_empty() || !self.components.is_empty() {
                    return Err(invalid(
                        "a dnf mirror has no distribution or components; put the repository path in upstream_url",
                    ));
                }
            }
        }
        for component in &self.components {
            validate_word("component", component)?;
        }
        for arch in &self.architectures {
            validate_word("architecture", arch)?;
        }
        if has_duplicates(&self.components) || has_duplicates(&self.architectures) {
            return Err(invalid("components and architectures must not repeat"));
        }
        if self.components.len() > 32 || self.architectures.len() > 32 {
            return Err(invalid("at most 32 components and 32 architectures"));
        }
        if self
            .sync_interval_s
            .is_some_and(|s| s < MIN_SYNC_INTERVAL_S)
        {
            return Err(invalid(format!(
                "sync_interval_s must be at least {MIN_SYNC_INTERVAL_S}"
            )));
        }
        Ok(())
    }
}

fn has_duplicates(values: &[String]) -> bool {
    let mut sorted: Vec<&String> = values.iter().collect();
    sorted.sort();
    sorted.windows(2).any(|w| w[0] == w[1])
}

/// A `mirrors` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, sqlx::FromRow)]
pub struct Mirror {
    pub id: Uuid,
    pub name: String,
    pub family: Family,
    pub upstream_url: String,
    pub distribution: String,
    pub components: Vec<String>,
    pub architectures: Vec<String>,
    pub sync_interval_s: Option<i32>,
    pub auto_snapshot: bool,
    pub created_at: DateTime<Utc>,
}

/// State of a snapshot; only `published` ones are served under their name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "text", rename_all = "lowercase")]
pub enum SnapshotState {
    Creating,
    Published,
    Failed,
}

/// A `mirror_snapshots` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, sqlx::FromRow)]
pub struct Snapshot {
    pub id: Uuid,
    pub mirror_id: Uuid,
    pub name: String,
    pub state: SnapshotState,
    pub size_bytes: Option<i64>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub published_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "text", rename_all = "lowercase")]
pub enum RunKind {
    Sync,
    Snapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "text", rename_all = "lowercase")]
pub enum Trigger {
    Schedule,
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "text", rename_all = "lowercase")]
pub enum RunState {
    Running,
    Succeeded,
    Failed,
}

/// A `mirror_runs` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, sqlx::FromRow)]
pub struct Run {
    pub id: Uuid,
    pub mirror_id: Uuid,
    pub kind: RunKind,
    pub trigger: Trigger,
    pub requested_by: Option<String>,
    pub state: RunState,
    pub snapshot_id: Option<Uuid>,
    pub output: String,
    pub error: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apt() -> MirrorSpec {
        MirrorSpec {
            name: "debian-12".into(),
            family: Family::Apt,
            upstream_url: "https://deb.debian.org/debian".into(),
            distribution: "bookworm".into(),
            components: vec!["main".into(), "non-free-firmware".into()],
            architectures: vec!["amd64".into()],
            sync_interval_s: Some(86_400),
            auto_snapshot: true,
        }
    }

    fn dnf() -> MirrorSpec {
        MirrorSpec {
            name: "rocky-9-baseos".into(),
            family: Family::Dnf,
            upstream_url: "https://dl.rockylinux.org/pub/rocky/9/BaseOS/x86_64/os/".into(),
            distribution: String::new(),
            components: vec![],
            architectures: vec!["x86_64".into(), "noarch".into()],
            sync_interval_s: None,
            auto_snapshot: false,
        }
    }

    #[test]
    fn valid_specs_pass() {
        apt().validate().unwrap();
        dnf().validate().unwrap();
        assert_eq!("dnf".parse::<Family>(), Ok(Family::Dnf));
        assert!("yum".parse::<Family>().is_err());
    }

    #[test]
    fn names_follow_the_url_rules() {
        for good in ["a", "debian-12", "ubuntu_24.04", &"x".repeat(64)] {
            validate_mirror_name(good).unwrap();
        }
        for bad in [
            "",
            "Debian",
            "-x",
            ".x",
            "a:b",
            "a/b",
            "a b",
            &"x".repeat(65),
        ] {
            assert!(validate_mirror_name(bad).is_err(), "{bad:?}");
        }
        for good in ["20261023T090000Z", "pre-release.1", "Q4_2026"] {
            validate_snapshot_name(good).unwrap();
        }
        for bad in [
            "",
            "current",
            ".creating-x",
            "a/b",
            "..",
            "é",
            &"x".repeat(65),
        ] {
            assert!(validate_snapshot_name(bad).is_err(), "{bad:?}");
        }
        let at = DateTime::parse_from_rfc3339("2026-10-23T09:00:05Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(snapshot_name_at(at), "20261023T090005Z");
        validate_snapshot_name(&snapshot_name_at(at)).unwrap();
    }

    #[test]
    fn upstream_urls_without_credentials() {
        for bad in [
            "ftp://mirror/debian",
            "https://user:pw@deb.debian.org/debian",
            "https://user@deb.debian.org/debian",
            "https://deb.debian.org/debian?token=x",
            "https://deb.debian.org/debian#x",
            "https://deb.debian.org/de bian",
            "not a url",
        ] {
            let mut spec = apt();
            spec.upstream_url = bad.into();
            assert!(spec.validate().is_err(), "{bad}");
        }
    }

    #[test]
    fn family_rules() {
        let mut spec = apt();
        spec.components.clear();
        assert!(spec.validate().unwrap_err().0.contains("component"));
        let mut spec = apt();
        spec.distribution = String::new();
        assert!(spec.validate().is_err());
        let mut spec = apt();
        spec.distribution = "../etc".into();
        assert!(spec.validate().is_err());
        let mut spec = dnf();
        spec.components = vec!["main".into()];
        assert!(spec.validate().is_err());
        let mut spec = dnf();
        spec.architectures = vec!["x86_64".into(), "x86_64".into()];
        assert!(spec.validate().is_err());
        let mut spec = dnf();
        spec.architectures = vec!["-rf".into()];
        assert!(spec.validate().is_err());
        let mut spec = dnf();
        spec.sync_interval_s = Some(60);
        assert!(spec.validate().unwrap_err().0.contains("900"));
        let mut spec = apt();
        spec.components = vec!["updates/main".into()];
        assert!(spec.validate().is_err());
    }
}
