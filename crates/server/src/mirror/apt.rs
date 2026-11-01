//! aptly command lines (ADR-0030). One aptly mirror per component, named
//! `mf:<mirror>:<component>`, because an aptly snapshot of a mirror with
//! several components merges them; their snapshots are
//! `mf:<mirror>:<component>:<snapshot>`, published together at the
//! prefix `<mirror>/<snapshot>`.

use std::ffi::OsString;

use super::layout::Layout;
use super::model::Mirror;
use super::runner::{Invocation, Tool};

pub fn mirror_name(mirror: &Mirror, component: &str) -> String {
    format!("mf:{}:{component}", mirror.name)
}

pub fn snapshot_name(mirror: &Mirror, component: &str, snapshot: &str) -> String {
    format!("mf:{}:{component}:{snapshot}", mirror.name)
}

fn aptly<const N: usize>(layout: &Layout, words: [&str; N]) -> Vec<OsString> {
    let mut args: Vec<OsString> = words.iter().map(OsString::from).collect();
    let mut config = OsString::from("-config=");
    config.push(layout.aptly_config());
    args.push(config);
    args
}

fn keyring(layout: &Layout) -> OsString {
    let mut arg = OsString::from("-keyring=");
    arg.push(layout.upstream_keyring());
    arg
}

fn architectures(mirror: &Mirror) -> Option<OsString> {
    (!mirror.architectures.is_empty())
        .then(|| format!("-architectures={}", mirror.architectures.join(",")).into())
}

/// `aptly mirror list -raw`: one aptly mirror name per line.
pub fn list(layout: &Layout) -> Invocation {
    let mut args = aptly(layout, ["mirror", "list"]);
    args.push("-raw".into());
    Invocation::new(Tool::Aptly, args)
}

pub fn create(layout: &Layout, mirror: &Mirror, component: &str) -> Invocation {
    let mut args = aptly(layout, ["mirror", "create"]);
    args.push(keyring(layout));
    // Debian's security archive lists `updates/main` in its `Release` but
    // serves `main`, as apt asks for it.
    args.push("-force-components".into());
    args.extend(architectures(mirror));
    args.extend(
        [
            mirror_name(mirror, component),
            mirror.upstream_url.clone(),
            mirror.distribution.clone(),
            component.to_owned(),
        ]
        .map(OsString::from),
    );
    Invocation::new(Tool::Aptly, args)
}

pub fn update(layout: &Layout, mirror: &Mirror, component: &str) -> Invocation {
    let mut args = aptly(layout, ["mirror", "update"]);
    args.push(keyring(layout));
    args.push(mirror_name(mirror, component).into());
    Invocation::new(Tool::Aptly, args)
}

pub fn snapshot(layout: &Layout, mirror: &Mirror, component: &str, snapshot: &str) -> Invocation {
    let mut args = aptly(layout, ["snapshot", "create"]);
    args.extend(
        [
            snapshot_name(mirror, component, snapshot),
            "from".into(),
            "mirror".into(),
            mirror_name(mirror, component),
        ]
        .map(OsString::from),
    );
    Invocation::new(Tool::Aptly, args)
}

/// Drops what a failed attempt at `snapshot` may have left; failures of
/// these commands are expected and ignored.
pub fn cleanup(layout: &Layout, mirror: &Mirror, snapshot: &str) -> Vec<Invocation> {
    let mut commands = vec![drop_published(
        layout,
        &mirror.distribution,
        &prefix(mirror, snapshot),
    )];
    for component in &mirror.components {
        commands.push(drop_snapshot(
            layout,
            &snapshot_name(mirror, component, snapshot),
        ));
    }
    commands
}

/// `aptly publish drop` of the publication at `prefix`.
pub fn drop_published(layout: &Layout, distribution: &str, prefix: &str) -> Invocation {
    let mut args = aptly(layout, ["publish", "drop"]);
    args.push("-force-drop".into());
    args.push(distribution.into());
    args.push(prefix.into());
    Invocation::new(Tool::Aptly, args)
}

/// `aptly snapshot drop` of an aptly snapshot, by its full name.
pub fn drop_snapshot(layout: &Layout, name: &str) -> Invocation {
    let mut args = aptly(layout, ["snapshot", "drop"]);
    args.push("-force".into());
    args.push(name.into());
    Invocation::new(Tool::Aptly, args)
}

/// `aptly mirror drop` of an aptly mirror, by its full name.
pub fn drop_mirror(layout: &Layout, name: &str) -> Invocation {
    let mut args = aptly(layout, ["mirror", "drop"]);
    args.push("-force".into());
    args.push(name.into());
    Invocation::new(Tool::Aptly, args)
}

/// Whether a line of [`list`] or [`list_snapshots`] is an object of
/// `mirror`, of any component or snapshot.
pub fn lists_object(line: &str, mirror: &Mirror) -> bool {
    line.starts_with(&format!("mf:{}:", mirror.name))
}

/// `(prefix, distribution)` of a line of [`list_published`] that is a
/// publication of `mirror`.
pub fn published_of<'a>(line: &'a str, mirror: &Mirror) -> Option<(&'a str, &'a str)> {
    let (prefix, distribution) = line.split_once(char::is_whitespace)?;
    let owner = prefix.split('/').next()?;
    (owner == mirror.name).then_some((prefix, distribution.trim()))
}

/// `aptly snapshot list -raw`: one aptly snapshot name per line.
pub fn list_snapshots(layout: &Layout) -> Invocation {
    let mut args = aptly(layout, ["snapshot", "list"]);
    args.push("-raw".into());
    Invocation::new(Tool::Aptly, args)
}

