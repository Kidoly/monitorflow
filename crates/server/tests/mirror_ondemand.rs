//! On-demand syncs and snapshots (ADR-0142) against a fake upstream over
//! HTTP and a fake `gpg`: indexes only, checked and stored once, our own
//! signed `Release`, the package list, and retention without aptly.
//! Ignored without `DATABASE_URL`.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use monitorflow_common::config::MirrorConfig;
use monitorflow_common::mirror::origin::{Origin, ORIGIN};
use monitorflow_common::mirror::packages::{PackageList, PACKAGES};
use monitorflow_sensors::testing::write_executable;
use monitorflow_server::mirror::{
    retention, store, CommandRunner, Family, JobKind, Layout, Mirror, MirrorSpec, Mirrors, Mode,
    RunState, SnapshotState, Trigger,
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

/// gpg: one secret key; `--verify` reports a valid signature unless the
/// `bad-signature` switch is on; signing writes recognizable files.
const GPG: &str = r#"
prev=; for a; do [ "$prev" = --output ] && out="$a"; prev="$a"; last="$a"; done
case "$*" in
*--list-secret-keys*) printf 'sec:u:4096:1:X:1::::::scESC:\nfpr:::::::::FPR0123:\n' ;;
*--verify*)
    if [ -e @STATE@/bad-signature ]; then echo "[GNUPG:] NO_PUBKEY 0123"; exit 2; fi
    echo "[GNUPG:] GOODSIG 0123 Archive"; echo "[GNUPG:] VALIDSIG 0123 2026-11-16" ;;
*--clearsign*) { echo "-----BEGIN PGP SIGNED MESSAGE-----"; cat "$last"; } > "$out" ;;
*--detach-sign*) echo SIGNATURE > "$out" ;;
esac
"#;

fn sha256(data: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, data)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn write(path: &Path, data: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, data).unwrap();
}

/// The upstream's files: a Debian-like suite and a dnf repository.
fn fill_upstream(dir: &Path, version: u32) {
    let deb = format!("deb {version}");
    let packages = format!(
        "Package: a\nVersion: {version}.0-1\nArchitecture: amd64\nFilename: pool/main/a/a_{version}.0-1_amd64.deb\nSize: {}\nSHA256: {}\n\n\
         Package: b\nVersion: 2\nArchitecture: all\nFilename: pool/main/b/b_2_all.deb\nSize: 5\nSHA256: {}\n",
        deb.len(),
        sha256(deb.as_bytes()),
        sha256(b"deb b"),
    );
    let translations = "Package: a\nDescription-en: a\n";
    let suite = dir.join("debian/dists/bookworm");
    write(
        &suite.join("main/binary-amd64/Packages"),
        packages.as_bytes(),
    );
    write(
        &suite.join("main/i18n/Translation-en"),
        translations.as_bytes(),
    );
    write(&dir.join("debian/pool/main/b/b_2_all.deb"), b"deb b");
    let line = |data: &[u8], path: &str| format!(" {} {} {path}\n", sha256(data), data.len());
    let release = format!(
        "Origin: Debian\nLabel: Debian\nSuite: oldstable\nCodename: bookworm\n\
         Valid-Until: {}\nAcquire-By-Hash: yes\n\
         Architectures: amd64 arm64\nComponents: main\nSHA256:\n{}{}{}{}",
        (chrono::Utc::now() + chrono::TimeDelta::days(7)).format("%a, %d %b %Y %H:%M:%S UTC"),
        // Listed, not served: the next compression is taken.
        line(b"never served", "main/binary-amd64/Packages.xz"),
        line(packages.as_bytes(), "main/binary-amd64/Packages"),
        line(b"arm", "main/binary-arm64/Packages"),
        line(translations.as_bytes(), "main/i18n/Translation-en"),
    );
    write(&suite.join("Release"), release.as_bytes());
    write(&suite.join("Release.gpg"), b"UPSTREAM SIGNATURE");

    let primary = format!(
        r#"<?xml version="1.0"?>
<metadata packages="1"><package type="rpm"><name>c</name><arch>x86_64</arch>
<version epoch="0" ver="{version}" rel="1.el9"/>
<checksum type="sha256" pkgid="YES">{}</checksum><size package="5"/>
<location href="Packages/c/c-{version}-1.el9.x86_64.rpm"/></package></metadata>"#,
        sha256(b"rpm c")
    );
    let repomd = format!(
        r#"<?xml version="1.0"?>
<repomd><data type="primary"><checksum type="sha256">{}</checksum>
<location href="repodata/{version}-primary.xml"/><size>{}</size></data></repomd>"#,
        sha256(primary.as_bytes()),
        primary.len()
    );
    let rocky = dir.join("rocky");
    write(
        &rocky.join(format!("repodata/{version}-primary.xml")),
        primary.as_bytes(),
    );
    write(&rocky.join("repodata/repomd.xml"), repomd.as_bytes());
}

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    state: PathBuf,
    upstream: PathBuf,
    base: String,
    mirrors: Mirrors,
}

