//! First-run enrollment (ADR-0007): exchange the one-time token and a CSR
//! for a certificate, over TLS pinned to the CA fingerprint in the token.
//!
//! The token and the private key are never logged; the token's `Debug`
//! hides its secret and the key PEM lives in a [`Secret`].

use std::time::Duration;

use monitorflow_common::config::AgentConfig;
use monitorflow_common::proto::v1::enroll_service_client::EnrollServiceClient;
use monitorflow_common::proto::v1::{EnrollRequest, EnrollResponse};
use monitorflow_common::redact::Secret;
use monitorflow_common::tls::{self, EnrollmentToken, Fingerprint, PeerIdentity, PeerRole};
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, PKCS_ECDSA_P256_SHA256};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use rustls::sign::CertifiedKey;
use tonic::Code;

use crate::backoff::Backoff;
use crate::identity::{self, AgentIdentity, Enrolled, IdentityError};
use crate::shutdown::Shutdown;

/// Time allowed for the whole `Enroll` call once connected.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum EnrollError {
    #[error("no identity in {} and no enrollment_token configured; create a token on the server and set MONITORFLOW_ENROLLMENT_TOKEN", dir)]
    NoToken { dir: String },
    #[error("enrollment_token: {0}")]
    BadToken(String),
    /// Unknown, expired or used token, or one for another CA. Final.
    #[error("enrollment refused: the token is unknown, expired, already used or for another server; create a new one")]
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

/// Loads the identity, enrolling first when there is none. Transient
/// enrollment failures are retried with backoff until `shutdown`.
pub async fn ensure_identity(
    config: &AgentConfig,
    mut backoff: Backoff,
    mut shutdown: Shutdown,
) -> Result<AgentIdentity, EnrollError> {
    let paths = config.tls_paths();
    let load_paths = paths.clone();
    if let Some(identity) = blocking(move || identity::load(&load_paths)).await? {
        if config.enrollment_token.is_some() {
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
    let token = config
        .enrollment_token
        .as_ref()
        .ok_or(EnrollError::NoToken { dir })?;
    let token =
        EnrollmentToken::parse(token.expose()).map_err(|e| EnrollError::BadToken(e.to_string()))?;
    let url = config.enroll_url();
    tracing::info!(
        enroll_url = %url,
        ca_fingerprint = %tls::fingerprint_hex(token.ca_fingerprint()),
        "not enrolled yet, enrolling"
    );
    let enrolled = loop {
        let attempt = tokio::select! {
            result = request(&url, &token) => result,
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
        identity::load(&paths)?.ok_or_else(|| {
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
pub async fn request(url: &str, token: &EnrollmentToken) -> Result<Enrolled, EnrollError> {
    let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)
        .map_err(|e| EnrollError::Transient(format!("cannot generate a key: {e}")))?;
    let csr = csr(&key)?;
    let pin = *token.ca_fingerprint();
    let config =
        tls::pinned_client_config(pin).map_err(|e| EnrollError::Transient(e.to_string()))?;
    let channel = tls::connect(url, config, tls::DEFAULT_TIMEOUT)
        .await
        .map_err(|e| EnrollError::Transient(e.to_string()))?;
    let request = EnrollRequest {
        token: token.to_secret_string().expose().to_owned(),
        csr_pem: csr,
    };
    let mut client = EnrollServiceClient::new(channel);
    let call = client.enroll(request);
    let response = match tokio::time::timeout(CALL_TIMEOUT, call).await {
        Err(_) => return Err(EnrollError::Transient("no answer in time".into())),
        Ok(Err(status)) => return Err(classify(&status)),
        Ok(Ok(response)) => response.into_inner(),
    };
    verify(&response, &pin, &key)?;
    Ok(Enrolled {
        key_pem: Secret::new(key.serialize_pem()),
        cert_pem: response.cert_pem,
        ca_cert_pem: response.ca_cert_pem,
    })
}

fn csr(key: &KeyPair) -> Result<String, EnrollError> {
    // The server ignores everything but the key; the CN only helps humans
    // reading the request.
    let mut params = CertificateParams::default();
    params.distinguished_name = DistinguishedName::new();
    params
        .distinguished_name
        .push(DnType::CommonName, "monitorflow-agent");
    params
        .serialize_request(key)
        .and_then(|csr| csr.pem())
        .map_err(|e| EnrollError::Transient(format!("cannot build the CSR: {e}")))
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
/// name an agent. Run before anything is written.
pub fn verify(
    response: &EnrollResponse,
    pin: &Fingerprint,
    key: &KeyPair,
) -> Result<(), EnrollError> {
    let bad = |why: String| EnrollError::BadResponse(why);
    let ca = pem_certs(&response.ca_cert_pem, "ca_cert_pem")?;
    if ca.len() != 1 || tls::fingerprint(&ca[0]) != *pin {
        return Err(bad(format!(
            "ca_cert_pem is not the CA pinned by the token (expected SHA-256 {})",
            tls::fingerprint_hex(pin)
        )));
    }
    let chain = pem_certs(&response.cert_pem, "cert_pem")?;
    let signer = rustls::crypto::ring::default_provider()
        .key_provider
        .load_private_key(PrivatePkcs8KeyDer::from(key.serialize_der()).into())
        .map_err(|e| bad(format!("cannot load our own key: {e}")))?;
    CertifiedKey::new(vec![chain[0].clone()], signer)
        .keys_match()
        .map_err(|_| bad("the certificate does not hold the key we sent".into()))?;
    let id = PeerIdentity::from_cert(&chain[0]).map_err(|e| bad(e.to_string()))?;
    if id.role() != PeerRole::Agent {
        return Err(bad(format!(
            "the certificate is for a {}, not an agent",
            id.role()
        )));
    }
    if id.uri() != response.peer_uri {
        return Err(bad(format!(
            "peer_uri `{}` does not match the certificate's `{}`",
            response.peer_uri,
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
        verify(response, pin, key).map_err(|e| e.to_string())
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
        let csr = csr(&KeyPair::generate().unwrap()).unwrap();
        assert!(
            csr.starts_with("-----BEGIN CERTIFICATE REQUEST-----"),
            "{csr}"
        );
    }
}
