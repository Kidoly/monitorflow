//! License keys (ADR-0100, ADR-0147): signed JSON, checked offline against
//! the public keys built in from `ee/license-keys.pem`. One key per
//! subscription, of any tier; format 1 keys, for the Enterprise edition
//! only, still read.
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

/// The newest format this build reads and writes (ADR-0147); format 1
/// keys of ADR-0100 still read.
pub const FORMAT: u32 = 2;
/// Longest license key accepted, in bytes.
pub const MAX_LEN: usize = 16 * 1024;
/// Longest support contact accepted, in characters.
pub const MAX_SUPPORT_LEN: usize = 200;

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

/// A paid subscription (ADR-0147). Community, the free tier, has no key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Basic,
    Standard,
    Premium,
    /// Premium with the Enterprise edition: more than one central server.
    Enterprise,
}

impl Tier {
    /// Its name, as people read it.
    pub fn name(self) -> &'static str {
        match self {
            Tier::Basic => "Basic",
            Tier::Standard => "Standard",
            Tier::Premium => "Premium",
            Tier::Enterprise => "Enterprise",
        }
    }

    /// Whether it includes the Enterprise edition, which adds central
    /// servers.
    pub fn includes_enterprise_edition(self) -> bool {
        self == Tier::Enterprise
    }
}

/// What a license key says, once its signature verified.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct License {
    pub format: u32,
    pub id: Uuid,
    pub customer: String,
    /// `enterprise` when the key includes the Enterprise edition, `core`
    /// otherwise; kept for scripts written against ADR-0100.
    pub edition: String,
    pub tier: Tier,
    /// Managed servers covered; `None` for a format 1 key, which does not
    /// say.
    pub managed_servers: Option<u32>,
    /// Central servers allowed on the database, the first one included.
    pub server_instances: u32,
    /// Where to ask for support: an email address or an `https://` URL.
    pub support: Option<String>,
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

/// Format 1 (ADR-0100): the Enterprise edition only.
#[derive(Deserialize)]
struct V1 {
    id: Uuid,
    customer: String,
    edition: String,
    server_instances: u32,
    issued: NaiveDate,
    expires: NaiveDate,
}

/// Format 2 (ADR-0147): every tier.
#[derive(Deserialize)]
struct V2 {
    id: Uuid,
    customer: String,
    tier: Tier,
    managed_servers: u32,
    server_instances: u32,
    #[serde(default)]
    support: Option<String>,
    issued: NaiveDate,
    expires: NaiveDate,
}

fn content<'a, T: Deserialize<'a>>(payload: &'a [u8]) -> Result<T, LicenseError> {
    serde_json::from_slice(payload)
        .map_err(|e| LicenseError::Malformed(format!("invalid content: {e}")))
}

/// Whether `text` is a contact the License page can link to: an email
/// address or an `https://` URL, without spaces or markup.
pub fn is_contact(text: &str) -> bool {
    let plain = !text.is_empty()
        && text.chars().count() <= MAX_SUPPORT_LEN
        && !text
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || "<>\"'`".contains(c));
    let url = text
        .strip_prefix("https://")
        .is_some_and(|rest| !rest.is_empty());
    let email = !text.contains(':')
        && text.split_once('@').is_some_and(|(user, host)| {
            !user.is_empty() && host.contains('.') && !host.contains('@')
        });
    plain && (url || email)
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

    /// Reads and checks `text`: signature, format and content. The dates
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
        let license = match content::<Format>(&payload)?.format {
            1 => from_v1(content(&payload)?)?,
            2 => from_v2(content(&payload)?)?,
            _ => return Err(LicenseError::UnknownFormat),
        };
        if license.server_instances == 0 {
            return Err(LicenseError::Malformed(
                "it allows no central server".into(),
            ));
        }
        Ok(license)
    }
}

fn from_v1(v1: V1) -> Result<License, LicenseError> {
    if v1.edition != "enterprise" {
        return Err(LicenseError::WrongEdition(v1.edition));
    }
    Ok(License {
        format: 1,
        id: v1.id,
        customer: v1.customer,
        edition: v1.edition,
        tier: Tier::Enterprise,
        managed_servers: None,
        server_instances: v1.server_instances,
        support: None,
        issued: v1.issued,
        expires: v1.expires,
    })
}

