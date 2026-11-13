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
    roots.add(der(&ca.ca_cert_pem())).unwrap();
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

    let cert_der = der(&ca.ca_cert_pem());
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
        (PeerRole::Relay, "site-b.relay", localhost()),
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
        assert_eq!(eku.server_auth, !names.is_empty(), "{role}");
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
    // Over MAX_CERT_VALIDITY: revocations would be forgotten too early.
    for bad in [Duration::ZERO, 91 * DAY, 3651 * DAY] {
        let err = ca.issue(&csr, &agent, Some(bad), &[]).unwrap_err();
        assert!(matches!(err, CaError::InvalidRequest(_)), "{err}");
    }
}

#[test]
fn server_names_only_for_servers_and_relays() {
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
fn ignores_the_ca_flag_a_csr_asks_for() {
    let (_dir, ca) = new_ca();
    let mut params = CertificateParams::default();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let (_, csr) = csr_with(params);
    let issued = ca
        .issue(&csr, &identity(PeerRole::Agent, "a1"), None, &[])
        .unwrap();
    let cert_der = der(&issued.cert_pem);
    let (_, cert) = X509Certificate::from_der(&cert_der).unwrap();
    assert!(!cert.basic_constraints().unwrap().unwrap().value.ca);
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
    assert!(!ca.is_revoked(&issued.serial));
    ca.revoke(
        &issued.serial,
        RevocationReason::KeyCompromise,
        issued.not_after,
    )
    .unwrap();
    ca.revoke(
        &issued.serial,
        RevocationReason::Superseded,
        issued.not_after,
    )
    .unwrap();
    assert!(ca.is_revoked(&issued.serial));
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

/// The CA in `dir` with `revocations.json` as given.
fn with_revocations(dir: &Path, json: serde_json::Value) -> CertificateAuthority {
    open(dir);
    std::fs::write(dir.join(REVOCATIONS_FILE), json.to_string()).unwrap();
    open(dir)
}

#[test]
fn revocations_of_expired_certificates_leave_the_crl_and_the_file() {
    let dir = TempDir::new().unwrap();
    let ca_dir = dir.path().join("ca");
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let ca = with_revocations(
        &ca_dir,
        serde_json::json!({
            "crl_number": 7,
            "revoked": [
                // Expired more than a margin ago: forgotten.
                { "serial": "0a", "revoked_at": now - 3 * 86400, "reason": "key_compromise",
                  "not_after": now - 86400 - 60 },
                // Expired within the margin: still listed.
                { "serial": "0b", "revoked_at": now - 3 * 86400, "reason": "key_compromise",
                  "not_after": now - 3600 },
                // Recorded before ADR-0113: kept for the longest validity
                // and the margin.
                { "serial": "0c", "revoked_at": now - 91 * 86400 - 60, "reason": "superseded" },
                { "serial": "0d", "revoked_at": now - 89 * 86400, "reason": "superseded" },
            ],
        }),
    );
    let serial = |hex: &str| Serial::from_str(hex).unwrap();
    let (number, listed, _, _) = parse_crl(&ca.crl().unwrap());
    assert_eq!(number, 8);
    assert_eq!(listed, vec![serial("0b"), serial("0d")]);
    assert!(!ca.is_revoked(&serial("0a")) && !ca.is_revoked(&serial("0c")));

    let stored = revocation::load(&ca_dir.join(REVOCATIONS_FILE)).unwrap();
    let kept: Vec<_> = stored.revoked.iter().map(|r| r.serial.clone()).collect();
    assert_eq!(kept, vec![serial("0b"), serial("0d")]);
    assert_eq!(stored.revoked[1].not_after, None);
}

#[test]
fn a_revocation_records_when_the_certificate_expires() {
    let dir = TempDir::new().unwrap();
    let options = CaOptions {
        revocation_margin: Duration::ZERO,
        ..CaOptions::default()
    };
    let ca = CertificateAuthority::open_or_create(&dir.path().join("ca"), options).unwrap();
    let (_, csr) = csr();
    let agent = identity(PeerRole::Agent, "a1");
    let issued = ca.issue(&csr, &agent, None, &[]).unwrap();
    ca.revoke(
        &issued.serial,
        RevocationReason::KeyCompromise,
        issued.not_after,
    )
    .unwrap();
    let stored = revocation::load(&dir.path().join("ca").join(REVOCATIONS_FILE)).unwrap();
    assert_eq!(stored.revoked[0].not_after, Some(unix(issued.not_after)));
    assert_eq!(parse_crl(&ca.crl().unwrap()).1, vec![issued.serial.clone()]);

    // Revoked after it expired: refused anyway, so not listed.
    let expired = SystemTime::now() - Duration::from_secs(60);
    let other = Serial::from_bytes(&[0x2a]);
    ca.revoke(&other, RevocationReason::KeyCompromise, expired)
        .unwrap();
    assert_eq!(parse_crl(&ca.crl().unwrap()).1, vec![issued.serial]);
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
    assert_revocation_is_enforced(dir.path(), &ca).await;
}

/// An agent connects until its certificate is on the CRL of `ca`.
async fn assert_revocation_is_enforced(dir: &Path, ca: &CertificateAuthority) {
    let (server_key, server_csr) = csr();
    let server_cert = ca
        .issue(
            &server_csr,
            &identity(PeerRole::Server, "central"),
            None,
            &localhost(),
        )
        .unwrap();
    let server_dir = dir.join("server");
    fs::create_dir(&server_dir).unwrap();
    let server = tls_identity(&server_dir, ca, &server_cert.cert_pem, &server_key);

    let (agent_key, agent_csr) = csr();
    let agent_cert = ca
        .issue(&agent_csr, &identity(PeerRole::Agent, "agent-1"), None, &[])
        .unwrap();
    let agent_dir = dir.join("agent");
    fs::create_dir(&agent_dir).unwrap();
    let agent = tls_identity(&agent_dir, ca, &agent_cert.cert_pem, &agent_key)
        .client_config()
        .unwrap();

    // Passes only if webpki accepts the CRL and its signature.
    let config = server.server_config(vec![ca.crl().unwrap()]).unwrap();
    handshake(config, agent.clone()).await.unwrap();

    ca.revoke(
        &agent_cert.serial,
        RevocationReason::KeyCompromise,
        agent_cert.not_after,
    )
    .unwrap();
    let config = server.server_config(vec![ca.crl().unwrap()]).unwrap();
    let err = handshake(config, agent).await.unwrap_err();
    assert!(err.contains("Revoked"), "{err}");
}

#[test]
fn verify_issued_accepts_only_this_cas_certificates() {
    let (_dir, ca) = new_ca();
    let (_other_dir, other) = new_ca();
    let id = PeerIdentity::new(PeerRole::Agent, "web-01").unwrap();
    let ours = ca.issue(&csr().1, &id, None, &[]).unwrap();
    let theirs = other.issue(&csr().1, &id, None, &[]).unwrap();
    assert!(ca.verify_issued(&der(&ours.cert_pem)).is_ok());
    assert!(ca.verify_issued(&der(&theirs.cert_pem)).is_err());
    assert!(ca
        .verify_issued(&CertificateDer::from(vec![1, 2, 3]))
        .is_err());
}

#[test]
fn commands_signed_by_the_ca_verify_against_its_certificate() {
    use monitorflow_common::command;
    use monitorflow_common::proto::v1::Command;

    let (_dir, ca) = new_ca();
    let mut cmd = Command {
        command_id: "c1".into(),
        kind: command::APPLY_UPDATES.into(),
        agent_id: "web-01".into(),
        expires_at_unix_ms: 1,
        ..Command::default()
    };
    cmd.signature = ca.sign_command(&command::signed_bytes(&cmd)).unwrap();
    let ca_der = der(&ca.ca_cert_pem());
    assert_eq!(command::verify(&cmd, std::slice::from_ref(&ca_der)), Ok(()));
    // The same CA reopened from disk signs with the same key.
    let reopened = open(&ca.dir);
    cmd.signature = reopened.sign_command(&command::signed_bytes(&cmd)).unwrap();
    assert_eq!(command::verify(&cmd, std::slice::from_ref(&ca_der)), Ok(()));
    let (_other_dir, other) = new_ca();
    assert!(command::verify(&cmd, &[der(&other.ca_cert_pem())]).is_err());
}

/// What a v0.2.0 server left on disk and handed out, written by that release
/// (rcgen 0.13) with a CA of 100 years and `V0_2_0_VALIDITY`, so that this
/// test outlives both. The key is here on purpose and is no secret: this CA
/// signs only this fixture and nothing trusts it.
const V0_2_0_CA_KEY: &str = "\
-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgfjtCFyy8P+gmVdq6
1nd9i2xTTz+3iDN4kJK8xzLy5OWhRANCAARCim0GronaZFRAXYzoHS+2qBmuLLYa
5rjpfxMfvGr5kEP6l3B4Kabrwn85X/KNQ26CSnaUKsI3AMZlux43bgmn
-----END PRIVATE KEY-----
";
const V0_2_0_CA_CERT: &str = "\
-----BEGIN CERTIFICATE-----
MIIBkTCCATigAwIBAgIQfmRcSqQQeTh12FzT9yvLnzAKBggqhkjOPQQDAjAmMSQw
IgYDVQQDDBtNb25pdG9yRmxvdyB0ZXN0IGZpeHR1cmUgQ0EwIBcNMjYxMTAxMjMw
NTQyWhgPMjEyNjEwMDgyMzEwNDJaMCYxJDAiBgNVBAMMG01vbml0b3JGbG93IHRl
c3QgZml4dHVyZSBDQTBZMBMGByqGSM49AgEGCCqGSM49AwEHA0IABEKKbQauidpk
VEBdjOgdL7aoGa4sthrmuOl/Ex+8avmQQ/qXcHgppuvCfzlf8o1DboJKdpQqwjcA
xmW7HjduCaejRjBEMA8GA1UdDwEB/wQFAwMHBgAwHQYDVR0OBBYEFPEQcodaU6D+
cmuwuS/nsyB0TtBPMBIGA1UdEwEB/wQIMAYBAf8CAQAwCgYIKoZIzj0EAwIDRwAw
RAIgcKx/RGG57HnhAN2kKcnm7605pgiihcZD4YpnkPgIi6wCIBHNbaQYmTWl62YV
xsTfuBybcG5hKOv2fxEFJc6yBLLS
-----END CERTIFICATE-----
";
const V0_2_0_SERVER_CSR: &str = "\
-----BEGIN CERTIFICATE REQUEST-----
MIHYMH8CAQAwHTEbMBkGA1UEAwwSbW9uaXRvcmZsb3ctc2VydmVyMFkwEwYHKoZI
zj0CAQYIKoZIzj0DAQcDQgAEjU2tbjuimaRAbNUxp0L/fE1EBr8YIC64purVXjqj
jPqVffJ6CsflQIpFZhYJYMvyZ7y573aUTCU0RaBstbTf0aAAMAoGCCqGSM49BAMC
A0kAMEYCIQChVJWlBlP5bSCb1fUp/d7DRLGfH08FqNB1tksHUx15+wIhAK+jSCKW
+vUA+j2GfwnysYOHsokaEVrOlPseQmnxvpz9
-----END CERTIFICATE REQUEST-----
";
const V0_2_0_SERVER_CERT: &str = "\
-----BEGIN CERTIFICATE-----
MIIB9zCCAZ2gAwIBAgIQa0QQMvPl5mlWjN8LZmB8dTAKBggqhkjOPQQDAjAmMSQw
IgYDVQQDDBtNb25pdG9yRmxvdyB0ZXN0IGZpeHR1cmUgQ0EwIBcNMjYxMTAxMjMw
NTQyWhgPMjExNjEwMTAyMzEwNDJaMBIxEDAOBgNVBAMMB2NlbnRyYWwwWTATBgcq
hkjOPQIBBggqhkjOPQMBBwNCAASNTa1uO6KZpEBs1TGnQv98TUQGvxggLrim6tVe
OqOM+pV98noKx+VAikVmFglgy/JnvLnvdpRMJTRFoGy1tN/Ro4G+MIG7MB8GA1Ud
IwQYMBaAFPEQcodaU6D+cmuwuS/nsyB0TtBPMDgGA1UdEQQxMC+GHG1vbml0b3Jm
bG93Oi8vc2VydmVyL2NlbnRyYWyCCWxvY2FsaG9zdIcEfwAAATAPBgNVHQ8BAf8E
BQMDB4AAMB0GA1UdJQQWMBQGCCsGAQUFBwMCBggrBgEFBQcDATAdBgNVHQ4EFgQU
rgyPvUF/vYU4st9CvL8WQh5jmZkwDwYDVR0TAQH/BAUwAwEBADAKBggqhkjOPQQD
AgNIADBFAiEAmnTTuT5oEKBgKBvawKobSnj1/Le5SVmdwUECmbNxK+MCIFX64X2e
nyZ1YxGvE3Qj866mtcfCMczQwwO+ZaJuiypi
-----END CERTIFICATE-----
";
const V0_2_0_VALIDITY: Duration = Duration::from_secs(90 * 365 * 86400);

fn v0_2_0_ca() -> (TempDir, CertificateAuthority) {
    let dir = TempDir::new().unwrap();
    let ca_dir = dir.path().join("ca");
    fs::create_dir(&ca_dir).unwrap();
    fs::write(ca_dir.join(KEY_FILE), V0_2_0_CA_KEY).unwrap();
    fs::set_permissions(ca_dir.join(KEY_FILE), fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(ca_dir.join(CERT_FILE), V0_2_0_CA_CERT).unwrap();
    // v0.2.0 issued for any validity; this fixture uses 90 years.
    let options = CaOptions {
        max_cert_validity: V0_2_0_VALIDITY,
        ..CaOptions::default()
    };
    let ca = CertificateAuthority::open_or_create(&ca_dir, options).unwrap();
    (dir, ca)
}

/// Extensions as a peer reads them: their DER may differ, as rcgen 0.14 fixed
/// the encoding of `keyUsage` and of `basicConstraints` without `cA`.
fn extensions(cert: &X509Certificate<'_>) -> Vec<String> {
    cert.extensions()
        .iter()
        .map(|e| format!("{} {} {:?}", e.oid, e.critical, e.parsed_extension()))
        .collect()
}

#[test]
fn v0_2_0_ca_keeps_issuing_the_same_certificates() {
    let (_dir, ca) = v0_2_0_ca();
    assert_eq!(ca.ca_cert_pem(), V0_2_0_CA_CERT);

    let old_der = der(V0_2_0_SERVER_CERT);
    assert!(ca.verify_issued(&old_der).is_ok());
    let old_id = PeerIdentity::from_cert(&old_der).unwrap();
    assert_eq!(old_id.uri(), "monitorflow://server/central");

    let central = identity(PeerRole::Server, "central");
    let issued = ca
        .issue(
            V0_2_0_SERVER_CSR,
            &central,
            Some(V0_2_0_VALIDITY),
            &localhost(),
        )
        .unwrap();
    let new_der = der(&issued.cert_pem);
    assert!(chains_to(&ca, &new_der));

    let (_, old) = X509Certificate::from_der(&old_der).unwrap();
    let (_, new) = X509Certificate::from_der(&new_der).unwrap();
    assert_eq!(new.version(), old.version());
    assert_eq!(new.subject().as_raw(), old.subject().as_raw());
    assert_eq!(new.issuer().as_raw(), old.issuer().as_raw());
    assert_eq!(new.public_key().raw, old.public_key().raw);
    assert_eq!(new.signature_algorithm, old.signature_algorithm);
    let span = |c: &X509Certificate<'_>| {
        c.validity().not_after.timestamp() - c.validity().not_before.timestamp()
    };
    assert_eq!(span(&new), span(&old));
    assert_eq!(extensions(&new), extensions(&old));
}

#[tokio::test]
async fn v0_2_0_ca_keeps_revoking() {
    let (dir, ca) = v0_2_0_ca();
    assert_revocation_is_enforced(dir.path(), &ca).await;
}

/// webpki does not read the identifier, other verifiers of the CRL may.
#[test]
fn v0_2_0_ca_names_itself_in_its_crls() {
    let (_dir, ca) = v0_2_0_ca();
    let ca_der = der(V0_2_0_CA_CERT);
    let (_, ca_cert) = X509Certificate::from_der(&ca_der).unwrap();
    let key_id = ca_cert
        .iter_extensions()
        .find_map(|e| match e.parsed_extension() {
            ParsedExtension::SubjectKeyIdentifier(id) => Some(id.0),
            _ => None,
        })
        .unwrap();

    let crl_der = ca.crl().unwrap();
    let (_, crl) = CertificateRevocationList::from_der(&crl_der).unwrap();
    assert_eq!(crl.issuer().as_raw(), ca_cert.subject().as_raw());
    let authority = crl
        .extensions()
        .iter()
        .find_map(|e| match e.parsed_extension() {
            ParsedExtension::AuthorityKeyIdentifier(id) => id.key_identifier.as_ref(),
            _ => None,
        })
        .unwrap();
    assert_eq!(authority.0, key_id);
}

#[test]
fn v0_2_0_ca_keeps_signing_commands() {
    use monitorflow_common::command;
    use monitorflow_common::proto::v1::Command;

    let (_dir, ca) = v0_2_0_ca();
    let mut cmd = Command {
        command_id: "c1".into(),
        ..Command::default()
    };
    cmd.signature = ca.sign_command(&command::signed_bytes(&cmd)).unwrap();
    assert_eq!(command::verify(&cmd, &[der(V0_2_0_CA_CERT)]), Ok(()));
}
