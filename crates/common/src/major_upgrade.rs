//! Major distribution upgrades (ADR-0182): the upgrades this version
//! knows, one release at a time, and the words the agent and the server
//! share about them (phases and checks).

/// How a distribution moves to its next release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// Debian: MonitorFlow points apt's sources at the new codename, then
    /// `apt-get upgrade --without-new-pkgs` and `full-upgrade`.
    Apt,
    /// Ubuntu: its own `do-release-upgrade`, non-interactive.
    ReleaseUpgrade,
    /// RHEL and its derivatives: `leapp preupgrade`, then `leapp upgrade`
    /// and the reboot into its upgrade environment; Rocky Linux and
    /// AlmaLinux through the ELevate data for leapp.
    Leapp,
}

/// One upgrade this version knows: from a release to the next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Upgrade {
    /// `ID` of `/etc/os-release`, like "debian".
    pub distribution: &'static str,
    /// `VERSION_ID`, its major part where minors follow ("12", "9"), the
    /// whole of it for Ubuntu ("22.04").
    pub from: &'static str,
    /// Empty for the RHEL family, whose releases have none worth naming.
    pub from_codename: &'static str,
    pub to: &'static str,
    pub to_codename: &'static str,
    /// The name people know it by, like "Debian".
    pub label: &'static str,
    pub method: Method,
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

    /// "Debian 12 (bookworm)", "Rocky Linux 8".
    pub fn from_label(&self) -> String {
        label(self.label, self.from, self.from_codename)
    }

    /// "Debian 13 (trixie)", "Rocky Linux 9".
    pub fn to_label(&self) -> String {
        label(self.label, self.to, self.to_codename)
    }

    /// The ELevate data package leapp needs on a derivative, like
    /// `leapp-data-rocky`; `None` on RHEL and outside leapp.
    pub fn elevate_data(&self) -> Option<String> {
        (self.method == Method::Leapp && self.distribution != "rhel")
            .then(|| format!("leapp-data-{}", self.distribution))
    }
}

fn label(name: &str, version: &str, codename: &str) -> String {
    if codename.is_empty() {
        format!("{name} {version}")
    } else {
        format!("{name} {version} ({codename})")
    }
}

const fn apt(from: &'static str, from_codename: &'static str) -> Upgrade {
    Upgrade {
        distribution: "debian",
        from,
        from_codename,
        to: "13",
        to_codename: "trixie",
        label: "Debian",
        method: Method::Apt,
    }
}

const fn ubuntu(
    from: &'static str,
    from_codename: &'static str,
    to: &'static str,
    to_codename: &'static str,
) -> Upgrade {
    Upgrade {
        distribution: "ubuntu",
        from,
        from_codename,
        to,
        to_codename,
        label: "Ubuntu",
        method: Method::ReleaseUpgrade,
    }
}

const fn leapp(
    distribution: &'static str,
    label: &'static str,
    from: &'static str,
    to: &'static str,
) -> Upgrade {
    Upgrade {
        distribution,
        from,
        from_codename: "",
        to,
        to_codename: "",
        label,
        method: Method::Leapp,
    }
}

/// Every upgrade this version knows (ADR-0182). Ubuntu LTS to the next
/// LTS only. Rocky Linux 9 to 10 is left out: ELevate publishes no
/// `leapp-data-rocky` for el9.
pub const KNOWN: &[Upgrade] = &[
    apt("12", "bookworm"),
    ubuntu("22.04", "jammy", "24.04", "noble"),
    ubuntu("24.04", "noble", "26.04", "resolute"),
    leapp("rhel", "Red Hat Enterprise Linux", "8", "9"),
    leapp("rhel", "Red Hat Enterprise Linux", "9", "10"),
    leapp("rocky", "Rocky Linux", "8", "9"),
    leapp("almalinux", "AlmaLinux", "8", "9"),
    leapp("almalinux", "AlmaLinux", "9", "10"),
];

/// Whether `version` ("12.8", "22.04", "9.4") is the release `release`
/// ("12", "22.04", "9"): the same, or it followed by a minor.
fn is_version(version: &str, release: &str) -> bool {
    version == release
        || version
            .strip_prefix(release)
            .is_some_and(|rest| rest.starts_with('.'))
}

