//! TLS and credentials of the server's links to VictoriaMetrics and
//! VictoriaLogs (ADR-0087).
//!
//! The built-in CA issues one certificate for the hosts of
//! `victoriametrics_url` and `victorialogs_url`, `serverAuth` only, into
//! `storage_tls_dir`, a directory the two stores read: `cert.pem`,
//! `key.pem` and `password`, the basic auth password they require. The
//! stores re-read the certificate every second, so renewing it is a file
//! write. The server's clients trust the CA alone and send the password.
//!
//! Blocking file I/O in [`ensure`] and [`StorageAuth::load`]: call them
//! from `spawn_blocking` in async code.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use monitorflow_common::redact::Secret;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use ring::rand::{SecureRandom, SystemRandom};

use crate::ca::{CaError, CertificateAuthority, ServerName};
use crate::enroll::server_cert::{self, Fit};

pub const CERT_FILE: &str = "cert.pem";
pub const KEY_FILE: &str = "key.pem";
pub const PASSWORD_FILE: &str = "password";
/// The stores' `-httpAuth.username`.
pub const USERNAME: &str = "monitorflow";
pub const VALIDITY: Duration = Duration::from_secs(90 * 24 * 60 * 60);
/// A certificate expiring sooner than this is replaced.
pub const RENEW_BEFORE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// Time between two checks of the running server.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60);
const PASSWORD_BYTES: usize = 32;

#[derive(Debug, thiserror::Error)]
pub enum StorageTlsError {
    #[error("{0}")]
    Url(String),
    #[error("{}: {error}", path.display())]
    Io { path: PathBuf, error: io::Error },
    #[error(transparent)]
    Ca(#[from] CaError),
    #[error("{0}")]
    Client(String),
}

/// What [`ensure`] did to the certificate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Kept,
    Issued,
}

/// The hosts of `urls`, which must be https: the certificate's names.
pub fn names(urls: &[&str]) -> Result<Vec<ServerName>, StorageTlsError> {
    let mut names = Vec::new();
    for url in urls {
        let parsed = reqwest::Url::parse(url)
            .map_err(|e| StorageTlsError::Url(format!("invalid store URL: {e}")))?;
        if parsed.scheme() != "https" {
            return Err(StorageTlsError::Url(format!(
                "store URL {}://{}… must be https when storage_tls_dir is set",
                parsed.scheme(),
                parsed.host_str().unwrap_or("")
            )));
        }
        let host = parsed
            .host_str()
            .ok_or_else(|| StorageTlsError::Url("store URL without a host".into()))?;
        // IPv6 hosts keep their brackets in a URL.
        let bare = host.trim_start_matches('[').trim_end_matches(']');
        let name = match bare.parse() {
            Ok(ip) => ServerName::Ip(ip),
            Err(_) => ServerName::Dns(host.to_owned()),
        };
        if !names.contains(&name) {
            names.push(name);
        }
    }
    Ok(names)
}

/// Makes `dir` hold a password and a certificate from `ca` for `names`,
/// valid for at least [`RENEW_BEFORE`]. The password is created once and
/// kept. The certificate is issued again when missing, expiring, missing a
/// name, not matching its key, or from another CA: these files are the
/// server's alone, and a restored or replaced CA must take over.
pub fn ensure(
    ca: &CertificateAuthority,
    dir: &Path,
    names: &[ServerName],
) -> Result<Outcome, StorageTlsError> {
    let password = dir.join(PASSWORD_FILE);
    if fs::metadata(&password).map_or(true, |m| m.len() == 0) {
        let mut bytes = [0u8; PASSWORD_BYTES];
        SystemRandom::new()
            .fill(&mut bytes)
            .map_err(|_| CaError::Signing("system random generator failed".into()))?;
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        server_cert::write(&password, hex.as_bytes(), 0o600)?;
        tracing::info!(dir = %dir.display(), "created the password of the metric and log stores");
    }
    let (cert, key) = (dir.join(CERT_FILE), dir.join(KEY_FILE));
    if server_cert::fit(ca, &cert, &key, names, RENEW_BEFORE)? == Fit::Current {
        return Ok(Outcome::Kept);
    }
    let serial = server_cert::issue_pair(&cert, &key, |csr| {
        ca.issue_tls_server(csr, names, Some(VALIDITY))
    })?;
    tracing::info!(cert = %cert.display(), %serial, "issued the certificate of the metric and log stores");
    Ok(Outcome::Issued)
}

