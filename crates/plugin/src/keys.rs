//! Ed25519 keys that sign plugins (ADR-0103): the catalogue's, built in
//! from `deploy/plugin-keys.pem`, and the publishers' an administrator
//! trusts. Private keys exist only on the publisher's machine, written by
//! `monitorflow-plugin keygen`.

use std::fmt;
use std::sync::OnceLock;

use base64::Engine as _;
use ring::signature::{UnparsedPublicKey, ED25519};

const BUILT_IN_KEYS: &str = include_str!(concat!(env!("OUT_DIR"), "/plugin-keys.pem"));

/// DER prefix of an Ed25519 SubjectPublicKeyInfo (RFC 8410).
const SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not an Ed25519 public key: {0}")]
pub struct KeyError(String);

/// An Ed25519 public key.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PublicKey(pub [u8; 32]);

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("PublicKey").field(&self.to_base64()).finish()
    }
}

impl PublicKey {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, KeyError> {
        <[u8; 32]>::try_from(bytes)
            .map(Self)
            .map_err(|_| KeyError(format!("{} bytes, not 32", bytes.len())))
    }

    /// The 32 bytes in standard base64, as packages carry them, or a PEM
    /// block, as `keygen` prints it.
    pub fn parse(text: &str) -> Result<Self, KeyError> {
        let text = text.trim();
        if text.starts_with("-----BEGIN") {
            let mut keys = parse_pem(text)?;
            return match (keys.pop(), keys.is_empty()) {
                (Some(key), true) => Ok(key),
                _ => Err(KeyError("expected one PEM block".into())),
            };
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(text)
            .map_err(|e| KeyError(e.to_string()))?;
        Self::from_bytes(&bytes)
    }

    pub fn to_base64(&self) -> String {
        base64::engine::general_purpose::STANDARD.encode(self.0)
    }

    /// The PEM SubjectPublicKeyInfo, as `deploy/plugin-keys.pem` holds it.
    pub fn to_pem(&self) -> String {
        let mut der = SPKI_PREFIX.to_vec();
        der.extend_from_slice(&self.0);
        pem::encode(&pem::Pem::new("PUBLIC KEY", der))
    }

    /// A short fingerprint for screens and the audit log: the first 16
    /// hex digits of the key's SHA-256.
    pub fn fingerprint(&self) -> String {
        let digest = ring::digest::digest(&ring::digest::SHA256, &self.0);
        digest.as_ref()[..8]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    pub fn verifies(&self, message: &[u8], signature: &[u8]) -> bool {
        UnparsedPublicKey::new(&ED25519, self.0)
            .verify(message, signature)
            .is_ok()
    }
}

/// Every `PUBLIC KEY` block of `text`; other text is ignored.
pub fn parse_pem(text: &str) -> Result<Vec<PublicKey>, KeyError> {
    pem::parse_many(text)
        .map_err(|e| KeyError(e.to_string()))?
        .into_iter()
        .filter(|block| block.tag() == "PUBLIC KEY")
        .map(|block| {
            block
                .contents()
                .strip_prefix(&SPKI_PREFIX[..])
                .ok_or_else(|| KeyError("not an Ed25519 key".into()))
                .and_then(PublicKey::from_bytes)
        })
        .collect()
}

/// The catalogue keys this build trusts; none until the owner adds one.
/// A file that does not parse trusts nothing, and says so in the log.
pub fn official() -> &'static [PublicKey] {
    static KEYS: OnceLock<Vec<PublicKey>> = OnceLock::new();
    KEYS.get_or_init(|| {
        parse_pem(BUILT_IN_KEYS).unwrap_or_else(|error| {
            tracing::error!(%error, "the built-in plugin keys do not parse");
            Vec::new()
        })
    })
}

#[cfg(test)]
mod tests {
    use ring::rand::SystemRandom;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    use super::*;

    #[test]
    fn keys_round_trip_through_base64_and_pem() {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
        let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let key = PublicKey::from_bytes(pair.public_key().as_ref()).unwrap();
        assert_eq!(PublicKey::parse(&key.to_base64()).unwrap(), key);
        assert_eq!(PublicKey::parse(&key.to_pem()).unwrap(), key);
        assert_eq!(
            parse_pem(&format!("# comment\n{}", key.to_pem())).unwrap(),
            [key]
        );
        assert_eq!(key.fingerprint().len(), 16);
        let signature = pair.sign(b"message");
        assert!(key.verifies(b"message", signature.as_ref()));
        assert!(!key.verifies(b"other", signature.as_ref()));
        assert!(PublicKey::parse("AAAA").is_err());
        assert!(PublicKey::parse(&format!("{}{}", key.to_pem(), key.to_pem())).is_err());
    }

    #[test]
    fn the_built_in_keys_parse() {
        assert_eq!(parse_pem(BUILT_IN_KEYS).unwrap(), official());
    }
}
