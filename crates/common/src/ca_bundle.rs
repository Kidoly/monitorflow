//! Signed CA bundles (ADR-0107): the central server signs, with the key of
//! each CA it trusts, which CAs the holder of one certificate must trust.
//! A peer takes a CA it does not trust yet only with such a signature from
//! a CA it already trusts, so a relay, which answers the renewals of the
//! peers below it, cannot give them a CA of its own.

use rustls::pki_types::CertificateDer;

use crate::command::{verify_signed, SignatureError};
use crate::tls::{self, Fingerprint};

/// Starts every signed bundle; commands and plugin grants, which the same
/// keys sign, start with their own domain, and certificates and CRLs with
/// 0x30.
const DOMAIN: &[u8] = b"monitorflow-ca-bundle-v1\0";

/// What the signatures cover: the SHA-256 of the certificate the bundle
/// comes with, then the SHA-256 of each CA in the bundle's order.
///
/// The certificate holds the key its peer made for that one renewal, with
/// the peer's identity, so the signatures are worth nothing to another
/// peer, nor to the same one later: no bundle can be replayed to bring a
/// retired CA back.
pub fn signed_bytes(leaf: &Fingerprint, bundle: &[Fingerprint]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(DOMAIN.len() + 36 + 32 * bundle.len());
    bytes.extend_from_slice(DOMAIN);
    bytes.extend_from_slice(leaf);
    // A bundle holds one or two CAs.
    bytes.extend_from_slice(&(bundle.len() as u32).to_be_bytes());
    for ca in bundle {
        bytes.extend_from_slice(ca);
    }
    bytes
}

/// The SHA-256 of the first certificate of `cert_pem`, which
/// [`signed_bytes`] takes.
pub fn leaf_fingerprint(cert_pem: &str) -> Option<Fingerprint> {
    let leaf = rustls_pemfile::certs(&mut cert_pem.as_bytes())
        .next()?
        .ok()?;
    Some(tls::fingerprint(&leaf))
}

