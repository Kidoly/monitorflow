//! Keeps the CRL of the mTLS listener fresh (ADR-0007).
//!
//! The listener enforces CRL expiry, so a CRL past its `next_update` locks
//! every client out. [`CrlPublisher`] builds a new CRL and `ServerConfig`
//! after each revocation and on a timer, and sends it through the watch
//! channel read by `tls::incoming`. The same CRL is published for
//! `RevocationService`, so relays enforce what the listener enforces.
//!
//! During a change of certificate authority there are two CAs, each with
//! its CRL (ADR-0088), and the server's own certificate changes: the
//! publisher also rebuilds the enrollment listener's config.

use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

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
    identity: Mutex<Identity>,
    tx: watch::Sender<Arc<ServerConfig>>,
    enroll: watch::Sender<Arc<ServerConfig>>,
    crl: watch::Sender<Crl>,
}

/// The CRLs in DER, as last published: the active CA's first, then the
/// other CA's during a change of certificate authority.
pub type Crl = Arc<Vec<CertificateRevocationListDer<'static>>>;

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
        let (enroll, _) = watch::channel(identity.server_config_without_client_auth()?);
        Ok((
            Self {
                ca,
                identity: Mutex::new(identity),
                tx,
                enroll,
                crl,
            },
            rx,
        ))
    }

    /// Follows the enrollment listener's config.
    pub fn enroll_config(&self) -> watch::Receiver<Arc<ServerConfig>> {
        self.enroll.subscribe()
    }

    /// Switches both listeners to `identity`, the server's new certificate
    /// and CAs, for new connections, with fresh CRLs.
    pub async fn reload(&self, identity: Identity) -> Result<(), CrlError> {
        let enroll = identity.server_config_without_client_auth()?;
        *self.identity.lock().unwrap_or_else(PoisonError::into_inner) = identity;
        self.refresh().await?;
        self.enroll.send_replace(enroll);
        Ok(())
    }

    fn identity(&self) -> Identity {
        self.identity
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Follows the published CRL, for `RevocationService`.
    pub fn subscribe(&self) -> watch::Receiver<Crl> {
        self.crl.subscribe()
    }

    /// Publishes a new CRL now.
    pub async fn refresh(&self) -> Result<(), CrlError> {
        let (config, crl) = build(self.ca.clone(), self.identity()).await?;
        self.tx.send_replace(config);
        self.crl.send_replace(crl);
        Ok(())
    }

    /// Whether the CA lists `serial` as revoked.
    pub fn is_revoked(&self, serial: &Serial) -> bool {
        self.ca.is_revoked(serial)
    }

    /// Revokes `serial`, a certificate that expires at `not_after`, in the
    /// CA, then publishes a CRL listing it: new connections with that
    /// certificate fail from then on. Recording the revocation in the
    /// database and the audit log is the caller's job.
    pub async fn revoke(
        &self,
        serial: Serial,
        reason: RevocationReason,
        not_after: SystemTime,
    ) -> Result<(), CrlError> {
        let ca = self.ca.clone();
        tokio::task::spawn_blocking(move || ca.revoke(&serial, reason, not_after)).await??;
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
        let crls = ca.crls()?;
        let config = identity.server_config(crls.clone())?;
        Ok((config, Arc::new(crls)))
    })
    .await?
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the revocation sync of several instances does (ADR-0089), in
    /// the middle of a change of certificate authority (ADR-0088): each
    /// CA's CRL lists the serial, whichever CA issued it.
    #[tokio::test]
    async fn a_revocation_during_a_change_reaches_both_crls() {
        use monitorflow_common::config::TlsPaths;
        use monitorflow_common::tls::{PeerIdentity, PeerRole};
        use x509_parser::prelude::FromDer;
        use x509_parser::revocation_list::CertificateRevocationList;

        let dir = tempfile::TempDir::new().unwrap();
        let ca = Arc::new(
            CertificateAuthority::open_or_create(&dir.path().join("ca"), CaOptions::default())
                .unwrap(),
        );
        let csr = || {
            rcgen::CertificateParams::default()
                .serialize_request(&rcgen::KeyPair::generate().unwrap())
                .unwrap()
                .pem()
                .unwrap()
        };
        let agent = PeerIdentity::new(PeerRole::Agent, "web-01").unwrap();
        let old = ca.issue(&csr(), &agent, None, &[]).unwrap();
        let paths = TlsPaths {
            ca_cert: dir.path().join("ca.pem"),
            cert: dir.path().join("cert.pem"),
            key: dir.path().join("key.pem"),
        };
        let localhost = [crate::ca::ServerName::Dns("localhost".into())];
        crate::enroll::server_cert::ensure(&ca, &paths, &localhost).unwrap();
        let (publisher, _rx) = CrlPublisher::start(ca.clone(), Identity::load(&paths).unwrap())
            .await
            .unwrap();
        ca.start_rotation(false).unwrap();
        ca.switch().unwrap();
        crate::enroll::server_cert::ensure(&ca, &paths, &localhost).unwrap();
        publisher
            .reload(Identity::load(&paths).unwrap())
            .await
            .unwrap();
        let new = ca.issue(&csr(), &agent, None, &[]).unwrap();

        for cert in [&old, &new] {
            publisher
                .revoke(
                    cert.serial.clone(),
                    RevocationReason::KeyCompromise,
                    cert.not_after,
                )
                .await
                .unwrap();
        }
        let crls = publisher.subscribe().borrow().clone();
        assert_eq!(crls.len(), 2);
        for crl in crls.iter() {
            let (_, parsed) = CertificateRevocationList::from_der(crl).unwrap();
            let listed: Vec<_> = parsed
                .iter_revoked_certificates()
                .map(|r| Serial::from_bytes(r.raw_serial()))
                .collect();
            assert!(listed.contains(&old.serial) && listed.contains(&new.serial));
        }
    }

    #[test]
    fn refresh_leaves_room_before_expiry() {
        assert_eq!(
            refresh_interval(&CaOptions::default()),
            Duration::from_secs(6 * 60 * 60)
        );
    }
}
