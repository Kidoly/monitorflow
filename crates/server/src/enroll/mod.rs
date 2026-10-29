//! Enrollment of new agents and relays (ADR-0007): an administrator creates
//! a one-time token, the peer exchanges it with a CSR for a certificate;
//! a reusable install key enrolls many agents of a site (ADR-0058).

pub mod install_key;
pub mod limiter;
pub mod server_cert;
mod service;
pub mod token;

pub use limiter::FailureLimiter;
pub use service::{serve, EnrollError, Enrolled, Enroller};

use monitorflow_common::tls::{fingerprint, Fingerprint};
use rustls::pki_types::CertificateDer;

use crate::ca::CertificateAuthority;

/// SHA-256 of the CA certificate, which tokens carry so that peers can pin
/// it.
pub fn ca_fingerprint(ca: &CertificateAuthority) -> Fingerprint {
    let pem = pem::parse(ca.ca_cert_pem()).expect("the CA checked its certificate when loading");
    fingerprint(&CertificateDer::from(pem.into_contents()))
}
