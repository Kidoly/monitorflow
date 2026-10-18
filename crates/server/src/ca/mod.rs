//! Built-in certificate authority: issues short-lived peer certificates and
//! maintains the revocation list. See `docs/architecture.md` §4 and ADR-0004.
//!
//! Pure Rust and file-backed: everything lives in one directory, typically
//! `<data_dir>/ca/`:
//!
//! - `ca-key.pem`: ECDSA P-256 root key, PKCS#8, mode 0600;
//! - `ca-cert.pem`: self-signed root certificate;
//! - `revocations.json`: revoked serials and the last CRL number.
//!
//! Every write goes to a temporary file that is then renamed. Methods do
//! blocking file I/O; call them from `spawn_blocking` in async code.

mod revocation;
pub(crate) mod store;

#[cfg(test)]
mod tests;

use std::fmt;
use std::fs;
use std::io;
use std::net::IpAddr;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use monitorflow_common::tls::{PeerIdentity, PeerRole};
use rcgen::{
    BasicConstraints, Certificate, CertificateParams, CertificateSigningRequestParams,
    DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose, SanType,
    SerialNumber, PKCS_ECDSA_P256_SHA256,
};
use ring::rand::{SecureRandom, SystemRandom};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use rustls::sign::CertifiedKey;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use tracing::{debug, info};

pub use revocation::RevocationReason;
use revocation::Revocations;

pub const KEY_FILE: &str = "ca-key.pem";
pub const CERT_FILE: &str = "ca-cert.pem";
pub const REVOCATIONS_FILE: &str = "revocations.json";

const DAY: Duration = Duration::from_secs(24 * 60 * 60);
/// How far `not_before` is backdated, for peers whose clock runs late.
pub const CLOCK_SKEW: Duration = Duration::from_secs(5 * 60);
const SERIAL_BYTES: usize = 16;

#[derive(Debug, thiserror::Error)]
pub enum CaError {
    #[error("{}: {error}", path.display())]
    Io { path: PathBuf, error: io::Error },
    #[error(
        "CA key {} is accessible by group or others (mode {mode:03o}); \
         run `chmod 600 {}` and start again",
        path.display(),
        path.display()
    )]
    KeyPermissions { path: PathBuf, mode: u32 },
    #[error(
        "{} exists but {} is missing: restore it, or remove the whole \
         directory to create a new CA (every issued certificate is then lost)",
        present.display(),
        missing.display()
    )]
    Incomplete { present: PathBuf, missing: PathBuf },
    #[error("{}: {reason}", path.display())]
    Corrupt { path: PathBuf, reason: String },
    #[error("CSR signature verification failed")]
    CsrSignature,
    #[error("invalid CSR: {0}")]
    InvalidCsr(String),
    #[error("invalid issuance request: {0}")]
    InvalidRequest(String),
    #[error("invalid serial number `{0}`: expected hexadecimal")]
    InvalidSerial(String),
    #[error("cannot sign: {0}")]
    Signing(String),
}

impl CaError {
    fn io(path: &Path, error: io::Error) -> Self {
        Self::Io {
            path: path.to_owned(),
            error,
        }
    }

    fn corrupt(path: &Path, reason: impl fmt::Display) -> Self {
        Self::Corrupt {
            path: path.to_owned(),
            reason: reason.to_string(),
        }
    }

    fn signing(error: rcgen::Error) -> Self {
        Self::Signing(error.to_string())
    }
}

/// Settings of the CA; [`Default`] gives the documented defaults.
#[derive(Debug, Clone)]
pub struct CaOptions {
    /// Subject CN of the root certificate, used only when creating it.
    pub common_name: String,
    /// Validity of the root certificate, used only when creating it.
    pub ca_validity: Duration,
    /// Validity of issued certificates when [`CertificateAuthority::issue`]
    /// gets none.
    pub cert_validity: Duration,
    /// Time from a CRL's `this_update` to its `next_update`.
    pub crl_validity: Duration,
}

impl Default for CaOptions {
    fn default() -> Self {
        Self {
            common_name: "MonitorFlow CA".into(),
            ca_validity: 3650 * DAY,
            cert_validity: 7 * DAY,
            crl_validity: DAY,
        }
    }
}

/// Certificate serial number: big-endian bytes without leading zeros,
/// written and parsed as lowercase hexadecimal.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub struct Serial(Vec<u8>);

impl Serial {
    /// 128 random bits. rcgen encodes serials as positive integers.
    fn random() -> Result<Self, CaError> {
        let mut bytes = [0u8; SERIAL_BYTES];
        loop {
            SystemRandom::new()
                .fill(&mut bytes)
                .map_err(|_| CaError::Signing("system random generator failed".into()))?;
            let serial = Self::from_bytes(&bytes);
            if !serial.0.is_empty() {
                return Ok(serial);
            }
        }
    }

    pub fn from_bytes(bytes: &[u8]) -> Self {
        let start = bytes.iter().position(|&b| b != 0).unwrap_or(bytes.len());
        Self(bytes[start..].to_vec())
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Display for Serial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))
    }
}

