//! Host facts sent in `Hello`: package family and version from
//! `/etc/os-release`, and the host name. On Windows the family is
//! `windows`, the version comes from the registry (ADR-0047).
//!
//! For tests only, [`OS_RELEASE_PATH_ENV`] points the agent at another
//! os-release file, so the end-to-end test can pose as a supported
//! distribution on any Linux host.
//!
//! No distribution is refused: on one outside the families, or without a
//! readable `os-release`, the agent monitors as usual and only its package
//! features are off (ADR-0063).

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use crate::packages::Tools;

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
    /// Any other Linux distribution: monitoring only (ADR-0063).
    Linux,
    /// Windows and Windows Server; no package manager support yet.
    #[cfg(windows)]
    Windows,
}

impl OsFamily {
    /// The value of `Hello.os_family`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Debian => "debian",
            Self::Rhel => "rhel",
            Self::Linux => "linux",
            #[cfg(windows)]
            Self::Windows => "windows",
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
    /// Distribution and version, like `debian 12` or `rocky 9.4`; empty
    /// when the host does not say.
    pub os_version: String,
    /// Whether the family's package tools are on this host. False on
    /// [`OsFamily::Linux`], and on an image-based system of a known family
    /// (ADR-0063).
    pub package_manager: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum OsError {
    #[error("cannot read {}: {error}", path.display())]
    Read { path: PathBuf, error: io::Error },
    #[error("no os-release file found in /etc or /usr/lib")]
    Missing,
    #[error("cannot read the Windows version from the registry: {0}")]
    Registry(String),
}

/// Reads the host name and `os-release`, which never fails: the `Result`
/// is for Windows. Honours [`OS_RELEASE_PATH_ENV`] when it is set and not
/// empty.
#[cfg(unix)]
pub fn detect() -> Result<HostInfo, OsError> {
    let path = std::env::var_os(OS_RELEASE_PATH_ENV)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from);
    if let Some(path) = &path {
        tracing::warn!(path = %path.display(), "using an os-release override ({OS_RELEASE_PATH_ENV}); this is meant for tests");
    }
    Ok(detect_from(path.as_deref()))
}

