//! A shutdown signal that any number of tasks can wait on.

use tokio::sync::watch;

/// Fires the shutdown. Dropping it fires it too, so a task can never wait
/// for a trigger that no longer exists.
#[derive(Debug)]
pub struct Trigger(watch::Sender<bool>);

impl Trigger {
    pub fn fire(&self) {
        self.0.send_replace(true);
    }
}

/// Cheap to clone; every clone sees the same trigger.
#[derive(Debug, Clone)]
pub struct Shutdown(watch::Receiver<bool>);

pub fn channel() -> (Trigger, Shutdown) {
    let (tx, rx) = watch::channel(false);
    (Trigger(tx), Shutdown(rx))
}

impl Shutdown {
    /// Resolves once the trigger fired or was dropped.
    pub async fn wait(&mut self) {
        let _ = self.0.wait_for(|fired| *fired).await;
    }

    pub fn is_fired(&self) -> bool {
        *self.0.borrow() || self.0.has_changed().is_err()
    }
}

/// Fires `trigger` on SIGINT or SIGTERM.
pub fn on_signals(trigger: Trigger) -> std::io::Result<()> {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = signal(SignalKind::terminate())?;
    let mut int = signal(SignalKind::interrupt())?;
    tokio::spawn(async move {
        let name = tokio::select! {
            _ = term.recv() => "SIGTERM",
            _ = int.recv() => "SIGINT",
        };
        tracing::info!(signal = name, "shutting down");
        trigger.fire();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn fire_and_drop_both_release_waiters() {
        let (trigger, mut shutdown) = channel();
        let mut other = shutdown.clone();
        assert!(!shutdown.is_fired());
        let waiter = tokio::spawn(async move { other.wait().await });
        trigger.fire();
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .unwrap()
            .unwrap();
        shutdown.wait().await;
        assert!(shutdown.is_fired());

        let (trigger, mut shutdown) = channel();
        drop(trigger);
        assert!(shutdown.is_fired());
        tokio::time::timeout(Duration::from_secs(1), shutdown.wait())
            .await
            .unwrap();
    }
}
