//! Enrollment token string, `mfe1.<secret>.<ca>` (ADR-0006).
//!
//! `<secret>` is 32 random bytes and `<ca>` the SHA-256 of the DER CA
//! certificate, both in unpadded base64url. The server stores only a hash of
//! the secret; the enrolling peer uses the fingerprint to authenticate the
//! server before it sends the token.

use std::fmt;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rustls::pki_types::CertificateDer;

use super::TlsError;
use crate::redact::{Secret, REDACTED};

/// Version prefix of the token string.
pub const TOKEN_PREFIX: &str = "mfe1";

/// Length of the random secret in a token.
pub const SECRET_LEN: usize = 32;

/// SHA-256 of a DER certificate.
pub type Fingerprint = [u8; 32];

/// SHA-256 fingerprint of a DER certificate.
pub fn fingerprint(cert: &CertificateDer<'_>) -> Fingerprint {
    let digest = ring::digest::digest(&ring::digest::SHA256, cert.as_ref());
    digest
        .as_ref()
        .try_into()
        .expect("SHA-256 digests are 32 bytes")
}

/// Lowercase hexadecimal, as `openssl x509 -fingerprint -sha256` shows it
/// without the colons.
pub fn fingerprint_hex(fingerprint: &Fingerprint) -> String {
    fingerprint.iter().map(|b| format!("{b:02x}")).collect()
}

/// A parsed enrollment token. `Debug` hides the secret.
#[derive(Clone, PartialEq, Eq)]
pub struct EnrollmentToken {
    secret: [u8; SECRET_LEN],
    ca_fingerprint: Fingerprint,
}

impl fmt::Debug for EnrollmentToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EnrollmentToken")
            .field("secret", &REDACTED)
            .field("ca_fingerprint", &fingerprint_hex(&self.ca_fingerprint))
            .finish()
    }
}

impl EnrollmentToken {
    pub fn new(secret: [u8; SECRET_LEN], ca_fingerprint: Fingerprint) -> Self {
        Self {
            secret,
            ca_fingerprint,
        }
    }

    pub fn secret(&self) -> &[u8; SECRET_LEN] {
        &self.secret
    }

    pub fn ca_fingerprint(&self) -> &Fingerprint {
        &self.ca_fingerprint
    }

    /// Parses a token string; surrounding whitespace is ignored. Errors never
    /// quote the input.
    pub fn parse(token: &str) -> Result<Self, TlsError> {
        let invalid = |reason: &'static str| TlsError::InvalidToken(reason);
        let mut parts = token.trim().split('.');
        if parts.next() != Some(TOKEN_PREFIX) {
            return Err(invalid("unknown prefix"));
        }
        let mut field = |what| {
            let bytes = URL_SAFE_NO_PAD
                .decode(parts.next().ok_or(invalid(what))?)
                .map_err(|_| invalid(what))?;
            <[u8; 32]>::try_from(bytes).map_err(|_| invalid(what))
        };
        let secret = field("bad secret")?;
        let ca_fingerprint = field("bad CA fingerprint")?;
        if parts.next().is_some() {
            return Err(invalid("trailing data"));
        }
        Ok(Self::new(secret, ca_fingerprint))
    }

    /// The token string, to hand to the administrator once.
    pub fn to_secret_string(&self) -> Secret {
        Secret::new(format!(
            "{TOKEN_PREFIX}.{}.{}",
            URL_SAFE_NO_PAD.encode(self.secret),
            URL_SAFE_NO_PAD.encode(self.ca_fingerprint)
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token() -> EnrollmentToken {
        let mut secret = [0u8; SECRET_LEN];
        secret
            .iter_mut()
            .enumerate()
            .for_each(|(i, b)| *b = i as u8);
        EnrollmentToken::new(secret, [0xab; 32])
    }

    #[test]
    fn round_trips_through_its_string() {
        let text = token().to_secret_string();
        assert!(text.expose().starts_with("mfe1."));
        assert_eq!(text.expose().len(), 92);
        assert_eq!(EnrollmentToken::parse(text.expose()).unwrap(), token());
        let padded = format!("  {}\n", text.expose());
        assert_eq!(EnrollmentToken::parse(&padded).unwrap(), token());
    }

    #[test]
    fn rejects_malformed_tokens_without_echoing_them() {
        let good = token().to_secret_string().expose().to_owned();
        let (_, rest) = good.split_once('.').unwrap();
        let (secret, ca) = rest.split_once('.').unwrap();
        for bad in [
            String::new(),
            format!("mfe2.{rest}"),
            format!("mfe1.{secret}"),
            format!("mfe1.{secret}.{ca}.x"),
            format!("mfe1.{}.{ca}", &secret[1..]),
            format!("mfe1.{secret}.{ca}A"),
            format!("mfe1.{secret}=.{ca}"),
        ] {
            let err = EnrollmentToken::parse(&bad).unwrap_err().to_string();
            assert!(
                err.starts_with("malformed enrollment token"),
                "{bad}: {err}"
            );
            assert!(!err.contains(secret), "{err}");
        }
    }

    #[test]
    fn debug_hides_the_secret() {
        let debug = format!("{:?}", token());
        let secret = URL_SAFE_NO_PAD.encode(token().secret());
        assert!(debug.contains(REDACTED), "{debug}");
        assert!(!debug.contains(&secret), "{debug}");
        assert!(debug.contains(&"ab".repeat(32)), "{debug}");
    }

    #[test]
    fn fingerprint_is_sha256_of_der() {
        let der = CertificateDer::from(b"abc".to_vec());
        assert_eq!(
            fingerprint_hex(&fingerprint(&der)),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
