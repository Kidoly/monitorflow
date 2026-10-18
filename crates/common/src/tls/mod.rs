//! Mutual TLS for every gRPC link.
//!
//! [`Identity::load`] reads the PEM files named by [`TlsPaths`];
//! [`Identity::server_config`] and [`Identity::client_config`] turn them into
//! rustls configurations (TLS 1.3 only, ALPN `h2`, ring provider, trust in
//! the configured CA only). [`incoming`] and [`connect`] plug those into
//! tonic, and [`peer_identity`] tells a handler which agent, relay or server
//! is calling.

mod peer;
mod transport;

#[cfg(test)]
mod tests;

/// Test PKI shared with the integration tests.
#[cfg(test)]
#[path = "../../tests/support/mod.rs"]
mod test_support;

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, CertificateRevocationListDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;
use rustls::sign::{CertifiedKey, SingleCertAndKey};
use rustls::{ClientConfig, RootCertStore, ServerConfig};

use crate::config::TlsPaths;

pub use peer::{peer_identity, validate_id, PeerIdentity, PeerRole, URI_SCHEME};
pub use transport::{connect, connector, endpoint, incoming, AcceptOptions, DEFAULT_TIMEOUT};

/// ALPN protocol of gRPC.
const ALPN_H2: &[u8] = b"h2";

#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    #[error("{}: file not found", path.display())]
    Missing { path: PathBuf },
    #[error("cannot read {}: {error}", path.display())]
    Read { path: PathBuf, error: io::Error },
    #[error("{}: malformed PEM: {message}", path.display())]
    Pem { path: PathBuf, message: String },
    #[error("{}: no certificate found, expected a PEM `CERTIFICATE` block", path.display())]
    NoCertificate { path: PathBuf },
    #[error(
        "{}: no private key found, expected a PKCS#8, PKCS#1 or SEC1 PEM block",
        path.display()
    )]
    NoKey { path: PathBuf },
    /// The message comes from rustls and never contains key bytes.
    #[error("{}: unsupported or invalid private key: {error}", path.display())]
    InvalidKey { path: PathBuf, error: rustls::Error },
    #[error("{}: invalid certificate: {error}", path.display())]
    InvalidCert { path: PathBuf, error: rustls::Error },
    #[error(
        "private key {} does not match certificate {}",
        key.display(),
        cert.display()
    )]
    KeyMismatch { cert: PathBuf, key: PathBuf },
    #[error("cannot build TLS configuration: {0}")]
    Config(String),
    #[error("invalid gRPC URL `{url}`: {reason}")]
    Url { url: String, reason: String },
    #[error("cannot connect to {url}: {}", error_chain(error))]
    Connect {
        url: String,
        error: tonic::transport::Error,
    },
    #[error("cannot identify peer: {0}")]
    PeerIdentity(String),
}

/// `error: cause: root cause`, since tonic's own message is just
/// "transport error".
pub fn error_chain(error: &dyn std::error::Error) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        // io::Error repeats its inner error as its source.
        let cause_text = cause.to_string();
        if !message.ends_with(&cause_text) {
            message = format!("{message}: {cause_text}");
        }
        source = cause.source();
    }
    message
}

/// The ring provider, passed explicitly so nothing depends on a process-wide
/// default being installed.
fn provider() -> Arc<CryptoProvider> {
    static PROVIDER: LazyLock<Arc<CryptoProvider>> =
        LazyLock::new(|| Arc::new(rustls::crypto::ring::default_provider()));
    PROVIDER.clone()
}

fn read(path: &Path) -> Result<Vec<u8>, TlsError> {
    std::fs::read(path).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => TlsError::Missing {
            path: path.to_owned(),
        },
        _ => TlsError::Read {
            path: path.to_owned(),
            error,
        },
    })
}

/// Reads every `CERTIFICATE` block of a PEM file, in order.
pub fn load_certs(path: &Path) -> Result<Vec<CertificateDer<'static>>, TlsError> {
    let pem = read(path)?;
    let certs = rustls_pemfile::certs(&mut pem.as_slice())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| TlsError::Pem {
            path: path.to_owned(),
            message: e.to_string(),
        })?;
    if certs.is_empty() {
        return Err(TlsError::NoCertificate {
            path: path.to_owned(),
        });
    }
    Ok(certs)
}