/// Whether `running` ("debian 13", "rocky 9.4") is the release `target`
/// ("debian 13", "rocky 9").
pub fn runs(running: &str, target: &str) -> bool {
    let split = |r: &str| {
        let r = r.trim();
        r.split_once(' ')
            .map(|(id, v)| (id.to_owned(), v.trim().to_owned()))
    };
    match (split(running), split(target)) {
        (Some((id, version)), Some((target_id, target_version))) => {
            id == target_id && is_version(&version, &target_version)
        }
        _ => false,
    }
}

/// The upgrade from `release` ("debian 12"), if this version knows one.
pub fn next(release: &str) -> Option<&'static Upgrade> {
    let (distribution, version) = release.trim().split_once(' ')?;
    KNOWN
        .iter()
        .find(|u| u.distribution == distribution && is_version(version.trim(), u.from))
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
    /// `leapp preupgrade`: its report blocks the upgrade on any inhibitor.
    Preupgrade,
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
            Self::Preupgrade => "preupgrade",
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
            Self::Preupgrade,
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

/// The agent's checks for `method`, in the order the report gives them.
pub fn checks(method: Method) -> &'static [&'static str] {
    match method {
        Method::Apt => &[
            "up_to_date",
            "held_packages",
            "broken_packages",
            "free_space",
            "reboot_pending",
            "sources",
            "third_party_sources",
        ],
        Method::ReleaseUpgrade => &[
            "up_to_date",
            "held_packages",
            "broken_packages",
            "free_space",
            "reboot_pending",
            "sources",
            "release_offered",
            "third_party_sources",
        ],
        Method::Leapp => &[
            "up_to_date",
            "held_packages",
            "broken_packages",
            "free_space",
            "reboot_pending",
            "sources",
            "leapp",
            "third_party_sources",
        ],
    }
}

/// Free space an upgrade needs, per path, in bytes; paths on one
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
        assert_eq!(next(""), None);
        assert_eq!(step("debian 12", "debian 13"), Some(debian));
        assert_eq!(step("debian 11", "debian 13"), None);
        assert_eq!(step("debian 12", "debian 14"), None);
    }

    #[test]
    fn knows_ubuntu_lts_and_the_rhel_family() {
        let jammy = next("ubuntu 22.04").unwrap();
        assert_eq!(jammy.to_release(), "ubuntu 24.04");
        assert_eq!(jammy.to_label(), "Ubuntu 24.04 (noble)");
        assert_eq!(jammy.method, Method::ReleaseUpgrade);
        assert_eq!(next("ubuntu 24.04").unwrap().to_release(), "ubuntu 26.04");
        assert_eq!(next("ubuntu 24.10"), None, "LTS to LTS only");
        assert_eq!(next("ubuntu 22"), None);
        assert_eq!(step("ubuntu 22.04", "ubuntu 26.04"), None);

        let rocky = next("rocky 8.10").unwrap();
        assert_eq!(rocky.to_release(), "rocky 9");
        assert_eq!(rocky.from_label(), "Rocky Linux 8");
        assert_eq!(rocky.elevate_data().as_deref(), Some("leapp-data-rocky"));
        assert_eq!(next("rocky 9.4"), None, "no ELevate data for Rocky 9");
        assert_eq!(next("almalinux 9.5").unwrap().to_release(), "almalinux 10");
        let rhel = next("rhel 9.4").unwrap();
        assert_eq!(rhel.to_label(), "Red Hat Enterprise Linux 10");
        assert_eq!(rhel.elevate_data(), None);
        assert_eq!(next("rhel 10.0"), None);
        assert_eq!(next("debian 12").unwrap().elevate_data(), None);
        assert!(KNOWN.iter().all(|u| checks(u.method).len() <= 8));
    }

    #[test]
    fn a_release_runs_with_its_minors() {
        assert!(runs("debian 13", "debian 13"));
        assert!(runs("rocky 9.4", "rocky 9"));
        assert!(runs("ubuntu 24.04", "ubuntu 24.04"));
        assert!(!runs("ubuntu 24.10", "ubuntu 24.04"));
        assert!(!runs("debian 13", "ubuntu 13"));
        assert!(!runs("debian 130", "debian 13"));
        assert!(!runs("", "debian 13"));
    }

    #[test]
    fn phases_round_trip() {
        for word in [
            "starting",
            "preupgrade",
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
