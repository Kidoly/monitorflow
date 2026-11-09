//! License keys (ADR-0100): signed JSON, checked offline against the
//! public keys built in from `ee/license-keys.pem`.
//!
//! A license key is two PEM-like blocks: `MONITORFLOW LICENSE` holds the
//! JSON in base64, `MONITORFLOW LICENSE SIGNATURE` the raw Ed25519
//! signature of exactly those bytes, as `openssl pkeyutl -sign -rawin`
//! writes it (`scripts/license-sign.sh`). Text around the blocks is
//! ignored, so a pasted email works.

use std::sync::{Arc, OnceLock};

use base64::Engine;
use chrono::NaiveDate;
use monitorflow_common::release::{parse_keys, PublicKey};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The only format this build reads.
pub const FORMAT: u32 = 1;
/// Longest license key accepted, in bytes.
pub const MAX_LEN: usize = 16 * 1024;

const BUILT_IN_KEYS: &str = include_str!(concat!(env!("OUT_DIR"), "/license-keys.pem"));
const LICENSE_BLOCK: &str = "MONITORFLOW LICENSE";
const SIGNATURE_BLOCK: &str = "MONITORFLOW LICENSE SIGNATURE";

/// Why a license key is refused. The messages are shown to administrators.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LicenseError {
    #[error("this build of MonitorFlow trusts no license signing key, so it accepts no license")]
    NoKeys,
    #[error("the built-in license signing keys are invalid: {0}")]
    BadKey(String),
    #[error("this is not a MonitorFlow license key: {0}")]
    Malformed(String),
    #[error(
        "the license key's signature does not verify: it was changed, or is not from MonitorFlow"
    )]
    BadSignature,
    #[error("this license needs a newer version of MonitorFlow")]
    UnknownFormat,
    #[error("this license is for the {0} edition, not the Enterprise edition")]
    WrongEdition(String),
}

/// What a license key says, once its signature verified.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct License {
    pub format: u32,
    pub id: Uuid,
    pub customer: String,
    pub edition: String,
    /// Central servers allowed on the database, the first one included.
    pub server_instances: u32,
    pub issued: NaiveDate,
    /// Valid through this whole day, UTC.
    pub expires: NaiveDate,
}

impl License {
    /// Whether it is valid on `today` (UTC).
    pub fn is_current(&self, today: NaiveDate) -> bool {
        today <= self.expires
    }
}

/// Checks license keys against a set of public keys; clones share them.
#[derive(Debug, Clone)]
pub struct Verifier {
    keys: Result<Arc<[PublicKey]>, LicenseError>,
}

impl Verifier {
    /// The keys this build was made with.
    pub fn built_in() -> Self {
        static BUILT_IN: OnceLock<Verifier> = OnceLock::new();
        BUILT_IN
            .get_or_init(|| Self::from_pem(BUILT_IN_KEYS))
            .clone()
    }

    /// Every `PUBLIC KEY` block of `text`, for tests and the built-in keys.
    pub fn from_pem(text: &str) -> Self {
        let keys = parse_keys(text)
            .map(Arc::from)
            .map_err(|e| LicenseError::BadKey(e.to_string()));
        Self { keys }
    }

    /// Reads and checks `text`: signature, format and edition. The dates
    /// are the caller's to judge: an expired license still says what it
    /// was.
    pub fn verify(&self, text: &str) -> Result<License, LicenseError> {
        let keys = self.keys.as_ref().map_err(Clone::clone)?;
        if keys.is_empty() {
            return Err(LicenseError::NoKeys);
        }
        if text.len() > MAX_LEN {
            return Err(LicenseError::Malformed("the text is too long".into()));
        }
        let payload = block(text, LICENSE_BLOCK)?;
        let signature = block(text, SIGNATURE_BLOCK)?;
        if !keys.iter().any(|key| key.verifies(&payload, &signature)) {
            return Err(LicenseError::BadSignature);
        }
        // Read after the signature: nothing unsigned is parsed.
        #[derive(Deserialize)]
        struct Format {
            format: u32,
        }
        let format: Format = serde_json::from_slice(&payload)
            .map_err(|e| LicenseError::Malformed(format!("invalid content: {e}")))?;
        if format.format != FORMAT {
            return Err(LicenseError::UnknownFormat);
        }
        let license: License = serde_json::from_slice(&payload)
            .map_err(|e| LicenseError::Malformed(format!("invalid content: {e}")))?;
        if license.edition != "enterprise" {
            return Err(LicenseError::WrongEdition(license.edition));
        }
        if license.server_instances == 0 {
            return Err(LicenseError::Malformed(
                "it allows no central server".into(),
            ));
        }
        Ok(license)
    }
}

