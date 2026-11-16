//! The distributions an administrator can mirror in one click (ADR-0067).
//! A distribution is every repository its hosts need, one mirror each,
//! for amd64, synced daily. Anything else stays the CLI's `mirror add`.
//!
//! Sizes are what the upstream package indexes added up to on 2026-10-31,
//! rounded up with room for metadata and a few months of updates. The
//! on-demand ones (ADR-0142) come from the index files an amd64 host reads,
//! measured on 2026-11-16 (up to 22 MiB per apt suite, up to 156 MiB per
//! dnf repository), times 7 snapshots, plus 3,000 packages of about 1 MiB.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::Serialize;

use super::model::{Family, MirrorSpec, Mode};

const GIB: u64 = 1 << 30;
const DAILY_S: i32 = 24 * 60 * 60;

/// One upstream repository: an apt suite, or a dnf repository when there
/// are no components.
struct Repository {
    name: &'static str,
    url: &'static str,
    suite: &'static str,
    components: &'static [&'static str],
    archive: Archive,
}

/// The distribution's own archive of past states, where an on-demand
/// mirror fetches a package its upstream no longer has (ADR-0142). Each
/// was checked to serve the same relative paths on 2026-11-16.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Archive {
    /// `<base>/<time>/`: the archive's state at that time.
    Dated(&'static str),
    /// `<prefix><minor><suffix>`: one tree per minor release.
    Vault {
        prefix: &'static str,
        suffix: &'static str,
    },
}

/// Highest minor release tried in a vault when the tree does not say
/// which one it is.
const MAX_MINOR: u32 = 23;

pub struct Distribution {
    pub id: &'static str,
    pub name: &'static str,
    pub family: Family,
    /// A full mirror (ADR-0067).
    pub size_bytes: u64,
    /// An on-demand mirror (ADR-0142): its indexes for 7 snapshots plus
    /// 3,000 packages, the high end of a fleet's working set.
    pub on_demand_bytes: u64,
    /// What an on-demand mirror needs at least: its indexes.
    pub index_bytes: u64,
    repositories: &'static [Repository],
}

/// An apt archive and its historical archive.
struct Upstream {
    url: &'static str,
    archive: &'static str,
}

const fn apt(
    name: &'static str,
    upstream: Upstream,
    suite: &'static str,
    components: &'static [&'static str],
) -> Repository {
    Repository {
        name,
        url: upstream.url,
        suite,
        components,
        archive: Archive::Dated(upstream.archive),
    }
}

const fn dnf(
    name: &'static str,
    url: &'static str,
    prefix: &'static str,
    suffix: &'static str,
) -> Repository {
    Repository {
        name,
        url,
        suite: "",
        components: &[],
        archive: Archive::Vault { prefix, suffix },
    }
}

const DEBIAN: Upstream = Upstream {
    url: "https://deb.debian.org/debian",
    archive: "https://snapshot.debian.org/archive/debian",
};
const DEBIAN_SECURITY: Upstream = Upstream {
    url: "https://deb.debian.org/debian-security",
    archive: "https://snapshot.debian.org/archive/debian-security",
};
/// Its security pockets are in the same archive, and in its history.
const UBUNTU: Upstream = Upstream {
    url: "http://archive.ubuntu.com/ubuntu",
    archive: "https://snapshot.ubuntu.com/ubuntu",
};
/// What the Debian installer enables.
const DEBIAN_COMPONENTS: &[&str] = &["main", "non-free-firmware"];
/// Without `restricted`, whose kernel modules are several hundred GiB in
/// `-updates`, and `multiverse`.
const UBUNTU_COMPONENTS: &[&str] = &["main", "universe"];

