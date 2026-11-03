//! Built-in certificate authority: issues short-lived peer certificates and
//! maintains the revocation list. See `docs/architecture.md` §4 and ADR-0004.
//!
//! Pure Rust and file-backed: everything lives in one directory, typically
//! `<data_dir>/ca/`:
//!
//! - `ca-key.pem`: ECDSA P-256 root key, PKCS#8, mode 0600;
//! - `ca-cert.pem`: self-signed root certificate;
//! - `revocations.json`: revoked serials and the last CRL number, shared
//!   by every CA generation.
//!
//! A change of certificate authority (ADR-0088) adds `rotation.json`, a
//! second generation in `next/` or `previous/`, and keeps the certificate
//! of each retired generation in `retired/`; see [`rotation`].
//!
//! Every write goes to a temporary file that is then renamed. Methods do
//! blocking file I/O; call them from `spawn_blocking` in async code.

mod revocation;
pub mod rotation;
pub(crate) mod store;

#[cfg(test)]
mod rotation_tests;
#[cfg(test)]
mod tests;

use std::fmt;
use std::fs;
use std::io;
use std::net::IpAddr;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex, PoisonError, RwLock, RwLockReadGuard};
use std::time::{Duration, SystemTime};

use monitorflow_common::tls::{fingerprint, Fingerprint, PeerIdentity, PeerRole};
use rcgen::{
    BasicConstraints, CertificateParams, CertificateSigningRequestParams, DistinguishedName,
    DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyIdMethod, KeyPair, KeyUsagePurpose, SanType,
    SerialNumber, PKCS_ECDSA_P256_SHA256,
};
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{EcdsaKeyPair, ECDSA_P256_SHA256_ASN1_SIGNING};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer, UnixTime};
use rustls::server::WebPkiClientVerifier;
use rustls::sign::CertifiedKey;
use rustls::RootCertStore;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use tracing::{debug, info};
use x509_parser::extensions::ParsedExtension;
use x509_parser::prelude::{FromDer, X509Certificate};

pub use revocation::RevocationReason;
use revocation::Revocations;
use rotation::RotationState;
pub use rotation::{CaInfo, CaStatus, Retired, Rotation, Stage};

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

/// Extra name of a `server` or `relay` certificate.
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
    /// SHA-256 of the CA certificate that signed it.
    pub issuer: Fingerprint,
    pub not_before: SystemTime,
    pub not_after: SystemTime,
}

/// One CA key and its self-signed certificate. Two exist during a change
/// of certificate authority (ADR-0088).
pub(crate) struct Generation {
    cert_pem: String,
    cert_der: CertificateDer<'static>,
    fingerprint: Fingerprint,
    /// The subject, key usages and key identifier of the certificate, with
    /// its key.
    issuer: Issuer<'static, KeyPair>,
    /// The subject key identifier of the certificate, for CRLs.
    key_id: KeyIdMethod,
    not_before: OffsetDateTime,
    not_after: OffsetDateTime,
    /// The same key for ring, which signs commands (ADR-0031).
    command_key: EcdsaKeyPair,
}

impl Generation {
    fn info(&self) -> CaInfo {
        CaInfo {
            fingerprint: self.fingerprint,
            cert_pem: self.cert_pem.clone(),
            not_before: self.not_before.into(),
            not_after: self.not_after.into(),
        }
    }
}

/// The CAs a peer trusts with a new certificate (ADR-0088).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trust {
    /// The CA that issues now.
    pub issuer: Fingerprint,
    pub issuer_pem: String,
    /// Every trusted CA in PEM, the issuer first.
    pub bundle_pem: String,
    pub bundle: Vec<Fingerprint>,
    /// An emergency change is under way: only the new CA counts for
    /// enrollment.
    pub emergency: bool,
}

