//! Certificates of a machine a probe or an agent checks, always verified:
//! by the machine's roots, or by the SHA-256 fingerprint a person
//! confirmed. Every handshake records the certificate it met, so a test
//! can show it and a failure can say what changed. Used by vCenter and
//! ESXi checks (ADR-0156) and Jolokia's (ADR-0183).

use std::sync::{Arc, Mutex, OnceLock};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::WebPkiServerVerifier;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, Error, RootCertStore, SignatureScheme};

pub use super::Fingerprint;
use super::{fingerprint as of_der, provider};

/// Reads a fingerprint written in hex, with or without colons or spaces.
pub fn parse_fingerprint(text: &str) -> Option<Fingerprint> {
    let hex: String = text
        .chars()
        .filter(|c| !matches!(c, ':' | ' ' | '-'))
        .collect();
    if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

/// `AB:CD:...`, as vCenter and browsers show fingerprints.
pub fn show_fingerprint(fingerprint: &Fingerprint) -> String {
    fingerprint
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// The SHA-256 fingerprint of a DER certificate.
pub fn fingerprint(der: &[u8]) -> Fingerprint {
    of_der(&CertificateDer::from(der))
}

/// The certificate a handshake met.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    pub fingerprint: Fingerprint,
    /// Whether the machine's roots trust it for the name.
    pub trusted: bool,
    pub subject: String,
    /// Unix seconds; 0 when it does not parse.
    pub not_after: i64,
}

/// The roots the machine trusts, or Mozilla's without any. Loaded once.
pub fn roots() -> Arc<RootCertStore> {
    static ROOTS: OnceLock<Arc<RootCertStore>> = OnceLock::new();
    ROOTS
        .get_or_init(|| {
            let mut store = RootCertStore::empty();
            let (added, _) =
                store.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
            if added == 0 {
                store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            }
            Arc::new(store)
        })
        .clone()
}

/// What a connection accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    /// The roots, or the pinned certificate.
    Verify(Option<Fingerprint>),
    /// Anything: only to look at the certificate, never to log in.
    Look,
}

#[derive(Debug)]
pub struct Verifier {
    roots: Arc<WebPkiServerVerifier>,
    trust: Trust,
    seen: Mutex<Option<Seen>>,
}

impl Verifier {
    pub fn new(roots: Arc<RootCertStore>, trust: Trust) -> Result<Arc<Self>, String> {
        let roots = WebPkiServerVerifier::builder_with_provider(roots, provider())
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Arc::new(Self {
            roots,
            trust,
            seen: Mutex::new(None),
        }))
    }

    /// The certificate of the last handshake.
    pub fn seen(&self) -> Option<Seen> {
        self.seen.lock().ok()?.clone()
    }

    /// Why the last handshake failed on its certificate, if it did: it
    /// changed from the one pinned, or nothing trusts it yet.
    pub fn failure(&self) -> Option<String> {
        let seen = self.seen()?;
        let pin = match self.trust {
            Trust::Look => return None,
            Trust::Verify(pin) => pin,
        };
        if seen.trusted || pin == Some(seen.fingerprint) {
            return None;
        }
        let shown = show_fingerprint(&seen.fingerprint);
        Some(match pin {
            Some(_) => format!("the certificate changed: its fingerprint is now {shown}"),
            None => format!(
                "the certificate is not trusted; its fingerprint is {shown}, confirm it to trust it"
            ),
        })
    }

    /// A client configuration that verifies with `self`.
    pub fn config(self: &Arc<Self>) -> Result<rustls::ClientConfig, String> {
        let mut config = rustls::ClientConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .map_err(|e| e.to_string())?
            .dangerous()
            .with_custom_certificate_verifier(self.clone())
            .with_no_client_auth();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(config)
    }
}

fn describe(der: &[u8]) -> (String, i64) {
    match x509_parser::parse_x509_certificate(der) {
        Ok((_, cert)) => {
            let subject = cert
                .subject()
                .iter_common_name()
                .next()
                .and_then(|cn| cn.as_str().ok().map(str::to_owned))
                .unwrap_or_else(|| cert.subject().to_string());
            let subject: String = subject
                .chars()
                .filter(|c| !c.is_control())
                .take(200)
                .collect();
            (subject, cert.validity().not_after.timestamp())
        }
        Err(_) => (String::new(), 0),
    }
}

impl ServerCertVerifier for Verifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        let verdict = self.roots.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        );
        let print = fingerprint(end_entity.as_ref());
        let (subject, not_after) = describe(end_entity.as_ref());
        if let Ok(mut seen) = self.seen.lock() {
            *seen = Some(Seen {
                fingerprint: print,
                trusted: verdict.is_ok(),
                subject,
                not_after,
            });
        }
        match self.trust {
            Trust::Look => Ok(ServerCertVerified::assertion()),
            Trust::Verify(Some(pin)) if pin == print => Ok(ServerCertVerified::assertion()),
            Trust::Verify(_) => verdict,
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.roots.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.roots.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.roots.supported_verify_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_read_and_show() {
        let hex = "ab".repeat(32);
        let print = parse_fingerprint(&hex).unwrap();
        assert_eq!(print, [0xab; 32]);
        let shown = show_fingerprint(&print);
        assert!(shown.starts_with("AB:AB:"));
        assert_eq!(parse_fingerprint(&shown), Some(print));
        assert!(parse_fingerprint("ab").is_none());
        assert!(parse_fingerprint(&"zz".repeat(32)).is_none());
        assert_eq!(fingerprint(b"").len(), 32);
    }

    #[test]
    fn a_handshake_says_what_it_met() {
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = rcgen::CertificateParams::new(vec!["jvm.example.test".to_owned()])
            .unwrap()
            .self_signed(&key)
            .unwrap();
        let print = fingerprint(cert.der());
        let shown = show_fingerprint(&print);
        let name = ServerName::try_from("jvm.example.test").unwrap();
        let meet = |trust: Trust| {
            let verifier = Verifier::new(roots(), trust).unwrap();
            let verdict = verifier.verify_server_cert(cert.der(), &[], &name, &[], UnixTime::now());
            (
                verdict.is_ok(),
                verifier.failure(),
                verifier.seen().unwrap(),
            )
        };
        let (ok, failure, seen) = meet(Trust::Verify(Some(print)));
        assert!(ok && failure.is_none());
        assert!(!seen.trusted);
        assert_eq!(seen.fingerprint, print);
        assert!(seen.not_after > 0);
        let (ok, failure, _) = meet(Trust::Verify(Some([0; 32])));
        assert!(!ok);
        assert_eq!(
            failure.unwrap(),
            format!("the certificate changed: its fingerprint is now {shown}")
        );
        let (ok, failure, _) = meet(Trust::Verify(None));
        assert!(!ok && failure.unwrap().contains("not trusted"));
        let (ok, failure, _) = meet(Trust::Look);
        assert!(ok && failure.is_none());
    }
}
