//! Major distribution upgrades (ADR-0182): the upgrades this version
//! knows, one release at a time, and the words the agent and the server
//! share about them (phases and checks).

/// One upgrade this version knows: from a release to the next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Upgrade {
    /// `ID` of `/etc/os-release`, like "debian".
    pub distribution: &'static str,
    pub from: &'static str,
    pub from_codename: &'static str,
    pub to: &'static str,
    pub to_codename: &'static str,
    /// The name people know it by, like "Debian".
    pub label: &'static str,
}

impl Upgrade {
    /// "debian 12", the form `Hello` reports (`os_version`).
    pub fn from_release(&self) -> String {
        format!("{} {}", self.distribution, self.from)
    }

    /// "debian 13".
    pub fn to_release(&self) -> String {
        format!("{} {}", self.distribution, self.to)
    }

    /// "Debian 12 (bookworm)".
    pub fn from_label(&self) -> String {
        format!("{} {} ({})", self.label, self.from, self.from_codename)
    }

    /// "Debian 13 (trixie)".
    pub fn to_label(&self) -> String {
        format!("{} {} ({})", self.label, self.to, self.to_codename)
    }
}

/// Every upgrade this version knows. Ubuntu and RHEL come later (ADR-0182).
pub const KNOWN: &[Upgrade] = &[Upgrade {
    distribution: "debian",
    from: "12",
    from_codename: "bookworm",
    to: "13",
    to_codename: "trixie",
    label: "Debian",
}];

/// The upgrade from `release` ("debian 12"), if this version knows one.
pub fn next(release: &str) -> Option<&'static Upgrade> {
    let (distribution, version) = release.trim().split_once(' ')?;
    // "12.8" and "12" are both Debian 12; os-release gives the major.
    let major = version.split('.').next()?;
    KNOWN
        .iter()
        .find(|u| u.distribution == distribution && u.from == major)
}

/// The upgrade from `from` to `to`, when this version knows exactly that
/// step: never two releases at once.
pub fn step(from: &str, to: &str) -> Option<&'static Upgrade> {
    next(from).filter(|u| u.to_release() == to.trim())
}

/// Where an upgrade is, in the state file and the report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Starting,
    Sources,
    Refreshing,
    Upgrading,
    FullUpgrade,
    Rebooting,
    Done,
    Failed,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Sources => "sources",
            Self::Refreshing => "refreshing",
            Self::Upgrading => "upgrading",
            Self::FullUpgrade => "full_upgrade",
            Self::Rebooting => "rebooting",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }

    pub fn parse(word: &str) -> Option<Self> {
        [
            Self::Starting,
            Self::Sources,
            Self::Refreshing,
            Self::Upgrading,
            Self::FullUpgrade,
            Self::Rebooting,
            Self::Done,
            Self::Failed,
        ]
        .into_iter()
        .find(|p| p.as_str() == word)
    }

    pub fn is_final(self) -> bool {
        matches!(self, Self::Done | Self::Failed)
    }

    /// Whether the package step started: from then on, only a snapshot
    /// goes back.
    pub fn packages_touched(self) -> bool {
        matches!(self, Self::Upgrading | Self::FullUpgrade | Self::Rebooting)
    }
}

/// The state of a check.
pub const OK: &str = "ok";
/// Never blocks.
pub const WARNING: &str = "warning";
pub const FAILED: &str = "failed";

/// The agent's checks, in the order the report gives them.
pub const CHECKS: &[&str] = &[
    "up_to_date",
    "held_packages",
    "broken_packages",
    "free_space",
    "reboot_pending",
    "sources",
    "third_party_sources",
];

/// Free space a Debian upgrade needs, per path, in bytes; paths on one
/// filesystem add up (ADR-0182).
pub const FREE_SPACE: &[(&str, u64)] = &[("/", 2 << 30), ("/var", 4 << 30), ("/boot", 300 << 20)];

/// "1.2 GiB", "300 MiB".
pub fn bytes(n: u64) -> String {
    const GIB: f64 = (1u64 << 30) as f64;
    const MIB: f64 = (1u64 << 20) as f64;
    let n = n as f64;
    if n >= GIB {
        format!("{:.1} GiB", n / GIB)
    } else {
        format!("{:.0} MiB", n / MIB)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knows_one_release_at_a_time() {
        let debian = next("debian 12").unwrap();
        assert_eq!(debian.to_release(), "debian 13");
        assert_eq!(debian.from_label(), "Debian 12 (bookworm)");
        assert_eq!(debian.to_label(), "Debian 13 (trixie)");
        assert_eq!(next("debian 12.8"), Some(debian));
        assert_eq!(next("debian 13"), None);
        assert_eq!(next("debian 11"), None, "never two releases at once");
        assert_eq!(next("ubuntu 22.04"), None);
        assert_eq!(next(""), None);
        assert_eq!(step("debian 12", "debian 13"), Some(debian));
        assert_eq!(step("debian 11", "debian 13"), None);
        assert_eq!(step("debian 12", "debian 14"), None);
    }

    #[test]
    fn phases_round_trip() {
        for word in [
            "starting",
            "sources",
            "refreshing",
            "upgrading",
            "full_upgrade",
            "rebooting",
            "done",
            "failed",
        ] {
            assert_eq!(Phase::parse(word).unwrap().as_str(), word);
        }
        assert!(Phase::Done.is_final() && Phase::Failed.is_final());
        assert!(!Phase::Rebooting.is_final());
        assert!(!Phase::Refreshing.packages_touched());
        assert!(Phase::Upgrading.packages_touched());
        assert_eq!(Phase::parse("x"), None);
    }

    #[test]
    fn sizes_read_well() {
        assert_eq!(bytes(300 << 20), "300 MiB");
        assert_eq!(bytes((12 << 30) / 10), "1.2 GiB");
    }
}