/// The decoded body of the block `-----BEGIN {label}-----`.
fn block(text: &str, label: &str) -> Result<Vec<u8>, LicenseError> {
    let begin = format!("-----BEGIN {label}-----");
    let end = format!("-----END {label}-----");
    let mut lines = text.lines().map(str::trim);
    if !lines.any(|line| line == begin) {
        return Err(LicenseError::Malformed(format!("no {label} block")));
    }
    let mut body = String::new();
    for line in lines {
        if line == end {
            return base64::engine::general_purpose::STANDARD
                .decode(body.as_bytes())
                .map_err(|e| LicenseError::Malformed(format!("{label}: {e}")));
        }
        body.push_str(line);
    }
    Err(LicenseError::Malformed(format!(
        "the {label} block has no end"
    )))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    /// A throwaway key pair and the PEM of its public key.
    pub(crate) fn key_pair() -> (Ed25519KeyPair, String) {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
        let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let mut der = vec![
            0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
        ];
        der.extend_from_slice(pair.public_key().as_ref());
        let b64 = base64::engine::general_purpose::STANDARD.encode(der);
        let pem = format!("-----BEGIN PUBLIC KEY-----\n{b64}\n-----END PUBLIC KEY-----\n");
        (pair, pem)
    }

    /// `payload` signed by `pair`, as scripts/license-sign.sh writes it.
    pub(crate) fn sign(pair: &Ed25519KeyPair, payload: &str) -> String {
        let engine = base64::engine::general_purpose::STANDARD;
        let wrap = |bytes: &[u8]| {
            let b64 = engine.encode(bytes);
            b64.as_bytes()
                .chunks(64)
                .map(|c| std::str::from_utf8(c).unwrap())
                .collect::<Vec<_>>()
                .join("\n")
        };
        format!(
            "-----BEGIN {LICENSE_BLOCK}-----\n{}\n-----END {LICENSE_BLOCK}-----\n\
             -----BEGIN {SIGNATURE_BLOCK}-----\n{}\n-----END {SIGNATURE_BLOCK}-----\n",
            wrap(payload.as_bytes()),
            wrap(pair.sign(payload.as_bytes()).as_ref()),
        )
    }

    pub(crate) fn payload(instances: u32, expires: &str) -> String {
        serde_json::json!({
            "format": 1,
            "id": "6f1c1d1e-8a43-4c55-9d43-3c2f6c0e9a01",
            "customer": "Example Corp",
            "edition": "enterprise",
            "server_instances": instances,
            "issued": "2026-11-01",
            "expires": expires,
        })
        .to_string()
    }

    #[test]
    fn verifies_a_signed_license() {
        let (pair, pem) = key_pair();
        let text = sign(&pair, &payload(3, "2027-11-01"));
        let license = Verifier::from_pem(&pem).verify(&text).unwrap();
        assert_eq!(license.customer, "Example Corp");
        assert_eq!(license.server_instances, 3);
        assert!(license.is_current("2027-11-01".parse().unwrap()));
        assert!(!license.is_current("2027-11-02".parse().unwrap()));
        // Text around the blocks, as in an email, is ignored.
        let pasted = format!("Hello,\n\nyour license:\n\n{text}\nRegards\n");
        assert_eq!(Verifier::from_pem(&pem).verify(&pasted).unwrap(), license);
    }

    #[test]
    fn refuses_another_key() {
        let (pair, pem) = key_pair();
        let (_, other) = key_pair();
        let text = sign(&pair, &payload(3, "2027-11-01"));
        assert_eq!(
            Verifier::from_pem(&other).verify(&text),
            Err(LicenseError::BadSignature)
        );
        // One of several keys is enough, for a rotation.
        assert!(Verifier::from_pem(&format!("{other}{pem}"))
            .verify(&text)
            .is_ok());
    }

    #[test]
    fn a_tampered_payload_does_not_verify() {
        let (pair, pem) = key_pair();
        let good = sign(&pair, &payload(2, "2027-11-01"));
        // The payload of a license for 9, with the signature of the one for 2.
        let forged_payload = sign(&pair, &payload(9, "2027-11-01"));
        let forged = format!(
            "{}{}",
            &forged_payload[..forged_payload
                .find("-----BEGIN MONITORFLOW LICENSE SIGNATURE")
                .unwrap()],
            &good[good
                .find("-----BEGIN MONITORFLOW LICENSE SIGNATURE")
                .unwrap()..],
        );
        assert_eq!(
            Verifier::from_pem(&pem).verify(&forged),
            Err(LicenseError::BadSignature)
        );
    }

    #[test]
    fn refuses_what_it_cannot_read() {
        let (pair, pem) = key_pair();
        let verifier = Verifier::from_pem(&pem);
        assert!(matches!(
            verifier.verify("hello"),
            Err(LicenseError::Malformed(_))
        ));
        let newer = payload(2, "2027-11-01").replace("\"format\":1", "\"format\":2");
        assert_eq!(
            verifier.verify(&sign(&pair, &newer)),
            Err(LicenseError::UnknownFormat)
        );
        let other = payload(2, "2027-11-01").replace("enterprise", "community");
        assert_eq!(
            verifier.verify(&sign(&pair, &other)),
            Err(LicenseError::WrongEdition("community".into()))
        );
        assert!(matches!(
            verifier.verify(&sign(&pair, &payload(0, "2027-11-01"))),
            Err(LicenseError::Malformed(_))
        ));
    }

    #[test]
    fn no_key_accepts_nothing() {
        let (pair, _) = key_pair();
        let text = sign(&pair, &payload(2, "2027-11-01"));
        assert_eq!(
            Verifier::from_pem("# no key yet\n").verify(&text),
            Err(LicenseError::NoKeys)
        );
    }
}
