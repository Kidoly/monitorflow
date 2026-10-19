//! Certificate renewal before expiry (ADR-0010): once two thirds of the
//! validity have passed, trade a CSR for a fresh key against a new
//! certificate with `CertificateService::Renew` over mTLS, store it, and
//! publish the new identity so the peer's connections use it.
//!
//! The private key is never logged; its PEM lives in a [`Secret`].

use std::time::{Duration, SystemTime};

use rcgen::{KeyPair, PKCS_ECDSA_P256_SHA256};
use tokio::sync::watch;
use tonic::Code;

use super::backoff::{random_u64, Backoff};
use super::enroll::{self, EnrollError};
use super::identity::{self, IdentityError, Renewed, StoredIdentity};
use super::shutdown::Shutdown;
use crate::config::{AgentConfig, TlsPaths};
use crate::proto::v1::certificate_service_client::CertificateServiceClient;
use crate::proto::v1::RenewRequest;
use crate::redact::Secret;
use crate::tls;

/// Time allowed for the whole `Renew` call once connected.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct RenewalSettings {
    pub upstream_url: String,
    /// Time between two checks of the certificate's age.
    pub check_every: Duration,
    /// Random delay added to each check, so a fleet does not renew in step.
    pub jitter: Duration,
    pub connect_timeout: Duration,
    /// Retries after a failed renewal.
    pub backoff: Backoff,
    /// Failures are logged as errors once less than this is left.
    pub urgent_before: Duration,
}

impl RenewalSettings {
    pub fn from_config(config: &AgentConfig) -> Self {
        Self::for_upstream(&config.upstream_url)
    }

    /// Hourly checks against `upstream_url`, the default for every peer.
    pub fn for_upstream(upstream_url: &str) -> Self {
        Self {
            upstream_url: upstream_url.to_owned(),
            check_every: Duration::from_secs(60 * 60),
            jitter: Duration::from_secs(5 * 60),
            connect_timeout: tls::DEFAULT_TIMEOUT,
            backoff: Backoff::new(Duration::from_secs(30), Duration::from_secs(30 * 60)),
            urgent_before: Duration::from_secs(24 * 60 * 60),
        }
    }
}

/// Why a renewal attempt failed. Every failure is retried.
#[derive(Debug, thiserror::Error)]
pub enum RenewError {
    #[error("the server refused the renewal: {0}")]
    Refused(String),
    #[error("renewal response rejected: {0}")]
    BadResponse(String),
    #[error("renewal failed: {0}")]
    Failed(String),
    #[error(transparent)]
    Identity(#[from] IdentityError),
}

fn classify(status: &tonic::Status) -> RenewError {
    let message = format!("{:?}: {}", status.code(), status.message());
    match status.code() {
        Code::PermissionDenied | Code::Unauthenticated => RenewError::Refused(message),
        _ => RenewError::Failed(message),
    }
}

/// The renewal task. Holds the current identity, which [`Self::new`] also
/// returns as a receiver, so the peer's connections can follow it.
pub struct Renewal {
    settings: RenewalSettings,
    paths: TlsPaths,
    identity: watch::Sender<StoredIdentity>,
}

impl Renewal {
    pub fn new(
        settings: RenewalSettings,
        paths: TlsPaths,
        identity: StoredIdentity,
    ) -> (Self, watch::Receiver<StoredIdentity>) {
        let (tx, rx) = watch::channel(identity);
        let renewal = Self {
            settings,
            paths,
            identity: tx,
        };
        (renewal, rx)
    }

    /// Checks now, then after every wait, until `shutdown`.
    pub async fn run(self, mut shutdown: Shutdown) {
        let mut backoff = self.settings.backoff.clone();
        loop {
            let wait = tokio::select! {
                wait = self.check(&mut backoff) => wait,
                () = shutdown.wait() => return,
            };
            tokio::select! {
                () = tokio::time::sleep(wait) => {}
                () = shutdown.wait() => return,
            }
        }
    }

    /// Renews if due; returns the time until the next check.
    async fn check(&self, backoff: &mut Backoff) -> Duration {
        let current = self.identity.borrow().clone();
        let now = SystemTime::now();
        if current.is_expired(now) {
            tracing::error!(
                peer = %current.id,
                identity_dir = %parent(&self.paths.cert),
                "fatal: the certificate has expired and can no longer be renewed; stop the service, remove the files in the identity directory, set enrollment_token to a new token created on the server, and start it again"
            );
            return self.settings.check_every;
        }
        if !current.renewal_due(now) {
            let until_due = current.renewal_at().duration_since(now).unwrap_or_default();
            return until_due.min(next_check(&self.settings));
        }
        let left = current.not_after.duration_since(now).unwrap_or_default();
        tracing::info!(peer = %current.id, expires_in = ?left, "renewing the certificate");
        match self.renew(&current).await {
            Ok(renewed) => {
                tracing::info!(peer = %renewed.id, expires_in = ?renewed.not_after.duration_since(now).unwrap_or_default(), "certificate renewed; reconnecting with it");
                self.identity.send_replace(renewed);
                backoff.reset();
                next_check(&self.settings)
            }
            Err(error) => {
                let retry_in = backoff.next_delay();
                if left < self.settings.urgent_before {
                    tracing::error!(%error, expires_in = ?left, ?retry_in, "certificate renewal failed and the certificate expires soon; check the server logs");
                } else {
                    tracing::warn!(%error, expires_in = ?left, ?retry_in, "certificate renewal failed");
                }
                retry_in
            }
        }
    }