pub const CATALOG: &[Distribution] = &[
    Distribution {
        id: "debian-13",
        name: "Debian 13 (trixie)",
        family: Family::Apt,
        size_bytes: 150 * GIB,
        on_demand_bytes: 5 * GIB,
        index_bytes: GIB,
        repositories: &[
            apt("debian-13", DEBIAN, "trixie", DEBIAN_COMPONENTS),
            apt(
                "debian-13-updates",
                DEBIAN,
                "trixie-updates",
                DEBIAN_COMPONENTS,
            ),
            apt(
                "debian-13-security",
                DEBIAN_SECURITY,
                "trixie-security",
                DEBIAN_COMPONENTS,
            ),
        ],
    },
    Distribution {
        id: "debian-12",
        name: "Debian 12 (bookworm)",
        family: Family::Apt,
        size_bytes: 125 * GIB,
        on_demand_bytes: 5 * GIB,
        index_bytes: GIB,
        repositories: &[
            apt("debian-12", DEBIAN, "bookworm", DEBIAN_COMPONENTS),
            apt(
                "debian-12-updates",
                DEBIAN,
                "bookworm-updates",
                DEBIAN_COMPONENTS,
            ),
            apt(
                "debian-12-security",
                DEBIAN_SECURITY,
                "bookworm-security",
                DEBIAN_COMPONENTS,
            ),
        ],
    },
    Distribution {
        id: "ubuntu-24.04",
        name: "Ubuntu 24.04 LTS (noble)",
        family: Family::Apt,
        size_bytes: 280 * GIB,
        on_demand_bytes: 6 * GIB,
        index_bytes: GIB,
        repositories: &[
            apt("ubuntu-24.04", UBUNTU, "noble", UBUNTU_COMPONENTS),
            apt(
                "ubuntu-24.04-updates",
                UBUNTU,
                "noble-updates",
                UBUNTU_COMPONENTS,
            ),
            apt(
                "ubuntu-24.04-security",
                UBUNTU,
                "noble-security",
                UBUNTU_COMPONENTS,
            ),
        ],
    },
    Distribution {
        id: "ubuntu-22.04",
        name: "Ubuntu 22.04 LTS (jammy)",
        family: Family::Apt,
        size_bytes: 390 * GIB,
        on_demand_bytes: 6 * GIB,
        index_bytes: GIB,
        repositories: &[
            apt("ubuntu-22.04", UBUNTU, "jammy", UBUNTU_COMPONENTS),
            apt(
                "ubuntu-22.04-updates",
                UBUNTU,
                "jammy-updates",
                UBUNTU_COMPONENTS,
            ),
            apt(
                "ubuntu-22.04-security",
                UBUNTU,
                "jammy-security",
                UBUNTU_COMPONENTS,
            ),
        ],
    },
    Distribution {
        id: "rocky-10",
        name: "Rocky Linux 10",
        family: Family::Dnf,
        size_bytes: 55 * GIB,
        on_demand_bytes: 8 * GIB,
        index_bytes: 2 * GIB,
        repositories: &[
            dnf(
                "rocky-10-baseos",
                "https://dl.rockylinux.org/pub/rocky/10/BaseOS/x86_64/os/",
                "https://dl.rockylinux.org/vault/rocky/10.",
                "/BaseOS/x86_64/os/",
            ),
            dnf(
                "rocky-10-appstream",
                "https://dl.rockylinux.org/pub/rocky/10/AppStream/x86_64/os/",
                "https://dl.rockylinux.org/vault/rocky/10.",
                "/AppStream/x86_64/os/",
            ),
        ],
    },
    Distribution {
        id: "rocky-9",
        name: "Rocky Linux 9",
        family: Family::Dnf,
        size_bytes: 60 * GIB,
        on_demand_bytes: 8 * GIB,
        index_bytes: 2 * GIB,
        repositories: &[
            dnf(
                "rocky-9-baseos",
                "https://dl.rockylinux.org/pub/rocky/9/BaseOS/x86_64/os/",
                "https://dl.rockylinux.org/vault/rocky/9.",
                "/BaseOS/x86_64/os/",
            ),
            dnf(
                "rocky-9-appstream",
                "https://dl.rockylinux.org/pub/rocky/9/AppStream/x86_64/os/",
                "https://dl.rockylinux.org/vault/rocky/9.",
                "/AppStream/x86_64/os/",
            ),
        ],
    },
    Distribution {
        id: "almalinux-10",
        name: "AlmaLinux 10",
        family: Family::Dnf,
        size_bytes: 55 * GIB,
        on_demand_bytes: 8 * GIB,
        index_bytes: 2 * GIB,
        repositories: &[
            dnf(
                "almalinux-10-baseos",
                "https://repo.almalinux.org/almalinux/10/BaseOS/x86_64/os/",
                "https://vault.almalinux.org/10.",
                "/BaseOS/x86_64/os/",
            ),
            dnf(
                "almalinux-10-appstream",
                "https://repo.almalinux.org/almalinux/10/AppStream/x86_64/os/",
                "https://vault.almalinux.org/10.",
                "/AppStream/x86_64/os/",
            ),
        ],
    },
    Distribution {
        id: "almalinux-9",
        name: "AlmaLinux 9",
        family: Family::Dnf,
        size_bytes: 60 * GIB,
        on_demand_bytes: 8 * GIB,
        index_bytes: 2 * GIB,
        repositories: &[
            dnf(
                "almalinux-9-baseos",
                "https://repo.almalinux.org/almalinux/9/BaseOS/x86_64/os/",
                "https://vault.almalinux.org/9.",
                "/BaseOS/x86_64/os/",
            ),
            dnf(
                "almalinux-9-appstream",
                "https://repo.almalinux.org/almalinux/9/AppStream/x86_64/os/",
                "https://vault.almalinux.org/9.",
                "/AppStream/x86_64/os/",
            ),
        ],
    },
];

