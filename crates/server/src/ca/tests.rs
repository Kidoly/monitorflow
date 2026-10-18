use std::net::Ipv4Addr;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use monitorflow_common::config::TlsPaths;
use monitorflow_common::tls::Identity;
use rcgen::{CertificateParams, KeyPair};
use rustls::pki_types::UnixTime;
use rustls::server::WebPkiClientVerifier;
use rustls::{ClientConfig, RootCertStore, ServerConfig};
use tempfile::TempDir;
use tokio_rustls::{TlsAcceptor, TlsConnector};
use x509_parser::prelude::{FromDer, X509Certificate};
use x509_parser::revocation_list::CertificateRevocationList;

use super::*;

fn open(dir: &Path) -> CertificateAuthority {
    CertificateAuthority::open_or_create(dir, CaOptions::default()).unwrap()
}

fn new_ca() -> (TempDir, CertificateAuthority) {
    let dir = TempDir::new().unwrap();
    let ca = open(&dir.path().join("ca"));
    (dir, ca)
}

fn der(pem: &str) -> CertificateDer<'static> {
    CertificateDer::from(pem::parse(pem).unwrap().into_contents())
}

/// A CSR from a fresh key, with whatever the peer asks for in `params`.
fn csr_with(params: CertificateParams) -> (KeyPair, String) {
    let key = KeyPair::generate().unwrap();
    let csr = params.serialize_request(&key).unwrap().pem().unwrap();
    (key, csr)
}

fn csr() -> (KeyPair, String) {
    csr_with(CertificateParams::default())
}

fn identity(role: PeerRole, id: &str) -> PeerIdentity {
    PeerIdentity::new(role, id).unwrap()
}

fn localhost() -> Vec<ServerName> {
    vec![
        ServerName::Dns("localhost".into()),
        ServerName::Ip(Ipv4Addr::LOCALHOST.into()),
    ]
}

fn unix(t: SystemTime) -> i64 {
    t.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs() as i64
}

/// Verifies the chain to the CA with webpki, as a TLS server would.
fn chains_to(ca: &CertificateAuthority, cert: &CertificateDer<'_>) -> bool {
    let mut roots = RootCertStore::empty();
    roots.add(der(ca.ca_cert_pem())).unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider)
        .build()
        .unwrap();
    verifier
        .verify_client_cert(cert, &[], UnixTime::now())
        .is_ok()
}

#[test]
fn is_send_and_sync() {
    fn check<T: Send + Sync>() {}
    check::<CertificateAuthority>();
}