/// Reads the first PKCS#8, PKCS#1 or SEC1 private key of a PEM file.
pub fn load_key(path: &Path) -> Result<PrivateKeyDer<'static>, TlsError> {
    let pem = read(path)?;
    rustls_pemfile::private_key(&mut pem.as_slice())
        // The parser's message could quote file content; keep it out.
        .map_err(|_| TlsError::Pem {
            path: path.to_owned(),
            message: "cannot decode private key file".into(),
        })?
        .ok_or_else(|| TlsError::NoKey {
            path: path.to_owned(),
        })
}

/// A loaded mTLS identity: the trusted CA and our own certificate and key.
///
/// The same identity serves both directions: a relay accepts agents with
/// [`Self::server_config`] and dials upstream with [`Self::client_config`].
#[derive(Clone)]
pub struct Identity {
    roots: Arc<RootCertStore>,
    key: Arc<CertifiedKey>,
}

impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Identity")
            .field("ca_certs", &self.roots.len())
            .field("chain_len", &self.key.cert.len())
            .finish_non_exhaustive()
    }
}

impl Identity {
    /// Loads the CA bundle, certificate chain and key, and checks that the
    /// key belongs to the first certificate of the chain.
    pub fn load(paths: &TlsPaths) -> Result<Self, TlsError> {
        let mut roots = RootCertStore::empty();
        for cert in load_certs(&paths.ca_cert)? {
            roots.add(cert).map_err(|error| TlsError::InvalidCert {
                path: paths.ca_cert.clone(),
                error,
            })?;
        }
        let chain = load_certs(&paths.cert)?;
        let signing_key = provider()
            .key_provider
            .load_private_key(load_key(&paths.key)?)
            .map_err(|error| TlsError::InvalidKey {
                path: paths.key.clone(),
                error,
            })?;
        let key = CertifiedKey::new(chain, signing_key);
        match key.keys_match() {
            // Unknown: the key type cannot report its public key; rustls
            // itself accepts that, and the handshake still proves possession.
            Ok(()) | Err(rustls::Error::InconsistentKeys(rustls::InconsistentKeys::Unknown)) => {}
            Err(rustls::Error::InconsistentKeys(_)) => {
                return Err(TlsError::KeyMismatch {
                    cert: paths.cert.clone(),
                    key: paths.key.clone(),
                })
            }
            Err(error) => {
                return Err(TlsError::InvalidCert {
                    path: paths.cert.clone(),
                    error,
                })
            }
        }
        Ok(Self {
            roots: Arc::new(roots),
            key: Arc::new(key),
        })
    }

    /// Server configuration that requires a client certificate issued by
    /// the configured CA.
    ///
    /// `crls` are DER revocation lists from that CA. When the list is not
    /// empty, the client's end-entity certificate must be covered by one of
    /// them: a certificate from an issuer without a CRL is rejected. Build a
    /// new config and send it to [`incoming`] when the CRL changes.
    pub fn server_config(
        &self,
        crls: Vec<CertificateRevocationListDer<'static>>,
    ) -> Result<Arc<ServerConfig>, TlsError> {
        let provider = provider();
        let verifier =
            WebPkiClientVerifier::builder_with_provider(self.roots.clone(), provider.clone())
                .with_crls(crls)
                .only_check_end_entity_revocation()
                .build()
                .map_err(|e| TlsError::Config(e.to_string()))?;
        let mut config = ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|e| TlsError::Config(e.to_string()))?
            .with_client_cert_verifier(verifier)
            .with_cert_resolver(Arc::new(SingleCertAndKey::from(self.key.clone())));
        config.alpn_protocols = vec![ALPN_H2.to_vec()];
        Ok(Arc::new(config))
    }

    /// Client configuration that trusts only the configured CA (no system
    /// roots) and presents our certificate.
    pub fn client_config(&self) -> Result<Arc<ClientConfig>, TlsError> {
        let mut config = ClientConfig::builder_with_provider(provider())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|e| TlsError::Config(e.to_string()))?
            .with_root_certificates(self.roots.clone())
            .with_client_cert_resolver(Arc::new(SingleCertAndKey::from(self.key.clone())));
        config.alpn_protocols = vec![ALPN_H2.to_vec()];
        Ok(Arc::new(config))
    }
}
