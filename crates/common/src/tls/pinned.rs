//! Client side of enrollment: authenticate the server with only the CA
//! fingerprint from the token, before any client certificate exists
//! (ADR-0006).

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::WebPkiServerVerifier;
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, Error, RootCertStore, SignatureScheme};

use super::{fingerprint, provider, Fingerprint, TlsError, ALPN_H2};

/// Client configuration for the enrollment listener: TLS 1.3, ALPN `h2`,
/// no client certificate. The server must present, after its own
/// certificate, a CA certificate whose SHA-256 is `ca_fingerprint`, and its
/// certificate must verify against that CA alone (name, validity, usage).
/// No system root is ever trusted.
pub fn pinned_client_config(ca_fingerprint: Fingerprint) -> Result<Arc<ClientConfig>, TlsError> {
    let provider = provider();
    let verifier = PinnedCa {
        fingerprint: ca_fingerprint,
        provider: provider.clone(),
    };
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| TlsError::Config(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    config.alpn_protocols = vec![ALPN_H2.to_vec()];
    Ok(Arc::new(config))
}

#[derive(Debug)]
struct PinnedCa {
    fingerprint: Fingerprint,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for PinnedCa {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        // The fingerprint is public: no need for a constant-time comparison.
        let ca = intermediates
            .iter()
            .find(|cert| fingerprint(cert) == self.fingerprint)
            .ok_or_else(|| Error::General("the server did not present the pinned CA".into()))?;
        let mut roots = RootCertStore::empty();
        roots.add(ca.clone().into_owned())?;
        WebPkiServerVerifier::builder_with_provider(Arc::new(roots), self.provider.clone())
            .build()
            .map_err(|e| Error::General(e.to_string()))?
            .verify_server_cert(end_entity, &[], server_name, ocsp_response, now)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}
