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

/// `aptly mirror list -raw`: one aptly mirror name per line.
pub fn list(layout: &Layout) -> Invocation {
    let mut args = aptly(layout, ["mirror", "list"]);
    args.push("-raw".into());
    Invocation::new(Tool::Aptly, args)
}

pub fn create(layout: &Layout, mirror: &Mirror, component: &str) -> Invocation {
    let mut args = aptly(layout, ["mirror", "create"]);
    args.push(keyring(layout));
    if !mirror.architectures.is_empty() {
        args.push(format!("-architectures={}", mirror.architectures.join(",")).into());
    }
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
    let mut drop_publish = aptly(layout, ["publish", "drop"]);
    drop_publish.push("-force-drop".into());
    drop_publish.push(mirror.distribution.clone().into());
    drop_publish.push(prefix(mirror, snapshot).into());
    let mut commands = vec![Invocation::new(Tool::Aptly, drop_publish)];
    for component in &mirror.components {
        let mut args = aptly(layout, ["snapshot", "drop"]);
        args.push("-force".into());
        args.push(snapshot_name(mirror, component, snapshot).into());
        commands.push(Invocation::new(Tool::Aptly, args));
    }
    commands
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
            format!("aptly mirror create {c} {k} -architectures=amd64 mf:m:main https://up/debian bookworm main")
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
            format!("aptly publish snapshot {c} -batch -gpg-key=FPR -component=main,contrib -distribution=bookworm mf:m:main:s1 mf:m:contrib:s1 m/s1")
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
    }
}
