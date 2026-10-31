//! Mirror syncs and snapshots against fake `aptly`, `reposync`,
//! `createrepo_c` and `gpg` scripts (ADR-0030): command lines, layout,
//! `current`, recorded runs and failures. Ignored without `DATABASE_URL`.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use monitorflow_common::config::MirrorConfig;
use monitorflow_sensors::testing::write_executable;
use monitorflow_server::mirror::{
    store, CommandRunner, Family, JobError, JobKind, Layout, Mirror, MirrorSpec, Mirrors, RunState,
    SnapshotState, Trigger,
};
use sqlx::PgPool;

/// Marks every test in the block as ignored when `DATABASE_URL` is unset.
macro_rules! db_tests {
    ($($item:item)*) => {
        $(
            #[cfg_attr(
                not(db_tests),
                ignore = "DATABASE_URL is not set; run `docker compose -f deploy/docker-compose.dev.yml up -d postgres` and export DATABASE_URL from .env.example"
            )]
            $item
        )*
    };
}

const APTLY: &str = r#"
state=@STATE@/aptly-mirrors
case "$1 $2" in
"mirror list") cat "$state" 2>/dev/null; exit 0 ;;
"mirror create") shift 2
    for a; do case "$a" in -*) ;; *) echo "$a" >> "$state"; break ;; esac; done
    echo "Mirror created" ;;
"mirror update")
    if [ -e @STATE@/fail-update ]; then
        echo "Downloading Release"; echo "ERROR: unable to update: HTTP code 404" >&2; exit 1
    fi
    echo "Mirror updated" ;;
"snapshot create") echo "Snapshot created" ;;
"snapshot drop"|"publish drop") echo "ERROR: not found" >&2; exit 1 ;;
"publish snapshot")
    for a; do prefix="$a"; done
    mkdir -p "@ROOT@/aptly/public/$prefix/dists/bookworm"
    echo Release > "@ROOT@/aptly/public/$prefix/dists/bookworm/Release"
    echo "Published" ;;
*) echo "unexpected: $*" >&2; exit 2 ;;
esac
"#;

const GPG: &str = r#"
case "$*" in
*--list-secret-keys*)
    [ -e @STATE@/no-key ] && exit 0
    printf 'sec:u:4096:1:X:1::::::scESC:\nfpr:::::::::FPR0123:\n' ;;
*--detach-sign*)
    prev=; for a; do [ "$prev" = --output ] && out="$a"; prev="$a"; done
    echo SIGNATURE > "$out" ;;
esac
"#;