#[test]
fn creates_a_root_ca_and_reopens_it() {
    let (dir, ca) = new_ca();
    let ca_dir = dir.path().join("ca");
    let mode = |name| {
        fs::metadata(ca_dir.join(name))
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(mode(KEY_FILE), 0o600);
    assert_eq!(mode(""), 0o700);

    let cert_der = der(ca.ca_cert_pem());
    let (_, cert) = X509Certificate::from_der(&cert_der).unwrap();
    let constraints = cert.basic_constraints().unwrap().unwrap().value;
    assert!(constraints.ca);
    assert_eq!(constraints.path_len_constraint, Some(0));
    let usage = cert.key_usage().unwrap().unwrap().value;
    assert!(usage.key_cert_sign() && usage.crl_sign());
    assert!(!usage.digital_signature());
    let validity = cert.validity();
    let years = (validity.not_after.timestamp() - validity.not_before.timestamp()) / 86400 / 365;
    assert_eq!(years, 10);
    assert_eq!(
        cert.subject().iter_common_name().next().unwrap().as_str(),
        Ok("MonitorFlow CA")
    );

    let reopened = open(&ca_dir);
    assert_eq!(reopened.ca_cert_pem(), ca.ca_cert_pem());
    // The reopened CA signs with the same key and name.
    let (_, csr) = csr();
    let issued = reopened
        .issue(&csr, &identity(PeerRole::Agent, "a1"), None, &[])
        .unwrap();
    assert!(chains_to(&ca, &der(&issued.cert_pem)));
}

#[test]
fn refuses_a_key_readable_by_others() {
    let (dir, _) = new_ca();
    let ca_dir = dir.path().join("ca");
    let key = ca_dir.join(KEY_FILE);
    for mode in [0o640, 0o604, 0o660] {
        fs::set_permissions(&key, fs::Permissions::from_mode(mode)).unwrap();
        let err = CertificateAuthority::open_or_create(&ca_dir, CaOptions::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains("accessible by group or others"), "{err}");
        assert!(err.contains(&format!("mode {mode:03o}")), "{err}");
        assert!(err.contains("chmod 600"), "{err}");
    }
}

#[test]
fn refuses_a_half_written_ca() {
    let (dir, _) = new_ca();
    let ca_dir = dir.path().join("ca");
    fs::remove_file(ca_dir.join(CERT_FILE)).unwrap();
    let err = CertificateAuthority::open_or_create(&ca_dir, CaOptions::default()).unwrap_err();
    assert!(matches!(err, CaError::Incomplete { .. }), "{err}");
    assert!(
        ca_dir.join(KEY_FILE).exists(),
        "the key must not be replaced"
    );
}

#[test]
fn refuses_a_key_that_does_not_match() {
    let (dir, _) = new_ca();
    let other = TempDir::new().unwrap();
    open(other.path());
    let ca_dir = dir.path().join("ca");
    fs::copy(other.path().join(KEY_FILE), ca_dir.join(KEY_FILE)).unwrap();
    let err = CertificateAuthority::open_or_create(&ca_dir, CaOptions::default())
        .unwrap_err()
        .to_string();
    assert!(err.contains("does not match"), "{err}");
}

#[test]
fn issues_for_each_role() {
    let (_dir, ca) = new_ca();
    for (role, id, names) in [
        (PeerRole::Agent, "agent-1", vec![]),
        (PeerRole::Relay, "site-a.relay", vec![]),
        (PeerRole::Server, "central", localhost()),
    ] {
        let (_, csr) = csr();
        let issued = ca.issue(&csr, &identity(role, id), None, &names).unwrap();
        let cert_der = der(&issued.cert_pem);
        let peer = PeerIdentity::from_cert(&cert_der).unwrap();
        assert_eq!((peer.role(), peer.id()), (role, id));
        assert!(chains_to(&ca, &cert_der), "{role}");

        let (_, cert) = X509Certificate::from_der(&cert_der).unwrap();
        assert_eq!(
            cert.subject().iter_common_name().next().unwrap().as_str(),
            Ok(id)
        );
        assert_eq!(Serial::from_bytes(cert.raw_serial()), issued.serial);
        let validity = cert.validity();
        assert_eq!(validity.not_before.timestamp(), unix(issued.not_before));
        assert_eq!(validity.not_after.timestamp(), unix(issued.not_after));
        let now = unix(SystemTime::now());
        assert!((now - 300 - unix(issued.not_before)).abs() <= 2);
        assert_eq!(
            unix(issued.not_after) - unix(issued.not_before),
            7 * 86400 + 300
        );

        let eku = cert.extended_key_usage().unwrap().unwrap().value;
        assert!(eku.client_auth);
        assert_eq!(eku.server_auth, role == PeerRole::Server, "{role}");
        assert!(!cert.basic_constraints().unwrap().unwrap().value.ca);
        let sans = &cert.subject_alternative_name().unwrap().unwrap().value;
        assert_eq!(sans.general_names.len(), 1 + names.len());
    }
}

#[test]
fn validity_can_be_overridden_within_the_ca_lifetime() {
    let (_dir, ca) = new_ca();
    let agent = identity(PeerRole::Agent, "a1");
    let (_, csr) = csr();
    let issued = ca.issue(&csr, &agent, Some(DAY), &[]).unwrap();
    assert_eq!(
        unix(issued.not_after) - unix(issued.not_before),
        86400 + 300
    );
    for bad in [Duration::ZERO, 3651 * DAY] {
        let err = ca.issue(&csr, &agent, Some(bad), &[]).unwrap_err();
        assert!(matches!(err, CaError::InvalidRequest(_)), "{err}");
    }
}

#[test]
fn server_names_only_for_servers() {
    let (_dir, ca) = new_ca();
    let (_, csr) = csr();
    let err = ca
        .issue(&csr, &identity(PeerRole::Agent, "a1"), None, &localhost())
        .unwrap_err();
    assert!(err.to_string().contains("not agent"), "{err}");
    let err = ca
        .issue(&csr, &identity(PeerRole::Server, "central"), None, &[])
        .unwrap_err();
    assert!(err.to_string().contains("needs a DNS name"), "{err}");
}

#[test]
fn ignores_the_names_a_csr_asks_for() {
    let (_dir, ca) = new_ca();
    let mut params = CertificateParams::new(vec!["evil.example".into()]).unwrap();
    params.subject_alt_names.push(SanType::URI(
        "monitorflow://server/central".try_into().unwrap(),
    ));
    params
        .distinguished_name
        .push(DnType::CommonName, "central");
    params
        .distinguished_name
        .push(DnType::OrganizationName, "evil");
    let (_, csr) = csr_with(params);
    let issued = ca
        .issue(&csr, &identity(PeerRole::Agent, "agent-9"), None, &[])
        .unwrap();
    let cert_der = der(&issued.cert_pem);
    assert_eq!(
        PeerIdentity::from_cert(&cert_der).unwrap().uri(),
        "monitorflow://agent/agent-9"
    );
    let (_, cert) = X509Certificate::from_der(&cert_der).unwrap();
    assert_eq!(cert.subject().to_string(), "CN=agent-9");
    let sans = cert.subject_alternative_name().unwrap().unwrap().value;
    assert_eq!(
        format!("{:?}", sans.general_names),
        "[URI(\"monitorflow://agent/agent-9\")]"
    );
}

#[test]
fn rejects_a_tampered_csr() {
    let (_dir, ca) = new_ca();
    let mut params = CertificateParams::default();
    params
        .distinguished_name
        .push(DnType::CommonName, "agent-1");
    let (_, csr) = csr_with(params);
    let mut bytes = pem::parse(&csr).unwrap().into_contents();
    let at = bytes.windows(7).position(|w| w == b"agent-1").unwrap();
    bytes[at + 6] = b'2';
    let tampered = pem::encode(&pem::Pem::new("CERTIFICATE REQUEST", bytes));
    let err = ca
        .issue(&tampered, &identity(PeerRole::Agent, "a1"), None, &[])
        .unwrap_err();
    assert!(matches!(err, CaError::CsrSignature), "{err}");

    let err = ca
        .issue("not a CSR", &identity(PeerRole::Agent, "a1"), None, &[])
        .unwrap_err();
    assert!(matches!(err, CaError::InvalidCsr(_)), "{err}");
}

fn parse_crl(der: &[u8]) -> (u64, Vec<Serial>, i64, i64) {
    let (_, crl) = CertificateRevocationList::from_der(der).unwrap();
    let number = crl.crl_number().unwrap().to_u64_digits();
    let serials = crl
        .iter_revoked_certificates()
        .map(|r| Serial::from_bytes(r.raw_serial()))
        .collect();
    let this_update = crl.last_update().timestamp();
    let next_update = crl.next_update().unwrap().timestamp();
    (
        number.first().copied().unwrap_or(0),
        serials,
        this_update,
        next_update,
    )
}

#[test]
fn revocations_appear_in_signed_crls_and_persist() {
    let (dir, ca) = new_ca();
    let (number, serials, this_update, next_update) = parse_crl(&ca.crl().unwrap());
    assert_eq!(number, 1);
    assert!(serials.is_empty());
    assert_eq!(next_update - this_update, 86400);

    let (_, csr) = csr();
    let issued = ca
        .issue(&csr, &identity(PeerRole::Agent, "a1"), None, &[])
        .unwrap();
    ca.revoke(&issued.serial, RevocationReason::KeyCompromise)
        .unwrap();
    ca.revoke(&issued.serial, RevocationReason::Superseded)
        .unwrap();
    let (number, serials, _, _) = parse_crl(&ca.crl().unwrap());
    assert_eq!(number, 2);
    assert_eq!(serials, vec![issued.serial.clone()]);

    let reopened = CertificateAuthority::open_or_create(
        &dir.path().join("ca"),
        CaOptions {
            crl_validity: Duration::from_secs(3600),
            ..CaOptions::default()
        },
    )
    .unwrap();
    let (number, serials, this_update, next_update) = parse_crl(&reopened.crl().unwrap());
    assert_eq!(number, 3);
    assert_eq!(serials.len(), 1);
    assert_eq!(next_update - this_update, 3600);
}

#[test]
fn serial_hex_round_trips() {
    let serial: Serial = "00ab0c".parse().unwrap();
    assert_eq!(serial.as_bytes(), [0xab, 0x0c]);
    assert_eq!(serial.to_string(), "ab0c");
    assert_eq!("abc".parse::<Serial>().unwrap().as_bytes(), [0x0a, 0xbc]);
    for bad in ["", "xyz", &"a".repeat(41)] {
        assert!(bad.parse::<Serial>().is_err(), "{bad}");
    }
    let random = Serial::random().unwrap();
    assert!(random.as_bytes().len() <= SERIAL_BYTES);
    assert_eq!(random.to_string().parse::<Serial>().unwrap(), random);
}

/// Writes PEM files for `Identity::load`.
fn tls_identity(dir: &Path, ca: &CertificateAuthority, cert_pem: &str, key: &KeyPair) -> Identity {
    let paths = TlsPaths {
        ca_cert: dir.join("ca.pem"),
        cert: dir.join("cert.pem"),
        key: dir.join("key.pem"),
    };
    fs::write(&paths.ca_cert, ca.ca_cert_pem()).unwrap();
    fs::write(&paths.cert, cert_pem).unwrap();
    fs::write(&paths.key, key.serialize_pem()).unwrap();
    Identity::load(&paths).unwrap()
}

/// One TLS handshake in memory; returns the server's verdict.
async fn handshake(server: Arc<ServerConfig>, client: Arc<ClientConfig>) -> Result<(), String> {
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
    let (accepted, _) = tokio::join!(
        TlsAcceptor::from(server).accept(server_io),
        TlsConnector::from(client).connect(name, client_io),
    );
    accepted.map(drop).map_err(|e| e.to_string())
}

#[tokio::test]
async fn revoked_agent_is_rejected_at_the_next_handshake() {
    let (dir, ca) = new_ca();
    let (server_key, server_csr) = csr();
    let server_cert = ca
        .issue(
            &server_csr,
            &identity(PeerRole::Server, "central"),
            None,
            &localhost(),
        )
        .unwrap();
    let server_dir = dir.path().join("server");
    fs::create_dir(&server_dir).unwrap();
    let server = tls_identity(&server_dir, &ca, &server_cert.cert_pem, &server_key);

    let (agent_key, agent_csr) = csr();
    let agent_cert = ca
        .issue(&agent_csr, &identity(PeerRole::Agent, "agent-1"), None, &[])
        .unwrap();
    let agent_dir = dir.path().join("agent");
    fs::create_dir(&agent_dir).unwrap();
    let agent = tls_identity(&agent_dir, &ca, &agent_cert.cert_pem, &agent_key)
        .client_config()
        .unwrap();

    // Passes only if webpki accepts the CRL and its signature.
    let config = server.server_config(vec![ca.crl().unwrap()]).unwrap();
    handshake(config, agent.clone()).await.unwrap();

    ca.revoke(&agent_cert.serial, RevocationReason::KeyCompromise)
        .unwrap();
    let config = server.server_config(vec![ca.crl().unwrap()]).unwrap();
    let err = handshake(config, agent).await.unwrap_err();
    assert!(err.contains("Revoked"), "{err}");
}
