//! The certificate a website showed (ADR-0150): whether the machine
//! running the check trusts it, whether it is for the name asked, and when
//! it expires.
//!
//! [`Recorder`] wraps rustls' own WebPKI verifier: it keeps the verdict
//! and the certificate, then lets the handshake go on, so the check can
//! still read the page and say what is wrong with the certificate rather
//! than only that TLS failed. Handshake signatures are still verified, so
//! the peer holds the key of the certificate it showed. Secrets are never
//! sent over a connection whose certificate failed (see `fetch`).

use std::sync::{Arc, Mutex, OnceLock};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::WebPkiServerVerifier;
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::server::ParsedCertificate;
use rustls::{CertificateError, DigitallySignedStruct, Error, RootCertStore, SignatureScheme};

/// Why a certificate is not good, in a word the interface translates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    Expired,
    NotYetValid,
    /// Signed by itself, not by an authority.
    SelfSigned,
    /// Signed by an authority this machine does not trust.
    Untrusted,
    /// Valid, but for other names than the one asked.
    Name,
    Revoked,
    /// Anything else: a bad encoding, an unsupported algorithm.
    Other,
}

impl Problem {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Expired => "expired",
            Self::NotYetValid => "not_yet_valid",
            Self::SelfSigned => "self_signed",
            Self::Untrusted => "untrusted",
            Self::Name => "name",
            Self::Revoked => "revoked",
            Self::Other => "other",
        }
    }
}

/// What the check learnt of the certificate of one connection.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    /// `None` when the chain is trusted, valid now and for the name.
    pub problem: Option<Problem>,
    /// Whether the certificate names the host asked, whatever else.
    pub name_ok: bool,
    /// Who issued it: its common name, or the whole name.
    pub issuer: String,
    /// Who it is for: its common name, or the whole name.
    pub subject: String,
    /// When it stops being valid, Unix seconds.
    pub not_after: i64,
}

impl Report {
    pub fn ok(&self) -> bool {
        self.problem.is_none()
    }

    /// Days left until it expires, negative once expired.
    pub fn days_left(&self, now_unix: i64) -> f64 {
        (self.not_after - now_unix) as f64 / 86_400.0
    }
}

/// The roots the machine trusts: the system's, or Mozilla's when the
/// system has none (a minimal container). Loaded once per process.
pub fn roots() -> Arc<RootCertStore> {
    static ROOTS: OnceLock<Arc<RootCertStore>> = OnceLock::new();
    ROOTS
        .get_or_init(|| {
            let mut store = RootCertStore::empty();
            let native = rustls_native_certs::load_native_certs();
            let (added, _) = store.add_parsable_certificates(native.certs);
            if added == 0 {
                tracing::info!("no system certificate authorities; website checks trust Mozilla's");
                store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            }
            Arc::new(store)
        })
        .clone()
}

pub fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// A verifier that records its verdict and lets the handshake continue.
#[derive(Debug)]
pub struct Recorder {
    inner: Arc<WebPkiServerVerifier>,
    report: Mutex<Option<Report>>,
}

impl Recorder {
    pub fn new(roots: Arc<RootCertStore>) -> Result<Arc<Self>, String> {
        let inner = WebPkiServerVerifier::builder_with_provider(roots, provider())
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Arc::new(Self {
            inner,
            report: Mutex::new(None),
        }))
    }

    /// What the last handshake through this verifier showed.
    pub fn take(&self) -> Option<Report> {
        self.report.lock().ok()?.take()
    }
}

/// The verdict of `error` from the WebPKI verifier.
fn problem(error: &Error, self_signed: bool) -> Problem {
    let Error::InvalidCertificate(error) = error else {
        return Problem::Other;
    };
    match error {
        CertificateError::Expired | CertificateError::ExpiredContext { .. } => Problem::Expired,
        CertificateError::NotValidYet | CertificateError::NotValidYetContext { .. } => {
            Problem::NotYetValid
        }
        CertificateError::UnknownIssuer if self_signed => Problem::SelfSigned,
        CertificateError::UnknownIssuer => Problem::Untrusted,
        CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. } => {
            Problem::Name
        }
        CertificateError::Revoked => Problem::Revoked,
        _ => Problem::Other,
    }
}

