//! Host facts sent in `Hello`: package family and version from
//! `/etc/os-release`, and the host name.
//!
//! For tests only, [`OS_RELEASE_PATH_ENV`] points the agent at another
//! os-release file, so the end-to-end test can pose as a supported
//! distribution on any Linux host. The family is still checked: a file
//! naming an unsupported distribution is refused like the real one.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// `os-release` locations, in the order systemd documents them.
const OS_RELEASE: [&str; 2] = ["/etc/os-release", "/usr/lib/os-release"];

/// Environment variable naming an os-release file to read instead of the
/// system ones. For testing; a real install never sets it.
pub const OS_RELEASE_PATH_ENV: &str = "MONITORFLOW_OS_RELEASE_PATH";

/// Package family, which decides the mirror type (architecture §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsFamily {
    /// Debian, Ubuntu and derivatives: apt.
    Debian,
    /// RHEL, Rocky, AlmaLinux, CentOS and derivatives: dnf.
    Rhel,
}

impl OsFamily {
    /// The value of `Hello.os_family`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Debian => "debian",
            Self::Rhel => "rhel",
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        match id {
            "debian" | "ubuntu" => Some(Self::Debian),
            "rhel" => Some(Self::Rhel),
            _ => None,
        }
    }
}

impl fmt::Display for OsFamily {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What the agent reports about its host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostInfo {
    pub hostname: String,
    pub os_family: OsFamily,
    /// Distribution and version, like `debian 12` or `rocky 9.4`.
    pub os_version: String,
}

#[derive(Debug, thiserror::Error)]
pub enum OsError {
    #[error("cannot read {}: {error}", path.display())]
    Read { path: PathBuf, error: io::Error },
    #[error("no os-release file found in /etc or /usr/lib")]
    Missing,
    #[error("os-release has no ID")]
    NoId,
    #[error(
        "unsupported distribution `{id}` (ID_LIKE `{id_like}`): the agent supports the debian and rhel families"
    )]
    Unsupported { id: String, id_like: String },
}

/// Reads the host name and `os-release`, failing on an unsupported family.
/// Honours [`OS_RELEASE_PATH_ENV`] when it is set and not empty.
pub fn detect() -> Result<HostInfo, OsError> {
    let path = std::env::var_os(OS_RELEASE_PATH_ENV)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from);
    if let Some(path) = &path {
        tracing::warn!(path = %path.display(), "using an os-release override ({OS_RELEASE_PATH_ENV}); this is meant for tests");
    }
    detect_from(path.as_deref())
}

/// Same as [`detect`], reading `os_release` if given, otherwise the system
/// files.
pub fn detect_from(os_release: Option<&Path>) -> Result<HostInfo, OsError> {
    let text = match os_release {
        Some(path) => read(path)?.ok_or_else(|| OsError::Read {
            path: path.to_owned(),
            error: io::ErrorKind::NotFound.into(),
        })?,
        None => read_os_release()?,
    };
    let (os_family, os_version) = parse_os_release(&text)?;
    Ok(HostInfo {
        hostname: hostname(),
        os_family,
        os_version,
    })
}

/// The file's text, or `None` if it does not exist.
fn read(path: &Path) -> Result<Option<String>, OsError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(OsError::Read {
            path: path.to_owned(),
            error,
        }),
    }
}

fn read_os_release() -> Result<String, OsError> {
    for path in OS_RELEASE.map(Path::new) {
        if let Some(text) = read(path)? {
            return Ok(text);
        }
    }
    Err(OsError::Missing)
}

