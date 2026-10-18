//! Who is on the other end of an mTLS connection.
//!
//! Convention: every MonitorFlow certificate carries exactly one URI SAN of
//! the form `monitorflow://<role>/<id>`, where role is `agent`, `relay` or
//! `server` and the ID matches [`validate_id`]. The CA writes it at issuance;
//! the subject CN is informational and never used to decide identity. Other
//! URI SANs (not `monitorflow://`) are ignored. See ADR-0004.

use std::fmt;

use rustls::pki_types::CertificateDer;
use tonic::{Request, Status};
use x509_parser::extensions::GeneralName;
use x509_parser::prelude::{FromDer, X509Certificate};

use super::TlsError;

/// Scheme of the identity URI SAN.
pub const URI_SCHEME: &str = "monitorflow://";

const MAX_ID_LEN: usize = 64;

/// What kind of MonitorFlow component a certificate belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PeerRole {
    Agent,
    Relay,
    Server,
}

impl PeerRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Relay => "relay",
            Self::Server => "server",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        [Self::Agent, Self::Relay, Self::Server]
            .into_iter()
            .find(|role| role.as_str() == s)
    }
}

impl fmt::Display for PeerRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The rule for agent, relay and server IDs: 1 to 64 characters from
/// `[A-Za-z0-9._-]`. `scripts/dev-certs.sh` applies the same rule.
pub fn validate_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > MAX_ID_LEN {
        return Err(format!("ID must be 1 to {MAX_ID_LEN} characters"));
    }
    if !id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return Err(format!(
            "ID `{id}` may only contain A-Z, a-z, 0-9, `.`, `_` and `-`"
        ));
    }
    Ok(())
}

/// Authenticated role and ID of a peer.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PeerIdentity {
    role: PeerRole,
    id: String,
}

impl PeerIdentity {
    pub fn new(role: PeerRole, id: &str) -> Result<Self, TlsError> {
        validate_id(id).map_err(TlsError::PeerIdentity)?;
        Ok(Self {
            role,
            id: id.to_owned(),
        })
    }

    pub fn role(&self) -> PeerRole {
        self.role
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// The URI SAN the CA puts in the certificate.
    pub fn uri(&self) -> String {
        format!("{URI_SCHEME}{}/{}", self.role, self.id)
    }

    /// Parses `monitorflow://<role>/<id>`.
    pub fn from_uri(uri: &str) -> Result<Self, TlsError> {
        let fail = |why: &str| TlsError::PeerIdentity(format!("`{uri}`: {why}"));
        let rest = uri
            .strip_prefix(URI_SCHEME)
            .ok_or_else(|| fail("not a monitorflow:// URI"))?;
        let (role, id) = rest
            .split_once('/')
            .ok_or_else(|| fail("expected monitorflow://<role>/<id>"))?;
        let role = PeerRole::parse(role).ok_or_else(|| fail("unknown role"))?;
        validate_id(id).map_err(|e| fail(&e))?;
        Ok(Self {
            role,
            id: id.to_owned(),
        })
    }

    /// Identity from the certificate's single `monitorflow://` URI SAN.
    pub fn from_cert(cert: &CertificateDer<'_>) -> Result<Self, TlsError> {
        let fail = |why: String| TlsError::PeerIdentity(why);
        let (_, cert) = X509Certificate::from_der(cert)
            .map_err(|e| fail(format!("cannot parse certificate: {e}")))?;
        let san = cert
            .subject_alternative_name()
            .map_err(|e| fail(format!("invalid subjectAltName: {e}")))?;
        let mut uris = san
            .iter()
            .flat_map(|san| &san.value.general_names)
            .filter_map(|name| match name {
                GeneralName::URI(uri) if uri.starts_with(URI_SCHEME) => Some(*uri),
                _ => None,
            });
        match (uris.next(), uris.next()) {
            (Some(uri), None) => Self::from_uri(uri),
            _ => Err(fail(format!(
                "certificate must have exactly one {URI_SCHEME} URI SAN"
            ))),
        }
    }
}

impl fmt::Display for PeerIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.role, self.id)
    }
}

