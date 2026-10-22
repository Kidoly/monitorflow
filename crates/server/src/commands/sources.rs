//! Package source configuration commands (ADR-0035): the
//! `ConfigureSources` spec the server builds from its mirrors (ADR-0030)
//! for one server, with the mirror signing key and the URL its agent
//! reaches the mirrors at.

use monitorflow_common::proto::v1::configure_sources::{
    AptRepository, DnfRepository, Family as ProtoFamily, Mode, SigningKey,
};
use monitorflow_common::proto::v1::ConfigureSources;
use sqlx::PgExecutor;

use crate::mirror::layout::Layout;
use crate::mirror::signing::{self, KeyError, SigningError};
use crate::mirror::{relays, store, Family, Mirror, RunError, Runner};

/// Snapshot name that follows the newest published snapshot.
pub const CURRENT: &str = "current";
/// Name of the mirror signing key on agents:
/// `monitorflow-archive.asc` (apt) or `RPM-GPG-KEY-monitorflow-archive`.
pub const KEY_NAME: &str = "archive";
/// Most mirrors in one spec.
pub const MAX_MIRRORS: usize = 32;

/// A mirror and the snapshot a server is pointed at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selected {
    pub mirror: Mirror,
    /// A published snapshot's name, or [`CURRENT`].
    pub snapshot: String,
}

/// The public half of the mirror signing key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveKey {
    pub fingerprint: String,
    pub armored: String,
}

