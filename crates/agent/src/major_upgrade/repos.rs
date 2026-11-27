//! The RHEL family's repositories before a leapp upgrade (ADR-0182).
//! leapp takes the target release's repositories from its own data
//! (RHEL's subscription or the ELevate data), so nothing here writes: the
//! plan lists the enabled repositories that are not the distribution's,
//! which leapp leaves as they are, and notes a server on the MonitorFlow
//! mirrors, whose files name the old release's mirror: the campaign puts
//! its own repositories back first.

use std::fs;
use std::path::{Path, PathBuf};

use monitorflow_common::major_upgrade::Upgrade;

use super::sources::Plan;

const REPOS: &str = "etc/yum.repos.d";

/// The hosts of each distribution's own repositories.
fn own_hosts(distribution: &str) -> &'static [&'static str] {
    match distribution {
        "rhel" => &["redhat.com"],
        "rocky" => &["rockylinux.org"],
        // The ELevate repository is AlmaLinux's too.
        "almalinux" => &["almalinux.org"],
        _ => &[],
    }
}

/// One enabled repository of a `.repo` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    pub id: String,
    /// `baseurl`, `mirrorlist` and `metalink` values.
    pub urls: Vec<String>,
}

/// The enabled sections of a `.repo` file. `enabled` defaults to on, as
/// in dnf.
pub fn enabled(text: &str) -> Vec<Repo> {
    let mut repos: Vec<(Repo, bool)> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with(['#', ';']) || line.is_empty() {
            continue;
        }
        if let Some(id) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            repos.push((
                Repo {
                    id: id.trim().to_owned(),
                    urls: Vec::new(),
                },
                true,
            ));
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            // A continuation of a list of URLs.
            if let Some((repo, _)) = repos.last_mut() {
                repo.urls.extend(line.split_whitespace().map(str::to_owned));
            }
            continue;
        };
        let Some((repo, on)) = repos.last_mut() else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "enabled" => *on = !matches!(value, "0" | "false" | "no" | "False"),
            "baseurl" | "mirrorlist" | "metalink" => {
                repo.urls.extend(
                    value
                        .split([' ', ','])
                        .filter(|u| !u.is_empty())
                        .map(str::to_owned),
                );
            }
            _ => {}
        }
    }
    repos
        .into_iter()
        .filter(|(repo, on)| *on && repo.id != "main")
        .map(|(repo, _)| repo)
        .collect()
}

fn host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let host = rest.split('/').next().unwrap_or("");
    let host = host.rsplit('@').next().unwrap_or(host);
    host.split(':').next().unwrap_or(host).to_ascii_lowercase()
}

/// Whether `repo` of the file `name` is the distribution's own.
fn own(upgrade: &Upgrade, name: &str, repo: &Repo) -> bool {
    // subscription-manager's file, on RHEL.
    if name == "redhat.repo" {
        return true;
    }
    // ELevate's own repository, where leapp and its data come from.
    if upgrade.elevate_data().is_some() && repo.id.starts_with("elevate") {
        return true;
    }
    let hosts = own_hosts(upgrade.distribution);
    !repo.urls.is_empty()
        && repo.urls.iter().all(|url| {
            let host = host(url);
            hosts
                .iter()
                .any(|h| host == *h || host.ends_with(&format!(".{h}")))
        })
}

