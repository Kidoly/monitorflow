//! JSON Web Signatures checked against a JSON Web Key Set, with `ring`
//! (RFC 7515, RFC 7517, RFC 7518, RFC 8037; ADR-0145). Only asymmetric
//! algorithms: never `none`, never HMAC, whose key would be the client
//! secret.

use base64::alphabet::URL_SAFE;
use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig};
use base64::engine::DecodePaddingMode;
use base64::Engine;
use ring::signature::{self, RsaPublicKeyComponents, UnparsedPublicKey};
use serde::Deserialize;
use serde_json::{Map, Value};

/// Largest token accepted; ID tokens are a few kilobytes.
pub const MAX_TOKEN: usize = 64 * 1024;

/// base64url with or without padding.
const B64: GeneralPurpose = GeneralPurpose::new(
    &URL_SAFE,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// The algorithms accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alg {
    Rs256,
    Rs384,
    Rs512,
    Ps256,
    Ps384,
    Ps512,
    Es256,
    Es384,
    EdDsa,
}

impl Alg {
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "RS256" => Self::Rs256,
            "RS384" => Self::Rs384,
            "RS512" => Self::Rs512,
            "PS256" => Self::Ps256,
            "PS384" => Self::Ps384,
            "PS512" => Self::Ps512,
            "ES256" => Self::Es256,
            "ES384" => Self::Es384,
            "EdDSA" => Self::EdDsa,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Rs256 => "RS256",
            Self::Rs384 => "RS384",
            Self::Rs512 => "RS512",
            Self::Ps256 => "PS256",
            Self::Ps384 => "PS384",
            Self::Ps512 => "PS512",
            Self::Es256 => "ES256",
            Self::Es384 => "ES384",
            Self::EdDsa => "EdDSA",
        }
    }

    /// The `kty` of the keys that verify it.
    fn key_type(self) -> &'static str {
        match self {
            Self::Rs256 | Self::Rs384 | Self::Rs512 | Self::Ps256 | Self::Ps384 | Self::Ps512 => {
                "RSA"
            }
            Self::Es256 | Self::Es384 => "EC",
            Self::EdDsa => "OKP",
        }
    }
}

/// One key of a JWKS; fields of other key types are absent.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Jwk {
    pub kty: String,
    #[serde(default)]
    pub kid: Option<String>,
    #[serde(default)]
    pub alg: Option<String>,
    #[serde(default, rename = "use")]
    pub use_: Option<String>,
    #[serde(default)]
    pub n: Option<String>,
    #[serde(default)]
    pub e: Option<String>,
    #[serde(default)]
    pub crv: Option<String>,
    #[serde(default)]
    pub x: Option<String>,
    #[serde(default)]
    pub y: Option<String>,
}

/// A JWKS. Keys that do not parse are left out rather than refusing the
/// set: providers publish encryption and symmetric keys alongside.
#[derive(Debug, Clone, Default)]
pub struct JwkSet {
    pub keys: Vec<Jwk>,
}

impl JwkSet {
    pub fn parse(body: &[u8]) -> Result<Self, JwsError> {
        #[derive(Deserialize)]
        struct Raw {
            keys: Vec<Value>,
        }
        let raw: Raw = serde_json::from_slice(body).map_err(|_| JwsError::BadKeySet)?;
        let keys = raw
            .keys
            .into_iter()
            .filter_map(|k| serde_json::from_value::<Jwk>(k).ok())
            .collect();
        Ok(Self { keys })
    }

    /// Keys that could verify a signature: signing keys of a type this
    /// module reads.
    pub fn usable(&self) -> usize {
        self.keys
            .iter()
            .filter(|k| k.use_.as_deref() != Some("enc"))
            .filter(|k| {
                [Alg::Rs256, Alg::Es256, Alg::Es384, Alg::EdDsa]
                    .iter()
                    .any(|a| fits(k, *a))
            })
            .count()
    }

