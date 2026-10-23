//! The incident loop (architecture §8 Flow, ADR-0043).
//!
//! A supervisor polls PostgreSQL. It opens a remediation incident for each
//! new threshold alert, drives every `open` or `running` incident no live
//! process holds a lease on, and sweeps incidents waiting for approval
//! (their duration limit, their alert resolving). Driving an incident is a
//! series of passes ([`drive`]): read its events, take the next action,
//! record it. Every pass is one or a few transactions, so a server stopped
//! anywhere resumes from the events, and the lease keeps two processes off
//! one incident.
//!
//! The AI's answers are untrusted text. Only the exact command of a
//! proposal runs, after the rules classified it (ADR-0041): read-only, or
//! low impact where the policy allows it, runs at once; other tiers wait
//! for approval in `remediation_pending_commands`; forbidden commands
//! escalate. Context and outputs are redacted before the AI or the audit
//! trail sees them (ADR-0040).

mod drive;
pub mod hook;
pub mod progress;
pub mod prompt;
pub mod store;

use std::collections::HashSet;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use monitorflow_ai::AiProvider;
use monitorflow_common::config::RemediationConfig;
use sqlx::PgPool;
use tokio::task::JoinSet;
use tokio::time::{interval, MissedTickBehavior};
use uuid::Uuid;

use self::hook::{ChangeHook, NoSnapshot};
use super::audit::{self, AuditError, IncidentState, NewIncident};
use super::context::{Backends, Limits};
use crate::commands::Commands;

const UNIQUE_VIOLATION: &str = "23505";

/// Supervisor time between two polls.
pub const POLL: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct Settings {
    /// Limits copied into each new incident.
    pub max_commands: u32,
    pub max_duration: Duration,
    /// Time one command may take; also its expiry and the agent's timeout.
    pub command_timeout: Duration,
    /// Time between two supervisor polls.
    pub poll_interval: Duration,
    /// Time between two looks at a command's result or an alert.
    pub result_poll: Duration,
    /// How long a lease lasts without renewal.
    pub lease_ttl: Duration,
    /// How long the sensor has to confirm a fix the AI announced.
    pub resolve_wait: Duration,
    /// Alerts opened longer ago are left alone: those open when
    /// remediation was enabled.
    pub alert_max_age: Duration,
    /// Incidents driven at once by this process.
    pub max_concurrent: usize,
    pub context_limits: Limits,
}

impl Default for Settings {
    fn default() -> Self {
        Self::from_config(&RemediationConfig::default())
    }
}