#[derive(Debug, thiserror::Error)]
pub enum KeyLookupError {
    #[error(transparent)]
    Key(#[from] KeyError),
    #[error(transparent)]
    Signing(#[from] SigningError),
    #[error(transparent)]
    Run(#[from] RunError),
    #[error("cannot read the public signing key: {0}")]
    Io(#[from] std::io::Error),
}

/// The base URL an agent reaches the `selected` mirrors at, without the
/// trailing slash: the mirror URL of `relay`, the relay the agent connects
/// through, when it holds every selected snapshot (ADR-0037); otherwise
/// the server's `mirror_public_url`. `None` when neither applies.
pub fn base_url(
    public_url: Option<&str>,
    relay: Option<&relays::Served>,
    selected: &[Selected],
) -> Option<String> {
    if let Some(relay) = relay.filter(|relay| {
        selected
            .iter()
            .all(|s| relay.holds(s.mirror.family, &s.mirror.name, &s.snapshot))
    }) {
        return Some(relay.url.trim_end_matches('/').to_owned());
    }
    public_url.map(|url| url.trim_end_matches('/').to_owned())
}

/// The OS family a mirror family serves, as in `servers.os_family`.
pub(crate) fn os_family(family: Family) -> &'static str {
    match family {
        Family::Apt => "debian",
        Family::Dnf => "rhel",
    }
}

/// The spec pointing a server of `server_os_family` at `selected`, signed
/// by `key`. Errors say why in one line, for the API.
pub fn build(
    base_url: &str,
    mode: Mode,
    server_os_family: &str,
    selected: &[Selected],
    key: &ArchiveKey,
) -> Result<ConfigureSources, String> {
    let Some(first) = selected.first() else {
        return Err("at least one mirror is needed".into());
    };
    if selected.len() > MAX_MIRRORS {
        return Err(format!("at most {MAX_MIRRORS} mirrors"));
    }
    let family = first.mirror.family;
    for (i, s) in selected.iter().enumerate() {
        if s.mirror.family != family {
            return Err("all mirrors must be of the same family".into());
        }
        if selected[..i].iter().any(|o| o.mirror.id == s.mirror.id) {
            return Err(format!("mirror `{}` is repeated", s.mirror.name));
        }
    }
    if os_family(family) != server_os_family {
        return Err(format!(
            "{} mirrors cannot serve a {server_os_family} server",
            family.as_str()
        ));
    }
    let mut spec = ConfigureSources {
        mirror_base_url: base_url.trim_end_matches('/').to_owned(),
        mode: mode as i32,
        keys: vec![SigningKey {
            name: KEY_NAME.into(),
            armored: key.armored.clone(),
            fingerprint: key.fingerprint.clone(),
        }],
        ..ConfigureSources::default()
    };
    match family {
        Family::Apt => {
            spec.family = ProtoFamily::Apt as i32;
            // No architectures: apt's own, so a host whose architecture
            // was not mirrored fails the post-check instead of silently
            // getting no packages.
            spec.apt = selected
                .iter()
                .map(|s| AptRepository {
                    mirror: s.mirror.name.clone(),
                    snapshot: s.snapshot.clone(),
                    suite: s.mirror.distribution.clone(),
                    components: s.mirror.components.clone(),
                    architectures: Vec::new(),
                })
                .collect();
        }
        Family::Dnf => {
            spec.family = ProtoFamily::Dnf as i32;
            spec.dnf = selected
                .iter()
                .map(|s| DnfRepository {
                    mirror: s.mirror.name.clone(),
                    snapshot: s.snapshot.clone(),
                    repo_id: s.mirror.name.clone(),
                    name: format!("MonitorFlow {} ({})", s.mirror.name, s.snapshot),
                    gpgcheck: None,
                })
                .collect();
        }
    }
    Ok(spec)
}

/// The mirror `name` with `snapshot` checked: published, or [`CURRENT`]
/// with at least one published snapshot. `Ok(Err(reason))` when it cannot
/// be used.
pub async fn select(
    db: impl PgExecutor<'_> + Copy,
    name: &str,
    snapshot: &str,
) -> Result<Result<Selected, String>, sqlx::Error> {
    let Some(mirror) = store::by_name(db, name).await? else {
        return Ok(Err(format!("no mirror named `{}`", name.escape_debug())));
    };
    let published = if snapshot == CURRENT {
        store::latest_published(db, mirror.id).await?.is_some()
    } else {
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM mirror_snapshots
             WHERE mirror_id = $1 AND name = $2 AND state = 'published'",
        )
        .bind(mirror.id)
        .bind(snapshot)
        .fetch_one(db)
        .await?
            > 0
    };
    if !published {
        return Ok(Err(format!(
            "mirror `{}` has no published snapshot `{}`",
            mirror.name,
            snapshot.escape_debug()
        )));
    }
    Ok(Ok(Selected {
        mirror,
        snapshot: snapshot.to_owned(),
    }))
}

/// The signing key's fingerprint from the mirror GNUPGHOME and its public
/// half from `keys/`, exported first when missing.
pub async fn archive_key(
    runner: &dyn Runner,
    layout: &Layout,
) -> Result<ArchiveKey, KeyLookupError> {
    let list = signing::list_secret_keys();
    let colons = runner.run(&list).await?.check(&list)?.stdout;
    let fingerprint = signing::the_key(&colons)?;
    let path = layout.public_key();
    let armored = match tokio::fs::read_to_string(&path).await {
        Ok(armored) => armored,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            signing::export(runner, layout).await?;
            tokio::fs::read_to_string(&path).await?
        }
        Err(error) => return Err(error.into()),
    };
    Ok(ArchiveKey {
        fingerprint,
        armored,
    })
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use uuid::Uuid;

    use super::*;

    fn mirror(name: &str, family: Family) -> Mirror {
        Mirror {
            id: Uuid::new_v4(),
            name: name.into(),
            family,
            upstream_url: "https://deb.debian.org/debian".into(),
            distribution: if family == Family::Apt {
                "bookworm".into()
            } else {
                String::new()
            },
            components: if family == Family::Apt {
                vec!["main".into(), "contrib".into()]
            } else {
                Vec::new()
            },
            architectures: vec!["amd64".into()],
            sync_interval_s: None,
            auto_snapshot: true,
            created_at: Utc::now(),
        }
    }

    fn selected(mirror: Mirror, snapshot: &str) -> Selected {
        Selected {
            mirror,
            snapshot: snapshot.into(),
        }
    }

    fn key() -> ArchiveKey {
        ArchiveKey {
            fingerprint: "AAAA".into(),
            armored: "-----BEGIN PGP PUBLIC KEY BLOCK-----".into(),
        }
    }

    #[test]
    fn apt_and_dnf_specs_follow_the_mirrors() {
        let spec = build(
            "http://mf.example:7480/",
            Mode::Replace,
            "debian",
            &[selected(mirror("debian-12", Family::Apt), "current")],
            &key(),
        )
        .unwrap();
        assert_eq!(spec.mirror_base_url, "http://mf.example:7480");
        assert_eq!(spec.family, ProtoFamily::Apt as i32);
        assert_eq!(spec.mode, Mode::Replace as i32);
        assert_eq!(
            spec.apt,
            [AptRepository {
                mirror: "debian-12".into(),
                snapshot: "current".into(),
                suite: "bookworm".into(),
                components: vec!["main".into(), "contrib".into()],
                architectures: Vec::new(),
            }]
        );
        assert!(spec.dnf.is_empty());
        assert_eq!(spec.keys.len(), 1);
        assert_eq!(spec.keys[0].name, KEY_NAME);
        assert_eq!(spec.keys[0].fingerprint, "AAAA");

        let spec = build(
            "https://mf.example",
            Mode::Add,
            "rhel",
            &[
                selected(mirror("rocky-9-baseos", Family::Dnf), "20261024T000000Z"),
                selected(mirror("rocky-9-appstream", Family::Dnf), "current"),
            ],
            &key(),
        )
        .unwrap();
        assert_eq!(spec.family, ProtoFamily::Dnf as i32);
        assert_eq!(spec.mode, Mode::Add as i32);
        assert_eq!(spec.dnf.len(), 2);
        assert_eq!(spec.dnf[0].repo_id, "rocky-9-baseos");
        assert_eq!(spec.dnf[0].snapshot, "20261024T000000Z");
        assert_eq!(spec.dnf[0].gpgcheck, None);
        assert!(spec.apt.is_empty());
    }

    #[test]
    fn mismatched_or_repeated_mirrors_are_refused() {
        let apt = mirror("debian-12", Family::Apt);
        let dnf = mirror("rocky-9", Family::Dnf);
        let refused = |os: &str, list: &[Selected]| {
            build("http://m", Mode::Replace, os, list, &key()).unwrap_err()
        };
        assert!(refused("debian", &[]).contains("at least one"));
        assert!(refused(
            "debian",
            &[
                selected(apt.clone(), "current"),
                selected(dnf.clone(), "current")
            ]
        )
        .contains("same family"));
        assert!(refused(
            "debian",
            &[selected(apt.clone(), "current"), selected(apt.clone(), "x")]
        )
        .contains("repeated"));
        assert_eq!(
            refused("rhel", &[selected(apt, "current")]),
            "apt mirrors cannot serve a rhel server"
        );
        let many: Vec<_> = (0..=MAX_MIRRORS)
            .map(|i| selected(mirror(&format!("m{i}"), Family::Dnf), "current"))
            .collect();
        assert!(refused("rhel", &many).contains("at most"));
    }

    #[test]
    fn the_relay_url_is_used_when_the_relay_holds_every_snapshot() {
        let public = Some("http://mf.example:7480/");
        let debian = selected(mirror("debian-12", Family::Apt), "current");
        let security = selected(mirror("debian-12-security", Family::Apt), "s1");
        assert_eq!(base_url(None, None, std::slice::from_ref(&debian)), None);
        assert_eq!(
            base_url(public, None, std::slice::from_ref(&debian)).as_deref(),
            Some("http://mf.example:7480")
        );
        let relay = relays::Served {
            url: "http://relay.example:7480".into(),
            mirrors: vec![relays::ServedMirror {
                family: Family::Apt,
                name: "debian-12".into(),
                current: Some("s2".into()),
                snapshots: vec!["s1".into(), "s2".into()],
            }],
        };
        let base = |list: &[Selected]| base_url(public, Some(&relay), list);
        assert_eq!(
            base(std::slice::from_ref(&debian)).as_deref(),
            Some("http://relay.example:7480")
        );
        assert_eq!(
            base(&[selected(mirror("debian-12", Family::Apt), "s1")]).as_deref(),
            Some("http://relay.example:7480")
        );
        // A snapshot or a mirror the relay lacks: the server for all.
        for list in [
            vec![selected(mirror("debian-12", Family::Apt), "s3")],
            vec![debian.clone(), security],
            vec![selected(mirror("debian-12", Family::Dnf), "current")],
        ] {
            assert_eq!(base(&list).as_deref(), Some("http://mf.example:7480"));
        }
        // Without a server URL, the relay alone.
        assert_eq!(
            base_url(None, Some(&relay), &[debian]).as_deref(),
            Some("http://relay.example:7480")
        );
    }
}