/// Role and ID of the authenticated peer that sent `request`, from its
/// client certificate (see the module docs). The chain was already verified
/// against our CA during the handshake.
///
/// Works only for servers fed by [`super::incoming`]; any failure maps to
/// `UNAUTHENTICATED`.
// Handlers return `tonic::Status` as is, so `?` works without conversion.
#[allow(clippy::result_large_err)]
pub fn peer_identity<T>(request: &Request<T>) -> Result<PeerIdentity, Status> {
    let certs = request
        .peer_certs()
        .ok_or_else(|| Status::unauthenticated("no client certificate"))?;
    let leaf = certs
        .first()
        .ok_or_else(|| Status::unauthenticated("no client certificate"))?;
    PeerIdentity::from_cert(leaf).map_err(|e| Status::unauthenticated(e.to_string()))
}

#[cfg(test)]
mod tests {
    use rcgen::SanType;

    use super::*;
    use crate::tls::test_support::{uri_san, Ca};

    fn identity(sans: Vec<SanType>) -> Result<PeerIdentity, String> {
        let ca = Ca::new("ca");
        let leaf = ca.leaf("cn-is-ignored", sans, 10);
        PeerIdentity::from_cert(leaf.cert.der()).map_err(|e| e.to_string())
    }

    #[test]
    fn reads_role_and_id_from_the_uri_san() {
        let ca = Ca::new("ca");
        let agent = PeerIdentity::from_cert(ca.client("agent-42", 10).cert.der()).unwrap();
        assert_eq!((agent.role(), agent.id()), (PeerRole::Agent, "agent-42"));
        let relay = PeerIdentity::from_cert(ca.relay("site-a.relay", 11).cert.der()).unwrap();
        assert_eq!(
            (relay.role(), relay.id()),
            (PeerRole::Relay, "site-a.relay")
        );
        let server = PeerIdentity::from_cert(ca.server().cert.der()).unwrap();
        assert_eq!(server.role(), PeerRole::Server);
    }

    #[test]
    fn other_uri_sans_are_ignored() {
        let id = identity(vec![
            uri_san("spiffe://example.org/x"),
            uri_san("monitorflow://relay/r1"),
        ]);
        assert_eq!(id.unwrap().uri(), "monitorflow://relay/r1");
    }

    #[test]
    fn rejects_bad_identity_sans() {
        let too_long = format!("monitorflow://agent/{}", "a".repeat(65));
        for (sans, why) in [
            (vec![], "exactly one monitorflow:// URI SAN"),
            (
                vec![
                    uri_san("monitorflow://agent/a"),
                    uri_san("monitorflow://agent/b"),
                ],
                "exactly one monitorflow:// URI SAN",
            ),
            (vec![uri_san("monitorflow://admin/a")], "unknown role"),
            (
                vec![uri_san("monitorflow://agent")],
                "expected monitorflow://<role>/<id>",
            ),
            (vec![uri_san("monitorflow://agent/a/b")], "may only contain"),
            (vec![uri_san("monitorflow://agent/")], "1 to 64 characters"),
            (vec![uri_san(&too_long)], "1 to 64 characters"),
        ] {
            let err = identity(sans).unwrap_err();
            assert!(err.contains(why), "{err}");
        }
    }

    #[test]
    fn id_rule() {
        for ok in ["a", "agent-1", "site_a.relay-01", &"x".repeat(64)] {
            assert!(validate_id(ok).is_ok(), "{ok}");
        }
        for bad in ["", "a b", "a/b", "é", "a=b", &"x".repeat(65)] {
            assert!(validate_id(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn uri_round_trips() {
        let id = PeerIdentity::new(PeerRole::Relay, "r1").unwrap();
        assert_eq!(PeerIdentity::from_uri(&id.uri()).unwrap(), id);
        assert!(PeerIdentity::new(PeerRole::Agent, "a/b").is_err());
    }
}
