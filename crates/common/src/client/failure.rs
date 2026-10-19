//! How a peer's connection to its upstream ended, and how often to retry
//! (ADR-0009).

use std::error::Error as StdError;
use std::io;

use rustls::AlertDescription;
use tonic::{Code, Status};

use crate::tls::error_chain;

/// Why a connection ended.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Failure {
    #[error("{0}")]
    Transient(String),
    /// The upstream registered a newer connection with our identity.
    #[error("replaced by a newer connection with the same identity")]
    Replaced,
    #[error("the upstream rejected this peer: {0}")]
    Rejected(String),
    #[error("the upstream says this peer's certificate is revoked")]
    Revoked,
    /// Not a failure: the certificate was renewed, reconnect with it.
    #[error("the certificate was renewed")]
    Renewed,
}

/// Maps a status from the upstream, looking through transport errors for
/// a TLS alert about our certificate.
pub fn classify_status(status: &Status) -> Failure {
    match status.code() {
        Code::Aborted => Failure::Replaced,
        Code::PermissionDenied
        | Code::Unauthenticated
        | Code::FailedPrecondition
        | Code::InvalidArgument => {
            Failure::Rejected(format!("{:?}: {}", status.code(), status.message()))
        }
        _ => classify_error(
            status,
            &format!("{:?}: {}", status.code(), error_chain(status)),
        ),
    }
}

/// Alerts the upstream sends when it refuses our certificate. rustls sends
/// `DecryptError` for a bad signature on it, as from a CA with the same
/// name but another key.
const CERT_ALERTS: [AlertDescription; 9] = [
    AlertDescription::CertificateRevoked,
    AlertDescription::BadCertificate,
    AlertDescription::UnsupportedCertificate,
    AlertDescription::CertificateExpired,
    AlertDescription::CertificateUnknown,
    AlertDescription::CertificateRequired,
    AlertDescription::UnknownCA,
    AlertDescription::AccessDenied,
    AlertDescription::DecryptError,
];

pub fn classify_error(error: &(dyn StdError + 'static), message: &str) -> Failure {
    match find_alert(error) {
        Some(AlertDescription::CertificateRevoked) => Failure::Revoked,
        Some(alert) if CERT_ALERTS.contains(&alert) => {
            Failure::Rejected(format!("TLS alert {alert:?}"))
        }
        _ => Failure::Transient(message.to_owned()),
    }
}

/// The TLS alert the peer sent about our certificate, if `error` was caused
/// by one. h2 and `io::Error` hide their inner error from `source()`, so
/// when downcasting finds nothing, look for rustls's own wording of the
/// alert in the error chain.
fn find_alert(error: &(dyn StdError + 'static)) -> Option<AlertDescription> {
    let mut next = Some(error);
    while let Some(error) = next {
        if let Some(rustls::Error::AlertReceived(alert)) = error.downcast_ref::<rustls::Error>() {
            return Some(*alert);
        }
        if let Some(inner) = error.downcast_ref::<io::Error>().and_then(|e| e.get_ref()) {
            if let Some(alert) = find_alert(inner) {
                return Some(alert);
            }
        }
        next = error.source();
    }
    let chain = error_chain(error);
    CERT_ALERTS
        .into_iter()
        .find(|alert| chain.contains(&rustls::Error::AlertReceived(*alert).to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_from_the_server_are_classified() {
        let replaced = Status::aborted("replaced by a newer connection with the same ID");
        assert_eq!(classify_status(&replaced), Failure::Replaced);
        for status in [
            Status::permission_denied("agent a may not speak for agent b"),
            Status::unauthenticated("no client certificate"),
            Status::failed_precondition("protocol version 2.0 is not compatible"),
            Status::invalid_argument("the first envelope must be Hello"),
        ] {
            assert!(
                matches!(classify_status(&status), Failure::Rejected(_)),
                "{status:?}"
            );
        }
        for status in [
            Status::deadline_exceeded("no message for 90s"),
            Status::unavailable("server shutting down"),
            Status::unknown("transport error"),
        ] {
            assert!(
                matches!(classify_status(&status), Failure::Transient(_)),
                "{status:?}"
            );
        }
    }

    #[test]
    fn tls_alerts_are_found_inside_io_errors() {
        let wrap = |alert| {
            let tls = rustls::Error::AlertReceived(alert);
            io::Error::new(io::ErrorKind::ConnectionAborted, tls)
        };
        let revoked = wrap(AlertDescription::CertificateRevoked);
        assert_eq!(classify_error(&revoked, "x"), Failure::Revoked);
        let unknown_ca = wrap(AlertDescription::UnknownCA);
        assert!(matches!(
            classify_error(&unknown_ca, "x"),
            Failure::Rejected(_)
        ));
        let reset = io::Error::new(io::ErrorKind::ConnectionReset, "reset");
        assert_eq!(
            classify_error(&reset, "reset"),
            Failure::Transient("reset".into())
        );
        let other = wrap(AlertDescription::InternalError);
        assert!(matches!(classify_error(&other, "x"), Failure::Transient(_)));
        // Hidden behind a wrapper that drops the source: found by its text.
        let flat = io::Error::other(format!(
            "connection error: {}",
            wrap(AlertDescription::CertificateRevoked)
        ));
        assert_eq!(classify_error(&flat, "x"), Failure::Revoked);
    }
}