/// Reads the `.repo` files under `root`.
pub fn plan(root: &Path, upgrade: &Upgrade) -> Plan {
    let mut plan = Plan::default();
    let dir = root.join(REPOS);
    let mut names: Vec<String> = match fs::read_dir(&dir) {
        Ok(dir) => dir
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".repo"))
            .collect(),
        Err(e) => {
            plan.refusal = Some(format!("cannot read /{REPOS}: {e}"));
            return plan;
        }
    };
    names.sort();
    let mut mirrored = false;
    for name in names {
        let path = PathBuf::from("/").join(REPOS).join(&name);
        let text = match fs::read_to_string(dir.join(&name)) {
            Ok(text) => text,
            Err(e) => {
                plan.refusal = Some(format!("cannot read {}: {e}", path.display()));
                return plan;
            }
        };
        for repo in enabled(&text) {
            if repo.id.starts_with("monitorflow-") {
                mirrored = true;
            } else if own(upgrade, &name, &repo) {
                plan.release_entries += 1;
            } else {
                plan.third_party.push(format!(
                    "{}: [{}] {}",
                    path.display(),
                    repo.id,
                    repo.urls.join(" ")
                ));
            }
        }
    }
    plan.mirrored = mirrored;
    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use monitorflow_common::major_upgrade::next;

    const ROCKY: &str = "[baseos]\nname=Rocky Linux $releasever - BaseOS\n\
        mirrorlist=https://mirrors.rockylinux.org/mirrorlist?arch=$basearch&repo=BaseOS-$releasever\n\
        #baseurl=http://dl.rockylinux.org/$contentdir/$releasever/BaseOS/$basearch/os/\n\
        gpgcheck=1\nenabled=1\n\n\
        [baseos-debug]\nname=debug\nmirrorlist=https://mirrors.rockylinux.org/x\nenabled=0\n";

    #[test]
    fn reads_the_enabled_sections() {
        let repos = enabled(ROCKY);
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].id, "baseos");
        assert_eq!(repos[0].urls.len(), 1, "a commented baseurl is not one");
        let defaults = enabled("[main]\ngpgcheck=1\n[epel]\nbaseurl=https://a/ https://b/\n");
        assert_eq!(defaults[0].id, "epel", "on by default; [main] is not one");
        assert_eq!(defaults[0].urls, ["https://a/", "https://b/"]);
    }

    #[test]
    fn lists_third_party_repositories_and_refuses_the_mirrors() {
        let dir = tempfile::tempdir().unwrap();
        let repos = dir.path().join(REPOS);
        fs::create_dir_all(&repos).unwrap();
        fs::write(repos.join("rocky.repo"), ROCKY).unwrap();
        fs::write(
            repos.join("epel.repo"),
            "[epel]\nmetalink=https://mirrors.fedoraproject.org/metalink?repo=epel-8\n",
        )
        .unwrap();
        fs::write(
            repos.join("elevate.repo"),
            "[elevate]\nbaseurl=https://repo.almalinux.org/elevate/el8/$basearch/\n",
        )
        .unwrap();
        let rocky = next("rocky 8.10").unwrap();
        let plan = plan(dir.path(), rocky);
        assert_eq!(plan.refusal, None);
        assert_eq!(plan.release_entries, 2, "rocky.repo and ELevate");
        assert_eq!(plan.third_party.len(), 1, "{:?}", plan.third_party);
        assert!(plan.third_party[0]
            .starts_with("/etc/yum.repos.d/epel.repo: [epel] https://mirrors.fedoraproject.org"));

        let rhel = super::plan(dir.path(), next("rhel 8.10").unwrap());
        assert_eq!(rhel.release_entries, 0);
        assert_eq!(rhel.third_party.len(), 3, "ELevate is not RHEL's");

        fs::write(
            repos.join("monitorflow-baseos.repo"),
            "[monitorflow-baseos]\nbaseurl=https://central.example.invalid/dnf/rocky-8/current/\nenabled=1\n",
        )
        .unwrap();
        let mirrored = super::plan(dir.path(), rocky);
        assert_eq!(mirrored.refusal, None);
        assert!(mirrored.mirrored);
        let blocked = super::super::sources::blocker(&mirrored, rocky.method).unwrap();
        assert!(blocked.contains("MonitorFlow mirrors"), "{blocked}");
    }

    #[test]
    fn rhel_trusts_the_subscription_file() {
        let dir = tempfile::tempdir().unwrap();
        let repos = dir.path().join(REPOS);
        fs::create_dir_all(&repos).unwrap();
        fs::write(
            repos.join("redhat.repo"),
            "[rhel-9-for-x86_64-baseos-rpms]\nbaseurl=https://cdn.redhat.com/content/dist/rhel9/$releasever/x86_64/baseos/os\nenabled=1\n",
        )
        .unwrap();
        let plan = plan(dir.path(), next("rhel 9.4").unwrap());
        assert_eq!((plan.release_entries, plan.third_party.len()), (1, 0));
    }
}
