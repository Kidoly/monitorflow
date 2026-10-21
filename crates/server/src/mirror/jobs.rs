//! Syncs and snapshots of one mirror (ADR-0030). [`Mirrors::begin`] takes
//! the mirror's lock and records a running run; [`Job::execute`] runs the
//! tools, stores their output as it goes and ends the run. A failing tool
//! fails the run, never the caller.

use std::sync::Arc;

use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

use super::layout::{self, Layout};
use super::model::{self, Family, Invalid, Mirror, Run, RunKind, Trigger};
use super::runner::{keep_tail, Invocation, Runner, OUTPUT_TAIL};
use super::store::{self, MirrorLock, StoreError};
use super::{apt, dnf, signing};

/// What a job does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobKind {
    /// Update from upstream, then snapshot if the mirror has
    /// `auto_snapshot`.
    Sync,
    /// Snapshot what the last sync fetched, named after the time unless
    /// named here.
    Snapshot(Option<String>),
}

#[derive(Debug, thiserror::Error)]
pub enum JobError {
    #[error("another sync or snapshot of this mirror is running")]
    Busy,
    #[error("the mirror has no successful sync to snapshot")]
    NotSynced,
    #[error(transparent)]
    Invalid(#[from] Invalid),
    #[error("database query failed: {0}")]
    Database(#[from] sqlx::Error),
}

/// Runs mirror jobs; clones are cheap.
#[derive(Clone)]
pub struct Mirrors {
    pool: PgPool,
    runner: Arc<dyn Runner>,
    layout: Layout,
}

/// A job holding its mirror's lock, ready to execute.
pub struct Job {
    mirrors: Mirrors,
    mirror: Mirror,
    kind: JobKind,
    lock: MirrorLock,
    run: Run,
    log: String,
}

impl Mirrors {
    pub fn new(pool: PgPool, runner: Arc<dyn Runner>, layout: Layout) -> Self {
        Self {
            pool,
            runner,
            layout,
        }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// Locks `mirror` and records a running run, or says why not.
    pub async fn begin(
        &self,
        mirror: &Mirror,
        kind: JobKind,
        trigger: Trigger,
        requested_by: Option<&str>,
    ) -> Result<Job, JobError> {
        if let JobKind::Snapshot(Some(name)) = &kind {
            model::validate_snapshot_name(name)?;
        }
        let mut lock = MirrorLock::try_acquire(&self.pool, mirror.id)
            .await?
            .ok_or(JobError::Busy)?;
        if matches!(kind, JobKind::Snapshot(_)) {
            let synced: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM mirror_runs
                                WHERE mirror_id = $1 AND kind = 'sync' AND state = 'succeeded')",
            )
            .bind(mirror.id)
            .fetch_one(&self.pool)
            .await?;
            if !synced {
                lock.release().await;
                return Err(JobError::NotSynced);
            }
        }
        let run_kind = match kind {
            JobKind::Sync => RunKind::Sync,
            JobKind::Snapshot(_) => RunKind::Snapshot,
        };
        let run = lock.start_run(run_kind, trigger, requested_by).await?;
        tracing::info!(mirror = %mirror.name, run = %run.id, kind = ?run_kind, ?trigger, "mirror job started");
        Ok(Job {
            mirrors: self.clone(),
            mirror: mirror.clone(),
            kind,
            lock,
            run,
            log: String::new(),
        })
    }
}

/// A failed step, with the message stored as the run's error.
struct Failed(String);

impl<E: std::fmt::Display> From<E> for Failed {
    fn from(error: E) -> Self {
        Self(error.to_string())
    }
}

impl Job {
    pub fn run(&self) -> &Run {
        &self.run
    }

    /// Runs the job to its end and returns the finished run. Errors are
    /// recorded in the run, and logged when even that fails.
    pub async fn execute(mut self) -> Run {
        let result = match self.kind.clone() {
            JobKind::Sync => match self.sync().await {
                Ok(()) if self.mirror.auto_snapshot => self.snapshot(None).await.map(Some),
                Ok(()) => Ok(None),
                Err(e) => Err(e),
            },
            JobKind::Snapshot(name) => self.snapshot(name).await.map(Some),
        };
        let (error, snapshot) = match result {
            Ok(snapshot) => (None, snapshot),
            Err(Failed(error)) => (Some(error), None),
        };
        let output = keep_tail(&self.log, OUTPUT_TAIL);
        let pool = self.mirrors.pool.clone();
        let finished =
            store::finish_run(&pool, self.run.id, &output, error.as_deref(), snapshot).await;
        self.lock.release().await;
        match &error {
            None => {
                tracing::info!(mirror = %self.mirror.name, run = %self.run.id, "mirror job succeeded")
            }
            Some(error) => {
                tracing::warn!(mirror = %self.mirror.name, run = %self.run.id, %error, "mirror job failed")
            }
        }
        match finished {
            Ok(run) => run,
            Err(e) => {
                tracing::error!(error = %e, run = %self.run.id, "cannot record the end of a mirror job");
                self.run
            }
        }
    }

