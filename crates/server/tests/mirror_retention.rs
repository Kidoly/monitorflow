//! Retention of mirror snapshots (ADR-0074): disk space really comes
//! back, what is in use stays, and a pass waits for running jobs. dnf
//! snapshots here are real hardlinked trees; `deploy/smoke-test.sh` proves
//! the same for apt with the real aptly. Ignored without `DATABASE_URL`.

use std::collections::HashSet;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use monitorflow_common::config::MirrorConfig;
use monitorflow_common::mirror::{Family as IndexFamily, Index, RemovedMirror, Served};
use monitorflow_common::proto::v1::configure_sources::{AptRepository, DnfRepository};
use monitorflow_common::proto::v1::{ConfigureSources, RevertSources};
use monitorflow_sensors::testing::write_executable;
use monitorflow_server::commands::{CommandStore, NewCommand};
use monitorflow_server::mirror::jobs::STOPPED;
use monitorflow_server::mirror::removal::{self, RemovalError};
use monitorflow_server::mirror::retention::{self, KEEP_SNAPSHOTS};
use monitorflow_server::mirror::{
    store, CommandRunner, Family, JobKind, Layout, Mirror, MirrorLock, MirrorSpec, Mirrors,
    SnapshotState, Trigger,
};
use prost::Message;
use sqlx::PgPool;
use uuid::Uuid;

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

/// Size of a fake package: enough to see on a real disk.
const PACKAGE_BYTES: u64 = 1 << 20;

/// Keeps its objects as lines of files, like aptly its database: what
/// `snapshot create` froze stays in `pool/` until `db cleanup` finds nothing
/// that refers to it.
const APTLY: &str = r#"
state=@STATE@
touch "$state/mirrors" "$state/snapshots" "$state/published"
[ -e "$state/fail-$1-$2" ] && { echo "ERROR: $1 $2 failed" >&2; exit 1; }
name() { for a; do case "$a" in -*) ;; *) echo "$a"; return ;; esac; done; }
case "$1 $2" in
"mirror list") cat "$state/mirrors" ;;
"mirror create") shift 2; name "$@" >> "$state/mirrors" ;;
"mirror drop") shift 2; n=$(name "$@"); sed -i "\|^$n\$|d" "$state/mirrors"; : > "$state/mirror-holds" ;;
"mirror update") v=$(cat "$state/version"); mkdir -p "@ROOT@/aptly/pool"
    head -c @BYTES@ /dev/zero | tr '\0' "$((v % 10))" > "@ROOT@/aptly/pool/pkg-$v.deb"
    echo "$v" > "$state/mirror-holds" ;;
"snapshot create") shift 2; n=$(name "$@")
    echo "$n $(cat "$state/mirror-holds")" >> "$state/snapshots" ;;
"snapshot list") cut -d' ' -f1 "$state/snapshots" ;;
"snapshot drop") shift 2; n=$(name "$@")
    grep -q "^$n " "$state/snapshots" || { echo "ERROR: not found" >&2; exit 1; }
    sed -i "\| *$n |d;/^$n /d" "$state/snapshots" ;;
"publish snapshot") for a; do prefix="$a"; done
    mkdir -p "@ROOT@/aptly/public/$prefix/dists"; echo "$prefix bookworm" >> "$state/published" ;;
"publish list") cat "$state/published" ;;
"publish drop") for a; do prefix="$a"; done
    grep -q "^$prefix " "$state/published" || { echo "ERROR: not found" >&2; exit 1; }
    sed -i "\|^$prefix |d" "$state/published"; rm -rf "@ROOT@/aptly/public/$prefix" ;;
