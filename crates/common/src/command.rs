//! Signed commands (ADR-0031): the bytes the server signs with its CA key
//! and the agent's check against the CA certificates it trusts.

use ring::signature::{UnparsedPublicKey, ECDSA_P256_SHA256_ASN1};
use rustls::pki_types::CertificateDer;

use crate::proto::v1::Command;

/// Kind of [`crate::proto::v1::ApplyUpdates`] commands.
pub const APPLY_UPDATES: &str = "apply_updates";
/// Kind of [`crate::proto::v1::ConfigureSources`] commands (ADR-0035).
pub const CONFIGURE_SOURCES: &str = "configure_sources";
/// Kind of [`crate::proto::v1::RevertSources`] commands (ADR-0035).
pub const REVERT_SOURCES: &str = "revert_sources";
/// Kind of [`crate::proto::v1::ShellCommand`] commands (ADR-0043).
pub const SHELL: &str = "shell";

/// Package names in an `ApplyUpdates` reach a command line on the agent:
/// letters, digits and `.+_:-`, at most 128 bytes, not starting with `-` so
/// none reads as an option. The API refuses others, the agent too.
pub fn valid_package_name(name: &str) -> bool {
    name.len() <= 128
        && name
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._+:-".contains(&b))
}

/// Names of Windows updates in an `ApplyUpdates` for a Windows agent
/// (ADR-0049): `KB` and 1 to 10 digits, or an update ID (a GUID), case
/// ignored. Both are also valid package names.
pub fn valid_windows_update_name(name: &str) -> bool {
    let kb = name.get(..2).is_some_and(|p| p.eq_ignore_ascii_case("kb"))
        && (3..=12).contains(&name.len())
        && name.bytes().skip(2).all(|b| b.is_ascii_digit());
    let guid = name.len() == 36
        && name.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        });
    kb || guid
}

/// The first of `names` an agent of `os_family` cannot take: Windows
/// agents only take Windows update names, Linux ones none of them, so one
/// platform's names never reach the other (ADR-0049).
pub fn unfit_name<'a>(os_family: &str, names: &'a [String]) -> Option<&'a str> {
    let windows = os_family == "windows";
    names
        .iter()
        .find(|n| valid_windows_update_name(n) != windows)
        .map(String::as_str)
}

/// Starts every signed message. Certificates and CRLs, which the same key
/// signs, are DER and start with 0x30, so neither can pass for the other.
const DOMAIN: &[u8] = b"monitorflow-command-v1\0";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SignatureError {
    #[error("the command is not signed")]
    Missing,
    #[error("the command signature does not verify against the CA")]
    Invalid,
}

/// What the signature covers: every field of `command` but the signature,
/// each length-prefixed so no two commands share the same bytes.
pub fn signed_bytes(command: &Command) -> Vec<u8> {
    let fields = [
        command.agent_id.as_bytes(),
        command.command_id.as_bytes(),
        command.kind.as_bytes(),
        &command.payload,
    ];
    let len = DOMAIN.len() + fields.iter().map(|f| 4 + f.len()).sum::<usize>() + 16;
    let mut bytes = Vec::with_capacity(len);
    bytes.extend_from_slice(DOMAIN);
    for field in fields {
        // Commands are far below 4 GiB: the stream refuses messages over 4 MiB.
        bytes.extend_from_slice(&(field.len() as u32).to_be_bytes());
        bytes.extend_from_slice(field);
    }
    bytes.extend_from_slice(&command.issued_at_unix_ms.to_be_bytes());
    bytes.extend_from_slice(&command.expires_at_unix_ms.to_be_bytes());
    bytes
}

