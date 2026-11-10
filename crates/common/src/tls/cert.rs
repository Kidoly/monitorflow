//! Reading certificates outside a TLS handshake: what the root helpers
//! need to build their own trust anchor (ADR-0108).

use ring::signature::{UnparsedPublicKey, ECDSA_P256_SHA256_ASN1};
use rustls::pki_types::CertificateDer;
use x509_parser::prelude::{FromDer, X509Certificate};

/// Every `CERTIFICATE` block of `pem`, in order; `None` when one does not
/// parse as PEM.
pub fn certs_from_pem(pem: &[u8]) -> Option<Vec<CertificateDer<'static>>> {
    rustls_pemfile::certs(&mut &*pem)
        .collect::<Result<Vec<_>, _>>()
        .ok()
}

/// Whether `ca`'s key signed `cert`: ECDSA P-256 with SHA-256, which the
/// built-in CA uses for every certificate (ADR-0004). Validity dates are
/// not looked at.
pub fn signed_by(cert: &CertificateDer<'_>, ca: &CertificateDer<'_>) -> bool {
    let (Ok((_, cert)), Ok((_, ca))) = (
        X509Certificate::from_der(cert),
        X509Certificate::from_der(ca),
    ) else {
        return false;
    };
    let key = &ca.public_key().subject_public_key.data;
    UnparsedPublicKey::new(&ECDSA_P256_SHA256_ASN1, key)
        .verify(cert.tbs_certificate.as_ref(), &cert.signature_value.data)
        .is_ok()
}

/// Start of `cert`'s validity, Unix seconds.
pub fn not_before_unix(cert: &CertificateDer<'_>) -> Option<i64> {
    let (_, cert) = X509Certificate::from_der(cert).ok()?;
    Some(cert.validity().not_before.timestamp())
}

#[cfg(test)]
mod tests {
    use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, IsCa, KeyPair};

    use super::*;

    fn ca() -> CertifiedIssuer<'static, KeyPair> {
        let mut params = CertificateParams::default();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        CertifiedIssuer::self_signed(params, KeyPair::generate().unwrap()).unwrap()
    }

    #[test]
    fn a_certificate_is_signed_by_its_ca_only() {
        let (ca_cert, other) = (ca(), ca());
        let leaf = CertificateParams::default()
            .signed_by(&KeyPair::generate().unwrap(), &ca_cert)
            .unwrap();
        assert!(signed_by(leaf.der(), ca_cert.der()));
        assert!(!signed_by(leaf.der(), other.der()));
        assert!(!signed_by(
            &CertificateDer::from(vec![1, 2, 3]),
            ca_cert.der()
        ));
        assert!(not_before_unix(leaf.der()).is_some());

        let pem = format!("{}{}", leaf.pem(), ca_cert.pem());
        let certs = certs_from_pem(pem.as_bytes()).unwrap();
        assert_eq!(certs.len(), 2);
        assert_eq!(&certs[0], leaf.der());
        assert_eq!(certs_from_pem(b"nothing here").unwrap().len(), 0);
    }
}