/// Same as [`detect`], reading `os_release` if given, otherwise the system
/// files. A file that cannot be read makes an unknown distribution.
pub fn detect_from(os_release: Option<&Path>) -> HostInfo {
    let text = match os_release {
        Some(path) => read(path).and_then(|text| {
            text.ok_or_else(|| OsError::Read {
                path: path.to_owned(),
                error: io::ErrorKind::NotFound.into(),
            })
        }),
        None => read_os_release(),
    };
    let text = text.unwrap_or_else(|error| {
        tracing::warn!(%error, "cannot tell the distribution; package features are unavailable");
        String::new()
    });
    let (os_family, os_version) = parse_os_release(&text);
    HostInfo {
        hostname: hostname(),
        os_family,
        os_version,
        package_manager: Tools::default().package_manager(os_family),
    }
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

/// Reads the host name and the Windows version from the registry.
#[cfg(windows)]
pub fn detect() -> Result<HostInfo, OsError> {
    let key = windows_registry::LOCAL_MACHINE
        .open(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion")
        .map_err(|e| OsError::Registry(e.to_string()))?;
    let product = key
        .get_string("ProductName")
        .map_err(|e| OsError::Registry(format!("ProductName: {e}")))?;
    let build = key
        .get_string("CurrentBuildNumber")
        .map_err(|e| OsError::Registry(format!("CurrentBuildNumber: {e}")))?;
    // Missing before Windows 10 20H2 and Server 2022.
    let display = key.get_string("DisplayVersion").ok();
    let ubr = key.get_u32("UBR").ok();
    Ok(HostInfo {
        hostname: hostname(),
        os_family: OsFamily::Windows,
        os_version: windows_version(&product, display.as_deref(), &build, ubr),
        package_manager: Tools::default().package_manager(OsFamily::Windows),
    })
}

/// Longest `os_version` the server accepts.
const MAX_VERSION: usize = 128;

/// Like `Windows Server 2022 Datacenter 21H2 build 20348.2849`, from the
/// registry's `ProductName`, `DisplayVersion`, `CurrentBuildNumber` and
/// `UBR`. Windows 11 still says "Windows 10" in `ProductName`; its builds
/// start at 22000.
pub fn windows_version(
    product: &str,
    display: Option<&str>,
    build: &str,
    ubr: Option<u32>,
) -> String {
    let product = product.trim();
    let eleven = build.trim().parse::<u32>().is_ok_and(|b| b >= 22000);
    let mut version = match product.strip_prefix("Windows 10") {
        Some(rest) if eleven => format!("Windows 11{rest}"),
        _ => product.to_owned(),
    };
    if let Some(display) = display.map(str::trim).filter(|d| !d.is_empty()) {
        version.push(' ');
        version.push_str(display);
    }
    version.push_str(" build ");
    version.push_str(build.trim());
    if let Some(ubr) = ubr {
        version.push_str(&format!(".{ubr}"));
    }
    version.chars().take(MAX_VERSION).collect()
}

/// The kernel's host name, or `unknown` if it cannot be read.
#[cfg(unix)]
pub fn hostname() -> String {
    ["/proc/sys/kernel/hostname", "/etc/hostname"]
        .iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .map(|name| name.trim().to_owned())
        .find(|name| !name.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

/// The DNS host name from the TCP/IP parameters, else `COMPUTERNAME`
/// (the NetBIOS name), else `unknown`.
#[cfg(windows)]
pub fn hostname() -> String {
    let dns = windows_registry::LOCAL_MACHINE
        .open(r"SYSTEM\CurrentControlSet\Services\Tcpip\Parameters")
        .and_then(|key| key.get_string("Hostname"))
        .ok();
    dns.into_iter()
        .chain(std::env::var("COMPUTERNAME").ok())
        .map(|name| name.trim().to_owned())
        .find(|name| !name.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

/// Family and `<ID> <VERSION_ID>` from `os-release` text. `ID` decides
/// first, then each `ID_LIKE` entry in order; anything else is
/// [`OsFamily::Linux`]. The version is cut to what the server accepts.
pub fn parse_os_release(text: &str) -> (OsFamily, String) {
    let field = |key: &str| {
        text.lines()
            .filter_map(|line| line.trim().split_once('='))
            .filter(|(k, _)| k.trim() == key)
            .map(|(_, v)| unquote(v.trim()))
            .next_back()
    };
    let id = field("ID").unwrap_or_default();
    let id_like = field("ID_LIKE").unwrap_or_default();
    let family = std::iter::once(id.as_str())
        .chain(id_like.split_whitespace())
        .find_map(OsFamily::from_id)
        .unwrap_or(OsFamily::Linux);
    let version = match field("VERSION_ID").filter(|v| !v.is_empty()) {
        Some(v) => format!("{id} {v}"),
        None => id,
    };
    let version = version.trim();
    let end = (0..=MAX_VERSION.min(version.len()))
        .rev()
        .find(|&i| version.is_char_boundary(i))
        .unwrap_or(0);
    (family, version[..end].to_owned())
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

    fn parse(text: &str) -> (OsFamily, String) {
        parse_os_release(text)
    }

    #[test]
    fn real_distributions() {
        let debian = "PRETTY_NAME=\"Debian GNU/Linux 12 (bookworm)\"\nNAME=\"Debian GNU/Linux\"\nVERSION_ID=\"12\"\nID=debian\n";
        assert_eq!(parse(debian), (OsFamily::Debian, "debian 12".into()));
        let ubuntu = "NAME=\"Ubuntu\"\nVERSION_ID=\"24.04\"\nID=ubuntu\nID_LIKE=debian\n";
        assert_eq!(parse(ubuntu), (OsFamily::Debian, "ubuntu 24.04".into()));
        let rocky = "NAME=\"Rocky Linux\"\nID=\"rocky\"\nID_LIKE=\"rhel centos fedora\"\nVERSION_ID=\"9.4\"\n";
        assert_eq!(parse(rocky), (OsFamily::Rhel, "rocky 9.4".into()));
        let rhel = "ID=\"rhel\"\nID_LIKE=\"fedora\"\nVERSION_ID=\"9.4\"\n";
        assert_eq!(parse(rhel), (OsFamily::Rhel, "rhel 9.4".into()));
        let mint = "ID=linuxmint\nID_LIKE=\"ubuntu debian\"\nVERSION_ID=\"22\"\n";
        assert_eq!(parse(mint).0, OsFamily::Debian);
    }

    #[test]
    fn missing_version_and_odd_formatting() {
        let testing = "# Debian testing\n\n  ID = debian \nVERSION_ID=\"\"\n";
        assert_eq!(parse(testing), (OsFamily::Debian, "debian".into()));
        let single = "ID='rhel'\nVERSION_ID='8.10'\n";
        assert_eq!(parse(single), (OsFamily::Rhel, "rhel 8.10".into()));
        // The last assignment wins, as when the file is sourced.
        let twice = "ID=arch\nID=debian\n";
        assert_eq!(parse(twice).0, OsFamily::Debian);
    }

    #[test]
    fn unknown_distributions_are_plain_linux() {
        assert_eq!(
            parse("ID=fedora\nVERSION_ID=40\n"),
            (OsFamily::Linux, "fedora 40".into())
        );
        assert_eq!(
            parse("ID=arch\nID_LIKE=\"\"\n"),
            (OsFamily::Linux, "arch".into())
        );
        assert_eq!(
            parse("ID=alpine\nVERSION_ID=3.20.1\n"),
            (OsFamily::Linux, "alpine 3.20.1".into())
        );
        assert_eq!(parse("NAME=x\n"), (OsFamily::Linux, String::new()));
        assert_eq!(parse("ID=\"\"\n"), (OsFamily::Linux, String::new()));
        assert_eq!(parse(""), (OsFamily::Linux, String::new()));
        assert_eq!(parse("VERSION_ID=1\n"), (OsFamily::Linux, "1".into()));
        assert_eq!(parse("not a key value file\n").0, OsFamily::Linux);
    }

    #[test]
    fn a_long_version_stays_within_what_the_server_accepts() {
        let (_, version) = parse(&format!("ID=x\nVERSION_ID={}\n", "é".repeat(100)));
        assert!(version.len() <= MAX_VERSION, "{}", version.len());
        assert!(version.starts_with("x é"));
    }

    #[test]
    fn unquote_handles_escapes() {
        assert_eq!(unquote(r#""a \"b\" \\ c""#), r#"a "b" \ c"#);
        assert_eq!(unquote("bare"), "bare");
        assert_eq!(unquote("'it''"), "it'");
    }

    #[test]
    fn override_file_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("os-release");
        std::fs::write(&path, "ID=debian\nVERSION_ID=\"12\"\n").unwrap();
        let host = detect_from(Some(&path));
        assert_eq!(host.os_family, OsFamily::Debian);
        assert_eq!(host.os_version, "debian 12");

        std::fs::write(&path, "ID=arch\n").unwrap();
        let host = detect_from(Some(&path));
        assert_eq!(host.os_family, OsFamily::Linux);
        assert_eq!(host.os_version, "arch");
        assert!(!host.package_manager);
    }

    #[test]
    fn an_unreadable_os_release_is_an_unknown_distribution() {
        let dir = tempfile::tempdir().unwrap();
        for path in [dir.path().join("missing"), dir.path().to_owned()] {
            let host = detect_from(Some(&path));
            assert_eq!(host.os_family, OsFamily::Linux);
            assert_eq!(host.os_version, "");
            assert!(!host.package_manager);
        }
        // Not UTF-8.
        let path = dir.path().join("os-release");
        std::fs::write(&path, b"ID=\xff\n").unwrap();
        assert_eq!(detect_from(Some(&path)).os_family, OsFamily::Linux);
    }

    #[test]
    fn windows_versions() {
        assert_eq!(
            windows_version(
                "Windows Server 2022 Datacenter",
                Some("21H2"),
                "20348",
                Some(2849)
            ),
            "Windows Server 2022 Datacenter 21H2 build 20348.2849"
        );
        assert_eq!(
            windows_version("Windows 10 Pro", Some("23H2"), "22631", Some(4317)),
            "Windows 11 Pro 23H2 build 22631.4317"
        );
        assert_eq!(
            windows_version("Windows 10 Enterprise", Some("22H2"), "19045", None),
            "Windows 10 Enterprise 22H2 build 19045"
        );
        assert_eq!(
            windows_version("Windows Server 2019 Standard", None, "17763", Some(1)),
            "Windows Server 2019 Standard build 17763.1"
        );
        assert_eq!(
            windows_version(&"x".repeat(200), None, "1", None).len(),
            128
        );
    }

    #[cfg(windows)]
    #[test]
    fn detects_this_windows_host() {
        let host = detect().unwrap();
        assert_eq!(host.os_family, OsFamily::Windows);
        assert!(
            host.os_version.starts_with("Windows"),
            "{}",
            host.os_version
        );
        assert!(host.os_version.contains(" build "), "{}", host.os_version);
    }

    #[test]
    fn hostname_is_never_empty() {
        assert!(!hostname().is_empty());
    }
}