fn from_v2(v2: V2) -> Result<License, LicenseError> {
    if v2.managed_servers == 0 {
        return Err(LicenseError::Malformed(
            "it covers no managed server".into(),
        ));
    }
    if v2.server_instances > 1 && !v2.tier.includes_enterprise_edition() {
        return Err(LicenseError::Malformed(format!(
            "a {} subscription allows one central server only",
            v2.tier.name()
        )));
    }
    let support = v2.support.filter(|s| !s.is_empty());
    if support.as_deref().is_some_and(|s| !is_contact(s)) {
        return Err(LicenseError::Malformed(
            "the support contact is neither an email address nor an https:// address".into(),
        ));
    }
    Ok(License {
        format: 2,
        id: v2.id,
        customer: v2.customer,
        edition: if v2.tier.includes_enterprise_edition() {
            "enterprise"
        } else {
            "core"
        }
        .into(),
        tier: v2.tier,
        managed_servers: Some(v2.managed_servers),
        server_instances: v2.server_instances,
        support,
        issued: v2.issued,
        expires: v2.expires,
    })
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
        let newer = payload(2, "2027-11-01").replace("\"format\":1", "\"format\":3");
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

    /// A format 2 payload (ADR-0147).
    pub(crate) fn payload_v2(tier: &str, managed: u32, instances: u32, expires: &str) -> String {
        serde_json::json!({
            "format": 2,
            "id": "0d7c2a4e-5b1f-4c3d-8e9a-1f2b3c4d5e6f",
            "customer": "Example Corp",
            "tier": tier,
            "managed_servers": managed,
            "server_instances": instances,
            "support": "support@example.com",
            "issued": "2026-11-01",
            "expires": expires,
        })
        .to_string()
    }

    #[test]
    fn reads_every_tier() {
        let (pair, pem) = key_pair();
        let verifier = Verifier::from_pem(&pem);
        for (name, tier) in [
            ("basic", Tier::Basic),
            ("standard", Tier::Standard),
            ("premium", Tier::Premium),
            ("enterprise", Tier::Enterprise),
        ] {
            let text = sign(&pair, &payload_v2(name, 40, 1, "2027-11-01"));
            let license = verifier.verify(&text).unwrap();
            assert_eq!(license.tier, tier);
            assert_eq!(license.format, 2);
            assert_eq!(license.managed_servers, Some(40));
            assert_eq!(license.server_instances, 1);
            assert_eq!(license.support.as_deref(), Some("support@example.com"));
            let edition = if tier == Tier::Enterprise {
                "enterprise"
            } else {
                "core"
            };
            assert_eq!(license.edition, edition);
        }
        let enterprise = sign(&pair, &payload_v2("enterprise", 40, 3, "2027-11-01"));
        assert_eq!(verifier.verify(&enterprise).unwrap().server_instances, 3);
    }

    #[test]
    fn a_format_1_key_reads_as_enterprise() {
        let (pair, pem) = key_pair();
        let license = Verifier::from_pem(&pem)
            .verify(&sign(&pair, &payload(2, "2027-11-01")))
            .unwrap();
        assert_eq!(license.format, 1);
        assert_eq!(license.tier, Tier::Enterprise);
        assert_eq!(license.edition, "enterprise");
        assert_eq!(license.managed_servers, None);
        assert_eq!(license.support, None);
    }

    #[test]
    fn refuses_format_2_keys_that_make_no_sense() {
        let (pair, pem) = key_pair();
        let verifier = Verifier::from_pem(&pem);
        let refused = |payload: String| {
            matches!(
                verifier.verify(&sign(&pair, &payload)),
                Err(LicenseError::Malformed(_))
            )
        };
        // Only Enterprise allows more than one central server.
        assert!(refused(payload_v2("premium", 40, 2, "2027-11-01")));
        assert!(refused(payload_v2("standard", 0, 1, "2027-11-01")));
        assert!(refused(payload_v2("enterprise", 40, 0, "2027-11-01")));
        assert!(refused(payload_v2("gold", 40, 1, "2027-11-01")));
        for contact in [
            "javascript:alert(1)",
            "http://example.com",
            "a b@example.com",
            "<x>@y.z",
        ] {
            let payload =
                payload_v2("basic", 40, 1, "2027-11-01").replace("support@example.com", contact);
            assert!(refused(payload), "{contact}");
        }
        // No contact at all is fine.
        let without = payload_v2("basic", 40, 1, "2027-11-01")
            .replace("\"support\":\"support@example.com\",", "");
        assert_eq!(
            verifier.verify(&sign(&pair, &without)).unwrap().support,
            None
        );
    }

    #[test]
    fn contacts() {
        assert!(is_contact("support@example.com"));
        assert!(is_contact("https://example.com/support"));
        assert!(!is_contact("https://"));
        assert!(!is_contact("mailto:support@example.com"));
        assert!(!is_contact("support"));
        assert!(!is_contact(&format!(
            "https://{}",
            "a".repeat(MAX_SUPPORT_LEN)
        )));
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