/// What the clients of the stores need: the CA to trust and the password.
#[derive(Debug, Clone)]
pub struct StorageAuth {
    ca_pem: String,
    password: Secret,
}

impl StorageAuth {
    pub fn new(ca_pem: impl Into<String>, password: Secret) -> Self {
        Self {
            ca_pem: ca_pem.into(),
            password,
        }
    }

    /// Reads the password [`ensure`] wrote in `dir`.
    pub fn load(ca_pem: impl Into<String>, dir: &Path) -> Result<Self, StorageTlsError> {
        let path = dir.join(PASSWORD_FILE);
        let password =
            fs::read_to_string(&path).map_err(|error| StorageTlsError::Io { path, error })?;
        Ok(Self::new(ca_pem, Secret::new(password.trim())))
    }

    /// Makes `builder` trust only the CA and send the password with every
    /// request.
    pub fn apply(
        &self,
        builder: reqwest::ClientBuilder,
    ) -> Result<reqwest::ClientBuilder, StorageTlsError> {
        let client = |e: &dyn std::fmt::Display| StorageTlsError::Client(e.to_string());
        let ca = reqwest::Certificate::from_pem(self.ca_pem.as_bytes()).map_err(|e| client(&e))?;
        let credentials = base64::engine::general_purpose::STANDARD
            .encode(format!("{USERNAME}:{}", self.password.expose()));
        let mut value =
            HeaderValue::from_str(&format!("Basic {credentials}")).map_err(|e| client(&e))?;
        value.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, value);
        Ok(builder
            .tls_built_in_root_certs(false)
            .add_root_certificate(ca)
            .default_headers(headers))
    }
}

