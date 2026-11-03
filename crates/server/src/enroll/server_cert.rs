//! The server's own TLS certificate, issued by the built-in CA so that
//! enrolling peers can verify it against the CA pinned in their token
//! (ADR-0007). Blocking file I/O: call from `spawn_blocking`.

use std::fs;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use monitorflow_common::config::TlsPaths;
use monitorflow_common::tls::{PeerIdentity, PeerRole};
use rcgen::{CertificateParams, KeyPair, PublicKeyData, PKCS_ECDSA_P256_SHA256};
use rustls::client::danger::ServerCertVerifier;
use rustls::client::WebPkiServerVerifier;
use rustls::pki_types::{CertificateDer, UnixTime};
use rustls::{CertificateError, RootCertStore};
use x509_parser::prelude::{FromDer, X509Certificate};

use crate::ca::{store, CaError, CertificateAuthority, IssuedCert, Serial, ServerName};

/// ID in the server's `monitorflow://server/<id>` SAN.
pub const SERVER_ID: &str = "central";
pub const VALIDITY: Duration = Duration::from_secs(90 * 24 * 60 * 60);
/// A certificate expiring sooner than this is replaced at start.
pub const RENEW_BEFORE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// What [`ensure`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Kept,
    Issued,
}

/// Reads configured names: IP addresses, anything else a DNS name.
pub fn parse_names(names: &[String]) -> Vec<ServerName> {
    names
        .iter()
        .map(|name| match name.parse() {
            Ok(ip) => ServerName::Ip(ip),
            Err(_) => ServerName::Dns(name.clone()),
        })
        .collect()
}

/// Makes `paths` hold the trusted CA certificates and a server certificate
/// and key from the active CA, valid for every name in `names` for at least
/// [`RENEW_BEFORE`]. Issues a new one when missing, expiring, missing a
/// name, not matching its key or from a CA that no longer issues (ADR-0088);
/// refuses a certificate from another CA rather than overwrite it. The
/// certificate never outlives the CA.
pub fn ensure(
    ca: &CertificateAuthority,
    paths: &TlsPaths,
    names: &[ServerName],
) -> Result<Outcome, CaError> {
    let write_ca = || write(&paths.ca_cert, ca.trust_bundle_pem().as_bytes(), 0o644);
    match fit(ca, &paths.cert, &paths.key, names, RENEW_BEFORE)? {
        Fit::Current => {
            write_ca()?;
            return Ok(Outcome::Kept);
        }
        Fit::Foreign => {
            return Err(CaError::Corrupt {
                path: paths.cert.clone(),
                reason: "not issued by the built-in CA; remove it and its key to let the server \
                         issue one"
                    .into(),
            })
        }
        Fit::Missing | Fit::Stale => {}
    }
    let identity = PeerIdentity::new(PeerRole::Server, SERVER_ID).expect("SERVER_ID is valid");
    // Never past the CA's own end, which `issue` refuses.
    let left = ca
        .not_after()
        .duration_since(std::time::SystemTime::now())
        .unwrap_or_default();
    let serial = issue_pair(&paths.cert, &paths.key, |csr| {
        ca.issue(csr, &identity, Some(VALIDITY.min(left)), names)
    })?;
    write_ca()?;
    tracing::info!(cert = %paths.cert.display(), %serial, "issued the server certificate");
    Ok(Outcome::Issued)
}

/// How the certificate at a path compares with what is needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fit {
    /// From the CA, for every name, with its key, and valid for longer
    /// than the margin.
    Current,
    Missing,
    /// Expiring, missing a name, not matching its key, or from a CA that
    /// is still trusted but no longer issues (ADR-0088).
    Stale,
    /// Signed by a CA the server does not trust.
    Foreign,
}

