//! Enrollment of new agents and relays (ADR-0007): an administrator creates
//! a one-time token, the peer exchanges it with a CSR for a certificate;
//! a reusable install key enrolls many agents of a site (ADR-0073), or one
//! relay, whose names an administrator confirms (ADR-0066).

pub mod install_key;
pub mod limiter;
pub mod relay_names;
pub mod server_cert;
mod service;
pub mod token;

pub use limiter::FailureLimiter;
pub use service::{serve, serve_following, EnrollError, Enrolled, Enroller};

use monitorflow_common::tls::Fingerprint;

use crate::ca::CertificateAuthority;

/// SHA-256 of the active CA certificate, which tokens carry so that peers
/// can pin it.
pub fn ca_fingerprint(ca: &CertificateAuthority) -> Fingerprint {
    ca.fingerprint()
}