/// Prepares `dir` for `urls`, then keeps the certificate renewed every
/// [`CHECK_INTERVAL`] for as long as the server runs. Returns what the
/// clients need.
pub async fn start(
    ca: Arc<CertificateAuthority>,
    dir: PathBuf,
    urls: &[&str],
) -> Result<StorageAuth, StorageTlsError> {
    let names = names(urls)?;
    let (issuer, path, list) = (ca.clone(), dir.clone(), names.clone());
    tokio::task::spawn_blocking(move || ensure(&issuer, &path, &list))
        .await
        .map_err(|e| StorageTlsError::Client(e.to_string()))??;
    let auth = StorageAuth::load(ca.ca_cert_pem(), &dir)?;
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(CHECK_INTERVAL);
        timer.tick().await;
        loop {
            timer.tick().await;
            let (issuer, path, list) = (ca.clone(), dir.clone(), names.clone());
            match tokio::task::spawn_blocking(move || ensure(&issuer, &path, &list)).await {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "cannot renew the certificate of the metric and log stores")
                }
                Err(e) => tracing::warn!(error = %e, "renewal of the stores' certificate panicked"),
            }
        }
    });
    Ok(auth)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use rcgen::{CertificateParams, KeyPair};
    use tempfile::TempDir;
    use x509_parser::prelude::{FromDer, X509Certificate};

    use super::*;
    use crate::ca::CaOptions;

    fn new_ca(dir: &TempDir, name: &str) -> CertificateAuthority {
        CertificateAuthority::open_or_create(&dir.path().join(name), CaOptions::default()).unwrap()
    }

    fn stores() -> Vec<ServerName> {
        names(&["https://victoriametrics:8428", "https://victorialogs:9428/"]).unwrap()
    }

    fn serial(dir: &Path) -> Vec<u8> {
        let pem = pem::parse(fs::read(dir.join(CERT_FILE)).unwrap()).unwrap();
        let (_, cert) = X509Certificate::from_der(pem.contents()).unwrap();
        cert.raw_serial().to_vec()
    }

    #[test]
    fn names_are_the_hosts_of_https_urls() {
        assert_eq!(
            stores(),
            [
                ServerName::Dns("victoriametrics".into()),
                ServerName::Dns("victorialogs".into())
            ]
        );
        assert_eq!(
            names(&["https://10.0.0.5:8428", "https://10.0.0.5:9428"]).unwrap(),
            [ServerName::Ip("10.0.0.5".parse().unwrap())]
        );
        let err = names(&["http://victoriametrics:8428"]).unwrap_err();
        assert!(err.to_string().contains("must be https"), "{err}");
    }

    #[test]
    fn issues_once_then_keeps_with_the_same_password() {
        let dir = TempDir::new().unwrap();
        let ca = new_ca(&dir, "ca");
        let out = dir.path().join("tls");
        assert_eq!(ensure(&ca, &out, &stores()).unwrap(), Outcome::Issued);
        let password = fs::read_to_string(out.join(PASSWORD_FILE)).unwrap();
        assert_eq!(password.len(), 2 * PASSWORD_BYTES);
        for file in [KEY_FILE, PASSWORD_FILE] {
            let mode = fs::metadata(out.join(file)).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "{file}");
        }
        let first = serial(&out);
        assert_eq!(ensure(&ca, &out, &stores()).unwrap(), Outcome::Kept);
        assert_eq!(serial(&out), first);
        assert_eq!(
            fs::read_to_string(out.join(PASSWORD_FILE)).unwrap(),
            password
        );
    }

    #[test]
    fn the_certificate_serves_tls_only() {
        let dir = TempDir::new().unwrap();
        let ca = new_ca(&dir, "ca");
        let out = dir.path().join("tls");
        ensure(&ca, &out, &stores()).unwrap();
        let pem = pem::parse(fs::read(out.join(CERT_FILE)).unwrap()).unwrap();
        let (_, cert) = X509Certificate::from_der(pem.contents()).unwrap();
        let eku = cert.extended_key_usage().unwrap().unwrap().value;
        assert!(eku.server_auth && !eku.client_auth);
        let sans = cert.subject_alternative_name().unwrap().unwrap().value;
        assert!(sans
            .general_names
            .iter()
            .all(|n| !matches!(n, x509_parser::extensions::GeneralName::URI(_))));
        // The listeners' verifier refuses it as a client certificate.
        let der = rustls::pki_types::CertificateDer::from(pem.contents().to_vec());
        assert!(ca.verify_issued(&der).is_err());
    }

    #[test]
    fn renews_when_expiring_or_from_another_ca() {
        let dir = TempDir::new().unwrap();
        let ca = new_ca(&dir, "ca");
        let out = dir.path().join("tls");
        fs::create_dir_all(&out).unwrap();
        // 20 days left: inside the margin.
        let key = KeyPair::generate().unwrap();
        let csr = CertificateParams::default()
            .serialize_request(&key)
            .unwrap()
            .pem()
            .unwrap();
        let short = ca
            .issue_tls_server(&csr, &stores(), Some(Duration::from_secs(20 * 86_400)))
            .unwrap();
        fs::write(out.join(CERT_FILE), &short.cert_pem).unwrap();
        fs::write(out.join(KEY_FILE), key.serialize_pem()).unwrap();
        assert_eq!(ensure(&ca, &out, &stores()).unwrap(), Outcome::Issued);
        assert_eq!(ensure(&ca, &out, &stores()).unwrap(), Outcome::Kept);

        let password = fs::read_to_string(out.join(PASSWORD_FILE)).unwrap();
        let other = new_ca(&dir, "other");
        assert_eq!(ensure(&other, &out, &stores()).unwrap(), Outcome::Issued);
        assert_eq!(
            fs::read_to_string(out.join(PASSWORD_FILE)).unwrap(),
            password
        );
    }

    #[test]
    fn auth_reads_the_password() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(PASSWORD_FILE), "s3cret\n").unwrap();
        let ca = new_ca(&dir, "ca");
        let auth = StorageAuth::load(ca.ca_cert_pem(), dir.path()).unwrap();
        assert_eq!(auth.password.expose(), "s3cret");
        assert!(!format!("{auth:?}").contains("s3cret"));
        auth.apply(reqwest::Client::builder())
            .unwrap()
            .build()
            .unwrap();
    }
}