/// Checks the signature of `command` against the public key of any of
/// `ca_certs`. Certificates that do not parse are skipped.
pub fn verify(command: &Command, ca_certs: &[CertificateDer<'_>]) -> Result<(), SignatureError> {
    if command.signature.is_empty() {
        return Err(SignatureError::Missing);
    }
    let message = signed_bytes(command);
    let valid = ca_certs.iter().any(|der| {
        let Ok((_, cert)) = x509_parser::parse_x509_certificate(der) else {
            return false;
        };
        let key = &cert.public_key().subject_public_key.data;
        UnparsedPublicKey::new(&ECDSA_P256_SHA256_ASN1, key)
            .verify(&message, &command.signature)
            .is_ok()
    });
    if valid {
        Ok(())
    } else {
        Err(SignatureError::Invalid)
    }
}

#[cfg(test)]
mod tests {
    use ring::rand::SystemRandom;
    use ring::signature::{EcdsaKeyPair, ECDSA_P256_SHA256_ASN1_SIGNING};

    use super::*;

    #[test]
    fn windows_update_names() {
        for good in [
            "KB5034441",
            "kb890830",
            "KB1",
            "KB1234567890",
            "4f2fc5ad-8a6b-4b61-a0f7-1a2e0f3b9c01",
            "4F2FC5AD-8A6B-4B61-A0F7-1A2E0F3B9C01",
        ] {
            assert!(valid_windows_update_name(good), "{good}");
            assert!(valid_package_name(good), "{good}");
        }
        for bad in [
            "KB",
            "KB12345678901",
            "KB-1",
            "KB 1",
            "5034441",
            "openssl",
            "4f2fc5ad8a6b4b61a0f71a2e0f3b9c01",
            "4f2fc5ad-8a6b-4b61-a0f7-1a2e0f3b9c0g",
            "ÄB1",
        ] {
            assert!(!valid_windows_update_name(bad), "{bad}");
        }
    }

    #[test]
    fn names_must_fit_the_os_family() {
        let names = |n: &[&str]| n.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            unfit_name("windows", &names(&["KB1", "openssl"])),
            Some("openssl")
        );
        assert_eq!(unfit_name("windows", &names(&["KB1", "kb2"])), None);
        assert_eq!(
            unfit_name("debian", &names(&["openssl", "KB1"])),
            Some("KB1")
        );
        assert_eq!(unfit_name("rhel", &names(&["openssl"])), None);
        assert_eq!(unfit_name("windows", &[]), None);
    }

    fn ca() -> (rcgen::KeyPair, CertificateDer<'static>) {
        let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
        let cert = rcgen::CertificateParams::default()
            .self_signed(&key)
            .unwrap();
        (key, cert.der().clone())
    }

    fn sign(key: &rcgen::KeyPair, command: &mut Command) {
        let rng = SystemRandom::new();
        let pair =
            EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &key.serialize_der(), &rng)
                .unwrap();
        command.signature = pair
            .sign(&rng, &signed_bytes(command))
            .unwrap()
            .as_ref()
            .to_vec();
    }

    fn command() -> Command {
        Command {
            command_id: "0b5c".into(),
            kind: APPLY_UPDATES.into(),
            payload: vec![1, 2, 3],
            issued_at_unix_ms: 1_000,
            expires_at_unix_ms: 2_000,
            agent_id: "web-01".into(),
            signature: Vec::new(),
        }
    }

    #[test]
    fn a_signed_command_verifies_and_any_change_breaks_it() {
        let (key, cert) = ca();
        let (_, other) = ca();
        let mut signed = command();
        assert_eq!(
            verify(&signed, std::slice::from_ref(&cert)),
            Err(SignatureError::Missing)
        );
        sign(&key, &mut signed);
        assert_eq!(verify(&signed, std::slice::from_ref(&cert)), Ok(()));
        assert_eq!(verify(&signed, &[other.clone(), cert.clone()]), Ok(()));
        assert_eq!(verify(&signed, &[other]), Err(SignatureError::Invalid));

        let changes: [fn(&mut Command); 6] = [
            |c| c.agent_id = "web-02".into(),
            |c| c.command_id.push('0'),
            |c| c.kind = "shell".into(),
            |c| c.payload.push(4),
            |c| c.issued_at_unix_ms += 1,
            |c| c.expires_at_unix_ms += 1,
        ];
        for change in changes {
            let mut forged = signed.clone();
            change(&mut forged);
            assert_eq!(
                verify(&forged, std::slice::from_ref(&cert)),
                Err(SignatureError::Invalid)
            );
        }
    }

    #[test]
    fn fields_are_length_prefixed() {
        // Moving a byte from one field to the next changes the message.
        let a = Command {
            agent_id: "ab".into(),
            command_id: "c".into(),
            ..command()
        };
        let b = Command {
            agent_id: "a".into(),
            command_id: "bc".into(),
            ..command()
        };
        assert_ne!(signed_bytes(&a), signed_bytes(&b));
        assert!(signed_bytes(&a).starts_with(b"monitorflow-command-v1\0"));
    }
}