    /// One attempt: new key, CSR, `Renew`, checks, then the files. Nothing
    /// is written unless the response matches our CA, key and identity.
    pub async fn renew(&self, current: &StoredIdentity) -> Result<StoredIdentity, RenewError> {
        let failed = |e: &dyn std::fmt::Display| RenewError::Failed(e.to_string());
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)
            .map_err(|e| RenewError::Failed(format!("cannot generate a key: {e}")))?;
        let role = current.id.role();
        let csr_pem = enroll::csr(&key, role)
            .map_err(|e| RenewError::Failed(format!("cannot build the CSR: {e}")))?;
        let config = current.tls.client_config().map_err(|e| failed(&e))?;
        let url = &self.settings.upstream_url;
        let channel = tls::connect(url, config, self.settings.connect_timeout)
            .await
            .map_err(|e| RenewError::Failed(tls::error_chain(&e)))?;
        let mut client = CertificateServiceClient::new(channel);
        let call = client.renew(RenewRequest { csr_pem });
        let response = match tokio::time::timeout(CALL_TIMEOUT, call).await {
            Err(_) => return Err(RenewError::Failed("no answer in time".into())),
            Ok(Err(status)) => return Err(classify(&status)),
            Ok(Ok(response)) => response.into_inner(),
        };

        let bad = |e: EnrollError| match e {
            EnrollError::BadResponse(why) => RenewError::BadResponse(why),
            other => RenewError::BadResponse(other.to_string()),
        };
        if !enroll::is_ca(&response.ca_cert_pem, &current.ca_fingerprint).map_err(bad)? {
            return Err(RenewError::BadResponse(format!(
                "ca_cert_pem is not the stored CA (expected SHA-256 {})",
                tls::fingerprint_hex(&current.ca_fingerprint)
            )));
        }
        enroll::verify_cert(&response.cert_pem, &response.peer_uri, &key, role).map_err(bad)?;
        if response.peer_uri != current.id.uri() {
            return Err(RenewError::BadResponse(format!(
                "the certificate is for `{}`, not `{}`",
                response.peer_uri,
                current.id.uri()
            )));
        }

        let renewed = Renewed {
            key_pem: Secret::new(key.serialize_pem()),
            cert_pem: response.cert_pem,
        };
        let paths = self.paths.clone();
        tokio::task::spawn_blocking(move || {
            identity::store_renewed(&paths, &renewed)?;
            identity::load(&paths, role)?.ok_or_else(|| {
                IdentityError::Invalid("the certificate vanished right after it was written".into())
            })
        })
        .await
        .map_err(|e| failed(&e))?
        .map_err(RenewError::from)
    }
}

fn next_check(settings: &RenewalSettings) -> Duration {
    let jitter = settings.jitter.as_millis().min(u128::from(u64::MAX - 1)) as u64;
    settings.check_every + Duration::from_millis(random_u64() % (jitter + 1))
}

fn parent(path: &std::path::Path) -> String {
    path.parent().unwrap_or(path).display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_are_classified() {
        for status in [
            tonic::Status::permission_denied("renewal refused: this agent is revoked"),
            tonic::Status::unauthenticated("no client certificate"),
        ] {
            assert!(
                matches!(classify(&status), RenewError::Refused(_)),
                "{status:?}"
            );
        }
        for status in [
            tonic::Status::resource_exhausted("retry later"),
            tonic::Status::unavailable("down"),
            tonic::Status::unimplemented("no CertificateService"),
        ] {
            assert!(
                matches!(classify(&status), RenewError::Failed(_)),
                "{status:?}"
            );
        }
    }

    #[test]
    fn checks_are_hourly_with_jitter() {
        let settings = RenewalSettings {
            jitter: Duration::from_millis(10),
            ..RenewalSettings::from_config(&AgentConfig::default())
        };
        let hour = Duration::from_secs(3600);
        assert_eq!(settings.check_every, hour);
        for _ in 0..100 {
            let wait = next_check(&settings);
            assert!(
                wait >= hour && wait <= hour + Duration::from_millis(10),
                "{wait:?}"
            );
        }
    }
}
