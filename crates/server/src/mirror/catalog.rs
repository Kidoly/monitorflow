//! The distributions an administrator can mirror in one click (ADR-0067).
//! A distribution is every repository its hosts need, one mirror each,
//! for amd64, synced daily. Anything else stays the CLI's `mirror add`.
//!
//! Sizes are what the upstream package indexes added up to on 2026-10-31,
//! rounded up with room for metadata and a few months of updates.

use std::path::Path;

use serde::Serialize;

use super::model::{Family, MirrorSpec};

const GIB: u64 = 1 << 30;
const DAILY_S: i32 = 24 * 60 * 60;

/// One upstream repository: an apt suite, or a dnf repository when there
/// are no components.
struct Repository {
    name: &'static str,
    url: &'static str,
    suite: &'static str,
    components: &'static [&'static str],
}

pub struct Distribution {
    pub id: &'static str,
    pub name: &'static str,
    pub family: Family,
    pub size_bytes: u64,
    repositories: &'static [Repository],
}

const fn apt(
    name: &'static str,
    url: &'static str,
    suite: &'static str,
    components: &'static [&'static str],
) -> Repository {
    Repository {
        name,
        url,
        suite,
        components,
    }
}

const fn dnf(name: &'static str, url: &'static str) -> Repository {
    apt(name, url, "", &[])
}

const DEBIAN: &str = "https://deb.debian.org/debian";
const DEBIAN_SECURITY: &str = "https://deb.debian.org/debian-security";
const UBUNTU: &str = "http://archive.ubuntu.com/ubuntu";
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
        repositories: &[
            dnf(
                "rocky-10-baseos",
                "https://dl.rockylinux.org/pub/rocky/10/BaseOS/x86_64/os/",
            ),
            dnf(
                "rocky-10-appstream",
                "https://dl.rockylinux.org/pub/rocky/10/AppStream/x86_64/os/",
            ),
        ],
    },
    Distribution {
        id: "rocky-9",
        name: "Rocky Linux 9",
        family: Family::Dnf,
        size_bytes: 60 * GIB,
        repositories: &[
            dnf(
                "rocky-9-baseos",
                "https://dl.rockylinux.org/pub/rocky/9/BaseOS/x86_64/os/",
            ),
            dnf(
                "rocky-9-appstream",
                "https://dl.rockylinux.org/pub/rocky/9/AppStream/x86_64/os/",
            ),
        ],
    },
    Distribution {
        id: "almalinux-10",
        name: "AlmaLinux 10",
        family: Family::Dnf,
        size_bytes: 55 * GIB,
        repositories: &[
            dnf(
                "almalinux-10-baseos",
                "https://repo.almalinux.org/almalinux/10/BaseOS/x86_64/os/",
            ),
            dnf(
                "almalinux-10-appstream",
                "https://repo.almalinux.org/almalinux/10/AppStream/x86_64/os/",
            ),
        ],
    },
    Distribution {
        id: "almalinux-9",
        name: "AlmaLinux 9",
        family: Family::Dnf,
        size_bytes: 60 * GIB,
        repositories: &[
            dnf(
                "almalinux-9-baseos",
                "https://repo.almalinux.org/almalinux/9/BaseOS/x86_64/os/",
            ),
            dnf(
                "almalinux-9-appstream",
                "https://repo.almalinux.org/almalinux/9/AppStream/x86_64/os/",
            ),
        ],
    },
];

pub fn find(id: &str) -> Option<&'static Distribution> {
    CATALOG.iter().find(|d| d.id == id)
}

impl Distribution {
    /// The mirrors of this distribution, snapshotted after every sync.
    pub fn specs(&self) -> Vec<MirrorSpec> {
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
            })
            .collect()
    }

    /// One plain sentence when `free_bytes` cannot hold this distribution.
    pub fn too_big(&self, free_bytes: u64) -> Option<String> {
        (free_bytes < self.size_bytes).then(|| {
            format!(
                "{} needs about {} GiB and this server has {} GiB free.",
                self.name,
                self.size_bytes / GIB,
                free_bytes / GIB
            )
        })
    }
}

/// Space of the file system holding `path`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Disk {
    /// What an unprivileged process can still use.
    pub free_bytes: u64,
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
    fn too_little_free_space_is_said_plainly() {
        let debian = find("debian-12").unwrap();
        assert_eq!(debian.too_big(125 * GIB), None);
        assert_eq!(
            debian.too_big(60 * GIB).unwrap(),
            "Debian 12 (bookworm) needs about 125 GiB and this server has 60 GiB free."
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
