//! dnf: one `.repo` file per repository, keys under `/etc/pki/rpm-gpg/`.
//! Repository IDs get a `monitorflow-` prefix, so they never clash with
//! the disabled originals (dnf refuses a repeated ID) and the post-check
//! can select them.
//! `replace` disables the other repositories in place rather than moving
//! their files: a missing `%config(noreplace)` file is recreated by rpm
//! when its package is updated, a modified one is kept.

use std::path::{Path, PathBuf};

use super::{list_dir, Error, Spec, HEADER};

pub(super) const REPOS: &str = "/etc/yum.repos.d";
pub(super) const KEYS: &str = "/etc/pki/rpm-gpg";

pub(super) fn key_path(name: &str) -> PathBuf {
    Path::new(KEYS).join(format!("RPM-GPG-KEY-monitorflow-{name}"))
}

/// MonitorFlow's files: the keys, then a `.repo` file per repository.
pub(super) fn files(spec: &Spec) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files: Vec<_> = spec
        .keys
        .iter()
        .map(|(name, key)| (key_path(name), key.armored.clone().into_bytes()))
        .collect();
    let gpgkey = spec
        .keys
        .iter()
        .map(|(name, _)| format!("file://{}", key_path(name).display()))
        .collect::<Vec<_>>()
        .join(" ");
    for repo in &spec.dnf {
        let id = format!("monitorflow-{}", repo.repo_id);
        let name = if repo.name.is_empty() {
            &id
        } else {
            &repo.name
        };
        let mut text = format!(
            "{HEADER}\n[{}]\nname={name}\nbaseurl={}/dnf/{}/{}/\nenabled=1\ngpgcheck={}\n",
            id,
            spec.base,
            repo.mirror,
            repo.snapshot,
            u8::from(repo.gpgcheck.unwrap_or(true)),
        );
        if !gpgkey.is_empty() {
            text.push_str(&format!("gpgkey={gpgkey}\n"));
        }
        let path = Path::new(REPOS).join(format!("{id}.repo"));
        files.push((path, text.into_bytes()));
    }
    files
}

/// The files dnf reads repositories from, which `replace` disables.
pub(super) fn others(root: &Path) -> Result<Vec<PathBuf>, Error> {
    list_dir(root, REPOS, &["repo"])
}

/// `content` with every repository disabled: each section's `enabled`
/// lines dropped and `enabled=0` added after its header. Disabling twice
/// gives the same result.
pub(super) fn disable(content: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(content);
    let mut out = String::with_capacity(text.len() + 64);
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim();
        let key = trimmed.split_once('=').map(|(key, _)| key.trim());
        if key == Some("enabled") {
            continue;
        }
        out.push_str(line);
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            if !line.ends_with('\n') {
                out.push('\n');
            }
            out.push_str("enabled=0\n");
        }
    }
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::disable;

    #[test]
    fn disables_every_section_once() {
        let repo =
            "# comment\n[baseos]\nname=BaseOS\nenabled = 1\ngpgcheck=1\n\n[extras]\nname=Extras\n";
        let disabled = disable(repo.as_bytes());
        assert_eq!(
            String::from_utf8(disabled.clone()).unwrap(),
            "# comment\n[baseos]\nenabled=0\nname=BaseOS\ngpgcheck=1\n\n[extras]\nenabled=0\nname=Extras\n"
        );
        assert_eq!(disable(&disabled), disabled);
        assert_eq!(disable(b"[last]"), b"[last]\nenabled=0\n");
    }
}