impl Trust {
    /// Whether a peer whose certificate `issuer` signed, and which trusts
    /// `trusted`, needs a new certificate.
    pub fn is_behind(&self, issuer: &Fingerprint, trusted: &[Fingerprint]) -> bool {
        issuer != &self.issuer
            || trusted.len() != self.bundle.len()
            || !trusted.iter().all(|fp| self.bundle.contains(fp))
    }

    /// Whether an enrollment token pinned to `pin` is still good.
    pub fn accepts_pin(&self, pin: &Fingerprint) -> bool {
        pin == &self.issuer || (!self.emergency && self.bundle.contains(pin))
    }
}

/// The CA. `Send + Sync`: share it in an `Arc`.
pub struct CertificateAuthority {
    dir: PathBuf,
    options: CaOptions,
    /// The active generation, which issues and signs, and the other one
    /// during a change of certificate authority.
    state: RwLock<RotationState>,
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
        let state = rotation::recover_and_load(dir, &options)?;
        let revocations = revocation::load(&dir.join(REVOCATIONS_FILE))?;
        Ok(Self {
            dir: dir.to_owned(),
            options,
            state: RwLock::new(state),
            revocations: Mutex::new(revocations),
        })
    }

    fn state(&self) -> RwLockReadGuard<'_, RotationState> {
        // Every change is applied whole after its files are written.
        self.state.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn active(&self) -> Arc<Generation> {
        self.state().active.clone()
    }

    /// SHA-256 of the active CA certificate, which enrollment tokens pin.
    pub fn fingerprint(&self) -> Fingerprint {
        self.active().fingerprint
    }

    /// The certificates peers must trust, the active one first, in PEM:
    /// two during a change of certificate authority.
    pub fn trust_bundle_pem(&self) -> String {
        self.state()
            .trusted()
            .map(|g| g.cert_pem.as_str())
            .collect::<Vec<_>>()
            .concat()
    }

    /// The certificates of [`Self::trust_bundle_pem`], in DER.
    pub fn trusted_certs(&self) -> Vec<CertificateDer<'static>> {
        self.state().trusted().map(|g| g.cert_der.clone()).collect()
    }

    /// What a peer gets with a certificate issued now, read at once.
    pub fn trust(&self) -> Trust {
        let state = self.state();
        Trust {
            issuer: state.active.fingerprint,
            issuer_pem: state.active.cert_pem.clone(),
            bundle_pem: state.trusted().map(|g| g.cert_pem.as_str()).collect(),
            bundle: state.trusted().map(|g| g.fingerprint).collect(),
            emergency: state.rotation.as_ref().is_some_and(|r| r.emergency),
        }
    }

    /// The active CA, the change of certificate authority under way if
    /// any, and whether a restore brought back a retired CA.
    pub fn status(&self) -> CaStatus {
        self.state().status()
    }
    /// Signs `message`, the bytes of `common::command::signed_bytes`, for
    /// agents to verify with the CA certificate (ADR-0031).
    pub fn sign_command(&self, message: &[u8]) -> Result<Vec<u8>, CaError> {
        self.active()
            .command_key
            .sign(&SystemRandom::new(), message)
            .map(|signature| signature.as_ref().to_vec())
            .map_err(|_| CaError::Signing("cannot sign the command".into()))
    }

    /// The active root certificate, which issues every new certificate.
    /// Peers trust [`Self::trust_bundle_pem`].
    pub fn ca_cert_pem(&self) -> String {
        self.active().cert_pem.clone()
    }

    /// Checks that `der` is a client certificate a trusted CA generation
    /// signed and that it is valid now, as a TLS listener would. For certificates a relay
    /// forwards on behalf of a peer it terminated (ADR-0028): the relay
    /// cannot make one up, only pass on one it was shown. Revocation is the
    /// caller's check.
    pub fn verify_issued(&self, der: &CertificateDer<'_>) -> Result<(), String> {
        let mut roots = RootCertStore::empty();
        for generation in self.state().trusted() {
            roots
                .add(generation.cert_der.clone())
                .map_err(|e| e.to_string())?;
        }
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let verifier = WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider)
            .build()
            .map_err(|e| e.to_string())?;
        verifier
            .verify_client_cert(&der.clone().into_owned(), &[], UnixTime::now())
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Signs the public key of `csr_pem` for `identity`.
    ///
    /// The CSR signature must verify; its subject, SANs and extensions are
    /// ignored. The certificate gets the single URI SAN of `identity`, the
    /// ID as CN, `clientAuth`, and with `server_names` also `serverAuth` and
    /// those names. A `server` needs names; a `relay` may have them, to
    /// accept its agents (ADR-0025); an `agent` may not. `validity`
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
        let is_server = identity.role() == PeerRole::Server;
        if is_server && server_names.is_empty() {
            return Err(invalid(
                "a server certificate needs a DNS name or an IP address".into(),
            ));
        }
        if identity.role() == PeerRole::Agent && !server_names.is_empty() {
            return Err(invalid(format!(
                "DNS names and IP addresses are for server and relay certificates, not {}",
                identity.role()
            )));
        }
        let uri = identity.uri().try_into().map_err(CaError::signing)?;
        let mut sans = vec![SanType::URI(uri)];
        sans.extend(name_sans(server_names)?);
        let mut usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        if !server_names.is_empty() {
            usages.push(ExtendedKeyUsagePurpose::ServerAuth);
        }
        let issued = self.sign(csr_pem, identity.id(), sans, usages, validity)?;
        debug!(peer = %identity, serial = %issued.serial, "issued a certificate");
        Ok(issued)
    }

    /// Signs the public key of `csr_pem` for a TLS server that is not a
    /// peer, like the metric and log stores (ADR-0087): `names` as SANs,
    /// the first as CN, `serverAuth` only and no `monitorflow://` URI, so
    /// the certificate can never authenticate a client to the server's
    /// listeners. Same CSR and validity rules as [`Self::issue`].
    pub fn issue_tls_server(
        &self,
        csr_pem: &str,
        names: &[ServerName],
        validity: Option<Duration>,
    ) -> Result<IssuedCert, CaError> {
        let common_name = match names.first() {
            Some(ServerName::Dns(dns)) => dns.clone(),
            Some(ServerName::Ip(ip)) => ip.to_string(),
            None => {
                return Err(CaError::InvalidRequest(
                    "a TLS server certificate needs a DNS name or an IP address".into(),
                ))
            }
        };
        let sans = name_sans(names)?;
        let usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let issued = self.sign(csr_pem, &common_name, sans, usages, validity)?;
        debug!(name = %common_name, serial = %issued.serial, "issued a TLS server certificate");
        Ok(issued)
    }

    fn sign(
        &self,
        csr_pem: &str,
        common_name: &str,
        sans: Vec<SanType>,
        usages: Vec<ExtendedKeyUsagePurpose>,
        validity: Option<Duration>,
    ) -> Result<IssuedCert, CaError> {
        // One generation for the whole certificate, even if a change of
        // certificate authority happens meanwhile.
        let active = self.active();
        let invalid = |why: String| CaError::InvalidRequest(why);
        let csr = CertificateSigningRequestParams::from_pem(csr_pem).map_err(|e| match e {
            rcgen::Error::InvalidCertificationRequestSignature => CaError::CsrSignature,
            other => CaError::InvalidCsr(other.to_string()),
        })?;

        let validity = validity.unwrap_or(self.options.cert_validity);
        let now = now();
        let not_before = now - CLOCK_SKEW;
        let not_after = time::Duration::try_from(validity)
            .ok()
            .and_then(|v| now.checked_add(v))
            .filter(|&t| t > now && t <= active.not_after)
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
            .push(DnType::CommonName, common_name);
        params.subject_alt_names = sans;
        params.serial_number = Some(SerialNumber::from_slice(serial.as_bytes()));
        params.not_before = not_before;
        params.not_after = not_after;
        params.is_ca = IsCa::ExplicitNoCa;
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = usages;
        params.use_authority_key_identifier_extension = true;
        let cert = CertificateSigningRequestParams {
            params,
            public_key: csr.public_key,
        }
        .signed_by(&active.issuer)
        .map_err(CaError::signing)?;

        Ok(IssuedCert {
            cert_pem: cert.pem(),
            serial,
            issuer: active.fingerprint,
            not_before: not_before.into(),
            not_after: not_after.into(),
        })
    }

    /// When the active CA certificate expires; issued certificates cannot
    /// outlive it.
    pub fn not_after(&self) -> SystemTime {
        self.active().not_after.into()
    }
}