/// Checks the certificate at `cert_path` and its key at `key_path`
/// against `ca` and `names`; `renew_before` is the margin before expiry.
pub(crate) fn fit(
    ca: &CertificateAuthority,
    cert_path: &Path,
    key_path: &Path,
    names: &[ServerName],
    renew_before: Duration,
) -> Result<Fit, CaError> {
    let cert_pem = match fs::read_to_string(cert_path) {
        Ok(pem) => pem,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Fit::Missing),
        Err(error) => return Err(io_error(cert_path, error)),
    };
    let corrupt = |reason: &str| CaError::Corrupt {
        path: cert_path.to_owned(),
        reason: reason.into(),
    };
    let der = pem_der(&cert_pem).ok_or_else(|| corrupt("not a PEM certificate"))?;
    let (_, cert) = X509Certificate::from_der(&der).map_err(|_| corrupt("invalid certificate"))?;

    let verifier = |ca_pem: &str| {
        let mut roots = RootCertStore::empty();
        for ca in pem::parse_many(ca_pem).map_err(|e| CaError::Signing(e.to_string()))? {
            roots
                .add(CertificateDer::from(ca.into_contents()))
                .map_err(|e| CaError::Signing(e.to_string()))?;
        }
        WebPkiServerVerifier::builder_with_provider(
            Arc::new(roots),
            Arc::new(rustls::crypto::ring::default_provider()),
        )
        .build()
        .map_err(|e| CaError::Signing(e.to_string()))
    };
    let (active, trusted) = (verifier(&ca.ca_cert_pem())?, verifier(&ca.trust_bundle_pem())?);
    let end_entity = CertificateDer::from(der.as_slice());
    for name in names {
        let name = match name {
            ServerName::Dns(dns) => match rustls::pki_types::ServerName::try_from(dns.as_str()) {
                Ok(name) => name.to_owned(),
                Err(_) => return Err(corrupt("invalid DNS name in server_names")),
            },
            ServerName::Ip(ip) => rustls::pki_types::ServerName::IpAddress((*ip).into()),
        };
        match active.verify_server_cert(&end_entity, &[], &name, &[], UnixTime::now()) {
            Ok(_) => {}
            Err(rustls::Error::InvalidCertificate(
                CertificateError::UnknownIssuer | CertificateError::BadSignature,
            )) if trusted
                .verify_server_cert(&end_entity, &[], &name, &[], UnixTime::now())
                .is_ok() =>
            {
                // From the CA a change of certificate authority replaces
                // (ADR-0088): issue again from the active one.
                return Ok(Fit::Stale);
            }
            Err(rustls::Error::InvalidCertificate(
                CertificateError::UnknownIssuer | CertificateError::BadSignature,
            )) => return Ok(Fit::Foreign),
            // Expired or missing this name.
            Err(_) => return Ok(Fit::Stale),
        }
    }
    let renew_at = cert.validity().not_after.timestamp() - renew_before.as_secs() as i64;
    if time::OffsetDateTime::now_utc().unix_timestamp() >= renew_at {
        return Ok(Fit::Stale);
    }
    let key_matches = fs::read_to_string(key_path)
        .ok()
        .and_then(|pem| KeyPair::from_pem(&pem).ok())
        .is_some_and(|key| key.subject_public_key_info() == cert.public_key().raw);
    Ok(if key_matches {
        Fit::Current
    } else {
        Fit::Stale
    })
}

/// Generates a key, has `issue` sign its CSR, and writes the key to
/// `key_path` and the certificate to `cert_path`. Returns the serial.
pub(crate) fn issue_pair(
    cert_path: &Path,
    key_path: &Path,
    issue: impl FnOnce(&str) -> Result<IssuedCert, CaError>,
) -> Result<Serial, CaError> {
    let signing = |e: rcgen::Error| CaError::Signing(e.to_string());
    let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).map_err(signing)?;
    let csr = CertificateParams::default()
        .serialize_request(&key)
        .and_then(|csr| csr.pem())
        .map_err(signing)?;
    let issued = issue(&csr)?;
    // Key first: a crash before the certificate is written leaves a pair
    // that does not match, which the next check replaces.
    write(key_path, key.serialize_pem().as_bytes(), 0o600)?;
    write(cert_path, issued.cert_pem.as_bytes(), 0o644)?;
    Ok(issued.serial)
}

