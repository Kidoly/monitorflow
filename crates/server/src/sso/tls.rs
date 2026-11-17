//! TLS toward sign-in providers (ADR-0145): the public certificate
//! authorities, plus one pasted with the provider. Names are always
//! checked; there is no way to turn verification off.

use std::sync::{Arc, LazyLock};

use rustls::crypto::CryptoProvider;
use rustls::pki_types::CertificateDer;
use rustls::{ClientConfig, RootCertStore};

#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    #[error("The certificate authority is not a PEM certificate.")]
    NotPem,
    #[error("The certificate authority holds no certificate.")]
    NoCertificate,
    #[error("The certificate authority cannot be used: {0}")]
    Unusable(String),
}

fn provider() -> Arc<CryptoProvider> {
    static PROVIDER: LazyLock<Arc<CryptoProvider>> =
        LazyLock::new(|| Arc::new(rustls::crypto::ring::default_provider()));
    PROVIDER.clone()
}

/// The certificates of a pasted PEM, refused without one.
pub fn parse_ca(pem: &str) -> Result<Vec<CertificateDer<'static>>, TlsError> {
    let blocks = pem::parse_many(pem).map_err(|_| TlsError::NotPem)?;
    let certs: Vec<_> = blocks
        .into_iter()
        .filter(|b| b.tag() == "CERTIFICATE")
        .map(|b| CertificateDer::from(b.into_contents()))
        .collect();
    if certs.is_empty() {
        return Err(TlsError::NoCertificate);
    }
    let mut roots = RootCertStore::empty();
    for cert in &certs {
        roots
            .add(cert.clone())
            .map_err(|e| TlsError::Unusable(e.to_string()))?;
    }
    Ok(certs)
}

/// What a client of a provider trusts: the public authorities and `ca`.
pub fn client_config(ca: Option<&str>) -> Result<Arc<ClientConfig>, TlsError> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if let Some(ca) = ca {
        for cert in parse_ca(ca)? {
            roots
                .add(cert)
                .map_err(|e| TlsError::Unusable(e.to_string()))?;
        }
    }
    let config = ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|e| TlsError::Unusable(e.to_string()))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ca_pem() -> String {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        params.self_signed(&key).unwrap().pem()
    }

    #[test]
    fn a_pasted_authority_is_parsed() {
        let pem = ca_pem();
        assert_eq!(parse_ca(&pem).unwrap().len(), 1);
        assert!(client_config(Some(&pem)).is_ok());
        assert!(client_config(None).is_ok());
    }

    #[test]
    fn other_text_is_refused() {
        assert!(matches!(
            parse_ca("hello"),
            Err(TlsError::NoCertificate | TlsError::NotPem)
        ));
        let key = "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n";
        assert!(matches!(parse_ca(key), Err(TlsError::NoCertificate)));
        let junk = "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n";
        assert!(parse_ca(junk).is_err());
    }
}
