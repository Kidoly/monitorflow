//! Certificate renewal before expiry (ADR-0010): once two thirds of the
//! validity have passed, trade a CSR for a fresh key against a new
//! certificate with `CertificateService::Renew` over mTLS, store it, and
//! publish the new identity so the peer's connections use it.
//!
//! The private key is never logged; its PEM lives in a [`Secret`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use rcgen::{KeyPair, PKCS_ECDSA_P256_SHA256};
use rustls::pki_types::CertificateDer;
use tokio::sync::watch;
use tonic::Code;

use super::backoff::{random_u64, Backoff};
use super::enroll::{self, EnrollError};
use super::identity::{self, IdentityError, Renewed, StoredIdentity};
use super::shutdown::Shutdown;
use crate::ca_bundle;
use crate::command::SignatureError;
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
    /// Time between two asks of a relay waiting for its names (ADR-0066).
    pub names_every: Duration,
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
            names_every: Duration::from_secs(10),
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
    lacks_names: Option<fn(&StoredIdentity) -> bool>,
    /// Cleared when the upstream turns out older than ADR-0088.
    rotation_checks: AtomicBool,
}

/// What a `Renew` call asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ask {
    /// A certificate, because the current one is due.
    Renewal,
    /// Only names an administrator confirmed (ADR-0066).
    NamesOnly,
    /// Only what a change of certificate authority needs (ADR-0088).
    RotationOnly,
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
            lacks_names: None,
            rotation_checks: AtomicBool::new(true),
        };
        (renewal, rx)
    }

    /// While `lacks_names` holds for the identity, a relay also asks for
    /// the names an administrator confirmed (ADR-0066), much more often
    /// than it checks its certificate's age.
    pub fn awaiting_names(mut self, lacks_names: fn(&StoredIdentity) -> bool) -> Self {
        self.lacks_names = Some(lacks_names);
        self
    }

    /// Checks now, then after every wait, until `shutdown`.
    pub async fn run(self, mut shutdown: Shutdown) {
        let mut backoff = self.settings.backoff.clone();
        loop {
            let wait = tokio::select! {
                wait = self.check_all(&mut backoff) => wait,
                () = shutdown.wait() => return,
            };
            tokio::select! {
                () = tokio::time::sleep(wait) => {}
                () = shutdown.wait() => return,
            }
        }
    }

    /// [`Self::check`], then the names a waiting relay asks for; returns
    /// the time until the next check.
    async fn check_all(&self, backoff: &mut Backoff) -> Duration {
        let wait = self.check(backoff).await;
        let current = self.identity.borrow().clone();
        if !self.lacks_names.is_some_and(|lacks| lacks(&current)) {
            return wait;
        }
        match self.request(&current, Ask::NamesOnly).await {
            Ok(renewed) => {
                tracing::info!(peer = %renewed.id, "an administrator confirmed the server names; reconnecting with them");
                self.identity.send_replace(renewed);
            }
            Err(error) => tracing::debug!(%error, "no confirmed server names yet"),
        }
        wait.min(self.settings.names_every)
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
            self.check_rotation(&current).await;
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

    /// Asks whether a change of certificate authority needs a new
    /// certificate or new CAs here, and takes them if so (ADR-0088). The
    /// server answers `FAILED_PRECONDITION` when nothing changed.
    async fn check_rotation(&self, current: &StoredIdentity) {
        if !self.rotation_checks.load(Ordering::Relaxed) {
            return;
        }
        match self.request(current, Ask::RotationOnly).await {
            Ok(renewed) => {
                tracing::info!(peer = %renewed.id, "the server changes its certificate authority; reconnecting with the new certificate");
                self.identity.send_replace(renewed);
            }
            Err(RenewError::Failed(why)) if why.starts_with("FailedPrecondition") => {}
            Err(error) => {
                tracing::debug!(%error, "cannot ask about a change of certificate authority")
            }
        }
    }

    /// One attempt: new key, CSR, `Renew`, checks, then the files. Nothing
    /// is written unless the response matches our CA, key and identity.
    pub async fn renew(&self, current: &StoredIdentity) -> Result<StoredIdentity, RenewError> {
        self.request(current, Ask::Renewal).await
    }

    /// [`Self::renew`] for `ask`: names only or rotation only get an
    /// answer only when there is something new.
    async fn request(
        &self,
        current: &StoredIdentity,
        ask: Ask,
    ) -> Result<StoredIdentity, RenewError> {
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
        let call = client.renew(RenewRequest {
            csr_pem,
            names_only: ask == Ask::NamesOnly,
            accepts_ca_bundle: true,
            rotation_only: ask == Ask::RotationOnly,
        });
        let response = match tokio::time::timeout(CALL_TIMEOUT, call).await {
            Err(_) => return Err(RenewError::Failed("no answer in time".into())),
            Ok(Err(status)) => return Err(classify(&status)),
            Ok(Ok(response)) => response.into_inner(),
        };

        let bad = |e: EnrollError| match e {
            EnrollError::BadResponse(why) => RenewError::BadResponse(why),
            other => RenewError::BadResponse(other.to_string()),
        };
        let issuer = enroll::single_ca(&response.ca_cert_pem)
            .map_err(bad)?
            .ok_or_else(|| RenewError::BadResponse("ca_cert_pem is not one certificate".into()))?;
        let ca_bundle_pem = if response.ca_bundle_pem.is_empty() {
            // A server older than ADR-0088: a CA we already trust, and no
            // change of certificate authority.
            if ask == Ask::RotationOnly {
                self.rotation_checks.store(false, Ordering::Relaxed);
            }
            if !current.ca_fingerprints.contains(&issuer) {
                return Err(RenewError::BadResponse(format!(
                    "ca_cert_pem is not a stored CA (SHA-256 {})",
                    tls::fingerprint_hex(&issuer)
                )));
            }
            None
        } else {
            let bundle =
                enroll::check_bundle(&response.ca_bundle_pem, &issuer, &current.ca_fingerprints)
                    .map_err(bad)?;
            check_new_cas(
                &bundle,
                &response.cert_pem,
                &response.ca_bundle_signatures,
                current.tls.ca_certs(),
            )?;
            Some(response.ca_bundle_pem)
        };
        enroll::verify_cert(&response.cert_pem, &response.peer_uri, &key, role).map_err(bad)?;
        enroll::verify_issued_by(&response.cert_pem, &response.ca_cert_pem).map_err(bad)?;
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
            ca_bundle_pem,
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

/// A bundle may hold a CA that is not among `stored`, the CAs of the
/// current identity, only if the central server signed the bundle for the
/// certificate `cert_pem` with a stored CA's key (ADR-0107). The upstream
/// that answers may be a relay: without this it could add a CA of its own
/// and sign commands with it. A bundle that only keeps, reorders or drops
/// stored CAs needs no signature: it gives nobody any trust.
fn check_new_cas(
    bundle: &[CertificateDer<'static>],
    cert_pem: &str,
    signatures: &[Vec<u8>],
    stored: &[CertificateDer<'static>],
) -> Result<(), RenewError> {
    let fingerprints: Vec<_> = bundle.iter().map(tls::fingerprint).collect();
    let Some(new) = bundle.iter().find(|ca| !stored.contains(ca)) else {
        return Ok(());
    };
    let new = tls::fingerprint_hex(&tls::fingerprint(new));
    let leaf = ca_bundle::leaf_fingerprint(cert_pem)
        .ok_or_else(|| RenewError::BadResponse("cert_pem: no certificate".into()))?;
    ca_bundle::verify(&leaf, &fingerprints, signatures, stored).map_err(|error| {
        let why = match error {
            SignatureError::Missing => "without the server's signature; the server, or a relay on the way to it, may be older than this version",
            SignatureError::Invalid => "with a signature that no certificate authority this peer trusts made",
        };
        RenewError::BadResponse(format!(
            "ca_bundle_pem adds a certificate authority (SHA-256 {new}) {why}"
        ))
    })
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

    struct Ca {
        key: KeyPair,
        der: CertificateDer<'static>,
    }

    impl Ca {
        fn new() -> Self {
            let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
            let cert = rcgen::CertificateParams::default()
                .self_signed(&key)
                .unwrap();
            let der = cert.der().clone();
            Self { key, der }
        }

        /// This CA's signature of `bundle` for the certificate `cert_pem`.
        fn sign(&self, cert_pem: &str, bundle: &[&Ca]) -> Vec<u8> {
            use ring::signature::{EcdsaKeyPair, ECDSA_P256_SHA256_ASN1_SIGNING};
            let fingerprints: Vec<_> = bundle.iter().map(|ca| tls::fingerprint(&ca.der)).collect();
            let leaf = ca_bundle::leaf_fingerprint(cert_pem).unwrap();
            let rng = ring::rand::SystemRandom::new();
            let der = self.key.serialize_der();
            EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &der, &rng)
                .unwrap()
                .sign(&rng, &ca_bundle::signed_bytes(&leaf, &fingerprints))
                .unwrap()
                .as_ref()
                .to_vec()
        }
    }

    fn leaf_pem() -> String {
        let key = KeyPair::generate().unwrap();
        rcgen::CertificateParams::default()
            .self_signed(&key)
            .unwrap()
            .pem()
    }

    #[test]
    fn a_new_ca_needs_the_signature_of_a_stored_one() {
        let (old, new, relay) = (Ca::new(), Ca::new(), Ca::new());
        let (leaf, other_leaf) = (leaf_pem(), leaf_pem());
        let ders = |cas: &[&Ca]| cas.iter().map(|ca| ca.der.clone()).collect::<Vec<_>>();
        let check = |bundle: &[&Ca], signatures: &[Vec<u8>], stored: &[&Ca]| {
            check_new_cas(&ders(bundle), &leaf, signatures, &ders(stored))
                .map_err(|e| e.to_string())
        };

        // Nothing new: the same CAs, in another order, or fewer.
        assert_eq!(check(&[&old], &[], &[&old]), Ok(()));
        assert_eq!(check(&[&new, &old], &[], &[&old, &new]), Ok(()));
        assert_eq!(check(&[&new], &[], &[&new, &old]), Ok(()));

        // Introduced and switched: the old CA signs for the new one. The
        // server sends the new CA's signature too, which proves nothing
        // here and does no harm.
        for bundle in [[&old, &new], [&new, &old]] {
            let by_old = old.sign(&leaf, &bundle);
            let by_new = new.sign(&leaf, &bundle);
            assert_eq!(check(&bundle, &[by_new.clone(), by_old], &[&old]), Ok(()));
            let unsigned = check(&bundle, &[], &[&old]).unwrap_err();
            assert!(
                unsigned.contains("without the server's signature"),
                "{unsigned}"
            );
            let by_itself = check(&bundle, &[by_new], &[&old]).unwrap_err();
            assert!(
                by_itself.contains("no certificate authority this peer trusts"),
                "{by_itself}"
            );
        }

        // What a relay can try: its own CA with its own signature, or with
        // signatures the server made for something else.
        let evil = [&relay, &old];
        for signatures in [
            vec![relay.sign(&leaf, &evil)],
            vec![old.sign(&leaf, &[&new, &old])],
            vec![old.sign(&leaf, &[&old])],
            vec![old.sign(&other_leaf, &evil)],
            vec![old.sign(&leaf, &[&old, &relay])],
        ] {
            assert!(check(&evil, &signatures, &[&old]).is_err());
        }
        // A CA dropped and another added at once is still an addition.
        assert!(check(&[&relay, &new], &[], &[&old, &new]).is_err());
        let signed = [new.sign(&leaf, &[&relay, &new])];
        assert_eq!(check(&[&relay, &new], &signed, &[&old, &new]), Ok(()));
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