"db cleanup") for f in "@ROOT@"/aptly/pool/*; do
        v=${f##*pkg-}; v=${v%.deb}
        { grep -q " $v\$" "$state/snapshots" || [ "$v" = "$(cat "$state/mirror-holds")" ]; } || rm -f "$f"
    done ;;
*) echo "unexpected: $*" >&2; exit 2 ;;
esac
"#;

const GPG: &str = r#"
case "$*" in
*--list-secret-keys*) printf 'sec:u:4096:1:X:1::::::scESC:\nfpr:::::::::FPR0123:\n' ;;
*--detach-sign*) prev=; for a; do [ "$prev" = --output ] && out="$a"; prev="$a"; done
    echo SIGNATURE > "$out" ;;
esac
"#;

/// What `reposync --delete` does: the working tree holds one version.
const REPOSYNC: &str = r#"
[ -e @STATE@/hang ] && { touch @STATE@/hanging; sleep 25; }
for a; do case "$a" in --download-path=*) dir="${a#--download-path=}" ;; esac; done
v=$(cat @STATE@/version); mkdir -p "$dir/Packages"; rm -f "$dir"/Packages/*
head -c @BYTES@ /dev/zero | tr '\0' "$((v % 10))" > "$dir/Packages/a-$v-1.x86_64.rpm"
"#;

const CREATEREPO: &str = r#"
for a; do dir="$a"; done
rm -rf "$dir/repodata"; mkdir "$dir/repodata"; ls "$dir/Packages" > "$dir/repodata/repomd.xml"
"#;

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
        std::fs::write(state.join("version"), "0").unwrap();
        for (name, body) in [
            ("aptly", APTLY),
            ("gpg", GPG),
            ("reposync", REPOSYNC),
            ("createrepo_c", CREATEREPO),
        ] {
            let body = body
                .replace("@STATE@", state.to_str().unwrap())
                .replace("@ROOT@", root.to_str().unwrap())
                .replace("@BYTES@", &PACKAGE_BYTES.to_string());
            write_executable(&bin.join(name), &format!("#!/bin/sh\n{body}"), 0o755);
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
        layout.prepare().unwrap();
        let runner = CommandRunner::new(&config, layout.gnupg());
        Self {
            mirrors: Mirrors::new(pool.clone(), Arc::new(runner), layout),
            _dir: dir,
            root,
            state,
        }
    }

    /// Syncs `count` new versions of the upstream, a snapshot `s<version>`
    /// after each.
    async fn snapshots(&self, mirror: &Mirror, versions: std::ops::Range<u32>) {
        for version in versions {
            std::fs::write(self.state.join("version"), version.to_string()).unwrap();
            let sync = self
                .mirrors
                .begin(mirror, JobKind::Sync, Trigger::Manual, None)
                .await
                .unwrap()
                .execute()
                .await;
            assert_eq!(sync.error, None);
            let name = format!("s{version:02}");
            let snapshot = self
                .mirrors
                .begin(mirror, JobKind::Snapshot(Some(name)), Trigger::Manual, None)
                .await
                .unwrap()
                .execute()
                .await;
            assert_eq!(snapshot.error, None, "{}", snapshot.output);
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

    fn served(&self, mirror: &Mirror, snapshot: &str) -> PathBuf {
        self.mirrors.layout().mirror_dir(mirror).join(snapshot)
    }
}

/// Bytes the files under `dir` take on disk: each inode once, by its
/// allocated blocks, so hardlinks count as what they cost.
fn disk_use(dir: &Path) -> u64 {
    fn walk(dir: &Path, seen: &mut HashSet<(u64, u64)>, total: &mut u64) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let meta = entry.path().symlink_metadata().unwrap();
            if meta.is_dir() {
                walk(&entry.path(), seen, total);
            } else if seen.insert((meta.dev(), meta.ino())) {
                *total += meta.blocks() * 512;
            }
        }
    }
    let mut total = 0;
    walk(dir, &mut HashSet::new(), &mut total);
    total
}

async fn create(pool: &PgPool, family: Family) -> Mirror {
    create_named(pool, "m", family).await
}

async fn create_named(pool: &PgPool, name: &str, family: Family) -> Mirror {
    let spec = MirrorSpec {
        name: name.into(),
        family,
        upstream_url: "https://up.example/os".into(),
        distribution: if family == Family::Apt {
            "bookworm".into()
        } else {
            String::new()
        },
        components: if family == Family::Apt {
            vec!["main".into()]
        } else {
            Vec::new()
        },
        architectures: vec!["amd64".into()],
        sync_interval_s: None,
        auto_snapshot: false,
    };
    store::create(pool, &spec, ("system", "test"))
        .await
        .unwrap()
}

async fn states(pool: &PgPool, mirror: &Mirror) -> Vec<(String, SnapshotState)> {
    sqlx::query_as("SELECT name, state FROM mirror_snapshots WHERE mirror_id = $1 ORDER BY name")
        .bind(mirror.id)
        .fetch_all(pool)
        .await
        .unwrap()
}

/// Names of the snapshots of `mirror` still on the way or on disk.
async fn left(pool: &PgPool, mirror: &Mirror) -> Vec<String> {
    let snapshots = store::snapshots(pool, mirror.id, 1000).await.unwrap();
    let mut names: Vec<String> = snapshots.into_iter().map(|s| s.name).collect();
    names.sort();
    names
}

/// A server of a new site, with a source command in `state`.
struct Host {
    server: Uuid,
    site: Uuid,
}

async fn host(pool: &PgPool, peer: &str) -> Host {
    let id = |sql: &'static str| sqlx::query_scalar::<_, Uuid>(sql);
    let customer = id("INSERT INTO customers (name) VALUES ($1) RETURNING id")
        .bind(peer)
        .fetch_one(pool)
        .await
        .unwrap();
    let site = id("INSERT INTO sites (name, customer_id) VALUES ($1, $2) RETURNING id")
        .bind(peer)
        .bind(customer)
        .fetch_one(pool)
        .await
        .unwrap();
    let server = id("INSERT INTO servers (peer_id, site_id, hostname, os_family)
         VALUES ($1, $2, $1, 'debian') RETURNING id")
    .bind(peer)
    .bind(site)
    .fetch_one(pool)
    .await
    .unwrap();
    Host { server, site }
}

async fn command(
    pool: &PgPool,
    server: Uuid,
    kind: &str,
    payload: Vec<u8>,
    state: &str,
    age_s: i32,
) -> Uuid {
    let new = NewCommand {
        server_id: server,
        kind: kind.into(),
        payload,
        ttl: Duration::from_secs(3600),
        requested_by: None,
        campaign_id: None,
    };
    let row = CommandStore::enqueue_in(pool, &new).await.unwrap();
    sqlx::query(
        "UPDATE commands SET state = $2,
             finished_at = CASE WHEN $2 <> 'queued' THEN now() - make_interval(secs => $3) END
         WHERE id = $1",
    )
    .bind(row.id)
    .bind(state)
    .bind(age_s)
    .execute(pool)
    .await
    .unwrap();
    row.id
}

fn pointed_at(mirror: &str, snapshot: &str) -> Vec<u8> {
    ConfigureSources {
        apt: vec![AptRepository {
            mirror: mirror.into(),
            snapshot: snapshot.into(),
            ..AptRepository::default()
        }],
        ..ConfigureSources::default()
    }
    .encode_to_vec()
}

fn pointed_at_dnf(mirror: &str, snapshot: &str) -> Vec<u8> {
    ConfigureSources {
        dnf: vec![DnfRepository {
            mirror: mirror.into(),
            snapshot: snapshot.into(),
            ..DnfRepository::default()
        }],
        ..ConfigureSources::default()
    }
    .encode_to_vec()
}

const CONFIGURE: &str = "configure_sources";
const REVERT: &str = "revert_sources";

db_tests! {
    #[sqlx::test(migrations = "./migrations")]
    async fn removing_dnf_snapshots_gives_the_disk_back(pool: PgPool) {
        let fixture = Fixture::new(&pool);
        let mirror = create(&pool, Family::Dnf).await;
        let total = KEEP_SNAPSHOTS as u32 + 3;
        fixture.snapshots(&mirror, 0..total).await;
        let oldest = fixture.served(&mirror, "s00");
        assert!(oldest.join("Packages/a-0-1.x86_64.rpm").is_file());
        let before = disk_use(&fixture.root);

        assert_eq!(retention::prune(&fixture.mirrors).await.unwrap(), 3);

        // Three packages only those snapshots held.
        let freed = before - disk_use(&fixture.root);
        assert!(freed >= 3 * PACKAGE_BYTES, "freed {freed} bytes");
        for gone in ["s00", "s01", "s02"] {
            assert!(!fixture.served(&mirror, gone).exists());
        }
        // Nothing the others need went with them.
        let newest = format!("s{:02}", total - 1);
        let latest = format!("Packages/a-{}-1.x86_64.rpm", total - 1);
        assert!(fixture.served(&mirror, &newest).join(&latest).is_file());
        assert!(fixture.served(&mirror, "s03").join("Packages/a-3-1.x86_64.rpm").is_file());
        assert!(fixture.served(&mirror, "current").join(&latest).is_file());
        assert!(fixture.mirrors.layout().dnf_work(&mirror).join(&latest).is_file());

        assert_eq!(left(&pool, &mirror).await.len(), KEEP_SNAPSHOTS);
        let all = states(&pool, &mirror).await;
        assert_eq!(all[0], ("s00".to_owned(), SnapshotState::Pruned));
        assert_eq!(all[3], ("s03".to_owned(), SnapshotState::Published));
        let audited: serde_json::Value = sqlx::query_scalar(
            "SELECT details FROM audit_log WHERE action = 'mirror.prune'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(audited["snapshots"].as_array().unwrap().len(), 3);

        // Nothing left to do, and a removed name can be used again.
        assert_eq!(retention::prune(&fixture.mirrors).await.unwrap(), 0);
        let run = fixture
            .mirrors
            .begin(&mirror, JobKind::Snapshot(Some("s00".into())), Trigger::Manual, None)
            .await
            .unwrap()
            .execute()
            .await;
        assert_eq!(run.error, None);
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn removing_apt_snapshots_frees_only_the_packages_nothing_holds(pool: PgPool) {
        let fixture = Fixture::new(&pool);
        let mirror = create(&pool, Family::Apt).await;
        fixture.snapshots(&mirror, 0..KEEP_SNAPSHOTS as u32 + 2).await;
        let pool_dir = fixture.mirrors.layout().aptly_root().join("pool");
        let packages = || std::fs::read_dir(&pool_dir).unwrap().count();
        assert_eq!(packages(), KEEP_SNAPSHOTS + 2);

        // A cleanup that fails leaves them to the next pass, with the reason.
        fixture.switch("fail-db-cleanup", true);
        let error = retention::prune(&fixture.mirrors).await.unwrap_err();
        assert!(error.to_string().contains("db cleanup"), "{error}");
        assert_eq!(packages(), KEEP_SNAPSHOTS + 2);
        let stuck = store::snapshots(&pool, mirror.id, 1000).await.unwrap();
        let stuck: Vec<_> = stuck.iter().filter(|s| s.state == SnapshotState::Pruning).collect();
        assert_eq!(stuck.len(), 2);
        assert!(stuck[0].error.as_deref().unwrap().contains("db cleanup"));

        // The second pass finds the objects gone and finishes all the same.
        fixture.switch("fail-db-cleanup", false);
        assert_eq!(retention::prune(&fixture.mirrors).await.unwrap(), 2);
        assert_eq!(packages(), KEEP_SNAPSHOTS);
        assert!(!pool_dir.join("pkg-0.deb").exists());
        assert!(!pool_dir.join("pkg-1.deb").exists());
        assert!(pool_dir.join("pkg-2.deb").exists());
        assert!(!fixture.served(&mirror, "s00").exists());
        assert!(fixture.served(&mirror, "s02").is_dir());
        let all = states(&pool, &mirror).await;
        assert_eq!(all[1], ("s01".to_owned(), SnapshotState::Pruned));
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn a_snapshot_aptly_still_holds_is_not_taken_for_removed(pool: PgPool) {
        let fixture = Fixture::new(&pool);
        let mirror = create(&pool, Family::Apt).await;
        fixture.snapshots(&mirror, 0..KEEP_SNAPSHOTS as u32 + 1).await;
        fixture.switch("fail-snapshot-drop", true);
        let error = retention::prune(&fixture.mirrors).await.unwrap_err();
        assert!(error.to_string().contains("m/s00"), "{error}");
        assert_eq!(states(&pool, &mirror).await[0].1, SnapshotState::Pruning);
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn what_is_in_use_stays(pool: PgPool) {
        let fixture = Fixture::new(&pool);
        let mirror = create(&pool, Family::Dnf).await;
        fixture.snapshots(&mirror, 0..KEEP_SNAPSHOTS as u32 + 9).await;
        let snapshot_id = |name: &'static str| {
            sqlx::query_scalar::<_, Uuid>(
                "SELECT id FROM mirror_snapshots WHERE mirror_id = $1 AND name = $2",
            )
            .bind(mirror.id)
            .bind(name)
            .fetch_one(&pool)
        };

        // A campaign not finished keeps its snapshot, a finished one not.
        let customer: Uuid = sqlx::query_scalar("INSERT INTO customers (name) VALUES ('acme') RETURNING id")
            .fetch_one(&pool)
            .await
            .unwrap();
        for (name, state, snapshot) in [
            ("active", "draft", "s00"),
            ("done", "completed", "s01"),
        ] {
            let campaign: Uuid = sqlx::query_scalar(
                "INSERT INTO campaigns (customer_id, name, state, sources_mode, started_at, finished_at)
                 VALUES ($1, $2, $3, 'replace', now(),
                         CASE WHEN $3 = 'completed' THEN now() END) RETURNING id",
            )
            .bind(customer)
            .bind(name)
            .bind(state)
            .fetch_one(&pool)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO campaign_snapshots
                     (campaign_id, mirror_id, snapshot_id, mirror_name, snapshot_name)
                 VALUES ($1, $2, $3, $4, $5)",
            )
            .bind(campaign)
            .bind(mirror.id)
            .bind(snapshot_id(snapshot).await.unwrap())
            .bind(&mirror.name)
            .bind(snapshot)
            .execute(&pool)
            .await
            .unwrap();
        }

        // A relay's `current`, unless the relay was removed.
        let served = host(&pool, "relayed").await;
        for (peer, revoked, snapshot) in [("r1", false, "s02"), ("r2", true, "s03")] {
            let relay: Uuid = sqlx::query_scalar(
                "INSERT INTO relays (peer_id, site_id, name, revoked_at)
                 VALUES ($1, $2, $1, CASE WHEN $3 THEN now() END) RETURNING id",
            )
            .bind(peer)
            .bind(served.site)
            .bind(revoked)
            .fetch_one(&pool)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO relay_mirrors (relay_id, family, name, current, snapshots)
                 VALUES ($1, 'dnf', 'm', $2, '{}')",
            )
            .bind(relay)
            .bind(snapshot)
            .execute(&pool)
            .await
            .unwrap();
        }

        // A server pointed at a snapshot; one that left it; one on its way.
        let a = host(&pool, "a").await.server;
        command(&pool, a, CONFIGURE, pointed_at_dnf("m", "s04"), "succeeded", 100).await;
        // A failure afterwards may have left that alone.
        command(&pool, a, CONFIGURE, pointed_at_dnf("m", "s09"), "failed", 50).await;
        let b = host(&pool, "b").await.server;
        command(&pool, b, CONFIGURE, pointed_at_dnf("m", "s05"), "succeeded", 100).await;
        command(&pool, b, REVERT, RevertSources {}.encode_to_vec(), "succeeded", 50).await;
        let c = host(&pool, "c").await.server;
        command(&pool, c, CONFIGURE, pointed_at_dnf("m", "s06"), "queued", 0).await;

        // Another mirror's snapshot of the same name is no reason, and
        // `current` names none.
        let d = host(&pool, "d").await.server;
        command(&pool, d, CONFIGURE, pointed_at("other", "s07"), "succeeded", 10).await;
        command(&pool, d, CONFIGURE, pointed_at_dnf("m", "current"), "succeeded", 5).await;

        // A server a campaign holds goes back to its snapshot afterwards.
        let e = host(&pool, "e").await.server;
        command(&pool, e, CONFIGURE, pointed_at_dnf("m", "s08"), "succeeded", 100).await;
        let campaign_step = command(&pool, e, CONFIGURE, pointed_at_dnf("m", "s15"), "succeeded", 50).await;
        let campaign: Uuid = sqlx::query_scalar(
            "INSERT INTO campaigns (customer_id, name, state, sources_mode, started_at, finished_at)
             VALUES ($1, 'step', 'completed', 'replace', now(), now()) RETURNING id",
        )
        .bind(customer)
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query("UPDATE commands SET campaign_id = $1 WHERE id = $2")
            .bind(campaign)
            .bind(campaign_step)
            .execute(&pool)
            .await
            .unwrap();

        // The newest seven are s09 to s15.
        assert_eq!(retention::prune(&fixture.mirrors).await.unwrap(), 4);
        assert_eq!(
            left(&pool, &mirror).await,
            ["s00", "s02", "s04", "s06", "s08", "s09", "s10", "s11", "s12", "s13", "s14", "s15"]
        );
        for kept in ["s00", "s02", "s04", "s06", "s08"] {
            assert!(fixture.served(&mirror, kept).is_dir(), "{kept}");
        }
        assert!(!fixture.served(&mirror, "s01").exists());
        // A finished campaign still shows what it pinned.
        assert_eq!(
            states(&pool, &mirror).await[1],
            ("s01".to_owned(), SnapshotState::Pruned)
        );

        // The mirror's page can say why.
        let reasons = retention::kept(&pool, Some(fixture.mirrors.layout()), &mirror)
            .await
            .unwrap();
        let s00 = snapshot_id("s00").await.unwrap();
        assert_eq!(serde_json::to_value(reasons[&s00]).unwrap(), "campaign");
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn an_unreadable_command_stops_the_pass(pool: PgPool) {
        let fixture = Fixture::new(&pool);
        let mirror = create(&pool, Family::Dnf).await;
        fixture.snapshots(&mirror, 0..KEEP_SNAPSHOTS as u32 + 1).await;
        let server = host(&pool, "a").await.server;
        command(&pool, server, CONFIGURE, vec![0xff, 0xff], "succeeded", 0).await;
        assert!(retention::prune(&fixture.mirrors).await.is_err());
        assert!(fixture.served(&mirror, "s00").is_dir());
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn a_pass_waits_for_running_jobs_and_holds_back_new_ones(pool: PgPool) {
        let fixture = Fixture::new(&pool);
        let mirror = create(&pool, Family::Dnf).await;
        let spec = MirrorSpec {
            name: "other".into(),
            ..MirrorSpec {
                name: String::new(),
                family: Family::Dnf,
                upstream_url: "https://up.example/other".into(),
                distribution: String::new(),
                components: Vec::new(),
                architectures: Vec::new(),
                sync_interval_s: None,
                auto_snapshot: false,
            }
        };
        let other = store::create(&pool, &spec, ("system", "test")).await.unwrap();
        fixture.snapshots(&mirror, 0..KEEP_SNAPSHOTS as u32 + 1).await;

        let running = MirrorLock::try_acquire(&pool, other.id).await.unwrap().unwrap();
        let mirrors = fixture.mirrors.clone();
        let pass = tokio::spawn(async move { retention::prune(&mirrors).await });
        let waiting = || async {
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM pg_locks l JOIN pg_database d ON d.oid = l.database
                 WHERE l.locktype = 'advisory' AND NOT l.granted AND d.datname = current_database()")
                .fetch_one(&pool)
                .await
                .unwrap()
        };
        tokio::time::timeout(Duration::from_secs(10), async {
            while waiting().await == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the pass waits for the running job");
        assert!(fixture.served(&mirror, "s00").is_dir());
        assert!(
            MirrorLock::try_acquire(&pool, mirror.id).await.unwrap().is_none(),
            "no new job while a pass waits"
        );

        running.release().await;
        assert_eq!(pass.await.unwrap().unwrap(), 1);
        assert!(!fixture.served(&mirror, "s00").exists());
        assert!(MirrorLock::try_acquire(&pool, mirror.id).await.unwrap().is_some());
    }

    #[sqlx::test]
    async fn deleting_an_apt_mirror_leaves_nothing_and_frees_the_disk(pool: PgPool) {
        let f = Fixture::new(&pool);
        let gone = create_named(&pool, "gone", Family::Apt).await;
        let other = create_named(&pool, "gone-too-not", Family::Apt).await;
        f.snapshots(&gone, 1..3).await;
        f.snapshots(&other, 3..4).await;
        let before = disk_use(&f.root);

        removal::request(&pool, gone.id, ("user", "someone")).await.unwrap();
        // Nothing new can use it from here on.
        assert_eq!(store::by_name(&pool, "gone").await.unwrap(), None);
        assert_eq!(
            store::latest_published(&pool, gone.id).await.unwrap(),
            None
        );
        retention::prune(&f.mirrors).await.unwrap();

        assert_eq!(store::get(&pool, gone.id).await.unwrap(), None);
        let runs: i64 = sqlx::query_scalar("SELECT count(*) FROM mirror_runs WHERE mirror_id = $1")
            .bind(gone.id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(runs, 0);
        assert!(states(&pool, &gone).await.is_empty());
        let layout = f.mirrors.layout();
        assert!(!layout.mirror_dir(&gone).exists());
        for object in ["mirrors", "snapshots", "published"] {
            let listed = std::fs::read_to_string(f.state.join(object)).unwrap();
            assert!(!listed.contains("gone/") && !listed.contains("mf:gone:"), "{listed}");
            assert!(listed.contains("gone-too-not"), "{listed}");
        }
        // Only the other mirror's package is left.
        assert_eq!(std::fs::read_dir(f.root.join("aptly/pool")).unwrap().count(), 1);
        assert!(f.served(&other, "s03").is_dir());
        assert!(disk_use(&f.root) < before - PACKAGE_BYTES);

        let actions: Vec<(String, String)> = sqlx::query_as(
            "SELECT actor_kind, action FROM audit_log
             WHERE target_kind = 'mirror' AND target_id = $1 AND action LIKE 'mirror.delete%'
             ORDER BY id",
        )
        .bind(gone.id.to_string())
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            actions,
            [
                ("user".to_owned(), "mirror.delete".to_owned()),
                ("system".to_owned(), "mirror.deleted".to_owned()),
            ]
        );
    }

    #[sqlx::test]
    async fn relays_learn_of_a_deletion_until_the_name_is_published_again(pool: PgPool) {
        let f = Fixture::new(&pool);
        let mirror = create_named(&pool, "rocky", Family::Dnf).await;
        f.snapshots(&mirror, 1..3).await;
        let layout = f.mirrors.layout();
        let served = Served {
            base: layout.root().to_owned(),
            apt: layout.served(Family::Apt),
            dnf: layout.served(Family::Dnf),
            public_key: layout.public_key(),
            agent: f.root.join("agent"),
        };
        let cache = layout.dnf_cache();
        std::fs::create_dir_all(cache.join("mf-rocky-0123456789abcdef")).unwrap();
        std::fs::create_dir_all(cache.join("mf-rocky-9-0123456789abcdef")).unwrap();
        assert_eq!(Index::scan(&served).unwrap().mirrors.len(), 1);
        assert!(Index::scan(&served).unwrap().removed.is_empty());
        let before = disk_use(&f.root);

        removal::request(&pool, mirror.id, ("user", "someone")).await.unwrap();
        retention::prune(&f.mirrors).await.unwrap();

        let index = Index::scan(&served).unwrap();
        assert!(index.mirrors.is_empty());
        assert_eq!(
            index.removed,
            [RemovedMirror { family: IndexFamily::Dnf, name: "rocky".into() }]
        );
        assert!(!layout.dnf_work(&mirror).exists());
        assert!(!cache.join("mf-rocky-0123456789abcdef").exists());
        assert!(cache.join("mf-rocky-9-0123456789abcdef").exists(), "another mirror's");
        assert!(disk_use(&f.root) < before - PACKAGE_BYTES);

        // Added again: it is announced as removed until it is published.
        let again = create_named(&pool, "rocky", Family::Dnf).await;
        assert_eq!(Index::scan(&served).unwrap().removed.len(), 1);
        f.snapshots(&again, 3..4).await;
        let index = Index::scan(&served).unwrap();
        assert!(index.removed.is_empty());
        assert_eq!(index.mirrors[0].current, "s03");
    }

    #[sqlx::test]
    async fn what_uses_a_mirror_refuses_its_deletion(pool: PgPool) {
        let f = Fixture::new(&pool);
        let mirror = create(&pool, Family::Apt).await;
        f.snapshots(&mirror, 1..3).await;
        let refused = |text: &str, error: RemovalError| match error {
            RemovalError::InUse(sentence) => assert_eq!(sentence, text),
            other => panic!("{other:?}"),
        };

        // A server following `current` holds no snapshot, but it uses the mirror.
        let web = host(&pool, "web-01").await;
        command(&pool, web.server, CONFIGURE, pointed_at("m", "current"), "succeeded", 0).await;
        // A later failure changed nothing.
        command(&pool, web.server, REVERT, RevertSources {}.encode_to_vec(), "failed", -5).await;
        // A command on its way.
        let queued = host(&pool, "web-02").await;
        command(&pool, queued.server, CONFIGURE, pointed_at("m", "s01"), "queued", 0).await;
        // Another mirror's user, and a removed server, do not count.
        let elsewhere = host(&pool, "web-03").await;
        command(&pool, elsewhere.server, CONFIGURE, pointed_at("m-updates", "current"), "succeeded", 0).await;
        let removed = host(&pool, "old").await;
        command(&pool, removed.server, CONFIGURE, pointed_at("m", "current"), "succeeded", 0).await;
        sqlx::query("UPDATE servers SET revoked_at = now() WHERE id = $1")
            .bind(removed.server)
            .execute(&pool)
            .await
            .unwrap();
        let error = removal::request(&pool, mirror.id, ("user", "someone")).await.unwrap_err();
        refused("m is still used by 2 servers (web-01, web-02)", error);

        // Back on their own sources, they let go.
        for server in [web.server, queued.server] {
            sqlx::query("UPDATE commands SET state = 'cancelled', finished_at = now() WHERE server_id = $1 AND state = 'queued'")
                .bind(server)
                .execute(&pool)
                .await
                .unwrap();
            command(&pool, server, REVERT, RevertSources {}.encode_to_vec(), "succeeded", -10).await;
        }

        // A draft holds it like a running campaign, a finished one does not.
        let customer: Uuid = sqlx::query_scalar("INSERT INTO customers (name) VALUES ('acme') RETURNING id")
            .fetch_one(&pool)
            .await
            .unwrap();
        let mut campaigns = Vec::new();
        for (name, state) in [("april", "draft"), ("march", "completed")] {
            let campaign: Uuid = sqlx::query_scalar(
                "INSERT INTO campaigns (customer_id, name, state, sources_mode, started_at, finished_at)
                 VALUES ($1, $2, $3, 'replace', now(),
                         CASE WHEN $3 = 'completed' THEN now() END) RETURNING id",
            )
            .bind(customer)
            .bind(name)
            .bind(state)
            .fetch_one(&pool)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO campaign_snapshots
                     (campaign_id, mirror_id, snapshot_id, mirror_name, snapshot_name)
                 SELECT $1, mirror_id, id, 'm', name FROM mirror_snapshots
                 WHERE mirror_id = $2 AND name = 's01'",
            )
            .bind(campaign)
            .bind(mirror.id)
            .execute(&pool)
            .await
            .unwrap();
            campaigns.push(campaign);
        }
        let error = removal::request(&pool, mirror.id, ("user", "someone")).await.unwrap_err();
        refused("m is still used by 1 update campaign (april)", error);
        assert!(store::get(&pool, mirror.id).await.unwrap().unwrap().deleting_at.is_none());
        assert_eq!(left(&pool, &mirror).await, ["s01", "s02"]);

        sqlx::query("UPDATE campaigns SET state = 'stopped', finished_at = now() WHERE id = $1")
            .bind(campaigns[0])
            .execute(&pool)
            .await
            .unwrap();
        removal::request(&pool, mirror.id, ("user", "someone")).await.unwrap();
        retention::prune(&f.mirrors).await.unwrap();
        assert_eq!(store::get(&pool, mirror.id).await.unwrap(), None);

        // A finished campaign still says what it pinned.
        let pins = monitorflow_server::campaign::query::pins(&pool, &campaigns).await.unwrap();
        assert_eq!(pins.len(), 2);
        assert_eq!(pins[0].1.name, "m");
        assert_eq!(pins[0].1.snapshot, "s01");
    }

    #[sqlx::test]
    async fn a_source_command_that_cannot_be_read_refuses_a_deletion(pool: PgPool) {
        let f = Fixture::new(&pool);
        let mirror = create(&pool, Family::Apt).await;
        f.snapshots(&mirror, 1..2).await;
        let server = host(&pool, "web-01").await;
        command(&pool, server.server, CONFIGURE, vec![0xff, 0xff], "succeeded", 0).await;
        let error = removal::request(&pool, mirror.id, ("user", "someone")).await.unwrap_err();
        assert!(matches!(error, RemovalError::Check(_)), "{error:?}");
        assert!(store::get(&pool, mirror.id).await.unwrap().unwrap().deleting_at.is_none());
    }

    #[sqlx::test]
    async fn deleting_stops_the_running_sync(pool: PgPool) {
        let f = Fixture::new(&pool);
        let mirror = create(&pool, Family::Dnf).await;
        f.snapshots(&mirror, 1..2).await;
        f.switch("hang", true);
        let job = f.mirrors.begin(&mirror, JobKind::Sync, Trigger::Manual, None).await.unwrap();
        let running = tokio::spawn(job.execute());
        for _ in 0..500 {
            if f.state.join("hanging").exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(f.state.join("hanging").exists());

        removal::request(&pool, mirror.id, ("user", "someone")).await.unwrap();
        // The pass waits for the job, which stops at once rather than
        // after its tool's 25 seconds.
        let started = std::time::Instant::now();
        tokio::time::timeout(Duration::from_secs(15), retention::prune(&f.mirrors))
            .await
            .expect("the job stopped")
            .unwrap();
        assert!(started.elapsed() < Duration::from_secs(15));
        assert_eq!(running.await.unwrap().error.as_deref(), Some(STOPPED));
        assert_eq!(store::get(&pool, mirror.id).await.unwrap(), None);
        assert!(!f.mirrors.layout().mirror_dir(&mirror).exists());
        assert!(!f.mirrors.layout().dnf_work(&mirror).exists());
    }

    #[sqlx::test]
    async fn no_job_starts_on_a_mirror_being_deleted(pool: PgPool) {
        let f = Fixture::new(&pool);
        let mirror = store::create(
            &pool,
            &MirrorSpec {
                sync_interval_s: Some(900),
                ..MirrorSpec {
                    name: "m".into(),
                    family: Family::Dnf,
                    upstream_url: "https://up.example/os".into(),
                    distribution: String::new(),
                    components: Vec::new(),
                    architectures: Vec::new(),
                    sync_interval_s: None,
                    auto_snapshot: false,
                }
            },
            ("system", "test"),
        )
        .await
        .unwrap();
        assert_eq!(store::due(&pool).await.unwrap().len(), 1);
        removal::request(&pool, mirror.id, ("user", "someone")).await.unwrap();
        assert!(store::due(&pool).await.unwrap().is_empty());
        let refused = f.mirrors.begin(&mirror, JobKind::Sync, Trigger::Manual, None).await;
        assert!(matches!(refused, Err(monitorflow_server::mirror::JobError::Going)));
        // The lock is free again.
        let lock = MirrorLock::try_acquire(&pool, mirror.id).await.unwrap();
        lock.unwrap().release().await;
    }

    #[sqlx::test]
    async fn a_failed_deletion_says_why_and_the_next_pass_finishes_it(pool: PgPool) {
        let f = Fixture::new(&pool);
        let mirror = create(&pool, Family::Apt).await;
        f.snapshots(&mirror, 1..2).await;
        removal::request(&pool, mirror.id, ("user", "someone")).await.unwrap();

        f.switch("fail-db-cleanup", true);
        retention::prune(&f.mirrors).await.unwrap();
        let row = store::get(&pool, mirror.id).await.unwrap().unwrap();
        assert!(row.delete_error.unwrap().contains("db cleanup failed"));
        assert!(row.deleting_at.is_some());

        // Asking again changes nothing and is not audited twice.
        removal::request(&pool, mirror.id, ("user", "someone")).await.unwrap();
        f.switch("fail-db-cleanup", false);
        retention::prune(&f.mirrors).await.unwrap();
        assert_eq!(store::get(&pool, mirror.id).await.unwrap(), None);
        assert_eq!(std::fs::read_dir(f.root.join("aptly/pool")).unwrap().count(), 0);
        let requests: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM audit_log WHERE action = 'mirror.delete' AND target_id = $1",
        )
        .bind(mirror.id.to_string())
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(requests, 1);
    }

    #[sqlx::test]
    async fn retention_leaves_a_mirror_being_deleted_alone(pool: PgPool) {
        let f = Fixture::new(&pool);
        let mirror = create(&pool, Family::Apt).await;
        f.snapshots(&mirror, 1..2).await;
        removal::request(&pool, mirror.id, ("user", "someone")).await.unwrap();
        // A pass cannot finish: the snapshots are not counted as pruned.
        f.switch("fail-db-cleanup", true);
        assert_eq!(retention::prune(&f.mirrors).await.unwrap(), 0);
        assert!(states(&pool, &mirror).await.iter().all(|(_, s)| *s == SnapshotState::Pruning));
    }
}
