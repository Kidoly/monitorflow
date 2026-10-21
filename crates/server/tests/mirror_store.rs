//! `mirrors`, `mirror_snapshots` and `mirror_runs`: schema checks, the
//! store functions and the per-mirror advisory lock (ADR-0030).

use std::time::Duration;

use monitorflow_server::mirror::store::{self, INTERRUPTED};
use monitorflow_server::mirror::{
    Family, MirrorLock, MirrorSpec, RunKind, RunState, SnapshotState, StoreError, Trigger,
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

const CHECK_VIOLATION: &str = "23514";
const UNIQUE_VIOLATION: &str = "23505";

fn apt(name: &str) -> MirrorSpec {
    MirrorSpec {
        name: name.into(),
        family: Family::Apt,
        upstream_url: "https://deb.debian.org/debian".into(),
        distribution: "bookworm".into(),
        components: vec!["main".into(), "contrib".into()],
        architectures: vec!["amd64".into()],
        sync_interval_s: Some(3600),
        auto_snapshot: true,
    }
}

fn code(error: sqlx::Error) -> String {
    match error {
        sqlx::Error::Database(e) => e.code().unwrap_or_default().into_owned(),
        other => panic!("not a database error: {other}"),
    }
}

db_tests! {
    #[sqlx::test]
    async fn mirrors_are_created_once_and_audited(pool: PgPool) {
        let mirror = store::create(&pool, &apt("debian-12"), ("system", "cli")).await.unwrap();
        assert_eq!(mirror.family, Family::Apt);
        assert_eq!(mirror.components, ["main", "contrib"]);
        assert_eq!(store::get(&pool, mirror.id).await.unwrap().as_ref(), Some(&mirror));
        assert_eq!(store::by_name(&pool, "debian-12").await.unwrap(), Some(mirror.clone()));
        let duplicate = store::create(&pool, &apt("debian-12"), ("system", "cli")).await;
        assert!(matches!(duplicate, Err(StoreError::Duplicate("mirror"))));
        let mut bad = apt("x");
        bad.components.clear();
        assert!(matches!(
            store::create(&pool, &bad, ("system", "cli")).await,
            Err(StoreError::Invalid(_))
        ));
        let audited: (String, String) = sqlx::query_as(
            "SELECT action, details->>'name' FROM audit_log WHERE target_kind = 'mirror'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(audited, ("mirror.create".into(), "debian-12".into()));

        store::create(&pool, &apt("a-first"), ("system", "cli")).await.unwrap();
        let names: Vec<String> =
            store::list(&pool).await.unwrap().into_iter().map(|m| m.name).collect();
        assert_eq!(names, ["a-first", "debian-12"]);
    }

    #[sqlx::test]
    async fn the_schema_checks_what_the_store_validates(pool: PgPool) {
        let insert = |name: &str, family: &str, url: &str, dist: &str, components: &[&str]| {
            sqlx::query(
                "INSERT INTO mirrors (name, family, upstream_url, distribution, components)
                 VALUES ($1, $2, $3, $4, $5)",
            )
            .bind(name.to_owned())
            .bind(family.to_owned())
            .bind(url.to_owned())
            .bind(dist.to_owned())
            .bind(components.iter().map(|c| c.to_string()).collect::<Vec<_>>())
            .execute(&pool)
        };
        let url = "https://deb.debian.org/debian";
        insert("ok", "apt", url, "bookworm", &["main"]).await.unwrap();
        insert("rocky", "dnf", "https://dl.rockylinux.org/pub/rocky/9/BaseOS/x86_64/os/", "", &[])
            .await
            .unwrap();
        for (name, family, url, dist, components) in [
            ("Upper", "apt", url, "bookworm", &["main"][..]),
            ("x", "yum", url, "", &[][..]),
            ("x", "apt", "https://u:p@host/debian", "bookworm", &["main"][..]),
            ("x", "apt", "ftp://host/debian", "bookworm", &["main"][..]),
            ("x", "apt", url, "", &["main"][..]),
            ("x", "apt", url, "bookworm", &[][..]),
            ("x", "dnf", url, "", &["main"][..]),
        ] {
            let error = insert(name, family, url, dist, components).await.unwrap_err();
            assert_eq!(code(error), CHECK_VIOLATION, "{name} {family} {url}");
        }
        let error = sqlx::query("UPDATE mirrors SET sync_interval_s = 60")
            .execute(&pool)
            .await
            .unwrap_err();
        assert_eq!(code(error), CHECK_VIOLATION);
    }

    #[sqlx::test]
    async fn one_lock_holder_per_mirror(pool: PgPool) {
        let a = store::create(&pool, &apt("a"), ("system", "cli")).await.unwrap();
        let b = store::create(&pool, &apt("b"), ("system", "cli")).await.unwrap();
        let held = MirrorLock::try_acquire(&pool, a.id).await.unwrap().unwrap();
        assert!(MirrorLock::try_acquire(&pool, a.id).await.unwrap().is_none());
        let other = MirrorLock::try_acquire(&pool, b.id).await.unwrap().unwrap();
        held.release().await;
        let again = MirrorLock::try_acquire(&pool, a.id).await.unwrap().unwrap();
        // Dropped without release, as when a job panics: closing the
        // session frees the lock.
        drop(again);
        let mut freed = false;
        for _ in 0..50 {
            if let Some(lock) = MirrorLock::try_acquire(&pool, a.id).await.unwrap() {
                lock.release().await;
                freed = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(freed, "the lock of a dropped MirrorLock was not freed");
        other.release().await;
    }

    #[sqlx::test]
    async fn runs_and_snapshots_follow_their_lifecycle(pool: PgPool) {
        let mirror = store::create(&pool, &apt("m"), ("system", "cli")).await.unwrap();
        let mut lock = MirrorLock::try_acquire(&pool, mirror.id).await.unwrap().unwrap();
        let run = lock.start_run(RunKind::Sync, Trigger::Manual, Some("cli")).await.unwrap();
        assert_eq!((run.state, run.finished_at), (RunState::Running, None));
        // The partial unique index: one running run per mirror.
        let error = sqlx::query(
            "INSERT INTO mirror_runs (mirror_id, kind, trigger) VALUES ($1, 'sync', 'manual')",
        )
        .bind(mirror.id)
        .execute(&pool)
        .await
        .unwrap_err();
        assert_eq!(code(error), UNIQUE_VIOLATION);
        store::set_output(&pool, run.id, "$ aptly mirror update\n").await.unwrap();
        let snapshot = store::insert_snapshot(&mut lock, "s1").await.unwrap();
        assert_eq!(snapshot.state, SnapshotState::Creating);
        assert!(matches!(
            store::insert_snapshot(&mut lock, "s1").await,
            Err(StoreError::Duplicate("snapshot"))
        ));
        assert!(matches!(
            store::insert_snapshot(&mut lock, "current").await,
            Err(StoreError::Invalid(_))
        ));
        let published = store::publish_snapshot(&pool, snapshot.id, Some(42)).await.unwrap();
        assert_eq!(published.state, SnapshotState::Published);
        assert_eq!(published.size_bytes, Some(42));
        let done = store::finish_run(&pool, run.id, "all good", None, Some(snapshot.id))
            .await
            .unwrap();
        assert_eq!(done.state, RunState::Succeeded);
        assert_eq!(done.snapshot_id, Some(snapshot.id));
        assert!(done.finished_at.is_some());

        // A failed snapshot's name can be retried; a published one's not.
        let run = lock.start_run(RunKind::Snapshot, Trigger::Manual, None).await.unwrap();
        let failed = store::insert_snapshot(&mut lock, "s2").await.unwrap();
        store::fail_snapshot(&pool, failed.id, "gpg: no secret key").await.unwrap();
        let failed_run = store::finish_run(&pool, run.id, "", Some("gpg: no secret key"), None)
            .await
            .unwrap();
        assert_eq!(failed_run.state, RunState::Failed);
        let retried = store::insert_snapshot(&mut lock, "s2").await.unwrap();
        assert_eq!((retried.id, retried.state, retried.error), (failed.id, SnapshotState::Creating, None));
        assert!(matches!(
            store::insert_snapshot(&mut lock, "s1").await,
            Err(StoreError::Duplicate(_))
        ));
        lock.release().await;

        let latest = store::latest_published(&pool, mirror.id).await.unwrap().unwrap();
        assert_eq!(latest.name, "s1");
        let names: Vec<String> = store::snapshots(&pool, mirror.id, 10)
            .await
            .unwrap()
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, ["s2", "s1"]);
        let runs = store::runs(&pool, mirror.id, 10).await.unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].kind, RunKind::Snapshot);
        assert_eq!(store::run(&pool, runs[1].id).await.unwrap().unwrap().output, "all good");
    }

    #[sqlx::test]
    async fn the_next_lock_holder_closes_interrupted_runs(pool: PgPool) {
        let mirror = store::create(&pool, &apt("m"), ("system", "cli")).await.unwrap();
        let mut lock = MirrorLock::try_acquire(&pool, mirror.id).await.unwrap().unwrap();
        let run = lock.start_run(RunKind::Snapshot, Trigger::Schedule, None).await.unwrap();
        let snapshot = store::insert_snapshot(&mut lock, "s").await.unwrap();
        // The process dies: its session ends, the rows stay `running`.
        lock.release().await;

        let mut lock = MirrorLock::try_acquire(&pool, mirror.id).await.unwrap().unwrap();
        let next = lock.start_run(RunKind::Sync, Trigger::Manual, None).await.unwrap();
        let old = store::run(&pool, run.id).await.unwrap().unwrap();
        assert_eq!(old.state, RunState::Failed);
        assert_eq!(old.error.as_deref(), Some(INTERRUPTED));
        let snapshots = store::snapshots(&pool, mirror.id, 10).await.unwrap();
        assert_eq!((snapshots[0].id, snapshots[0].state), (snapshot.id, SnapshotState::Failed));
        assert_eq!(next.state, RunState::Running);
        lock.release().await;
    }

    #[sqlx::test]
    async fn due_mirrors_have_an_elapsed_interval_and_nothing_running(pool: PgPool) {
        let never = store::create(&pool, &apt("never"), ("system", "cli")).await.unwrap();
        let mut manual = apt("manual");
        manual.sync_interval_s = None;
        store::create(&pool, &manual, ("system", "cli")).await.unwrap();
        let recent = store::create(&pool, &apt("recent"), ("system", "cli")).await.unwrap();
        let old = store::create(&pool, &apt("old"), ("system", "cli")).await.unwrap();
        let running = store::create(&pool, &apt("running"), ("system", "cli")).await.unwrap();
        let insert = "INSERT INTO mirror_runs (mirror_id, kind, trigger, state, started_at,
                                               finished_at, error)
                      VALUES ($1, $2, 'schedule', $3, now() - make_interval(secs => $4),
                              CASE WHEN $3 = 'running' THEN NULL ELSE now() END,
                              CASE WHEN $3 = 'failed' THEN 'x' END)";
        for (mirror, kind, state, age) in [
            (recent.id, "sync", "succeeded", 60.0),
            (old.id, "sync", "failed", 7200.0),
            // A snapshot run does not count as a sync.
            (old.id, "snapshot", "succeeded", 10.0),
            (running.id, "sync", "running", 7200.0),
        ] {
            sqlx::query(insert)
                .bind(mirror)
                .bind(kind)
                .bind(state)
                .bind(age)
                .execute(&pool)
                .await
                .unwrap();
        }
        let due: Vec<_> = store::due(&pool).await.unwrap().into_iter().map(|m| m.id).collect();
        assert_eq!(due, [never.id, old.id]);
    }
}