impl Fixture {
    async fn new(pool: &PgPool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let (root, state, bin, upstream) = (
            dir.path().join("mirrors"),
            dir.path().join("state"),
            dir.path().join("bin"),
            dir.path().join("upstream"),
        );
        for d in [&state, &bin, &upstream] {
            std::fs::create_dir(d).unwrap();
        }
        fill_upstream(&upstream, 1);
        let log = state.join("log");
        for (name, body) in [("gpg", GPG), ("aptly", "exit 0"), ("reposync", "exit 1")] {
            let body = body.replace("@STATE@", state.to_str().unwrap());
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
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let served = upstream.clone();
        tokio::spawn(async move {
            let app =
                axum::Router::new().fallback_service(tower_http::services::ServeDir::new(served));
            axum::serve(listener, app).await.unwrap();
        });
        let layout = Layout::new(&root);
        let runner = CommandRunner::new(&config, layout.gnupg());
        let mirrors = Mirrors::new(pool.clone(), Arc::new(runner), layout);
        Self {
            _dir: dir,
            root,
            state,
            upstream,
            base,
            mirrors,
        }
    }

    fn take_log(&self) -> Vec<String> {
        let path = self.state.join("log");
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let _ = std::fs::remove_file(path);
        text.lines().map(str::to_owned).collect()
    }

    async fn create(&self, pool: &PgPool, family: Family) -> Mirror {
        let apt = family == Family::Apt;
        let spec = MirrorSpec {
            name: if apt { "debian" } else { "rocky" }.into(),
            family,
            upstream_url: format!("{}/{}", self.base, if apt { "debian" } else { "rocky" }),
            distribution: if apt {
                "bookworm".into()
            } else {
                String::new()
            },
            components: if apt { vec!["main".into()] } else { vec![] },
            architectures: if apt { vec!["amd64".into()] } else { vec![] },
            sync_interval_s: None,
            auto_snapshot: true,
            mode: Mode::OnDemand,
        };
        store::create(pool, &spec, ("system", "test"))
            .await
            .unwrap()
    }

    async fn run(&self, mirror: &Mirror, kind: JobKind) -> monitorflow_server::mirror::Run {
        self.mirrors
            .begin(mirror, kind, Trigger::Manual, Some("test"))
            .await
            .unwrap()
            .execute()
            .await
    }

    /// Directory of the snapshot `current` names.
    fn current(&self, family: Family, name: &str) -> PathBuf {
        let served = match family {
            Family::Apt => self.root.join("aptly/public"),
            Family::Dnf => self.root.join("dnf"),
        };
        let dir = served.join(name);
        dir.join(std::fs::read_link(dir.join("current")).unwrap())
    }
}

db_tests! {
    #[sqlx::test]
    async fn an_apt_sync_downloads_the_indexes_only_and_signs_its_own_release(pool: PgPool) {
        let fx = Fixture::new(&pool).await;
        let mirror = fx.create(&pool, Family::Apt).await;
        let run = fx.run(&mirror, JobKind::Sync).await;
        assert_eq!(run.state, RunState::Succeeded, "{}", run.output);
        assert!(run.output.contains("2 index files downloaded"), "{}", run.output);
        assert!(run.output.contains("2 packages listed"), "{}", run.output);
        let log = fx.take_log();
        assert!(log[0].contains("--keyring") && log[0].contains("--verify"), "{log:?}");
        assert!(log.iter().all(|l| !l.starts_with("aptly")), "{log:?}");

        let snapshot = fx.current(Family::Apt, "debian");
        let dists = snapshot.join("dists/bookworm");
        let release = std::fs::read_to_string(dists.join("Release")).unwrap();
        assert!(release.starts_with("Origin: Debian\nLabel: Debian\nSuite: oldstable\nCodename: bookworm\n"));
        assert!(release.contains("Architectures: amd64\nComponents: main\n"), "{release}");
        assert!(!release.contains("Valid-Until") && !release.contains("By-Hash") && !release.contains("arm64"));
        assert!(release.contains(" main/binary-amd64/Packages\n") && !release.contains("Packages.xz"));
        assert!(std::fs::read_to_string(dists.join("InRelease")).unwrap().ends_with(&release));
        assert_eq!(std::fs::read_to_string(dists.join("Release.gpg")).unwrap(), "SIGNATURE\n");
        assert!(dists.join("main/i18n/Translation-en").is_file());
        // No package was downloaded, and nothing of the upstream's own.
        assert!(!snapshot.join("pool").exists());
        assert!(!snapshot.join(".upstream").exists());

        let list = PackageList::read(&snapshot.join(PACKAGES)).unwrap().unwrap();
        let a = list.get("pool/main/a/a_1.0-1_amd64.deb").unwrap();
        assert_eq!((a.name.as_str(), a.version.as_str(), a.arch.as_str()), ("a", "1.0-1", "amd64"));
        assert_eq!(list.len(), 2);
        let origin = Origin::read(&snapshot.join(ORIGIN)).unwrap().unwrap();
        assert_eq!(origin.upstream, mirror.upstream_url);

        let snapshots = store::snapshots(&pool, mirror.id, 10).await.unwrap();
        assert_eq!(snapshots.len(), 1);
        assert_eq!((snapshots[0].state, snapshots[0].mode), (SnapshotState::Published, Mode::OnDemand));

        // Unchanged upstream: nothing downloaded, the index stored once.
        next_second().await;
        let run = fx.run(&mirror, JobKind::Sync).await;
        assert_eq!(run.state, RunState::Succeeded, "{}", run.output);
        assert!(run.output.contains("0 index files downloaded"), "{}", run.output);
        assert!(run.output.contains("2 already stored"), "{}", run.output);
        let second = fx.current(Family::Apt, "debian");
        assert_ne!(second, snapshot);
        let ino = |p: &Path| std::fs::metadata(p).unwrap().ino();
        let packages = "dists/bookworm/main/binary-amd64/Packages";
        assert_eq!(ino(&snapshot.join(packages)), ino(&second.join(packages)));
    }

    #[sqlx::test]
    async fn a_release_without_a_valid_upstream_signature_publishes_nothing(pool: PgPool) {
        let fx = Fixture::new(&pool).await;
        let mirror = fx.create(&pool, Family::Apt).await;
        std::fs::write(fx.state.join("bad-signature"), "").unwrap();
        let run = fx.run(&mirror, JobKind::Sync).await;
        assert_eq!(run.state, RunState::Failed);
        assert!(run.error.unwrap().contains("exit"), "gpg's failure is the error");
        assert!(store::snapshots(&pool, mirror.id, 10).await.unwrap().is_empty());
        assert!(!fx.root.join("ondemand-work/debian").exists());
    }

    #[sqlx::test]
    async fn a_signed_release_of_another_suite_or_expired_publishes_nothing(pool: PgPool) {
        let fx = Fixture::new(&pool).await;
        let mirror = fx.create(&pool, Family::Apt).await;
        let path = fx.upstream.join("debian/dists/bookworm/Release");
        let release = std::fs::read_to_string(&path).unwrap();
        let replay = |from: &str, to: &str| {
            assert!(release.contains(from));
            std::fs::write(&path, release.replacen(from, to, 1)).unwrap();
        };
        // Another suite of the same archive, signed by the same key.
        replay("Codename: bookworm\n", "Codename: bookworm-proposed-updates\n");
        let run = fx.run(&mirror, JobKind::Sync).await;
        assert_eq!(run.state, RunState::Failed);
        assert!(run.error.unwrap().contains("not of `bookworm`"));
        // An old copy, past its Valid-Until.
        let until = release.lines().find(|l| l.starts_with("Valid-Until: ")).unwrap();
        replay(until, "Valid-Until: Sun, 15 Nov 2026 11:43:19 UTC");
        let run = fx.run(&mirror, JobKind::Sync).await;
        assert_eq!(run.state, RunState::Failed);
        assert!(run.error.unwrap().contains("expired on"));
        assert!(store::snapshots(&pool, mirror.id, 10).await.unwrap().is_empty());
        assert!(!fx.root.join("ondemand-work/debian").exists());
    }

    #[sqlx::test]
    async fn an_index_that_does_not_match_release_fails_the_sync(pool: PgPool) {
        let fx = Fixture::new(&pool).await;
        let mirror = fx.create(&pool, Family::Apt).await;
        let packages = fx.upstream.join("debian/dists/bookworm/main/binary-amd64/Packages");
        let mut tampered = std::fs::read(&packages).unwrap();
        tampered[0] = b'X';
        std::fs::write(&packages, tampered).unwrap();
        let run = fx.run(&mirror, JobKind::Sync).await;
        assert_eq!(run.state, RunState::Failed);
        assert!(run.error.unwrap().contains("does not match"));
        assert!(store::snapshots(&pool, mirror.id, 10).await.unwrap().is_empty());
    }

    #[sqlx::test]
    async fn a_dnf_sync_keeps_repomd_and_signs_it(pool: PgPool) {
        let fx = Fixture::new(&pool).await;
        let mirror = fx.create(&pool, Family::Dnf).await;
        let run = fx.run(&mirror, JobKind::Sync).await;
        assert_eq!(run.state, RunState::Succeeded, "{}", run.output);
        let snapshot = fx.current(Family::Dnf, "rocky");
        assert_eq!(
            std::fs::read(snapshot.join("repodata/repomd.xml")).unwrap(),
            std::fs::read(fx.upstream.join("rocky/repodata/repomd.xml")).unwrap()
        );
        assert!(snapshot.join("repodata/repomd.xml.asc").is_file());
        assert!(snapshot.join("repodata/1-primary.xml").is_file());
        let list = PackageList::read(&snapshot.join(PACKAGES)).unwrap().unwrap();
        let c = list.get("Packages/c/c-1-1.el9.x86_64.rpm").unwrap();
        assert_eq!((c.name.as_str(), c.version.as_str()), ("c", "1-1.el9"));
        assert!(fx.take_log().iter().all(|l| !l.starts_with("reposync")));
    }

    #[sqlx::test]
    async fn retention_removes_on_demand_snapshots_without_aptly(pool: PgPool) {
        let fx = Fixture::new(&pool).await;
        let mirror = fx.create(&pool, Family::Apt).await;
        assert_eq!(fx.run(&mirror, JobKind::Sync).await.state, RunState::Succeeded);
        let first = fx.current(Family::Apt, "debian");
        let packages = "dists/bookworm/main/binary-amd64/Packages";
        let old = std::fs::metadata(first.join(packages)).unwrap().ino();
        // A new upstream state, whose index files differ from the first,
        // then enough snapshots of it for the first to go.
        fill_upstream(&fx.upstream, 2);
        next_second().await;
        assert_eq!(fx.run(&mirror, JobKind::Sync).await.state, RunState::Succeeded);
        for i in 1..retention::KEEP_SNAPSHOTS {
            let run = fx.run(&mirror, JobKind::Snapshot(Some(format!("s{i}")))).await;
            assert_eq!(run.state, RunState::Succeeded, "{}", run.output);
        }
        let stored = |ino| {
            walk(&fx.root.join("ondemand/index"))
                .iter()
                .any(|p| std::fs::metadata(p).unwrap().ino() == ino)
        };
        assert!(stored(old));
        fx.take_log();
        assert_eq!(retention::prune(&fx.mirrors).await.unwrap(), 1);
        assert!(!first.exists());
        assert!(fx.take_log().iter().all(|l| !l.starts_with("aptly")));
        // Only the removed snapshot linked the first state's index.
        assert!(!stored(old));
        assert!(fx.current(Family::Apt, "debian").join(packages).is_file());
    }

    #[sqlx::test]
    async fn switching_to_on_demand_drops_the_full_copy_at_the_next_pass(pool: PgPool) {
        let fx = Fixture::new(&pool).await;
        let mirror = fx.create(&pool, Family::Apt).await;
        // What full syncs left: two published full snapshots.
        for (name, days) in [("f1", 2), ("f2", 1)] {
            sqlx::query(
                "INSERT INTO mirror_snapshots (mirror_id, name, state, mode, created_at, published_at)
                 VALUES ($1, $2, 'published', 'full', now() - make_interval(days => $3),
                         now() - make_interval(days => $3))",
            )
            .bind(mirror.id)
            .bind(name)
            .bind(days)
            .execute(&pool)
            .await
            .unwrap();
            std::fs::create_dir_all(fx.root.join("aptly/public/debian").join(name).join("pool")).unwrap();
        }
        let switched = store::set_mode(&pool, &Mirror { mode: Mode::Full, ..mirror.clone() }, Mode::OnDemand, ("user", "u1"))
            .await
            .unwrap();
        assert_eq!(switched.mode, Mode::OnDemand);
        let audited: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = 'mirror.mode'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(audited, 1);

        // Until an on-demand snapshot exists, the newest full one is
        // current and stays.
        fx.take_log();
        assert_eq!(retention::prune(&fx.mirrors).await.unwrap(), 1);
        assert!(!fx.root.join("aptly/public/debian/f1").exists());
        assert!(fx.root.join("aptly/public/debian/f2").exists());

        // After the first on-demand sync, the last full one goes too, with
        // aptly's mirrors and the packages only they held.
        assert_eq!(fx.run(&switched, JobKind::Sync).await.state, RunState::Succeeded);
        fx.take_log();
        assert_eq!(retention::prune(&fx.mirrors).await.unwrap(), 1);
        assert!(!fx.root.join("aptly/public/debian/f2").exists());
        let log = fx.take_log();
        assert!(log.iter().any(|l| l.starts_with("aptly mirror list")), "{log:?}");
        assert!(log.iter().any(|l| l.starts_with("aptly db cleanup")), "{log:?}");
        // The on-demand snapshot is served.
        assert!(fx.current(Family::Apt, "debian").join(PACKAGES).is_file());
    }
}

/// Snapshots are named after the second they are made in.
async fn next_second() {
    let into = chrono::Utc::now().timestamp_subsec_millis();
    tokio::time::sleep(std::time::Duration::from_millis(1001 - u64::from(into))).await;
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(walk(&path));
        } else {
            found.push(path);
        }
    }
    found
}
