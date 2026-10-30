//! First-run enrollment (ADR-0007): exchange the one-time token and a CSR
//! for a certificate, over TLS pinned to the CA fingerprint in the token.
//!
//! The token and the private key are never logged; the token's `Debug`
//! hides its secret and the key PEM lives in a [`Secret`].

use std::time::Duration;

use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, PKCS_ECDSA_P256_SHA256};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use rustls::sign::CertifiedKey;
use tonic::Code;

use super::backoff::Backoff;
use super::identity::{self, article, Enrolled, IdentityError, StoredIdentity};
use super::shutdown::Shutdown;
use crate::config::{AgentConfig, RelayConfig, TlsPaths};
use crate::proto::v1::enroll_service_client::EnrollServiceClient;
use crate::proto::v1::{EnrollRequest, EnrollResponse};
use crate::redact::Secret;
use crate::tls::{self, EnrollmentToken, Fingerprint, PeerIdentity, PeerRole};

/// Time allowed for the whole `Enroll` call once connected.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum EnrollError {
    #[error("no identity in {} and no enrollment_token configured; create a token on the server and set MONITORFLOW_ENROLLMENT_TOKEN", dir)]
    NoToken { dir: String },
    #[error("enrollment_token: {0}")]
    BadToken(String),
    /// Unknown, expired or used token, or one for another CA. Final.
    #[error("enrollment refused: the token or install key is unknown, expired, used up, revoked or for another server; create a new one")]
    Refused,
    /// The server answered something that does not match the token or our
    /// key. Nothing was stored.
    #[error("enrollment response rejected: {0}")]
    BadResponse(String),
    /// Network trouble or a busy server; worth retrying.
    #[error("enrollment failed, will retry: {0}")]
    Transient(String),
    #[error(transparent)]
    Identity(#[from] IdentityError),
    #[error("shutdown requested during enrollment")]
    Shutdown,
}

impl EnrollError {
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Transient(_))
    }
}

/// What a peer needs to load its identity or enroll.
#[derive(Debug, Clone)]
pub struct EnrollSettings {
    /// The role the certificate must name.
    pub role: PeerRole,
    pub paths: TlsPaths,
    pub token: Option<Secret>,
    pub enroll_url: String,
    /// The host's name, sent by agents: with an install key it becomes
    /// their ID (ADR-0058). Empty for relays.
    pub hostname: String,
}

impl EnrollSettings {
    pub fn agent(config: &AgentConfig) -> Self {
        Self {
            role: PeerRole::Agent,
            paths: config.tls_paths(),
            token: config.enrollment_token.clone(),
            enroll_url: config.enroll_url(),
            hostname: String::new(),
        }
    }

    pub fn relay(config: &RelayConfig) -> Self {
        Self {
            role: PeerRole::Relay,
            paths: config.tls_paths(),
            token: config.enrollment_token.clone(),
            enroll_url: config.enroll_url(),
            hostname: String::new(),
        }
    }
}

/// Loads the identity, enrolling first when there is none. Transient
/// enrollment failures are retried with backoff until `shutdown`.
pub async fn ensure_identity(
    settings: &EnrollSettings,
    mut backoff: Backoff,
    mut shutdown: Shutdown,
) -> Result<StoredIdentity, EnrollError> {
    let (paths, role) = (settings.paths.clone(), settings.role);
    let load_paths = paths.clone();
    if let Some(identity) = blocking(move || identity::load(&load_paths, role)).await? {
        if settings.token.is_some() {
            tracing::info!(
                "already enrolled; the enrollment_token is ignored, remove it from the configuration"
            );
        }
        return Ok(identity);
    }
    let dir = paths
        .cert
        .parent()
        .unwrap_or(&paths.cert)
        .display()
        .to_string();
    let token = settings
        .token
        .as_ref()
        .ok_or(EnrollError::NoToken { dir })?;
    let token =
        EnrollmentToken::parse(token.expose()).map_err(|e| EnrollError::BadToken(e.to_string()))?;
    let url = &settings.enroll_url;
    tracing::info!(
        enroll_url = %url,
        ca_fingerprint = %tls::fingerprint_hex(token.ca_fingerprint()),
        "not enrolled yet, enrolling"
    );
    let enrolled = loop {
        let attempt = tokio::select! {
            result = request(url, &token, role, &settings.hostname) => result,
            () = shutdown.wait() => return Err(EnrollError::Shutdown),
        };
        match attempt {
            Ok(enrolled) => break enrolled,
            Err(error) if error.is_transient() => {
                let delay = backoff.next_delay();
                tracing::warn!(%error, retry_in = ?delay, "enrollment attempt failed");
                tokio::select! {
                    () = tokio::time::sleep(delay) => {}
                    () = shutdown.wait() => return Err(EnrollError::Shutdown),
                }
            }
            Err(error) => return Err(error),
        }
    };
    let identity = blocking(move || {
        identity::store(&paths, &enrolled)?;
        identity::load(&paths, role)?.ok_or_else(|| {
            IdentityError::Invalid("the certificate vanished right after it was written".into())
        })
    })
    .await?;
    tracing::info!(peer = %identity.id, "enrolled; remove the enrollment_token from the configuration");
    Ok(identity)
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, IdentityError> + Send + 'static,
) -> Result<T, EnrollError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| EnrollError::Identity(IdentityError::Invalid(e.to_string())))?
        .map_err(EnrollError::Identity)
}