    /// Runs one command, appending it and its output to the log.
    async fn step(&mut self, invocation: Invocation) -> Result<String, Failed> {
        self.log.push_str(&format!("$ {invocation}\n"));
        let result = self.mirrors.runner.run(&invocation).await;
        let output = match &result {
            Ok(output) => Some(output),
            Err(error) => error.output(),
        };
        if let Some(output) = output {
            self.log.push_str(&output.combined());
        }
        let checked = result.and_then(|output| output.check(&invocation));
        if let Err(error) = &checked {
            self.log.push_str(&format!("{error}\n"));
        }
        let shown = keep_tail(&self.log, OUTPUT_TAIL);
        if let Err(error) = store::set_output(&self.mirrors.pool, self.run.id, &shown).await {
            tracing::warn!(%error, run = %self.run.id, "cannot store mirror job output");
        }
        Ok(checked?.stdout)
    }

    async fn blocking<T: Send + 'static>(
        f: impl FnOnce() -> std::io::Result<T> + Send + 'static,
    ) -> Result<T, Failed> {
        Ok(tokio::task::spawn_blocking(f).await??)
    }

    async fn sync(&mut self) -> Result<(), Failed> {
        let layout = self.mirrors.layout.clone();
        let prepared = layout.clone();
        Self::blocking(move || prepared.prepare()).await?;
        match self.mirror.family {
            Family::Apt => {
                let existing = self.step(apt::list(&layout)).await?;
                let existing: Vec<&str> = existing.lines().map(str::trim).collect();
                for component in self.mirror.components.clone() {
                    if !existing.contains(&apt::mirror_name(&self.mirror, &component).as_str()) {
                        self.step(apt::create(&layout, &self.mirror, &component))
                            .await?;
                    }
                    self.step(apt::update(&layout, &self.mirror, &component))
                        .await?;
                }
            }
            Family::Dnf => {
                let work = layout.dnf_work(&self.mirror);
                Self::blocking(move || std::fs::create_dir_all(work)).await?;
                self.step(dnf::reposync(&layout, &self.mirror)).await?;
                self.step(dnf::createrepo(&layout, &self.mirror)).await?;
            }
        }
        Ok(())
    }

    /// Creates, publishes and records one snapshot, then moves `current`.
    async fn snapshot(&mut self, name: Option<String>) -> Result<Uuid, Failed> {
        let name = name.unwrap_or_else(|| model::snapshot_name_at(Utc::now()));
        let snapshot = store::insert_snapshot(&mut self.lock, &name)
            .await
            .map_err(|e| match e {
                StoreError::Duplicate(_) => Failed(format!("snapshot `{name}` already exists")),
                other => Failed::from(other),
            })?;
        self.log.push_str(&format!("# snapshot {name}\n"));
        match self.build_snapshot(&name).await {
            Ok(size) => {
                store::publish_snapshot(&self.mirrors.pool, snapshot.id, size).await?;
                Ok(snapshot.id)
            }
            Err(Failed(error)) => {
                store::fail_snapshot(&self.mirrors.pool, snapshot.id, &error).await?;
                Err(Failed(error))
            }
        }
    }

    /// The published snapshot's size, if it could be counted.
    async fn build_snapshot(&mut self, name: &str) -> Result<Option<i64>, Failed> {
        let layout = self.mirrors.layout.clone();
        let keys = self.step(signing::list_secret_keys()).await?;
        let key = signing::the_key(&keys)?;
        let dir = layout.mirror_dir(&self.mirror);
        let published = dir.join(name);
        match self.mirror.family {
            Family::Apt => {
                for cleanup in apt::cleanup(&layout, &self.mirror, name) {
                    // Absent objects are the normal case; only the log
                    // shows these.
                    let _ = self.step(cleanup).await;
                }
                for component in self.mirror.components.clone() {
                    self.step(apt::snapshot(&layout, &self.mirror, &component, name))
                        .await?;
                }
                self.step(apt::publish(&layout, &self.mirror, name, &key))
                    .await?;
            }
            Family::Dnf => {
                let work = layout.dnf_work(&self.mirror);
                let building = dir.join(format!(".creating-{name}"));
                let target = building.clone();
                Self::blocking(move || {
                    std::fs::create_dir_all(target.parent().expect("has a parent"))?;
                    layout::remove_tree(&target)?;
                    layout::link_tree(&work, &target, &["repodata"])
                })
                .await?;
                self.step(dnf::sign_repomd(&building, &key)).await?;
                let target = published.clone();
                Self::blocking(move || std::fs::rename(building, target)).await?;
            }
        }
        let current_dir = dir.clone();
        let snapshot = name.to_owned();
        Self::blocking(move || layout::swap_current(&current_dir, &snapshot)).await?;
        let size = tokio::task::spawn_blocking(move || layout::tree_size(&published))
            .await
            .ok()
            .and_then(Result::ok)
            .and_then(|s| i64::try_from(s).ok());
        Ok(size)
    }
}

/// Audits a sync or snapshot requested by `actor` (`(kind, id)` as in the
/// audit log) as `mirror.sync` or `mirror.snapshot`. Scheduled runs are
/// not audited; their rows are the record.
pub async fn audit_request(
    pool: &PgPool,
    actor: (&str, &str),
    mirror: &Mirror,
    run: &Run,
) -> Result<(), sqlx::Error> {
    let action = match run.kind {
        RunKind::Sync => "mirror.sync",
        RunKind::Snapshot => "mirror.snapshot",
    };
    crate::api::audit::record(
        pool,
        crate::api::audit::Event {
            actor_kind: actor.0,
            actor_id: actor.1,
            action,
            target: Some(("mirror", &mirror.id.to_string())),
            details: serde_json::json!({ "name": mirror.name, "run_id": run.id }),
        },
    )
    .await
}