/// Checks that one of `signatures` is the signature of `bundle` for the
/// certificate `leaf` by one of `trusted`, the CAs the peer had before.
pub fn verify(
    leaf: &Fingerprint,
    bundle: &[Fingerprint],
    signatures: &[Vec<u8>],
    trusted: &[CertificateDer<'_>],
) -> Result<(), SignatureError> {
    if signatures.is_empty() {
        return Err(SignatureError::Missing);
    }
    let message = signed_bytes(leaf, bundle);
    match signatures
        .iter()
        .any(|signature| verify_signed(&message, signature, trusted).is_ok())
    {
        true => Ok(()),
        false => Err(SignatureError::Invalid),
    }
}

/// A bundle a renewal brought, with the certificate it came with and the
/// server's signatures: what the peer keeps for the root helpers, which
/// have their own copy of the CAs and move it only on such a proof
/// (ADR-0108). Kept by the service's user, so only ever carried.
#[derive(Clone, PartialEq, prost::Message)]
pub struct SignedBundle {
    #[prost(string, tag = "1")]
    pub cert_pem: String,
    #[prost(string, tag = "2")]
    pub ca_bundle_pem: String,
    #[prost(bytes = "vec", repeated, tag = "3")]
    pub signatures: Vec<Vec<u8>>,
}

/// The last bundles kept, oldest first.
#[derive(Clone, PartialEq, prost::Message)]
pub struct SignedBundles {
    #[prost(message, repeated, tag = "1")]
    pub bundles: Vec<SignedBundle>,
}

/// Most bundles kept: a change of CA brings three (introduced, switched,
/// retired).
pub const MAX_KEPT: usize = 4;

/// Where the bundles are kept, next to the CA bundle `ca_cert`.
pub fn kept_path(ca_cert: &std::path::Path) -> std::path::PathBuf {
    ca_cert.with_file_name("ca-bundles.pb")
}

impl SignedBundles {
    /// Adds `bundle`: over the last one when it holds the same CAs, as a
    /// renewal without a change of CA does, else after it.
    pub fn keep(&mut self, bundle: SignedBundle) {
        if self
            .bundles
            .last()
            .is_some_and(|last| last.ca_bundle_pem == bundle.ca_bundle_pem)
        {
            self.bundles.pop();
        }
        self.bundles.push(bundle);
        let extra = self.bundles.len().saturating_sub(MAX_KEPT);
        self.bundles.drain(..extra);
    }
}

#[cfg(test)]
mod tests {
    use ring::rand::SystemRandom;
    use ring::signature::{EcdsaKeyPair, ECDSA_P256_SHA256_ASN1_SIGNING};

    use super::*;

    fn ca() -> (rcgen::KeyPair, CertificateDer<'static>) {
        let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
        let cert = rcgen::CertificateParams::default()
            .self_signed(&key)
            .unwrap();
        (key, cert.der().clone())
    }

    fn sign(key: &rcgen::KeyPair, message: &[u8]) -> Vec<u8> {
        let rng = SystemRandom::new();
        EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &key.serialize_der(), &rng)
            .unwrap()
            .sign(&rng, message)
            .unwrap()
            .as_ref()
            .to_vec()
    }

    #[test]
    fn a_signed_bundle_verifies_and_any_change_breaks_it() {
        let (old_key, old) = ca();
        let (new_key, new) = ca();
        let (leaf, other_leaf) = ([7; 32], [8; 32]);
        let bundle = [tls::fingerprint(&new), tls::fingerprint(&old)];
        let message = signed_bytes(&leaf, &bundle);
        let by_old = sign(&old_key, &message);
        let by_new = sign(&new_key, &message);
        let trusted = std::slice::from_ref(&old);

        assert_eq!(
            verify(&leaf, &bundle, std::slice::from_ref(&by_old), trusted),
            Ok(())
        );
        // One good signature among several is enough, in any place.
        for signatures in [
            vec![by_new.clone(), by_old.clone()],
            vec![by_old.clone(), by_new.clone()],
        ] {
            assert_eq!(verify(&leaf, &bundle, &signatures, trusted), Ok(()));
        }
        assert_eq!(
            verify(&leaf, &bundle, &[], trusted),
            Err(SignatureError::Missing)
        );
        // The new CA cannot speak for itself, and nothing verifies without
        // a trusted CA.
        let invalid = Err(SignatureError::Invalid);
        let by_old = std::slice::from_ref(&by_old);
        assert_eq!(
            verify(&leaf, &bundle, std::slice::from_ref(&by_new), trusted),
            invalid
        );
        assert_eq!(verify(&leaf, &bundle, by_old, &[]), invalid);
        assert_eq!(
            verify(&leaf, &bundle, &[vec![], vec![0; 70]], trusted),
            invalid
        );

        // Another certificate, order, or set of CAs.
        let (_, third) = ca();
        let third = tls::fingerprint(&third);
        assert_eq!(verify(&other_leaf, &bundle, by_old, trusted), invalid);
        for changed in [
            vec![bundle[1], bundle[0]],
            vec![bundle[0]],
            vec![bundle[0], bundle[1], third],
            vec![third, bundle[1]],
        ] {
            assert_eq!(verify(&leaf, &changed, by_old, trusted), invalid);
        }
    }

    #[test]
    fn signatures_of_other_things_are_not_bundle_signatures() {
        let (key, cert) = ca();
        let (leaf, bundle) = ([7; 32], [tls::fingerprint(&cert)]);
        let message = signed_bytes(&leaf, &bundle);
        assert!(message.starts_with(b"monitorflow-ca-bundle-v1\0"));
        assert_eq!(message.len(), DOMAIN.len() + 32 + 4 + 32);
        // The same content under another domain, as a command would be.
        let mut command = b"monitorflow-command-v1\0".to_vec();
        command.extend_from_slice(&message[DOMAIN.len()..]);
        let signature = sign(&key, &command);
        assert_eq!(
            verify(&leaf, &bundle, &[signature], std::slice::from_ref(&cert)),
            Err(SignatureError::Invalid)
        );
    }

    #[test]
    fn the_leaf_is_the_first_certificate() {
        let (_, first) = ca();
        let (_, second) = ca();
        let pem =
            |der: &CertificateDer<'_>| pem::encode(&pem::Pem::new("CERTIFICATE", der.to_vec()));
        let chain = format!("{}{}", pem(&first), pem(&second));
        assert_eq!(leaf_fingerprint(&chain), Some(tls::fingerprint(&first)));
        assert_eq!(leaf_fingerprint("not a certificate"), None);
    }
}