/// The historical archives of the catalog mirror `name`, newest first,
/// for a snapshot synced at `synced`: the archive's state then and a day
/// later, or the vault of the minor release `minor` and older ones, all of
/// them when the tree does not say. Empty for a mirror outside the
/// catalog.
pub fn archives(name: &str, synced: DateTime<Utc>, minor: Option<u32>) -> Vec<String> {
    let Some(repository) = CATALOG
        .iter()
        .flat_map(|d| d.repositories.iter())
        .find(|r| r.name == name)
    else {
        return Vec::new();
    };
    match repository.archive {
        Archive::Dated(base) => [synced, synced + chrono::Days::new(1)]
            .iter()
            .map(|at| format!("{base}/{}/", at.format("%Y%m%dT%H%M%SZ")))
            .collect(),
        Archive::Vault { prefix, suffix } => (0..=minor.unwrap_or(MAX_MINOR).min(MAX_MINOR))
            .rev()
            .map(|m| format!("{prefix}{m}{suffix}"))
            .collect(),
    }
}

pub fn find(id: &str) -> Option<&'static Distribution> {
    CATALOG.iter().find(|d| d.id == id)
}

/// The distribution a server's `os_version` names, like `debian 12` or
/// `rocky 9.4`: an exact release first, then its major version.
pub fn for_os_version(os_version: &str) -> Option<&'static Distribution> {
    let (name, version) = os_version.split_once(' ')?;
    let major = version.split('.').next()?;
    find(&format!("{name}-{version}")).or_else(|| find(&format!("{name}-{major}")))
}

impl Distribution {
    /// The mirrors of the distribution, as [`Self::specs`] names them.
    pub fn mirror_names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.repositories.iter().map(|repository| repository.name)
    }

    /// The mirrors of this distribution, snapshotted after every sync.
    pub fn specs(&self) -> Vec<MirrorSpec> {
        self.specs_in(Default::default())
    }

    /// [`Self::specs`] in `mode`.
    pub fn specs_in(&self, mode: Mode) -> Vec<MirrorSpec> {
        self.repositories
            .iter()
            .map(|repository| MirrorSpec {
                name: repository.name.to_owned(),
                family: self.family,
                upstream_url: repository.url.to_owned(),
                distribution: repository.suite.to_owned(),
                components: repository
                    .components
                    .iter()
                    .map(|c| (*c).to_owned())
                    .collect(),
                // A dnf URL names its architecture.
                architectures: match self.family {
                    Family::Apt => vec!["amd64".to_owned()],
                    Family::Dnf => Vec::new(),
                },
                sync_interval_s: Some(DAILY_S),
                auto_snapshot: true,
                mode,
            })
            .collect()
    }

    /// What `mode` needs on the disk at least: the whole distribution, or
    /// its indexes.
    pub fn needs(&self, mode: Mode) -> u64 {
        match mode {
            Mode::Full => self.size_bytes,
            Mode::OnDemand => self.index_bytes,
        }
    }

    /// [`Self::too_big`] in `mode`: on demand, only when even the indexes
    /// do not fit.
    pub fn too_big_in(&self, mode: Mode, free_bytes: u64) -> Option<String> {
        match mode {
            Mode::Full => self.too_big(free_bytes),
            Mode::OnDemand => (free_bytes < self.index_bytes).then(|| {
                format!(
                    "{} needs about {} GiB for its indexes and this server has {} GiB free.",
                    self.name,
                    self.index_bytes / GIB,
                    gib(free_bytes)
                )
            }),
        }
    }

    /// One plain sentence when `free_bytes` cannot hold this distribution,
    /// for the API. Free space is in GiB with one decimal, as the web
    /// interface writes it, so both say the same number.
    pub fn too_big(&self, free_bytes: u64) -> Option<String> {
        (free_bytes < self.size_bytes).then(|| {
            format!(
                "{} needs about {} GiB and this server has {} GiB free.",
                self.name,
                self.size_bytes / GIB,
                gib(free_bytes)
            )
        })
    }
}

/// `bytes` in GiB with one decimal, rounded like the web interface's
/// `formatBytes`: `3.3`, or `60` when the decimal is zero.
fn gib(bytes: u64) -> String {
    let tenths = (bytes as f64 / GIB as f64 * 10.0).round() / 10.0;
    format!("{tenths}")
}

/// Space of the file system holding `path`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct Disk {
    /// What an unprivileged process can still use, in bytes.
    pub free_bytes: u64,
    /// In bytes.
    pub total_bytes: u64,
}

