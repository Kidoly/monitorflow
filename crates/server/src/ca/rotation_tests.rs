//! A change of certificate authority on disk (ADR-0088).

use std::fs;

use monitorflow_common::tls::fingerprint;
use rustls::pki_types::UnixTime;
use rustls::server::WebPkiClientVerifier;
use rustls::RootCertStore;
use tempfile::TempDir;
use x509_parser::prelude::FromDer;
use x509_parser::revocation_list::CertificateRevocationList;

use super::rotation::{NEXT_DIR, PREVIOUS_DIR, RETIRED_DIR, ROTATION_FILE};
use super::*;

fn open(dir: &Path) -> CertificateAuthority {
    CertificateAuthority::open_or_create(dir, CaOptions::default()).unwrap()
}

fn der(pem: &str) -> CertificateDer<'static> {
    CertificateDer::from(pem::parse(pem).unwrap().into_contents())
}

fn issue(ca: &CertificateAuthority, id: &str) -> IssuedCert {
    let key = KeyPair::generate().unwrap();
    let csr = CertificateParams::default()
        .serialize_request(&key)
        .unwrap()
        .pem()
        .unwrap();
    let identity = PeerIdentity::new(PeerRole::Agent, id).unwrap();
    ca.issue(&csr, &identity, None, &[]).unwrap()
}