/// One enrollment attempt: new key, CSR, `Enroll`, checks. A fresh key per
/// attempt costs nothing and never reuses a key that may have left a CSR on
/// a failed connection.
pub async fn request(
    url: &str,
    token: &EnrollmentToken,
    role: PeerRole,
    hostname: &str,
) -> Result<Enrolled, EnrollError> {
    let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)
        .map_err(|e| EnrollError::Transient(format!("cannot generate a key: {e}")))?;
    let csr = csr(&key, role)
        .map_err(|e| EnrollError::Transient(format!("cannot build the CSR: {e}")))?;
    let pin = *token.ca_fingerprint();
    let config =
        tls::pinned_client_config(pin).map_err(|e| EnrollError::Transient(e.to_string()))?;
    let channel = tls::connect(url, config, tls::DEFAULT_TIMEOUT)
        .await
        .map_err(|e| EnrollError::Transient(e.to_string()))?;
    let request = EnrollRequest {
        token: token.to_secret_string().expose().to_owned(),
        csr_pem: csr,
        hostname: hostname.to_owned(),
    };
    let mut client = EnrollServiceClient::new(channel);
    let call = client.enroll(request);
    let response = match tokio::time::timeout(CALL_TIMEOUT, call).await {
        Err(_) => return Err(EnrollError::Transient("no answer in time".into())),
        Ok(Err(status)) => return Err(classify(&status)),
        Ok(Ok(response)) => response.into_inner(),
    };
    verify(&response, &pin, &key, role)?;
    Ok(Enrolled {
        key_pem: Secret::new(key.serialize_pem()),
        cert_pem: response.cert_pem,
        ca_cert_pem: response.ca_cert_pem,
    })
}

pub(crate) fn csr(key: &KeyPair, role: PeerRole) -> Result<String, rcgen::Error> {
    // The server ignores everything but the key; the CN only helps humans
    // reading the request.
    let mut params = CertificateParams::default();
    params.distinguished_name = DistinguishedName::new();
    params
        .distinguished_name
        .push(DnType::CommonName, format!("monitorflow-{role}"));
    params.serialize_request(key).and_then(|csr| csr.pem())
}

fn classify(status: &tonic::Status) -> EnrollError {
    match status.code() {
        Code::PermissionDenied => EnrollError::Refused,
        // Our own request was malformed: retrying sends the same thing.
        Code::InvalidArgument => EnrollError::BadResponse(format!(
            "the server rejected the request: {}",
            status.message()
        )),
        _ => EnrollError::Transient(format!("{:?}: {}", status.code(), status.message())),
    }
}

fn pem_certs(pem: &str, what: &str) -> Result<Vec<CertificateDer<'static>>, EnrollError> {
    let certs = rustls_pemfile::certs(&mut pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| EnrollError::BadResponse(format!("{what}: {e}")))?;
    if certs.is_empty() {
        return Err(EnrollError::BadResponse(format!("{what}: no certificate")));
    }
    Ok(certs)
}

/// The CA must be the pinned one and the certificate must hold our key and
/// name `role`. Run before anything is written.
pub fn verify(
    response: &EnrollResponse,
    pin: &Fingerprint,
    key: &KeyPair,
    role: PeerRole,
) -> Result<(), EnrollError> {
    if !is_ca(&response.ca_cert_pem, pin)? {
        return Err(EnrollError::BadResponse(format!(
            "ca_cert_pem is not the CA pinned by the token (expected SHA-256 {})",
            tls::fingerprint_hex(pin)
        )));
    }
    verify_cert(&response.cert_pem, &response.peer_uri, key, role)
}

/// True if `ca_cert_pem` holds exactly one certificate, with SHA-256 `pin`.
pub(crate) fn is_ca(ca_cert_pem: &str, pin: &Fingerprint) -> Result<bool, EnrollError> {
    let ca = pem_certs(ca_cert_pem, "ca_cert_pem")?;
    Ok(ca.len() == 1 && tls::fingerprint(&ca[0]) == *pin)
}

