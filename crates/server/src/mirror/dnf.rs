//! reposync and createrepo_c command lines (ADR-0030). The working tree
//! `dnf-work/<mirror>` is a repository of its own after every sync; a
//! snapshot is a hardlinked copy of it ([`super::layout::link_tree`]).

use std::ffi::OsString;
use std::path::Path;

use super::layout::Layout;
use super::model::Mirror;
use super::runner::{Invocation, Tool};

/// The dnf repository ID used for the upstream while syncing.
pub fn repo_id(mirror: &Mirror) -> String {
    format!("mf-{}", mirror.name)
}

pub fn reposync(layout: &Layout, mirror: &Mirror) -> Invocation {
    let id = repo_id(mirror);
    let mut path = OsString::from("--download-path=");
    path.push(layout.dnf_work(mirror));
    let mut args: Vec<OsString> = vec![
        format!("--repofrompath={id},{}", mirror.upstream_url).into(),
        format!("--repoid={id}").into(),
        path,
        "--norepopath".into(),
        "--delete".into(),
        // Groups, modules and advisories, which `createrepo_c --update`
        // keeps.
        "--download-metadata".into(),
    ];
    for arch in &mirror.architectures {
        args.push(format!("--arch={arch}").into());
    }
    // `--arch` is an exact match: a repository is mostly noarch packages.
    if !mirror.architectures.is_empty() {
        args.push("--arch=noarch".into());
    }
    Invocation::new(Tool::Reposync, args)
}

/// Regenerates `repodata/` of the working tree, reusing unchanged entries.
/// In gzip, which every dnf and yum reads, unlike the default zstd.
pub fn createrepo(layout: &Layout, mirror: &Mirror) -> Invocation {
    let mut args: Vec<OsString> = vec![
        "--update".into(),
        "--workers=4".into(),
        "--general-compress-type=gz".into(),
    ];
    args.push(layout.dnf_work(mirror).into());
    Invocation::new(Tool::CreaterepoC, args)
}

/// Signs `<repo>/repodata/repomd.xml` into `repomd.xml.asc` with `key`.
pub fn sign_repomd(repo: &Path, key: &str) -> Invocation {
    let repomd = repo.join("repodata").join("repomd.xml");
    let mut asc = repomd.clone().into_os_string();
    asc.push(".asc");
    let mut args: Vec<OsString> = ["--batch", "--yes", "--local-user", key, "--armor"]
        .map(OsString::from)
        .into();
    args.extend([
        "--output".into(),
        asc,
        "--detach-sign".into(),
        repomd.into(),
    ]);
    Invocation::new(Tool::Gpg, args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mirror::apt::tests::mirror;
    use crate::mirror::Family;

    #[test]
    fn command_lines() {
        let layout = Layout::new("/srv/m");
        let m = mirror(Family::Dnf);
        assert_eq!(
            reposync(&layout, &m).to_string(),
            "reposync --repofrompath=mf-m,https://up/debian --repoid=mf-m --download-path=/srv/m/dnf-work/m --norepopath --delete --download-metadata --arch=amd64 --arch=noarch"
        );
        assert_eq!(
            createrepo(&layout, &m).to_string(),
            "createrepo_c --update --workers=4 --general-compress-type=gz /srv/m/dnf-work/m"
        );
        assert_eq!(
            sign_repomd(Path::new("/srv/m/dnf/m/.creating-s1"), "FPR").to_string(),
            "gpg --batch --yes --local-user FPR --armor --output /srv/m/dnf/m/.creating-s1/repodata/repomd.xml.asc --detach-sign /srv/m/dnf/m/.creating-s1/repodata/repomd.xml"
        );
    }
}