/// `None` when the system does not tell. Blocking.
pub fn disk(path: &Path) -> Option<Disk> {
    let stats = rustix::fs::statvfs(path).ok()?;
    Some(Disk {
        free_bytes: stats.f_bavail.saturating_mul(stats.f_frsize),
        total_bytes: stats.f_blocks.saturating_mul(stats.f_frsize),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_distribution_makes_valid_mirrors_with_unique_names() {
        let mut names = Vec::new();
        for distribution in CATALOG {
            assert!(find(distribution.id).is_some());
            for spec in distribution.specs() {
                spec.validate().unwrap();
                assert!(spec.name.starts_with(distribution.id), "{}", spec.name);
                assert_eq!(spec.sync_interval_s, Some(DAILY_S));
                assert!(spec.auto_snapshot);
                names.push(spec.name);
            }
        }
        let total = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), total);
        assert!(find("debian-1").is_none());
    }

    #[test]
    fn servers_find_their_distribution() {
        for (os, id) in [
            ("debian 12", "debian-12"),
            ("ubuntu 24.04", "ubuntu-24.04"),
            ("rocky 9.4", "rocky-9"),
            ("almalinux 10.0", "almalinux-10"),
        ] {
            assert_eq!(for_os_version(os).map(|d| d.id), Some(id), "{os}");
        }
        for os in ["debian 11", "ubuntu 24.10", "rhel 9.4", "debian", ""] {
            assert!(for_os_version(os).is_none(), "{os}");
        }
    }

    #[test]
    fn too_little_free_space_is_said_plainly() {
        let debian = find("debian-12").unwrap();
        assert_eq!(debian.too_big(125 * GIB), None);
        assert_eq!(
            debian.too_big(60 * GIB).unwrap(),
            "Debian 12 (bookworm) needs about 125 GiB and this server has 60 GiB free."
        );
        // The same number the page shows in its header, not 3.
        let free = 3 * GIB + GIB * 3 / 10;
        assert_eq!(
            debian.too_big(free).unwrap(),
            "Debian 12 (bookworm) needs about 125 GiB and this server has 3.3 GiB free."
        );
    }

    #[test]
    fn every_repository_has_its_distributions_archive() {
        let at = DateTime::parse_from_rfc3339("2026-11-16T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            archives("debian-13-security", at, None),
            [
                "https://snapshot.debian.org/archive/debian-security/20261116T100000Z/",
                "https://snapshot.debian.org/archive/debian-security/20261117T100000Z/"
            ]
        );
        assert_eq!(
            archives("debian-12-updates", at, None)[0],
            "https://snapshot.debian.org/archive/debian/20261116T100000Z/"
        );
        assert_eq!(
            archives("ubuntu-22.04-security", at, None)[0],
            "https://snapshot.ubuntu.com/ubuntu/20261116T100000Z/"
        );
        assert_eq!(
            archives("rocky-9-appstream", at, Some(8)),
            (0..=8)
                .rev()
                .map(|m| format!(
                    "https://dl.rockylinux.org/vault/rocky/9.{m}/AppStream/x86_64/os/"
                ))
                .collect::<Vec<_>>()
        );
        let alma = archives("almalinux-10-baseos", at, None);
        assert_eq!(alma.len(), 24);
        assert_eq!(
            alma[0],
            "https://vault.almalinux.org/10.23/BaseOS/x86_64/os/"
        );
        assert!(archives("my-own-mirror", at, None).is_empty());
        for repository in CATALOG.iter().flat_map(|d| d.repositories.iter()) {
            assert!(
                !archives(repository.name, at, Some(0)).is_empty(),
                "{}",
                repository.name
            );
        }
    }

    #[test]
    fn on_demand_needs_a_fraction_and_refuses_only_without_room_for_indexes() {
        for distribution in CATALOG {
            assert!(distribution.index_bytes < distribution.on_demand_bytes);
            assert!(
                distribution.on_demand_bytes * 5 < distribution.size_bytes,
                "{}",
                distribution.id
            );
            assert!(distribution
                .specs_in(Mode::OnDemand)
                .iter()
                .all(|s| s.mode == Mode::OnDemand));
        }
        let debian = find("debian-13").unwrap();
        assert_eq!(debian.needs(Mode::OnDemand), GIB);
        let free = 3 * GIB + GIB * 3 / 10;
        assert!(debian.too_big_in(Mode::Full, free).is_some());
        assert_eq!(debian.too_big_in(Mode::OnDemand, free), None);
        assert_eq!(
            debian.too_big_in(Mode::OnDemand, GIB / 2).unwrap(),
            "Debian 13 (trixie) needs about 1 GiB for its indexes and this server has 0.5 GiB free."
        );
    }

    #[test]
    fn the_disk_of_a_directory_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let disk = disk(dir.path()).unwrap();
        assert!(disk.total_bytes > 0 && disk.free_bytes <= disk.total_bytes);
        assert_eq!(super::disk(Path::new("/nonexistent/x")), None);
    }
}
