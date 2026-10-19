//! Keeps the CRL of the mTLS listener fresh (ADR-0007).
//!
//! The listener enforces CRL expiry, so a CRL past its `next_update` locks
//! every client out. [`CrlPublisher`] builds a new CRL and `ServerConfig`
//! after each revocation and on a timer, and sends it through the watch
//! channel read by `tls::incoming`. The same CRL is published for
//! `RevocationService`, so relays enforce what the listener enforces.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use monitorflow_common::tls::{Identity, TlsError};
use rustls::pki_types::CertificateRevocationListDer;
use rustls::ServerConfig;
use tokio::sync::watch;

use crate::ca::{CaError, CaOptions, CertificateAuthority, RevocationReason, Serial};

/// Wait after a failed refresh before trying again.
const RETRY: Duration = Duration::from_secs(60);

#[derive(Debug, thiserror::Error)]
pub enum CrlError {
    #[error(transparent)]
    Ca(#[from] CaError),
    #[error(transparent)]
    Tls(#[from] TlsError),
    #[error("CRL task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
}

/// Refreshes every quarter of the CRL validity, leaving three tries before
/// the current CRL expires.
pub fn refresh_interval(options: &CaOptions) -> Duration {
    options.crl_validity / 4
}

pub struct CrlPublisher {
    ca: Arc<CertificateAuthority>,
    identity: Identity,
    tx: watch::Sender<Arc<ServerConfig>>,
    crl: watch::Sender<Crl>,
}

/// The CRL in DER, as last published.
pub type Crl = Arc<CertificateRevocationListDer<'static>>;

impl CrlPublisher {
    /// Builds the first config with a fresh CRL. Pass the receiver to the
    /// gRPC listener.
    pub async fn start(
        ca: Arc<CertificateAuthority>,
        identity: Identity,
    ) -> Result<(Self, watch::Receiver<Arc<ServerConfig>>), CrlError> {
        let (config, crl) = build(ca.clone(), identity.clone()).await?;
        let (tx, rx) = watch::channel(config);
        let (crl, _) = watch::channel(crl);
        Ok((
            Self {
                ca,
                identity,
                tx,
                crl,
            },
            rx,
        ))
    }

    /// Follows the published CRL, for `RevocationService`.
    pub fn subscribe(&self) -> watch::Receiver<Crl> {
        self.crl.subscribe()
    }

    /// Publishes a new CRL now.
    pub async fn refresh(&self) -> Result<(), CrlError> {
        let (config, crl) = build(self.ca.clone(), self.identity.clone()).await?;
        self.tx.send_replace(config);
        self.crl.send_replace(crl);
        Ok(())
    }

    /// Revokes `serial` in the CA, then publishes a CRL listing it: new
    /// connections with that certificate fail from then on. Recording the
    /// revocation in the database and the audit log is the caller's job.
    pub async fn revoke(&self, serial: Serial, reason: RevocationReason) -> Result<(), CrlError> {
        let ca = self.ca.clone();
        tokio::task::spawn_blocking(move || ca.revoke(&serial, reason)).await??;
        self.refresh().await
    }

    /// Refreshes every `every` until `shutdown` resolves, retrying sooner
    /// after a failure.
    pub async fn run(&self, every: Duration, shutdown: impl Future<Output = ()>) {
        tokio::pin!(shutdown);
        let mut wait = every;
        loop {
            tokio::select! {
                _ = &mut shutdown => return,
                _ = tokio::time::sleep(wait) => {}
            }
            wait = match self.refresh().await {
                Ok(()) => {
                    tracing::debug!("published a fresh CRL");
                    every
                }
                Err(error) => {
                    tracing::error!(%error, "cannot refresh the CRL; clients are locked out when it expires");
                    RETRY.min(every)
                }
            };
        }
    }
}

async fn build(
    ca: Arc<CertificateAuthority>,
    identity: Identity,
) -> Result<(Arc<ServerConfig>, Crl), CrlError> {
    tokio::task::spawn_blocking(move || {
        let crl = ca.crl()?;
        let config = identity.server_config(vec![crl.clone()])?;
        Ok((config, Arc::new(crl)))
    })
    .await?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_leaves_room_before_expiry() {
        assert_eq!(
            refresh_interval(&CaOptions::default()),
            Duration::from_secs(6 * 60 * 60)
        );
    }
}