impl FromStr for Serial {
    type Err = CaError;

    fn from_str(s: &str) -> Result<Self, CaError> {
        let invalid = || CaError::InvalidSerial(s.to_owned());
        if s.is_empty() || s.len() > 40 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(invalid());
        }
        let padded = if s.len() % 2 == 1 {
            format!("0{s}")
        } else {
            s.to_owned()
        };
        let bytes = (0..padded.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&padded[i..i + 2], 16).map_err(|_| invalid()))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self::from_bytes(&bytes))
    }
}

impl From<Serial> for String {
    fn from(serial: Serial) -> Self {
        serial.to_string()
    }
}

impl TryFrom<String> for Serial {
    type Error = CaError;

    fn try_from(s: String) -> Result<Self, CaError> {
        s.parse()
    }
}

/// Extra name of a `server` certificate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerName {
    Dns(String),
    Ip(IpAddr),
}

/// What the caller stores about an issued certificate.
#[derive(Debug, Clone)]
pub struct IssuedCert {
    pub cert_pem: String,
    pub serial: Serial,
    pub not_before: SystemTime,
    pub not_after: SystemTime,
}

/// The CA. `Send + Sync`: share it in an `Arc`.
pub struct CertificateAuthority {
    dir: PathBuf,
    options: CaOptions,
    cert_pem: String,
    /// Rebuilt from `ca-cert.pem` with the same subject and key identifier;
    /// rcgen needs it to sign. Its own signature is never published.
    issuer: Certificate,
    key: KeyPair,
    revocations: Mutex<Revocations>,
}

impl fmt::Debug for CertificateAuthority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CertificateAuthority")
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

impl CertificateAuthority {
    /// Loads the CA in `dir`, or creates it if the directory holds neither
    /// the key nor the certificate.
    pub fn open_or_create(dir: &Path, options: CaOptions) -> Result<Self, CaError> {
        let key_path = dir.join(KEY_FILE);
        let cert_path = dir.join(CERT_FILE);
        let exists = |path: &Path| path.try_exists().map_err(|e| CaError::io(path, e));
        match (exists(&key_path)?, exists(&cert_path)?) {
            (true, true) => {}
            (false, false) => {
                create(dir, &options)?;
                info!(dir = %dir.display(), "created a new certificate authority");
            }
            (true, false) => return Err(incomplete(key_path, cert_path)),
            (false, true) => return Err(incomplete(cert_path, key_path)),
        }
        let ca = Self::load(dir, options)?;
        info!(dir = %dir.display(), "loaded the certificate authority");
        Ok(ca)
    }

    fn load(dir: &Path, options: CaOptions) -> Result<Self, CaError> {
        let key_path = dir.join(KEY_FILE);
        let cert_path = dir.join(CERT_FILE);
        store::check_key_permissions(&key_path)?;
        let key_pem = fs::read_to_string(&key_path).map_err(|e| CaError::io(&key_path, e))?;
        // TODO(ca-key-at-rest): decrypt the key here once it is encrypted.
        // rcgen's message never quotes the key; drop it anyway.
        let key = KeyPair::from_pem(&key_pem)
            .map_err(|_| CaError::corrupt(&key_path, "cannot decode the private key"))?;
        if key.algorithm() != &PKCS_ECDSA_P256_SHA256 {
            return Err(CaError::corrupt(&key_path, "expected an ECDSA P-256 key"));
        }

        let cert_pem = fs::read_to_string(&cert_path).map_err(|e| CaError::io(&cert_path, e))?;
        let params = CertificateParams::from_ca_cert_pem(&cert_pem)
            .map_err(|e| CaError::corrupt(&cert_path, e))?;
        if !matches!(params.is_ca, IsCa::Ca(_)) {
            return Err(CaError::corrupt(&cert_path, "not a CA certificate"));
        }
        let cert_der = pem::parse(&cert_pem).map_err(|e| CaError::corrupt(&cert_path, e))?;
        check_key_matches(cert_der.contents(), &key)
            .map_err(|why| CaError::corrupt(&key_path, why))?;
        let issuer = params.self_signed(&key).map_err(CaError::signing)?;

        let revocations = revocation::load(&dir.join(REVOCATIONS_FILE))?;
        Ok(Self {
            dir: dir.to_owned(),
            options,
            cert_pem,
            issuer,
            key,
            revocations: Mutex::new(revocations),
        })
    }

    /// The root certificate, for peers' trust stores.
    pub fn ca_cert_pem(&self) -> &str {
        &self.cert_pem
    }