fn pem_der(text: &str) -> Option<Vec<u8>> {
    pem::parse(text).ok().map(pem::Pem::into_contents)
}

pub(crate) fn write(path: &Path, contents: &[u8], mode: u32) -> Result<(), CaError> {
    if fs::read(path).is_ok_and(|current| current == contents) {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| io_error(dir, e))?;
    }
    store::write_atomic(path, contents, mode)
}

fn io_error(path: &Path, error: io::Error) -> CaError {
    CaError::Io {
        path: path.to_owned(),
        error,
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use monitorflow_common::tls::Identity;
    use tempfile::TempDir;

    use super::*;
    use crate::ca::CaOptions;

    fn setup() -> (TempDir, CertificateAuthority, TlsPaths) {
        let dir = TempDir::new().unwrap();
        let ca = CertificateAuthority::open_or_create(&dir.path().join("ca"), CaOptions::default())
            .unwrap();
        let tls = dir.path().join("tls");
        let paths = TlsPaths {
            ca_cert: tls.join("ca.pem"),
            cert: tls.join("cert.pem"),
            key: tls.join("key.pem"),
        };
        (dir, ca, paths)
    }

    fn names(list: &[&str]) -> Vec<ServerName> {
        parse_names(&list.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn issues_once_then_keeps() {
        let (_dir, ca, paths) = setup();
        let localhost = names(&["localhost", "127.0.0.1"]);
        assert_eq!(ensure(&ca, &paths, &localhost).unwrap(), Outcome::Issued);
        let mode = fs::metadata(&paths.key).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let cert = fs::read(&paths.cert).unwrap();
        assert_eq!(ensure(&ca, &paths, &localhost).unwrap(), Outcome::Kept);
        assert_eq!(fs::read(&paths.cert).unwrap(), cert);

        let identity = Identity::load(&paths).unwrap();
        identity.server_config_without_client_auth().unwrap();
        let der = pem_der(std::str::from_utf8(&cert).unwrap()).unwrap();
        let peer = PeerIdentity::from_cert(&CertificateDer::from(der)).unwrap();
        assert_eq!(peer.uri(), "monitorflow://server/central");
    }

    #[test]
    fn reissues_for_a_new_name_or_a_lost_key() {
        let (_dir, ca, paths) = setup();
        ensure(&ca, &paths, &names(&["localhost"])).unwrap();
        let with_new_name = names(&["localhost", "mf.example.com"]);
        assert_eq!(
            ensure(&ca, &paths, &with_new_name).unwrap(),
            Outcome::Issued
        );
        assert_eq!(ensure(&ca, &paths, &with_new_name).unwrap(), Outcome::Kept);

        fs::write(&paths.key, KeyPair::generate().unwrap().serialize_pem()).unwrap();
        assert_eq!(
            ensure(&ca, &paths, &with_new_name).unwrap(),
            Outcome::Issued
        );
        Identity::load(&paths).unwrap();
    }

    #[test]
    fn refuses_a_certificate_from_another_ca() {
        let (_dir, ca, paths) = setup();
        let (_other_dir, other, _) = setup();
        ensure(&other, &paths, &names(&["localhost"])).unwrap();
        let foreign = fs::read(&paths.cert).unwrap();
        let err = ensure(&ca, &paths, &names(&["localhost"])).unwrap_err();
        assert!(
            err.to_string().contains("not issued by the built-in CA"),
            "{err}"
        );
        assert_eq!(fs::read(&paths.cert).unwrap(), foreign);
    }

    #[test]
    fn names_are_ips_or_dns() {
        assert_eq!(
            names(&["10.0.0.1", "::1", "mf.example.com"]),
            [
                ServerName::Ip("10.0.0.1".parse().unwrap()),
                ServerName::Ip("::1".parse().unwrap()),
                ServerName::Dns("mf.example.com".into()),
            ]
        );
    }
}