/// `names` as subject alternative names.
fn name_sans(names: &[ServerName]) -> Result<Vec<SanType>, CaError> {
    names
        .iter()
        .map(|name| match name {
            ServerName::Dns(dns) => dns
                .as_str()
                .try_into()
                .map(SanType::DnsName)
                .map_err(|_| CaError::InvalidRequest(format!("invalid DNS name `{dns}`"))),
            ServerName::Ip(ip) => Ok(SanType::IpAddress(*ip)),
        })
        .collect()
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
    create_named(dir, &options.common_name, options.ca_validity)
}

/// Creates a key and a self-signed CA certificate named `common_name` in
/// `dir`.
fn create_named(dir: &Path, common_name: &str, validity: Duration) -> Result<(), CaError> {
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
        .push(DnType::CommonName, common_name);
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    params.serial_number = Some(SerialNumber::from_slice(Serial::random()?.as_bytes()));
    params.not_before = now - CLOCK_SKEW;
    params.not_after = time::Duration::try_from(validity)
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

/// Loads the key and certificate in `dir` and checks they match.
fn load_generation(dir: &Path) -> Result<Generation, CaError> {
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
    let cert_der = pem::parse(&cert_pem).map_err(|e| CaError::corrupt(&cert_path, e))?;
    let (_, cert) = X509Certificate::from_der(cert_der.contents())
        .map_err(|e| CaError::corrupt(&cert_path, e))?;
    let is_ca = cert
        .basic_constraints()
        .map_err(|e| CaError::corrupt(&cert_path, e))?
        .is_some_and(|constraints| constraints.value.ca);
    if !is_ca {
        return Err(CaError::corrupt(&cert_path, "not a CA certificate"));
    }
    check_key_matches(cert_der.contents(), &key).map_err(|why| CaError::corrupt(&key_path, why))?;
    let command_key = EcdsaKeyPair::from_pkcs8(
        &ECDSA_P256_SHA256_ASN1_SIGNING,
        &key.serialize_der(),
        &SystemRandom::new(),
    )
    .map_err(|_| CaError::corrupt(&key_path, "cannot load the key for signing"))?;
    let issuer =
        Issuer::from_ca_cert_pem(&cert_pem, key).map_err(|e| CaError::corrupt(&cert_path, e))?;
    let der = CertificateDer::from(cert_der.contents().to_vec());
    Ok(Generation {
        fingerprint: fingerprint(&der),
        key_id: key_id_of(&cert),
        not_before: cert.validity().not_before.to_datetime(),
        not_after: cert.validity().not_after.to_datetime(),
        cert_der: der,
        cert_pem,
        issuer,
        command_key,
    })
}

/// How rcgen names the key of `cert`: the identifier it states, or the hash
/// of its key if it states none.
fn key_id_of(cert: &X509Certificate<'_>) -> KeyIdMethod {
    cert.iter_extensions()
        .find_map(|ext| match ext.parsed_extension() {
            ParsedExtension::SubjectKeyIdentifier(id) => {
                Some(KeyIdMethod::PreSpecified(id.0.to_vec()))
            }
            _ => None,
        })
        .unwrap_or(KeyIdMethod::Sha256)
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