const REPOSYNC: &str = r#"
for a; do case "$a" in --download-path=*) dir="${a#--download-path=}" ;; esac; done
v=$(cat @STATE@/version 2>/dev/null || echo 1)
mkdir -p "$dir/Packages"; rm -f "$dir"/Packages/*
echo "rpm $v" > "$dir/Packages/a-$v-1.x86_64.rpm"
echo "downloaded a-$v"
"#;

const CREATEREPO: &str = r#"
for a; do dir="$a"; done
rm -rf "$dir/repodata"; mkdir "$dir/repodata"; ls "$dir/Packages" > "$dir/repodata/repomd.xml"
"#;

/// Fake tools logging their arguments, a mirror directory, and switches
/// (files in `state`) that change what the tools do.
struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    state: PathBuf,
    mirrors: Mirrors,
}

impl Fixture {
    fn new(pool: &PgPool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let (root, state, bin) = (
            dir.path().join("mirrors"),
            dir.path().join("state"),
            dir.path().join("bin"),
        );
        for d in [&state, &bin] {
            std::fs::create_dir(d).unwrap();
        }
        let log = state.join("log");
        for (name, body) in [
            ("aptly", APTLY),
            ("gpg", GPG),
            ("reposync", REPOSYNC),
            ("createrepo_c", CREATEREPO),
        ] {
            let body = body
                .replace("@STATE@", state.to_str().unwrap())
                .replace("@ROOT@", root.to_str().unwrap());
            let script = format!(
                "#!/bin/sh\necho \"{name} $*\" >> '{}'\n{body}",
                log.display()
            );
            write_executable(&bin.join(name), &script, 0o755);
        }
        let config = MirrorConfig {
            aptly: bin.join("aptly"),
            reposync: bin.join("reposync"),
            createrepo_c: bin.join("createrepo_c"),
            gpg: bin.join("gpg"),
            sync_timeout: 30,
            max_concurrent_syncs: 1,
        };
        let layout = Layout::new(&root);
        let runner = CommandRunner::new(&config, layout.gnupg());
        let mirrors = Mirrors::new(pool.clone(), Arc::new(runner), layout);
        Self {
            _dir: dir,
            root,
            state,
            mirrors,
        }
    }

    fn switch(&self, name: &str, on: bool) {
        let path = self.state.join(name);
        if on {
            std::fs::write(path, "").unwrap();
        } else {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Commands run so far, one per line, and forgets them.
    fn take_log(&self) -> Vec<String> {
        let path = self.state.join("log");
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let _ = std::fs::remove_file(path);
        text.lines().map(str::to_owned).collect()
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }
}

async fn create(pool: &PgPool, family: Family, auto_snapshot: bool) -> Mirror {
    let spec = match family {
        Family::Apt => MirrorSpec {
            name: "debian".into(),
            family,
            upstream_url: "https://deb.debian.org/debian".into(),
            distribution: "bookworm".into(),
            components: vec!["main".into(), "contrib".into()],
            architectures: vec!["amd64".into()],
            sync_interval_s: None,
            auto_snapshot,
        },
        Family::Dnf => MirrorSpec {
            name: "rocky".into(),
            family,
            upstream_url: "https://dl.rockylinux.org/pub/rocky/9/BaseOS/x86_64/os/".into(),
            distribution: String::new(),
            components: vec![],
            architectures: vec!["x86_64".into(), "noarch".into()],
            sync_interval_s: None,
            auto_snapshot,
        },
    };
    store::create(pool, &spec, ("system", "test"))
        .await
        .unwrap()
}

fn link(path: &Path) -> String {
    std::fs::read_link(path).unwrap().display().to_string()
}

db_tests! {
    #[sqlx::test]
    async fn apt_sync_creates_updates_and_publishes(pool: PgPool) {
        let fx = Fixture::new(&pool);
        let mirror = create(&pool, Family::Apt, true).await;
        let job = fx.mirrors.begin(&mirror, JobKind::Sync, Trigger::Manual, Some("cli")).await.unwrap();
        let run = job.execute().await;
        assert_eq!(run.state, RunState::Succeeded, "{}", run.output);
        let config = format!("-config={}", fx.path("aptly.conf").display());
        let keyring = format!("-keyring={}", fx.path("upstream-keyring.gpg").display());
        let log = fx.take_log();
        let expected = [
            format!("aptly mirror list {config} -raw"),
            format!("aptly mirror create {config} {keyring} -force-components -architectures=amd64 mf:debian:main https://deb.debian.org/debian bookworm main"),
            format!("aptly mirror update {config} {keyring} mf:debian:main"),
            format!("aptly mirror create {config} {keyring} -force-components -architectures=amd64 mf:debian:contrib https://deb.debian.org/debian bookworm contrib"),
            format!("aptly mirror update {config} {keyring} mf:debian:contrib"),
            "gpg --batch --with-colons --list-secret-keys".to_owned(),
        ];
        assert_eq!(log[..6], expected);
        let snapshot = store::snapshots(&pool, mirror.id, 1).await.unwrap().remove(0);
        let name = &snapshot.name;
        assert!(log[6..].contains(&format!("aptly snapshot create {config} mf:debian:main:{name} from mirror mf:debian:main")), "{log:?}");
        assert_eq!(
            log.last().unwrap(),
            &format!("aptly publish snapshot {config} -batch -gpg-key=FPR0123 -component=main,contrib -distribution=bookworm -architectures=amd64 mf:debian:main:{name} mf:debian:contrib:{name} debian/{name}")
        );
        assert_eq!(snapshot.state, SnapshotState::Published);
        assert_eq!(snapshot.size_bytes, Some(8));
        assert_eq!(run.snapshot_id, Some(snapshot.id));
        assert_eq!(link(&fx.path("aptly/public/debian/current")), *name);
        assert!(fx.path("aptly/public/debian/current/dists/bookworm/Release").is_file());
        assert!(run.output.starts_with("$ aptly mirror list"), "{}", run.output);
        assert!(run.output.contains("Mirror updated\n"));

        // The aptly mirrors exist now: the next sync only updates them.
        let mut manual = mirror.clone();
        manual.auto_snapshot = false;
        let run = fx.mirrors.begin(&manual, JobKind::Sync, Trigger::Schedule, None).await.unwrap().execute().await;
        assert_eq!(run.state, RunState::Succeeded);
        let log = fx.take_log();
        assert_eq!(log.len(), 3);
        assert!(log.iter().all(|l| !l.contains("create")), "{log:?}");
        assert_eq!(run.snapshot_id, None);
    }

    #[sqlx::test]
    async fn failures_are_recorded_and_snapshots_wait_for_a_sync(pool: PgPool) {
        let fx = Fixture::new(&pool);
        let mirror = create(&pool, Family::Apt, true).await;
        assert!(matches!(
            fx.mirrors.begin(&mirror, JobKind::Snapshot(None), Trigger::Manual, None).await,
            Err(JobError::NotSynced)
        ));
        fx.switch("fail-update", true);
        let run = fx.mirrors.begin(&mirror, JobKind::Sync, Trigger::Manual, None).await.unwrap().execute().await;
        assert_eq!(run.state, RunState::Failed);
        assert_eq!(run.error.as_deref(), Some("aptly failed (exit status: 1): ERROR: unable to update: HTTP code 404"));
        assert!(run.output.contains("Downloading Release\nERROR: unable to update"), "{}", run.output);
        assert!(store::snapshots(&pool, mirror.id, 10).await.unwrap().is_empty());

        // Without a signing key the sync succeeds but its snapshot fails;
        // the same name works once a key exists.
        fx.switch("fail-update", false);
        fx.switch("no-key", true);
        let mut manual = mirror.clone();
        manual.auto_snapshot = false;
        let run = fx.mirrors.begin(&manual, JobKind::Sync, Trigger::Manual, None).await.unwrap().execute().await;
        assert_eq!(run.state, RunState::Succeeded);
        let job = fx.mirrors.begin(&mirror, JobKind::Snapshot(Some("q4".into())), Trigger::Manual, None).await.unwrap();
        assert!(matches!(
            fx.mirrors.begin(&mirror, JobKind::Sync, Trigger::Manual, None).await,
            Err(JobError::Busy)
        ));
        let run = job.execute().await;
        assert_eq!(run.state, RunState::Failed);
        assert!(run.error.unwrap().starts_with("no signing key"));
        let failed = store::snapshots(&pool, mirror.id, 1).await.unwrap().remove(0);
        assert_eq!((failed.name.as_str(), failed.state), ("q4", SnapshotState::Failed));
        fx.switch("no-key", false);
        fx.take_log();
        let run = fx.mirrors.begin(&mirror, JobKind::Snapshot(Some("q4".into())), Trigger::Manual, None).await.unwrap().execute().await;
        assert_eq!(run.state, RunState::Succeeded, "{}", run.output);
        // Leftovers of the failed attempt are dropped first; their errors
        // do not fail the run.
        let log = fx.take_log();
        assert!(log[1].starts_with("aptly publish drop"), "{log:?}");
        assert!(log[2].starts_with("aptly snapshot drop"), "{log:?}");
        assert_eq!(link(&fx.path("aptly/public/debian/current")), "q4");
        let run = fx.mirrors.begin(&mirror, JobKind::Snapshot(Some("q4".into())), Trigger::Manual, None).await.unwrap().execute().await;
        assert_eq!(run.error.as_deref(), Some("snapshot `q4` already exists"));
        assert!(matches!(
            fx.mirrors.begin(&mirror, JobKind::Snapshot(Some("current".into())), Trigger::Manual, None).await,
            Err(JobError::Invalid(_))
        ));
    }

    #[sqlx::test]
    async fn dnf_snapshots_are_hardlinked_signed_and_stay_frozen(pool: PgPool) {
        let fx = Fixture::new(&pool);
        let mirror = create(&pool, Family::Dnf, false).await;
        let run = fx.mirrors.begin(&mirror, JobKind::Sync, Trigger::Manual, None).await.unwrap().execute().await;
        assert_eq!(run.state, RunState::Succeeded, "{}", run.output);
        let work = fx.path("dnf-work/rocky");
        assert_eq!(
            fx.take_log(),
            [
                format!("reposync --repofrompath=mf-rocky,https://dl.rockylinux.org/pub/rocky/9/BaseOS/x86_64/os/ --repoid=mf-rocky --download-path={} --norepopath --delete --download-metadata --arch=x86_64 --arch=noarch", work.display()),
                format!("createrepo_c --update --workers=4 --general-compress-type=gz {}", work.display()),
            ]
        );
        let run = fx.mirrors.begin(&mirror, JobKind::Snapshot(Some("s1".into())), Trigger::Manual, None).await.unwrap().execute().await;
        assert_eq!(run.state, RunState::Succeeded, "{}", run.output);
        let s1 = fx.path("dnf/rocky/s1");
        let rpm = "Packages/a-1-1.x86_64.rpm";
        let ino = |p: PathBuf| std::fs::metadata(p).unwrap().ino();
        assert_eq!(ino(s1.join(rpm)), ino(work.join(rpm)));
        assert_eq!(std::fs::read_to_string(s1.join("repodata/repomd.xml.asc")).unwrap(), "SIGNATURE\n");
        assert!(!work.join("repodata/repomd.xml.asc").exists());
        assert!(!fx.path("dnf/rocky/.creating-s1").exists());
        assert_eq!(link(&fx.path("dnf/rocky/current")), "s1");

        // A new upstream version: the next snapshot has it, s1 keeps its own.
        std::fs::write(fx.state.join("version"), "2").unwrap();
        fx.mirrors.begin(&mirror, JobKind::Sync, Trigger::Manual, None).await.unwrap().execute().await;
        let run = fx.mirrors.begin(&mirror, JobKind::Snapshot(Some("s2".into())), Trigger::Manual, None).await.unwrap().execute().await;
        assert_eq!(run.state, RunState::Succeeded, "{}", run.output);
        assert!(s1.join(rpm).is_file());
        assert_eq!(std::fs::read_to_string(s1.join("repodata/repomd.xml")).unwrap(), "a-1-1.x86_64.rpm\n");
        assert!(fx.path("dnf/rocky/s2/Packages/a-2-1.x86_64.rpm").is_file());
        assert!(!fx.path("dnf/rocky/s2").join(rpm).exists());
        assert_eq!(link(&fx.path("dnf/rocky/current")), "s2");
        let latest = store::latest_published(&pool, mirror.id).await.unwrap().unwrap();
        assert_eq!(latest.name, "s2");
    }
}