impl Settings {
    pub fn from_config(config: &RemediationConfig) -> Self {
        Self {
            max_commands: config.max_commands,
            max_duration: Duration::from_secs(config.max_duration),
            command_timeout: Duration::from_secs(config.command_timeout),
            poll_interval: POLL,
            result_poll: Duration::from_secs(1),
            lease_ttl: Duration::from_secs(60),
            resolve_wait: Duration::from_secs(5 * 60),
            alert_max_age: Duration::from_secs(60 * 60),
            max_concurrent: 16,
            context_limits: Limits::default(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DriveError {
    #[error(transparent)]
    Audit(#[from] AuditError),
    #[error("database: {0}")]
    Database(#[from] sqlx::Error),
}

/// What every driver shares.
#[derive(Debug)]
struct Shared {
    pool: PgPool,
    provider: Arc<dyn AiProvider>,
    /// `provider/model`, the AI's actor in the audit trail.
    model: String,
    commands: Commands,
    backends: Backends,
    hook: Arc<dyn ChangeHook>,
    settings: Settings,
    /// This process's lease holder ID.
    holder: Uuid,
}

/// The remediation loop of one server process.
#[derive(Debug, Clone)]
pub struct Remediation {
    shared: Arc<Shared>,
}

impl Remediation {
    /// `model` names the provider's model for the audit trail. Commands
    /// are queued through `commands`; the context reads `backends`.
    pub fn new(
        provider: Arc<dyn AiProvider>,
        model: &str,
        commands: Commands,
        backends: Backends,
        settings: Settings,
    ) -> Self {
        let model = format!("{}/{model}", provider.name());
        Self {
            shared: Arc::new(Shared {
                pool: backends.pool.clone(),
                provider,
                model,
                commands,
                backends,
                hook: Arc::new(NoSnapshot),
                settings,
                holder: Uuid::new_v4(),
            }),
        }
    }

    /// Runs `hook` before every change-tier command instead of
    /// [`NoSnapshot`]. Call before [`Self::run`].
    pub fn with_hook(self, hook: Arc<dyn ChangeHook>) -> Self {
        let mut shared = Arc::try_unwrap(self.shared).expect("with_hook before run");
        shared.hook = hook;
        Self {
            shared: Arc::new(shared),
        }
    }

    /// Polls until `shutdown`, then stops every driver and gives up its
    /// leases, so another process (or the next start) resumes them.
    pub async fn run(self, shutdown: impl Future<Output = ()>) {
        tracing::info!(holder = %self.shared.holder, "remediation loop started");
        let mut tick = interval(self.shared.settings.poll_interval);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut drivers = JoinSet::new();
        let mut driven = HashSet::new();
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                () = &mut shutdown => break,
                Some(done) = drivers.join_next(), if !drivers.is_empty() => {
                    if let Ok(id) = done {
                        driven.remove(&id);
                    }
                }
                _ = tick.tick() => self.poll(&mut drivers, &mut driven).await,
            }
        }
        drivers.shutdown().await;
        if let Err(error) = store::release(&self.shared.pool, None, self.shared.holder).await {
            tracing::warn!(%error, "could not release remediation leases; they expire");
        }
        tracing::info!("remediation loop stopped");
    }

    async fn poll(&self, drivers: &mut JoinSet<Uuid>, driven: &mut HashSet<Uuid>) {
        if let Err(error) = self.open_new().await {
            tracing::warn!(%error, "could not open remediation incidents; will retry");
        }
        if let Err(error) = self.sweep_parked().await {
            tracing::warn!(%error, "could not sweep parked remediation incidents; will retry");
        }
        if let Err(error) = self.start_drivers(drivers, driven).await {
            tracing::warn!(%error, "could not start remediation drivers; will retry");
        }
    }

    /// Opens one incident per new alert.
    async fn open_new(&self) -> Result<(), DriveError> {
        let shared = &self.shared;
        let settings = &shared.settings;
        for alert in store::new_alerts(&shared.pool, settings.alert_max_age, 100).await? {
            let new = NewIncident {
                server_id: alert.server_id,
                alert_incident_id: Some(alert.id),
                metric: alert.metric,
                instance: alert.instance,
                max_commands: settings.max_commands,
                max_duration: settings.max_duration,
            };
            match audit::open(&shared.pool, &new).await {
                Ok(id) => {
                    tracing::info!(incident = %id, alert = %alert.id, "remediation incident opened")
                }
                // Another process opened it first.
                Err(AuditError::Database(sqlx::Error::Database(e)))
                    if e.code().as_deref() == Some(UNIQUE_VIOLATION) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    /// Ends incidents waiting for approval whose alert resolved or whose
    /// duration limit passed.
    async fn sweep_parked(&self) -> Result<(), DriveError> {
        let pool = &self.shared.pool;
        for id in store::parked(pool).await? {
            let Some(incident) = store::incident(pool, id).await? else {
                continue;
            };
            let resolved = match incident.alert_incident_id {
                Some(alert) => store::alert_resolved(pool, alert).await?,
                None => false,
            };
            if resolved {
                drive::close(
                    pool,
                    id,
                    IncidentState::Resolved,
                    "the sensor returned to normal",
                    drive::Pending::Cancel,
                )
                .await?;
            } else if chrono::Utc::now() >= incident.deadline() {
                let reason = format!(
                    "the duration limit of {} s was reached",
                    incident.max_duration_s
                );
                drive::close(
                    pool,
                    id,
                    IncidentState::Escalated,
                    &reason,
                    drive::Pending::Expire,
                )
                .await?;
            }
        }
        Ok(())
    }

    /// Leases and drives incidents nobody drives.
    async fn start_drivers(
        &self,
        drivers: &mut JoinSet<Uuid>,
        driven: &mut HashSet<Uuid>,
    ) -> Result<(), DriveError> {
        let shared = &self.shared;
        let room = shared.settings.max_concurrent.saturating_sub(driven.len());
        if room == 0 {
            return Ok(());
        }
        let candidates =
            store::unleased(&shared.pool, shared.holder, (room + driven.len()) as i64).await?;
        for id in candidates {
            if driven.len() >= shared.settings.max_concurrent {
                break;
            }
            if driven.contains(&id) {
                continue;
            }
            if !store::acquire(&shared.pool, id, shared.holder, shared.settings.lease_ttl).await? {
                continue;
            }
            driven.insert(id);
            let shared = shared.clone();
            drivers.spawn(async move {
                tokio::select! {
                    result = drive::drive(&shared, id) => if let Err(error) = result {
                        tracing::warn!(incident = %id, %error, "remediation pass failed; will resume");
                    },
                    () = keep_lease(&shared, id) => {
                        tracing::warn!(incident = %id, "lost the remediation lease; stopped driving");
                    }
                }
                id
            });
        }
        Ok(())
    }
}

/// Renews the lease of `id` until it is lost.
async fn keep_lease(shared: &Shared, id: Uuid) {
    let every = shared.settings.lease_ttl / 3;
    loop {
        tokio::time::sleep(every).await;
        match store::renew(&shared.pool, id, shared.holder, shared.settings.lease_ttl).await {
            Ok(true) => {}
            Ok(false) => return,
            Err(error) => {
                tracing::warn!(incident = %id, %error, "could not renew a remediation lease");
                return;
            }
        }
    }
}
