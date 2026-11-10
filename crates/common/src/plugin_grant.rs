//! Grants of check plugins (ADR-0103): the server signs, with its command
//! key, that the agent `agent_id` may run the package of SHA-256 `sha256`
//! as the plugin `name`. The agent runs no plugin without one, so a relay,
//! which forwards configurations, cannot give an agent a module.
//!
//! A grant alone never ends: a relay could keep one and send it again
//! after the plugin was updated, turned off or removed. So the server also
//! signs the grants of a configuration as a set, with the time: the agent
//! takes no set older than the newest it has seen.

use rustls::pki_types::CertificateDer;

use crate::command::{verify_signed, SignatureError};
use crate::proto::v1::{PluginGrant, PluginGrantSet};

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

/// Starts every signed set of grants.
const SET_DOMAIN: &[u8] = b"monitorflow-plugin-grants-v2\0";

/// How long a set holds: a week, the life of a certificate. An agent gets
/// a new one with every configuration, so only one that stays away longer
/// starts without its plugins until it connects.
pub const SET_LIFETIME_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// What a set's signature covers: the agent, both times, and the name and
/// package SHA-256 of each of `grants`, in order.
pub fn set_signed_bytes(
    agent_id: &str,
    issued_at_unix_ms: i64,
    expires_at_unix_ms: i64,
    grants: &[PluginGrant],
) -> Vec<u8> {
    let mut bytes = SET_DOMAIN.to_vec();
    bytes.extend_from_slice(&(agent_id.len() as u32).to_be_bytes());
    bytes.extend_from_slice(agent_id.as_bytes());
    bytes.extend_from_slice(&issued_at_unix_ms.to_be_bytes());
    bytes.extend_from_slice(&expires_at_unix_ms.to_be_bytes());
    bytes.extend_from_slice(&(grants.len() as u32).to_be_bytes());
    for grant in grants {
        bytes.extend_from_slice(&(grant.name.len() as u32).to_be_bytes());
        bytes.extend_from_slice(grant.name.as_bytes());
        bytes.extend_from_slice(&sha256(&grant.package));
    }
    bytes
}

/// Checks that `set` was signed for `agent_id` and exactly `grants` by any
/// of `ca_certs`. Its times are the caller's to judge.
pub fn verify_set(
    set: &PluginGrantSet,
    grants: &[PluginGrant],
    agent_id: &str,
    ca_certs: &[CertificateDer<'_>],
) -> Result<(), SignatureError> {
    if set.signature.is_empty() {
        return Err(SignatureError::Missing);
    }
    let message = set_signed_bytes(
        agent_id,
        set.issued_at_unix_ms,
        set.expires_at_unix_ms,
        grants,
    );
    verify_signed(&message, &set.signature, ca_certs)
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
            ..grant.clone()
        };
        assert_eq!(
            verify(&unsigned, "web1", &certs),
            Err(SignatureError::Missing)
        );

        // The set: these grants, for this agent, at these times.
        let sign = |grants: &[PluginGrant]| {
            let message = set_signed_bytes("web1", 1_000, 2_000, grants);
            PluginGrantSet {
                issued_at_unix_ms: 1_000,
                expires_at_unix_ms: 2_000,
                signature: pair
                    .sign(&SystemRandom::new(), &message)
                    .unwrap()
                    .as_ref()
                    .to_vec(),
            }
        };
        let grants = [grant.clone(), renamed.clone()];
        let set = sign(&grants);
        assert_eq!(verify_set(&set, &grants, "web1", &certs), Ok(()));
        let invalid = Err(SignatureError::Invalid);
        assert_eq!(verify_set(&set, &grants, "web2", &certs), invalid);
        assert_eq!(verify_set(&set, &grants[..1], "web1", &certs), invalid);
        assert_eq!(verify_set(&set, &[], "web1", &certs), invalid);
        let swapped = [renamed, grant.clone()];
        assert_eq!(verify_set(&set, &swapped, "web1", &certs), invalid);
        let other = [grant, changed];
        assert_eq!(verify_set(&set, &other, "web1", &certs), invalid);
        for later in [
            PluginGrantSet {
                issued_at_unix_ms: 1_001,
                ..set.clone()
            },
            PluginGrantSet {
                expires_at_unix_ms: 2_001,
                ..set.clone()
            },
        ] {
            assert_eq!(verify_set(&later, &grants, "web1", &certs), invalid);
        }
        // No plugin at all is signed too.
        let none = sign(&[]);
        assert_eq!(verify_set(&none, &[], "web1", &certs), Ok(()));
        assert_eq!(verify_set(&none, &grants, "web1", &certs), invalid);
        let unsigned = PluginGrantSet {
            signature: Vec::new(),
            ..none
        };
        assert_eq!(
            verify_set(&unsigned, &[], "web1", &certs),
            Err(SignatureError::Missing)
        );
    }
}
