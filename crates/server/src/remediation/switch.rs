//! Turns the remediation loop on and off from the web interface (ADR-0064).
//!
//! Without `[ai] enabled = true` in the configuration, what the loop runs
//! with lives in `server_settings`. A supervisor polls it and starts,
//! replaces or stops the loop, on every server process, without a restart.
//! The loop resumes from its audit events, so stopping it anywhere is safe.

use std::future::Future;
use std::time::Duration;

use monitorflow_common::config::DEFAULT_AI_MODEL;
use monitorflow_common::redact::Secret;
use sqlx::PgPool;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::{interval, MissedTickBehavior};

use super::runner::Remediation;

/// What the stored settings ask the loop to run with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wanted {
    pub api_key: Secret,
    pub model: String,
}

/// The loop to run, or `None` while AI is off.
pub async fn wanted(pool: &PgPool) -> Result<Option<Wanted>, sqlx::Error> {
    let row: Option<(Option<String>, Option<String>)> =
        sqlx::query_as("SELECT ai_api_key, ai_model FROM server_settings WHERE ai_enabled")
            .fetch_optional(pool)
            .await?;
    Ok(row.and_then(|(key, model)| {
        Some(Wanted {
            api_key: Secret::new(key?),
            model: model.unwrap_or_else(|| DEFAULT_AI_MODEL.to_owned()),
        })
    }))
}

struct Running {
    wanted: Wanted,
    stop: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl Running {
    async fn stop(self) {
        let _ = self.stop.send(());
        let _ = self.task.await;
    }
}

/// Polls the stored settings every `every` until `shutdown`. `start` builds the loop for what they ask; when it fails,
/// the next poll tries again.
pub async fn supervise<S, F, E>(
    pool: PgPool,
    every: Duration,
    start: S,
    shutdown: impl Future<Output = ()>,
) where
    S: Fn(Wanted) -> F,
    F: Future<Output = Result<Remediation, E>>,
    E: std::fmt::Display,
{
    let mut tick = interval(every);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut running: Option<Running> = None;
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            () = &mut shutdown => break,
            _ = tick.tick() => {}
        }
        let want = match wanted(&pool).await {
            Ok(want) => want,
            Err(error) => {
                tracing::warn!(%error, "could not read the AI settings; will retry");
                continue;
            }
        };
        if running.as_ref().map(|r| &r.wanted) == want.as_ref() {
            continue;
        }
        if let Some(old) = running.take() {
            old.stop().await;
            tracing::info!("AI remediation stopped: its settings changed");
        }
        let Some(want) = want else { continue };
        match start(want.clone()).await {
            Ok(remediation) => {
                tracing::info!(model = %want.model, "AI remediation started from the web interface settings");
                let (stop, stopped) = oneshot::channel::<()>();
                let task = tokio::spawn(remediation.run(async move {
                    let _ = stopped.await;
                }));
                running = Some(Running {
                    wanted: want,
                    stop,
                    task,
                });
            }
            Err(error) => {
                tracing::warn!(%error, "could not start AI remediation; will retry");
            }
        }
    }
    if let Some(old) = running {
        old.stop().await;
    }
}