/// Whether `cert` verifies against the CAs of `bundle_pem` with `crls`, as
/// a TLS listener checks a client.
fn accepted(
    bundle_pem: &str,
    crls: &[rustls::pki_types::CertificateRevocationListDer<'static>],
    cert: &IssuedCert,
) -> bool {
    let mut roots = RootCertStore::empty();
    for ca in pem::parse_many(bundle_pem).unwrap() {
        roots.add(CertificateDer::from(ca.into_contents())).unwrap();
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider)
        .with_crls(crls.to_vec())
        .only_check_end_entity_revocation()
        .enforce_revocation_expiration()
        .build()
        .unwrap()
        .verify_client_cert(&der(&cert.cert_pem), &[], UnixTime::now())
        .is_ok()
}

#[test]
fn a_planned_change_introduces_switches_and_retires() {
    let dir = TempDir::new().unwrap();
    let ca_dir = dir.path().join("ca");
    let ca = open(&ca_dir);
    let old = ca.fingerprint();
    let before = issue(&ca, "web-01");

    // Introduced: both trusted, the old CA still issues and signs.
    let rotation = ca.start_rotation(false).unwrap();
    assert_eq!(rotation.stage, Stage::Introduced);
    assert!(!rotation.emergency);
    let trust = ca.trust();
    assert_eq!(trust.issuer, old);
    assert_eq!(trust.bundle.len(), 2);
    assert_eq!(trust.bundle[0], old);
    let new = trust.bundle[1];
    assert_ne!(new, old);
    assert_eq!(issue(&ca, "web-02").issuer, old);
    assert!(ca.start_rotation(false).is_err(), "one change at a time");
    assert!(ca.retire().is_err(), "nothing to retire before the switch");
    // The new CA has its own name.
    let status = ca.status();
    let other = der(&status.other.unwrap().cert_pem);
    let (_, cert) = X509Certificate::from_der(&other).unwrap();
    let name = cert.subject().iter_common_name().next().unwrap().as_str().unwrap().to_owned();
    assert!(name.starts_with("MonitorFlow CA 20"), "{name}");
    // Reopened, the same state.
    assert_eq!(open(&ca_dir).trust(), ca.trust());

    // Switched: the new CA issues; certificates of both are accepted.
    let rotation = ca.switch().unwrap();
    assert_eq!(rotation.stage, Stage::Switched);
    assert_eq!(rotation.distrust_at, None);
    assert_eq!(ca.fingerprint(), new);
    assert_eq!(ca.trust().bundle, vec![new, old]);
    assert_eq!(fingerprint(&der(&ca.ca_cert_pem())), new);
    let after = issue(&ca, "web-01");
    assert_eq!(after.issuer, new);
    let crls = ca.crls().unwrap();
    assert_eq!(crls.len(), 2);
    let bundle = ca.trust_bundle_pem();
    assert!(accepted(&bundle, &crls, &before));
    assert!(accepted(&bundle, &crls, &after));
    assert!(ca.verify_issued(&der(&before.cert_pem)).is_ok());
    assert!(ca_dir.join(PREVIOUS_DIR).join(KEY_FILE).exists());
    assert!(!ca_dir.join(NEXT_DIR).exists());
    assert_eq!(open(&ca_dir).trust(), ca.trust());

    // Retired: only the new CA, the old key gone, its certificate kept.
    let retired = ca.retire().unwrap();
    assert_eq!(retired.fingerprint, monitorflow_common::tls::fingerprint_hex(&old));
    assert_eq!(ca.trust().bundle, vec![new]);
    assert!(ca.rotation().is_none());
    assert!(!ca_dir.join(PREVIOUS_DIR).exists());
    assert!(!ca_dir.join(ROTATION_FILE).exists());
    assert!(ca_dir
        .join(RETIRED_DIR)
        .join(format!("{}.json", retired.fingerprint))
        .exists());
    let crls = ca.crls().unwrap();
    assert_eq!(crls.len(), 1);
    assert!(!accepted(&ca.trust_bundle_pem(), &crls, &before));
    assert!(accepted(&ca.trust_bundle_pem(), &crls, &after));
    assert!(ca.verify_issued(&der(&before.cert_pem)).is_err());
    let reopened = open(&ca_dir);
    assert_eq!(reopened.trust(), ca.trust());
    assert!(reopened.status().restored_retired.is_none());
}

#[test]
fn commands_are_signed_by_the_ca_that_issues() {
    use monitorflow_common::command;
    use monitorflow_common::proto::v1::Command;

    let dir = TempDir::new().unwrap();
    let ca = open(&dir.path().join("ca"));
    let old = der(&ca.ca_cert_pem());
    ca.start_rotation(false).unwrap();
    ca.switch().unwrap();
    let new = der(&ca.ca_cert_pem());
    let mut cmd = Command {
        command_id: "c1".into(),
        agent_id: "web-01".into(),
        ..Command::default()
    };
    cmd.signature = ca.sign_command(&command::signed_bytes(&cmd)).unwrap();
    assert!(command::verify(&cmd, std::slice::from_ref(&new)).is_ok());
    assert!(command::verify(&cmd, std::slice::from_ref(&old)).is_err());
    assert!(command::verify(&cmd, &[old, new]).is_ok());
}

#[test]
fn an_emergency_switches_at_once_and_sets_the_distrust_time() {
    let dir = TempDir::new().unwrap();
    let ca = open(&dir.path().join("ca"));
    let old = ca.fingerprint();
    let rotation = ca.start_rotation(true).unwrap();
    assert_eq!(rotation.stage, Stage::Switched);
    assert!(rotation.emergency);
    let window = rotation.distrust_at.unwrap() - rotation.switched_at.unwrap();
    assert_eq!(window, rotation::EMERGENCY_WINDOW.as_secs() as i64);
    assert_ne!(ca.fingerprint(), old);
    let trust = ca.trust();
    assert!(trust.emergency);
    // Tokens pinned to the compromised CA no longer enroll.
    assert!(!trust.accepts_pin(&old));
    assert!(trust.accepts_pin(&ca.fingerprint()));
    assert!(ca.retire().unwrap().emergency);
}

#[test]
fn an_emergency_cuts_a_planned_change_short() {
    let dir = TempDir::new().unwrap();
    let ca = open(&dir.path().join("ca"));
    ca.start_rotation(false).unwrap();
    let introduced = ca.status().other.unwrap().fingerprint;
    let rotation = ca.start_rotation(true).unwrap();
    assert!(rotation.emergency && rotation.stage == Stage::Switched);
    assert_eq!(ca.fingerprint(), introduced);
    assert!(ca.start_rotation(true).is_err());
}

#[test]
fn peers_behind_a_change_are_told_apart() {
    let dir = TempDir::new().unwrap();
    let ca = open(&dir.path().join("ca"));
    let old = ca.fingerprint();
    assert!(!ca.trust().is_behind(&old, &[old]));
    ca.start_rotation(false).unwrap();
    let new = ca.status().other.unwrap().fingerprint;
    let trust = ca.trust();
    assert!(trust.is_behind(&old, &[old]));
    assert!(!trust.is_behind(&old, &[new, old]));
    // Planned: tokens of either CA still enroll.
    assert!(trust.accepts_pin(&old) && trust.accepts_pin(&new));
    ca.switch().unwrap();
    let trust = ca.trust();
    assert!(trust.is_behind(&old, &[old, new]));
    assert!(!trust.is_behind(&new, &[old, new]));
    ca.retire().unwrap();
    assert!(ca.trust().is_behind(&new, &[old, new]));
    assert!(!ca.trust().is_behind(&new, &[new]));
}

#[test]
fn an_interrupted_switch_is_finished_at_the_next_start() {
    let dir = TempDir::new().unwrap();
    let ca_dir = dir.path().join("ca");
    let ca = open(&ca_dir);
    let old = ca.fingerprint();
    ca.start_rotation(false).unwrap();
    let new = ca.status().other.unwrap().fingerprint;
    drop(ca);
    // As if the switch stopped after `rotation.json` said so, with only
    // the new key copied over the old one.
    let previous = ca_dir.join(PREVIOUS_DIR);
    fs::create_dir(&previous).unwrap();
    for name in [KEY_FILE, CERT_FILE] {
        fs::copy(ca_dir.join(name), previous.join(name)).unwrap();
    }
    let mut rotation: Rotation =
        serde_json::from_slice(&fs::read(ca_dir.join(ROTATION_FILE)).unwrap()).unwrap();
    rotation.stage = Stage::Switched;
    rotation.switched_at = Some(rotation.started_at);
    fs::write(ca_dir.join(ROTATION_FILE), serde_json::to_vec(&rotation).unwrap()).unwrap();
    fs::copy(ca_dir.join(NEXT_DIR).join(KEY_FILE), ca_dir.join(KEY_FILE)).unwrap();

    let ca = open(&ca_dir);
    assert_eq!(ca.trust().bundle, vec![new, old]);
    assert!(!ca_dir.join(NEXT_DIR).exists());
}

#[test]
fn leftovers_of_an_interrupted_start_or_retirement_are_removed() {
    let dir = TempDir::new().unwrap();
    let ca_dir = dir.path().join("ca");
    let ca = open(&ca_dir);
    ca.start_rotation(false).unwrap();
    drop(ca);
    // The start stopped before `rotation.json`.
    fs::remove_file(ca_dir.join(ROTATION_FILE)).unwrap();
    let ca = open(&ca_dir);
    assert!(ca.rotation().is_none());
    assert_eq!(ca.trust().bundle.len(), 1);
    assert!(!ca_dir.join(NEXT_DIR).exists());
}

#[test]
fn a_restored_retired_ca_says_so() {
    let dir = TempDir::new().unwrap();
    let ca_dir = dir.path().join("ca");
    let ca = open(&ca_dir);
    let (old_key, old_cert) = (
        fs::read(ca_dir.join(KEY_FILE)).unwrap(),
        fs::read(ca_dir.join(CERT_FILE)).unwrap(),
    );
    ca.start_rotation(false).unwrap();
    ca.switch().unwrap();
    let retired = ca.retire().unwrap();
    drop(ca);
    // A backup from before the change brings the old files back; the
    // restore keeps `retired/` (ADR-0058).
    fs::write(ca_dir.join(KEY_FILE), old_key).unwrap();
    fs::write(ca_dir.join(CERT_FILE), old_cert).unwrap();
    let ca = open(&ca_dir);
    assert_eq!(ca.status().restored_retired, Some(retired));
}

#[test]
fn crls_of_both_cas_list_every_revocation() {
    let dir = TempDir::new().unwrap();
    let ca = open(&dir.path().join("ca"));
    let before = issue(&ca, "web-01");
    let older = issue(&ca, "web-04");
    ca.start_rotation(false).unwrap();
    ca.switch().unwrap();
    let after = issue(&ca, "web-02");
    ca.revoke(&before.serial, RevocationReason::KeyCompromise).unwrap();
    ca.revoke(&after.serial, RevocationReason::KeyCompromise).unwrap();
    let crls = ca.crls().unwrap();
    let bundle = ca.trust_bundle_pem();
    assert!(!accepted(&bundle, &crls, &before));
    assert!(!accepted(&bundle, &crls, &after));
    let still = issue(&ca, "web-03");
    assert!(accepted(&bundle, &crls, &still));
    // Each CRL names its own CA and carries the same number.
    let numbers: Vec<_> = crls
        .iter()
        .map(|crl| {
            let (_, parsed) = CertificateRevocationList::from_der(crl).unwrap();
            assert_eq!(parsed.iter_revoked_certificates().count(), 2);
            parsed.crl_number().unwrap().to_u64_digits()
        })
        .collect();
    assert_eq!(numbers[0], numbers[1]);
    assert!(accepted(&bundle, &crls, &older));
    // A relay with only the active CA's CRL refuses the old CA's
    // certificates: it needs both.
    assert!(!accepted(&bundle, &crls[..1], &older));
    assert!(accepted(&bundle, &crls[..1], &still));
}
