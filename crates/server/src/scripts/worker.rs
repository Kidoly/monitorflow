//! The task of the script library (ADR-0189): every [`POLL`], it queues
//! the commands of runs whose time came, expires runs nobody approved in
//! time, and lets automation rules take new events. Every instance of a
//! cluster runs it; claims make each run and event taken once.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use sqlx::PgPool;
use tokio::sync::Notify;

use super::{automation, run};
use crate::commands::Commands;

/// How often the worker looks.
pub const POLL: Duration = Duration::from_secs(5);
/// Most runs dispatched in one pass.
const BATCH: i64 = 50;

#[derive(Clone)]
pub struct Worker {
    pool: PgPool,
    commands: Option<Commands>,
    wake: Arc<Notify>,
}

impl Worker {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            commands: None,
            wake: Arc::new(Notify::new()),
        }
    }

    /// Sends queued commands at once through `commands`.
    pub fn with_commands(mut self, commands: Commands) -> Self {
        self.commands = Some(commands);
        self
    }

    /// Looks now rather than at the next poll.
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    /// One pass; errors are logged and the next pass tries again.
    pub async fn tick(&self) {
        match run::due(&self.pool, BATCH).await {
            Ok(due) => {
                for id in due {
                    if let Err(error) = run::dispatch(&self.pool, self.commands.as_ref(), id).await
                    {
                        tracing::warn!(run = %id, %error, "cannot dispatch a script run");
                    }
                }
            }
            Err(error) => tracing::warn!(%error, "cannot read due script runs"),
        }
        match run::expire(&self.pool).await {
            Ok(0) => {}
            Ok(n) => tracing::info!(runs = n, "script runs expired without approval"),
            Err(error) => tracing::warn!(%error, "cannot expire script runs"),
        }
        if let Err(error) = automation::fire(&self.pool, self.commands.as_ref()).await {
            tracing::warn!(%error, "cannot run automation rules");
        }
    }

    /// Passes every `poll` until `shutdown`.
    pub async fn run(self, poll: Duration, shutdown: impl Future<Output = ()>) {
        let mut interval = tokio::time::interval(poll);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                () = &mut shutdown => break,
                _ = interval.tick() => {}
                () = self.wake.notified() => {}
            }
            self.tick().await;
        }
    }
}
