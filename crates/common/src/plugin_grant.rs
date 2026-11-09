//! Grants of check plugins (ADR-0103): the server signs, with its command
//! key, that the agent `agent_id` may run the package of SHA-256 `sha256`
//! as the plugin `name`. The agent runs no plugin without one, so a relay,
//! which forwards configurations, cannot give an agent a module.

use rustls::pki_types::CertificateDer;

use crate::command::{verify_signed, SignatureError};
use crate::proto::v1::PluginGrant;

/// Starts every signed grant; commands start with their own domain.
const DOMAIN: &[u8] = b"monitorflow-plugin-grant-v1\0";

/// What the grant's signature covers, each text length-prefixed.
pub fn signed_bytes(agent_id: &str, name: &str, sha256: &[u8; 32]) -> Vec<u8> {
    let mut bytes = DOMAIN.to_vec();
    for field in [agent_id.as_bytes(), name.as_bytes()] {
        // IDs and plugin names are under 100 bytes.
        bytes.extend_from_slice(&(field.len() as u32).to_be_bytes());
        bytes.extend_from_slice(field);
    }
    bytes.extend_from_slice(sha256);
    bytes
}

/// The SHA-256 of `bytes`.
pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    crate::release::sha256(bytes)
}

/// Checks that `grant` was signed for `agent_id` by any of `ca_certs`.
pub fn verify(
    grant: &PluginGrant,
    agent_id: &str,
    ca_certs: &[CertificateDer<'_>],
) -> Result<(), SignatureError> {
    if grant.signature.is_empty() {
        return Err(SignatureError::Missing);
    }
    let message = signed_bytes(agent_id, &grant.name, &sha256(&grant.package));
    verify_signed(&message, &grant.signature, ca_certs)
}

#[cfg(test)]
mod tests {
    use ring::rand::SystemRandom;
    use ring::signature::{EcdsaKeyPair, ECDSA_P256_SHA256_ASN1_SIGNING};

    use super::*;

    #[test]
    fn a_grant_holds_for_its_agent_name_and_package_only() {
        let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
        let cert = rcgen::CertificateParams::default()
            .self_signed(&key)
            .unwrap();
        let certs = [cert.der().clone()];
        let pair = EcdsaKeyPair::from_pkcs8(
            &ECDSA_P256_SHA256_ASN1_SIGNING,
            &key.serialize_der(),
            &SystemRandom::new(),
        )
        .unwrap();
        let package = b"package bytes".to_vec();
        let message = signed_bytes("web1", "http-json", &sha256(&package));
        let grant = PluginGrant {
            name: "http-json".into(),
            package: package.clone(),
            signature: pair
                .sign(&SystemRandom::new(), &message)
                .unwrap()
                .as_ref()
                .to_vec(),
        };
        assert_eq!(verify(&grant, "web1", &certs), Ok(()));
        assert_eq!(verify(&grant, "web2", &certs), Err(SignatureError::Invalid));
        let renamed = PluginGrant {
            name: "other".into(),
            ..grant.clone()
        };
        assert_eq!(
            verify(&renamed, "web1", &certs),
            Err(SignatureError::Invalid)
        );
        let changed = PluginGrant {
            package: b"other bytes".to_vec(),
            ..grant.clone()
        };
        assert_eq!(
            verify(&changed, "web1", &certs),
            Err(SignatureError::Invalid)
        );
        let unsigned = PluginGrant {
            signature: Vec::new(),
            ..grant
        };
        assert_eq!(
            verify(&unsigned, "web1", &certs),
            Err(SignatureError::Missing)
        );
    }
}