/// The common name of `name`, or all of it, at most 256 bytes.
fn short_name(name: &x509_parser::x509::X509Name<'_>) -> String {
    let common = name
        .iter_common_name()
        .next()
        .and_then(|cn| cn.as_str().ok().map(str::to_owned));
    let text = common.unwrap_or_else(|| name.to_string());
    let mut text: String = text.chars().filter(|c| !c.is_control()).collect();
    while text.len() > 256 {
        text.pop();
    }
    text
}

/// Reads `end_entity` and judges it with `verdict` from the verifier.
pub fn report(
    end_entity: &CertificateDer<'_>,
    server_name: &ServerName<'_>,
    verdict: &Result<(), Error>,
) -> Report {
    let parsed = x509_parser::parse_x509_certificate(end_entity.as_ref()).ok();
    let (issuer, subject, not_after, self_signed) = match &parsed {
        Some((_, cert)) => (
            short_name(cert.issuer()),
            short_name(cert.subject()),
            cert.validity().not_after.timestamp(),
            cert.issuer() == cert.subject(),
        ),
        None => (String::new(), String::new(), 0, false),
    };
    let name_ok = ParsedCertificate::try_from(end_entity)
        .and_then(|cert| rustls::client::verify_server_name(&cert, server_name))
        .is_ok();
    let problem = match verdict {
        Ok(()) => None,
        Err(error) => Some(problem(error, self_signed)),
    };
    Report {
        problem,
        name_ok,
        issuer,
        subject,
        not_after,
    }
}

impl ServerCertVerifier for Recorder {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        let verdict = self
            .inner
            .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
            .map(drop);
        let report = report(end_entity, server_name, &verdict);
        if let Ok(mut slot) = self.report.lock() {
            *slot = Some(report);
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rcgen::{CertificateParams, DnType, KeyPair};

    fn self_signed(names: &[&str]) -> CertificateDer<'static> {
        let mut params =
            CertificateParams::new(names.iter().map(|n| n.to_string()).collect::<Vec<_>>())
                .unwrap();
        params.distinguished_name.push(DnType::CommonName, names[0]);
        params
            .self_signed(&KeyPair::generate().unwrap())
            .unwrap()
            .der()
            .clone()
    }

    #[test]
    fn problems_have_plain_codes() {
        let unknown = Err(Error::InvalidCertificate(CertificateError::UnknownIssuer));
        let cert = self_signed(&["web.test"]);
        let name = ServerName::try_from("web.test").unwrap();
        let r = report(&cert, &name, &unknown);
        assert_eq!(r.problem, Some(Problem::SelfSigned));
        assert!(r.name_ok);
        assert_eq!(
            (r.issuer.as_str(), r.subject.as_str()),
            ("web.test", "web.test")
        );
        assert!(r.not_after > 0);
        let other = ServerName::try_from("other.test").unwrap();
        assert!(!report(&cert, &other, &Ok(())).name_ok);
        assert_eq!(
            problem(&Error::InvalidCertificate(CertificateError::Expired), false),
            Problem::Expired
        );
        assert_eq!(
            problem(
                &Error::InvalidCertificate(CertificateError::UnknownIssuer),
                false
            ),
            Problem::Untrusted
        );
        assert_eq!(
            problem(
                &Error::InvalidCertificate(CertificateError::NotValidForName),
                false
            ),
            Problem::Name
        );
        assert_eq!(problem(&Error::General("x".into()), false), Problem::Other);
        assert_eq!(Problem::NotYetValid.as_str(), "not_yet_valid");
    }

    #[test]
    fn days_left_go_negative_once_expired() {
        let r = Report {
            problem: None,
            name_ok: true,
            issuer: String::new(),
            subject: String::new(),
            not_after: 86_400 * 10,
        };
        assert_eq!(r.days_left(86_400 * 5), 5.0);
        assert_eq!(r.days_left(86_400 * 12), -2.0);
    }
}