/// `aptly publish list -raw`: `<prefix> <distribution>` per line.
pub fn list_published(layout: &Layout) -> Invocation {
    let mut args = aptly(layout, ["publish", "list"]);
    args.push("-raw".into());
    Invocation::new(Tool::Aptly, args)
}

/// Removes every package and pool file that no aptly mirror or snapshot
/// refers to, which is what frees the disk. Needs a
/// [`super::store::RetentionLock`].
pub fn db_cleanup(layout: &Layout) -> Invocation {
    Invocation::new(Tool::Aptly, aptly(layout, ["db", "cleanup"]))
}

/// Whether a line of [`list_snapshots`] is one of `snapshot`'s.
pub fn lists_snapshot(line: &str, mirror: &Mirror, snapshot: &str) -> bool {
    mirror
        .components
        .iter()
        .any(|component| line == snapshot_name(mirror, component, snapshot))
}

/// Whether a line of [`list_published`] is `snapshot`'s.
pub fn lists_published(line: &str, mirror: &Mirror, snapshot: &str) -> bool {
    line.split_whitespace().next() == Some(prefix(mirror, snapshot).as_str())
}

fn prefix(mirror: &Mirror, snapshot: &str) -> String {
    format!("{}/{snapshot}", mirror.name)
}

/// Publishes the snapshots of every component at `<mirror>/<snapshot>`,
/// signed with `key` (a fingerprint in `GNUPGHOME`).
pub fn publish(layout: &Layout, mirror: &Mirror, snapshot: &str, key: &str) -> Invocation {
    let mut args = aptly(layout, ["publish", "snapshot"]);
    args.extend(
        [
            "-batch".to_owned(),
            format!("-gpg-key={key}"),
            format!("-component={}", mirror.components.join(",")),
            format!("-distribution={}", mirror.distribution),
        ]
        .map(OsString::from),
    );
    // aptly cannot guess them for a component with no packages.
    args.extend(architectures(mirror));
    for component in &mirror.components {
        args.push(snapshot_name(mirror, component, snapshot).into());
    }
    args.push(prefix(mirror, snapshot).into());
    Invocation::new(Tool::Aptly, args)
}

#[cfg(test)]
pub(crate) mod tests {
    use chrono::Utc;
    use uuid::Uuid;

    use super::*;
    use crate::mirror::Family;

    pub(crate) fn mirror(family: Family) -> Mirror {
        let apt = family == Family::Apt;
        Mirror {
            id: Uuid::nil(),
            name: "m".into(),
            family,
            upstream_url: "https://up/debian".into(),
            distribution: if apt {
                "bookworm".into()
            } else {
                String::new()
            },
            components: if apt {
                vec!["main".into(), "contrib".into()]
            } else {
                vec![]
            },
            architectures: vec!["amd64".into()],
            sync_interval_s: None,
            auto_snapshot: true,
            created_at: Utc::now(),
            deleting_at: None,
            delete_error: None,
        }
    }

    #[test]
    fn command_lines() {
        let layout = Layout::new("/srv/m");
        let m = mirror(Family::Apt);
        let c = "-config=/srv/m/aptly.conf";
        let k = "-keyring=/srv/m/upstream-keyring.gpg";
        assert_eq!(
            list(&layout).to_string(),
            format!("aptly mirror list {c} -raw")
        );
        assert_eq!(
            create(&layout, &m, "main").to_string(),
            format!("aptly mirror create {c} {k} -force-components -architectures=amd64 mf:m:main https://up/debian bookworm main")
        );
        assert_eq!(
            update(&layout, &m, "contrib").to_string(),
            format!("aptly mirror update {c} {k} mf:m:contrib")
        );
        assert_eq!(
            snapshot(&layout, &m, "main", "s1").to_string(),
            format!("aptly snapshot create {c} mf:m:main:s1 from mirror mf:m:main")
        );
        assert_eq!(
            publish(&layout, &m, "s1", "FPR").to_string(),
            format!("aptly publish snapshot {c} -batch -gpg-key=FPR -component=main,contrib -distribution=bookworm -architectures=amd64 mf:m:main:s1 mf:m:contrib:s1 m/s1")
        );
        let cleanup: Vec<String> = cleanup(&layout, &m, "s1")
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            cleanup,
            [
                format!("aptly publish drop {c} -force-drop bookworm m/s1"),
                format!("aptly snapshot drop {c} -force mf:m:main:s1"),
                format!("aptly snapshot drop {c} -force mf:m:contrib:s1"),
            ]
        );
        assert_eq!(
            list_snapshots(&layout).to_string(),
            format!("aptly snapshot list {c} -raw")
        );
        assert_eq!(
            list_published(&layout).to_string(),
            format!("aptly publish list {c} -raw")
        );
        assert_eq!(
            db_cleanup(&layout).to_string(),
            format!("aptly db cleanup {c}")
        );
        assert!(lists_snapshot("mf:m:contrib:s1", &m, "s1"));
        assert!(!lists_snapshot("mf:m:contrib:s11", &m, "s1"));
        assert!(lists_published("m/s1 bookworm", &m, "s1"));
        assert!(!lists_published("m/s11 bookworm", &m, "s1"));
        assert_eq!(
            drop_mirror(&layout, "mf:m:main").to_string(),
            format!("aptly mirror drop {c} -force mf:m:main")
        );
        assert!(lists_object("mf:m:contrib", &m) && lists_object("mf:m:main:s1", &m));
        assert!(!lists_object("mf:m-updates:main", &m));
        assert_eq!(
            published_of("m/s1 bookworm", &m),
            Some(("m/s1", "bookworm"))
        );
        assert_eq!(published_of("m-updates/s1 bookworm", &m), None);
    }
}