    /// Whether the set has a key named `kid`.
    pub fn has(&self, kid: &str) -> bool {
        self.keys.iter().any(|k| k.kid.as_deref() == Some(kid))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JwsError {
    #[error("the token is not a signed JWT")]
    Malformed,
    #[error("the token is signed with {0}, which MonitorFlow does not accept; use RS256 or ES256")]
    Algorithm(String),
    #[error("the token asks for extensions MonitorFlow does not understand")]
    Critical,
    /// No key of the set may verify it: fetch the set again once.
    #[error("the token is signed with a key the provider does not publish")]
    UnknownKey,
    #[error("the token's signature is not valid")]
    BadSignature,
    #[error("the provider's key set is not valid JSON")]
    BadKeySet,
}

/// The header fields this module reads.
#[derive(Debug, Deserialize)]
struct Header {
    alg: String,
    #[serde(default)]
    kid: Option<String>,
    #[serde(default)]
    crit: Option<Value>,
}

/// A verified token's header and claims.
#[derive(Debug, Clone)]
pub struct Verified {
    pub alg: Alg,
    pub kid: Option<String>,
    pub claims: Map<String, Value>,
}

/// The `kid` and algorithm a token names, before verifying: what the
/// caller needs to know whether to fetch the key set again.
pub fn peek_kid(token: &str) -> Result<Option<String>, JwsError> {
    let (header, _, _) = split(token)?;
    Ok(header.kid)
}

fn split(token: &str) -> Result<(Header, &str, &str), JwsError> {
    if token.len() > MAX_TOKEN {
        return Err(JwsError::Malformed);
    }
    let mut parts = token.split('.');
    let (Some(header), Some(payload), Some(signature), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(JwsError::Malformed);
    };
    let header_bytes = B64.decode(header).map_err(|_| JwsError::Malformed)?;
    let parsed: Header = serde_json::from_slice(&header_bytes).map_err(|_| JwsError::Malformed)?;
    Ok((
        parsed,
        &token[..header.len() + 1 + payload.len()],
        signature,
    ))
}

/// Verifies `token` with a key of `keys` and returns its claims.
pub fn verify(token: &str, keys: &JwkSet) -> Result<Verified, JwsError> {
    let (header, signed, signature) = split(token)?;
    let alg = Alg::parse(&header.alg).ok_or_else(|| JwsError::Algorithm(header.alg.clone()))?;
    if header.crit.is_some() {
        return Err(JwsError::Critical);
    }
    let signature = B64.decode(signature).map_err(|_| JwsError::Malformed)?;
    let candidates: Vec<&Jwk> = keys
        .keys
        .iter()
        .filter(|k| match &header.kid {
            Some(kid) => k.kid.as_deref() == Some(kid),
            None => true,
        })
        .filter(|k| k.use_.as_deref() != Some("enc"))
        .filter(|k| k.alg.as_deref().is_none_or(|a| a == alg.name()))
        .filter(|k| fits(k, alg))
        .collect();
    if candidates.is_empty() {
        return Err(JwsError::UnknownKey);
    }
    if !candidates
        .iter()
        .any(|key| check(key, alg, signed.as_bytes(), &signature))
    {
        return Err(JwsError::BadSignature);
    }
    let payload = signed.split_once('.').map(|(_, p)| p).unwrap_or_default();
    let payload = B64.decode(payload).map_err(|_| JwsError::Malformed)?;
    let claims: Map<String, Value> =
        serde_json::from_slice(&payload).map_err(|_| JwsError::Malformed)?;
    Ok(Verified {
        alg,
        kid: header.kid,
        claims,
    })
}

/// Whether `key` is of the type and curve `alg` needs.
fn fits(key: &Jwk, alg: Alg) -> bool {
    if key.kty != alg.key_type() {
        return false;
    }
    match alg {
        Alg::Es256 => key.crv.as_deref() == Some("P-256"),
        Alg::Es384 => key.crv.as_deref() == Some("P-384"),
        Alg::EdDsa => key.crv.as_deref() == Some("Ed25519"),
        _ => key.n.is_some() && key.e.is_some(),
    }
}

fn decode(value: Option<&String>) -> Option<Vec<u8>> {
    B64.decode(value?).ok()
}

/// `bytes` left-padded with zeros to `len`, or `None` when longer.
fn padded(bytes: Vec<u8>, len: usize) -> Option<Vec<u8>> {
    if bytes.len() > len {
        return None;
    }
    let mut out = vec![0; len - bytes.len()];
    out.extend(bytes);
    Some(out)
}

fn check(key: &Jwk, alg: Alg, message: &[u8], sig: &[u8]) -> bool {
    match alg {
        Alg::Es256 | Alg::Es384 => {
            let (len, algorithm) = if alg == Alg::Es256 {
                (32, &signature::ECDSA_P256_SHA256_FIXED)
            } else {
                (48, &signature::ECDSA_P384_SHA384_FIXED)
            };
            let (Some(x), Some(y)) = (
                decode(key.x.as_ref()).and_then(|x| padded(x, len)),
                decode(key.y.as_ref()).and_then(|y| padded(y, len)),
            ) else {
                return false;
            };
            let mut point = vec![4];
            point.extend(x);
            point.extend(y);
            UnparsedPublicKey::new(algorithm, point)
                .verify(message, sig)
                .is_ok()
        }
        Alg::EdDsa => match decode(key.x.as_ref()) {
            Some(x) if x.len() == 32 => UnparsedPublicKey::new(&signature::ED25519, x)
                .verify(message, sig)
                .is_ok(),
            _ => false,
        },
        _ => {
            let params: &'static signature::RsaParameters = match alg {
                Alg::Rs256 => &signature::RSA_PKCS1_2048_8192_SHA256,
                Alg::Rs384 => &signature::RSA_PKCS1_2048_8192_SHA384,
                Alg::Rs512 => &signature::RSA_PKCS1_2048_8192_SHA512,
                Alg::Ps256 => &signature::RSA_PSS_2048_8192_SHA256,
                Alg::Ps384 => &signature::RSA_PSS_2048_8192_SHA384,
                _ => &signature::RSA_PSS_2048_8192_SHA512,
            };
            let (Some(n), Some(e)) = (decode(key.n.as_ref()), decode(key.e.as_ref())) else {
                return false;
            };
            let n: Vec<u8> = n.into_iter().skip_while(|b| *b == 0).collect();
            let e: Vec<u8> = e.into_iter().skip_while(|b| *b == 0).collect();
            RsaPublicKeyComponents { n: &n, e: &e }
                .verify(params, message, sig)
                .is_ok()
        }
    }
}

#[cfg(test)]
mod tests;