    /// Signs the public key of `csr_pem` for `identity`.
    ///
    /// The CSR signature must verify; its subject, SANs and extensions are
    /// ignored. The certificate gets the single URI SAN of `identity`, the
    /// ID as CN, `clientAuth`, and for role `server` also `serverAuth` and
    /// `server_names`, which must be empty for other roles. `validity`
    /// defaults to [`CaOptions::cert_validity`]; `not_before` is backdated
    /// by [`CLOCK_SKEW`].
    pub fn issue(
        &self,
        csr_pem: &str,
        identity: &PeerIdentity,
        validity: Option<Duration>,
        server_names: &[ServerName],
    ) -> Result<IssuedCert, CaError> {
        let invalid = |why: String| CaError::InvalidRequest(why);
        let csr = CertificateSigningRequestParams::from_pem(csr_pem).map_err(|e| match e {
            rcgen::Error::RingUnspecified => CaError::CsrSignature,
            other => CaError::InvalidCsr(other.to_string()),
        })?;

        let is_server = identity.role() == PeerRole::Server;
        if is_server && server_names.is_empty() {
            return Err(invalid(
                "a server certificate needs a DNS name or an IP address".into(),
            ));
        }
        if !is_server && !server_names.is_empty() {
            return Err(invalid(format!(
                "DNS names and IP addresses are for server certificates, not {}",
                identity.role()
            )));
        }
        let uri = identity.uri().try_into().map_err(CaError::signing)?;
        let mut sans = vec![SanType::URI(uri)];
        for name in server_names {
            sans.push(match name {
                ServerName::Dns(dns) => SanType::DnsName(
                    dns.as_str()
                        .try_into()
                        .map_err(|_| invalid(format!("invalid DNS name `{dns}`")))?,
                ),
                ServerName::Ip(ip) => SanType::IpAddress(*ip),
            });
        }

        let validity = validity.unwrap_or(self.options.cert_validity);
        let now = now();
        let not_before = now - CLOCK_SKEW;
        let not_after = time::Duration::try_from(validity)
            .ok()
            .and_then(|v| now.checked_add(v))
            .filter(|&t| t > now && t <= self.issuer.params().not_after)
            .ok_or_else(|| {
                invalid(format!(
                    "validity of {validity:?} is zero or ends after the CA certificate"
                ))
            })?;

        let serial = Serial::random()?;
        let mut params = CertificateParams::default();
        params.distinguished_name = DistinguishedName::new();
        params
            .distinguished_name
            .push(DnType::CommonName, identity.id());
        params.subject_alt_names = sans;
        params.serial_number = Some(SerialNumber::from_slice(serial.as_bytes()));
        params.not_before = not_before;
        params.not_after = not_after;
        params.is_ca = IsCa::ExplicitNoCa;
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        if is_server {
            params
                .extended_key_usages
                .push(ExtendedKeyUsagePurpose::ServerAuth);
        }
        params.use_authority_key_identifier_extension = true;
        let cert = CertificateSigningRequestParams {
            params,
            public_key: csr.public_key,
        }
        .signed_by(&self.issuer, &self.key)
        .map_err(CaError::signing)?;

        debug!(peer = %identity, %serial, "issued a certificate");
        Ok(IssuedCert {
            cert_pem: cert.pem(),
            serial,
            not_before: not_before.into(),
            not_after: not_after.into(),
        })
    }
}

fn incomplete(present: PathBuf, missing: PathBuf) -> CaError {
    CaError::Incomplete { present, missing }
}

/// Now, to the second: X.509 times have no fractions.
fn now() -> OffsetDateTime {
    let now = OffsetDateTime::now_utc();
    now - time::Duration::nanoseconds(now.nanosecond().into())
}

fn create(dir: &Path, options: &CaOptions) -> Result<(), CaError> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| CaError::io(dir, e))?;
    let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).map_err(CaError::signing)?;
    let now = now();
    let mut params = CertificateParams::default();
    params.distinguished_name = DistinguishedName::new();
    params
        .distinguished_name
        .push(DnType::CommonName, options.common_name.as_str());
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    params.serial_number = Some(SerialNumber::from_slice(Serial::random()?.as_bytes()));
    params.not_before = now - CLOCK_SKEW;
    params.not_after = time::Duration::try_from(options.ca_validity)
        .ok()
        .and_then(|v| now.checked_add(v))
        .ok_or_else(|| CaError::InvalidRequest("CA validity is too long".into()))?;
    let cert = params.self_signed(&key).map_err(CaError::signing)?;
    // The certificate is written last: a crash in between leaves a key
    // alone, which `open_or_create` reports instead of replacing it.
    // TODO(ca-key-at-rest): encrypt the key before writing it.
    store::write_atomic(&dir.join(KEY_FILE), key.serialize_pem().as_bytes(), 0o600)?;
    store::write_atomic(&dir.join(CERT_FILE), cert.pem().as_bytes(), 0o644)
}

/// Checks that `key` is the key of the certificate `cert_der`.
fn check_key_matches(cert_der: &[u8], key: &KeyPair) -> Result<(), String> {
    let signer = rustls::crypto::ring::default_provider()
        .key_provider
        .load_private_key(PrivatePkcs8KeyDer::from(key.serialize_der()).into())
        .map_err(|e| format!("cannot load the private key: {e}"))?;
    CertifiedKey::new(vec![CertificateDer::from(cert_der.to_vec())], signer)
        .keys_match()
        .map_err(|_| format!("the key does not match {CERT_FILE}"))
}