/// The kernel's host name, or `unknown` if it cannot be read.
pub fn hostname() -> String {
    ["/proc/sys/kernel/hostname", "/etc/hostname"]
        .iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .map(|name| name.trim().to_owned())
        .find(|name| !name.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

/// Family and `<ID> <VERSION_ID>` from `os-release` text. `ID` decides
/// first, then each `ID_LIKE` entry in order.
pub fn parse_os_release(text: &str) -> Result<(OsFamily, String), OsError> {
    let field = |key: &str| {
        text.lines()
            .filter_map(|line| line.trim().split_once('='))
            .filter(|(k, _)| k.trim() == key)
            .map(|(_, v)| unquote(v.trim()))
            .next_back()
    };
    let id = field("ID")
        .filter(|id| !id.is_empty())
        .ok_or(OsError::NoId)?;
    let id_like = field("ID_LIKE").unwrap_or_default();
    let family = std::iter::once(id.as_str())
        .chain(id_like.split_whitespace())
        .find_map(OsFamily::from_id)
        .ok_or_else(|| OsError::Unsupported {
            id: id.clone(),
            id_like: id_like.clone(),
        })?;
    let version = match field("VERSION_ID").filter(|v| !v.is_empty()) {
        Some(v) => format!("{id} {v}"),
        None => id,
    };
    Ok((family, version))
}

/// Shell-style value: `"a b"`, `'a b'` or bare, with `\` escapes in
/// double quotes.
fn unquote(value: &str) -> String {
    if let Some(inner) = value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')) {
        return inner.to_owned();
    }
    let Some(inner) = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) else {
        return value.to_owned();
    };
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.extend(chars.next()),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<(OsFamily, String), String> {
        parse_os_release(text).map_err(|e| e.to_string())
    }

    #[test]
    fn real_distributions() {
        let debian = "PRETTY_NAME=\"Debian GNU/Linux 12 (bookworm)\"\nNAME=\"Debian GNU/Linux\"\nVERSION_ID=\"12\"\nID=debian\n";
        assert_eq!(
            parse(debian).unwrap(),
            (OsFamily::Debian, "debian 12".into())
        );
        let ubuntu = "NAME=\"Ubuntu\"\nVERSION_ID=\"24.04\"\nID=ubuntu\nID_LIKE=debian\n";
        assert_eq!(
            parse(ubuntu).unwrap(),
            (OsFamily::Debian, "ubuntu 24.04".into())
        );
        let rocky = "NAME=\"Rocky Linux\"\nID=\"rocky\"\nID_LIKE=\"rhel centos fedora\"\nVERSION_ID=\"9.4\"\n";
        assert_eq!(parse(rocky).unwrap(), (OsFamily::Rhel, "rocky 9.4".into()));
        let rhel = "ID=\"rhel\"\nID_LIKE=\"fedora\"\nVERSION_ID=\"9.4\"\n";
        assert_eq!(parse(rhel).unwrap(), (OsFamily::Rhel, "rhel 9.4".into()));
        let mint = "ID=linuxmint\nID_LIKE=\"ubuntu debian\"\nVERSION_ID=\"22\"\n";
        assert_eq!(parse(mint).unwrap().0, OsFamily::Debian);
    }

    #[test]
    fn missing_version_and_odd_formatting() {
        let testing = "# Debian testing\n\n  ID = debian \nVERSION_ID=\"\"\n";
        assert_eq!(parse(testing).unwrap(), (OsFamily::Debian, "debian".into()));
        let single = "ID='rhel'\nVERSION_ID='8.10'\n";
        assert_eq!(parse(single).unwrap(), (OsFamily::Rhel, "rhel 8.10".into()));
        // The last assignment wins, as when the file is sourced.
        let twice = "ID=arch\nID=debian\n";
        assert_eq!(parse(twice).unwrap().0, OsFamily::Debian);
    }

    #[test]
    fn unsupported_or_empty_is_refused() {
        let err = parse("ID=fedora\nVERSION_ID=40\n").unwrap_err();
        assert!(err.contains("unsupported distribution `fedora`"), "{err}");
        let err = parse("ID=arch\nID_LIKE=\"\"\n").unwrap_err();
        assert!(err.contains("`arch`"), "{err}");
        assert_eq!(parse("NAME=x\n").unwrap_err(), "os-release has no ID");
        assert_eq!(parse("ID=\"\"\n").unwrap_err(), "os-release has no ID");
    }

    #[test]
    fn unquote_handles_escapes() {
        assert_eq!(unquote(r#""a \"b\" \\ c""#), r#"a "b" \ c"#);
        assert_eq!(unquote("bare"), "bare");
        assert_eq!(unquote("'it''"), "it'");
    }

    #[test]
    fn override_file_is_read_and_still_checked() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("os-release");
        std::fs::write(&path, "ID=debian\nVERSION_ID=\"12\"\n").unwrap();
        let host = detect_from(Some(&path)).unwrap();
        assert_eq!(host.os_family, OsFamily::Debian);
        assert_eq!(host.os_version, "debian 12");

        std::fs::write(&path, "ID=arch\n").unwrap();
        let err = detect_from(Some(&path)).unwrap_err().to_string();
        assert!(err.contains("unsupported distribution `arch`"), "{err}");

        let missing = dir.path().join("missing");
        let err = detect_from(Some(&missing)).unwrap_err().to_string();
        assert!(
            err.starts_with("cannot read") && err.contains("missing"),
            "{err}"
        );
    }

    #[test]
    fn hostname_is_never_empty() {
        assert!(!hostname().is_empty());
    }
}