/// The certificate must hold our key, name `role`, and match `peer_uri`.
pub(crate) fn verify_cert(
    cert_pem: &str,
    peer_uri: &str,
    key: &KeyPair,
    role: PeerRole,
) -> Result<(), EnrollError> {
    let bad = |why: String| EnrollError::BadResponse(why);
    let chain = pem_certs(cert_pem, "cert_pem")?;
    let signer = rustls::crypto::ring::default_provider()
        .key_provider
        .load_private_key(PrivatePkcs8KeyDer::from(key.serialize_der()).into())
        .map_err(|e| bad(format!("cannot load our own key: {e}")))?;
    CertifiedKey::new(vec![chain[0].clone()], signer)
        .keys_match()
        .map_err(|_| bad("the certificate does not hold the key we sent".into()))?;
    let id = PeerIdentity::from_cert(&chain[0]).map_err(|e| bad(e.to_string()))?;
    if id.role() != role {
        return Err(bad(format!(
            "the certificate is for {}, not {}",
            article(id.role()),
            article(role)
        )));
    }
    if id.uri() != peer_uri {
        return Err(bad(format!(
            "peer_uri `{peer_uri}` does not match the certificate's `{}`",
            id.uri()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use rcgen::{BasicConstraints, IsCa, SanType};

    use super::*;

    struct Ca {
        cert: rcgen::Certificate,
        key: KeyPair,
    }

    impl Ca {
        fn new() -> Self {
            let key = KeyPair::generate().unwrap();
            let mut params = CertificateParams::default();
            params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
            let cert = params.self_signed(&key).unwrap();
            Self { cert, key }
        }

        fn pin(&self) -> Fingerprint {
            tls::fingerprint(self.cert.der())
        }

        fn response(&self, key: &KeyPair, uri: &str) -> EnrollResponse {
            let mut params = CertificateParams::default();
            params.subject_alt_names = vec![SanType::URI(uri.try_into().unwrap())];
            let cert = params.signed_by(key, &self.cert, &self.key).unwrap();
            EnrollResponse {
                cert_pem: cert.pem(),
                ca_cert_pem: self.cert.pem(),
                peer_uri: uri.into(),
                not_after_unix: 0,
            }
        }
    }

    fn check(response: &EnrollResponse, pin: &Fingerprint, key: &KeyPair) -> Result<(), String> {
        verify(response, pin, key, PeerRole::Agent).map_err(|e| e.to_string())
    }

    #[test]
    fn accepts_a_matching_response() {
        let (ca, key) = (Ca::new(), KeyPair::generate().unwrap());
        let response = ca.response(&key, "monitorflow://agent/web-01");
        check(&response, &ca.pin(), &key).unwrap();
    }

    #[test]
    fn rejects_another_ca() {
        let (ca, key) = (Ca::new(), KeyPair::generate().unwrap());
        let response = ca.response(&key, "monitorflow://agent/web-01");
        let err = check(&response, &Ca::new().pin(), &key).unwrap_err();
        assert!(err.contains("not the CA pinned by the token"), "{err}");
        // An extra CA next to the pinned one is not accepted either.
        let mut two = response.clone();
        two.ca_cert_pem = format!("{}{}", ca.cert.pem(), Ca::new().cert.pem());
        assert!(check(&two, &ca.pin(), &key).is_err());
    }

    #[test]
    fn rejects_a_certificate_for_another_key() {
        let ca = Ca::new();
        let response = ca.response(&KeyPair::generate().unwrap(), "monitorflow://agent/web-01");
        let err = check(&response, &ca.pin(), &KeyPair::generate().unwrap()).unwrap_err();
        assert!(err.contains("does not hold the key"), "{err}");
    }

    #[test]
    fn rejects_other_roles_and_mismatched_uri() {
        let (ca, key) = (Ca::new(), KeyPair::generate().unwrap());
        let relay = ca.response(&key, "monitorflow://relay/r1");
        let err = check(&relay, &ca.pin(), &key).unwrap_err();
        assert!(err.contains("for a relay"), "{err}");
        let mut lying = ca.response(&key, "monitorflow://agent/web-01");
        lying.peer_uri = "monitorflow://agent/web-02".into();
        let err = check(&lying, &ca.pin(), &key).unwrap_err();
        assert!(err.contains("does not match"), "{err}");
        let mut empty = lying;
        empty.cert_pem.clear();
        assert!(check(&empty, &ca.pin(), &key)
            .unwrap_err()
            .contains("no certificate"));
    }

    #[test]
    fn statuses_are_classified() {
        assert!(matches!(
            classify(&tonic::Status::permission_denied("enrollment refused")),
            EnrollError::Refused
        ));
        assert!(matches!(
            classify(&tonic::Status::invalid_argument("invalid CSR")),
            EnrollError::BadResponse(_)
        ));
        for status in [
            tonic::Status::unavailable("down"),
            tonic::Status::resource_exhausted("slow down"),
            tonic::Status::internal("db"),
        ] {
            assert!(classify(&status).is_transient(), "{status:?}");
        }
    }

    #[test]
    fn csr_is_valid_pem() {
        let csr = csr(&KeyPair::generate().unwrap(), PeerRole::Relay).unwrap();
        assert!(
            csr.starts_with("-----BEGIN CERTIFICATE REQUEST-----"),
            "{csr}"
        );
    }
}
