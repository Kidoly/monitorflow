//! Scheduled mirror syncs (ADR-0030): due mirrors only, bounded
//! concurrency, failures recorded. Ignored without `DATABASE_URL`.

use std::sync::{Arc, Mutex};

use monitorflow_server::mirror::{
    scheduler, store, Family, Invocation, Layout, MirrorSpec, Mirrors, Output, RunError, RunState,
    Runner, Trigger,
};
use sqlx::PgPool;
use tokio::sync::{Notify, Semaphore};

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

/// Every command waits for `release`, then succeeds unless it names a
/// mirror in `failing`.
#[derive(Default)]
struct Gate {
    release: Notify,
    calls: Mutex<Vec<String>>,
    failing: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl Runner for Gate {
    async fn run(&self, invocation: &Invocation) -> Result<Output, RunError> {
        let line = invocation.to_string();
        self.calls.lock().unwrap().push(line.clone());
        self.release.notified().await;
        let fail = self
            .failing
            .lock()
            .unwrap()
            .iter()
            .any(|m| line.contains(m.as_str()));
        Ok(Output {
            success: !fail,
            status: if fail {
                "exit status: 1"
            } else {
                "exit status: 0"
            }
            .into(),
            stdout: String::new(),
            stderr: if fail {
                "Error: Cannot download repomd.xml".into()
            } else {
                String::new()
            },
        })
    }
}

fn dnf(name: &str, interval: Option<i32>) -> MirrorSpec {
    MirrorSpec {
        name: name.into(),
        family: Family::Dnf,
        upstream_url: format!("https://up.example/{name}/"),
        distribution: String::new(),
        components: vec![],
        architectures: vec![],
        sync_interval_s: interval,
        auto_snapshot: false,
    }
}

db_tests! {
    #[sqlx::test]
    async fn due_mirrors_sync_within_the_concurrency_limit(pool: PgPool) {
        let dir = tempfile::tempdir().unwrap();
        let gate = Arc::new(Gate::default());
        let mirrors = Mirrors::new(pool.clone(), gate.clone(), Layout::new(dir.path()));
        let a = store::create(&pool, &dnf("a", Some(3600)), ("system", "test")).await.unwrap();
        let b = store::create(&pool, &dnf("b", Some(3600)), ("system", "test")).await.unwrap();
        store::create(&pool, &dnf("manual", None), ("system", "test")).await.unwrap();
        gate.failing.lock().unwrap().push("mf-b".into());

        let slots = Arc::new(Semaphore::new(1));
        let first = scheduler::tick(&mirrors, &slots).await;
        assert_eq!(first.len(), 1, "one slot, one sync");
        // A running mirror is not due, and the slot is taken.
        assert!(scheduler::tick(&mirrors, &slots).await.is_empty());
        let runs = store::runs(&pool, a.id, 10).await.unwrap();
        assert_eq!((runs[0].state, runs[0].trigger), (RunState::Running, Trigger::Schedule));
        while !first[0].is_finished() {
            gate.release.notify_waiters();
            tokio::task::yield_now().await;
        }
        assert_eq!(store::runs(&pool, a.id, 10).await.unwrap()[0].state, RunState::Succeeded);

        let second = scheduler::tick(&mirrors, &slots).await;
        assert_eq!(second.len(), 1);
        for handle in second {
            while !handle.is_finished() {
                gate.release.notify_waiters();
                tokio::task::yield_now().await;
            }
        }
        let failed = store::runs(&pool, b.id, 10).await.unwrap().remove(0);
        assert_eq!(failed.state, RunState::Failed);
        assert!(failed.error.unwrap().contains("Cannot download repomd.xml"));
        // Both synced within their interval: nothing is due.
        assert!(scheduler::tick(&mirrors, &slots).await.is_empty());
        assert!(gate.calls.lock().unwrap().iter().all(|c| !c.contains("manual")));
    }
}
