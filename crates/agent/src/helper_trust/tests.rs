use monitorflow_common::ca_bundle::{self, SignedBundle, SignedBundles};
use monitorflow_common::command::{signed_bytes, CONFIGURE_SOURCES, SELF_UPDATE};
use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, IsCa, KeyPair, SanType};
use ring::rand::SystemRandom;
use ring::signature::{EcdsaKeyPair, ECDSA_P256_SHA256_ASN1_SIGNING};
use tempfile::TempDir;

use super::*;

const NOW: i64 = 1_800_000_000_000;

/// A certificate authority that signs commands, as the central server's.
pub(crate) struct Ca(CertifiedIssuer<'static, KeyPair>);

impl Ca {
    pub(crate) fn new() -> Self {
        let mut params = CertificateParams::default();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        Self(CertifiedIssuer::self_signed(params, KeyPair::generate().unwrap()).unwrap())
    }

    fn pin(&self) -> String {
        tls::fingerprint_hex(&tls::fingerprint(self.0.der()))
    }

    /// The certificate of `role`/`id`, in PEM.
    fn issue(&self, role: PeerRole, id: &str) -> String {
        self.issue_on(role, id, 1)
    }

    /// The same, valid from day `day` of January 2026: a later renewal.
    fn issue_on(&self, role: PeerRole, id: &str, day: u8) -> String {
        let mut params = CertificateParams::default();
        params.not_before = rcgen::date_time_ymd(2026, 1, day);
        let uri = PeerIdentity::new(role, id).unwrap().uri();
        params.subject_alt_names = vec![SanType::URI(uri.try_into().unwrap())];
        params
            .signed_by(&KeyPair::generate().unwrap(), &self.0)
            .unwrap()
            .pem()
    }

    pub(crate) fn sign(&self, mut command: Command) -> Command {
        command.signature = self.sign_bytes(&signed_bytes(&command));
        command
    }

    fn sign_bytes(&self, message: &[u8]) -> Vec<u8> {
        let rng = SystemRandom::new();
        let key = EcdsaKeyPair::from_pkcs8(
            &ECDSA_P256_SHA256_ASN1_SIGNING,
            &self.0.key().serialize_der(),
            &rng,
        )
        .unwrap();
        key.sign(&rng, message).unwrap().as_ref().to_vec()
    }

    /// What a renewal of `web-01` on `day` brings when the server trusts
    /// `cas`: its certificate from the first of them, and the bundle
    /// signed by each of `signers` (ADR-0107).
    fn renewal(day: u8, cas: &[&Ca], signers: &[&Ca]) -> SignedBundle {
        Self::renewal_of("web-01", day, cas, signers)
    }

    fn renewal_of(id: &str, day: u8, cas: &[&Ca], signers: &[&Ca]) -> SignedBundle {
        let cert_pem = cas[0].issue_on(PeerRole::Agent, id, day);
        let leaf = ca_bundle::leaf_fingerprint(&cert_pem).unwrap();
        let fingerprints: Vec<_> = cas.iter().map(|ca| tls::fingerprint(ca.0.der())).collect();
        let message = ca_bundle::signed_bytes(&leaf, &fingerprints);
        SignedBundle {
            cert_pem,
            ca_bundle_pem: cas.iter().map(|ca| ca.0.pem()).collect(),
            signatures: signers.iter().map(|ca| ca.sign_bytes(&message)).collect(),
        }
    }
}

/// A machine: the service's files, written as its user would, and the
/// helper's own directory.
pub(crate) struct Host {
    _dir: TempDir,
    pub(crate) place: Place,
}

impl Host {
    /// `web-01`, enrolled with `ca`, before any helper ran.
    fn enrolled(ca: &Ca) -> Self {
        let dir = TempDir::new().unwrap();
        let tls = TlsPaths {
            ca_cert: dir.path().join("ca.pem"),
            cert: dir.path().join("cert.pem"),
            key: dir.path().join("key.pem"),
        };
        let state_dir = dir.path().join("helper");
        let pin_file = dir.path().join("ca.pin");
        let place = Place::of(
            PeerRole::Agent,
            state_dir.to_str().unwrap(),
            pin_file.to_str().unwrap(),
            &tls,
        );
        fs::write(&place.ca_cert, ca.0.pem()).unwrap();
        fs::write(&place.cert, ca.issue(PeerRole::Agent, "web-01")).unwrap();
        Self { _dir: dir, place }
    }

    pub(crate) fn pinned(ca: &Ca) -> Self {
        let host = Self::enrolled(ca);
        fs::write(&host.place.pin_file, format!("{}\n", ca.pin())).unwrap();
        host
    }

    fn run(&self, command: &Command, kind: &str) -> Result<Command, String> {
        authorize(&self.place, &HelperRequest::of(command), kind, NOW).map_err(|e| e.to_string())
    }

    /// Writes what the service keeps of its renewals.
    fn keeps(&self, bundles: &[&SignedBundle]) {
        let kept = SignedBundles {
            bundles: bundles.iter().map(|b| (*b).clone()).collect(),
        };
        fs::write(
            ca_bundle::kept_path(&self.place.ca_cert),
            kept.encode_to_vec(),
        )
        .unwrap();
    }

    fn anchor(&self) -> Anchor {
        let bytes = fs::read(self.place.state_dir.join(ANCHOR_FILE)).unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }
}

/// An unsigned command of `kind` with `payload`, as the dispatcher hands
/// one to a handler once it has checked it.
pub(crate) fn unsigned(kind: &str, payload: &[u8]) -> Command {
    Command {
        kind: kind.into(),
        payload: payload.to_vec(),
        ..command("c-1")
    }
}

/// The command a fake helper saved from its stdin, and nothing after it.
#[cfg(unix)]
pub(crate) fn sent(stdin: &[u8]) -> Command {
    let mut input = stdin;
    let request = HelperRequest::read(&mut input).unwrap();
    assert!(input.is_empty(), "nothing follows the request");
    request.command.unwrap()
}

fn command(id: &str) -> Command {
    Command {
        command_id: id.into(),
        kind: CONFIGURE_SOURCES.into(),
        payload: vec![1, 2, 3],
        issued_at_unix_ms: NOW - 1_000,
        expires_at_unix_ms: NOW + 60_000,
        agent_id: "web-01".into(),
        signature: Vec::new(),
    }
}

#[test]
fn a_request_is_framed_and_leaves_the_rest_of_the_input() {
    let request = HelperRequest::of(&command("c1"));
    let mut input = request.framed();
    input.extend_from_slice(b"binary");
    let mut reader = input.as_slice();
    assert_eq!(HelperRequest::read(&mut reader).unwrap(), request);
    assert_eq!(reader, b"binary");

    let error = |bytes: &[u8]| HelperRequest::read(&mut &*bytes).unwrap_err().to_string();
    assert!(error(b"").contains("no signed command"));
    assert!(error(&[0, 0, 0, 9, 1]).contains("cut short"));
    assert!(error(&[0xff; 8]).contains("larger than"));
    // What helpers read before: a JSON spec is not a request.
    assert!(HelperRequest::read(&mut &br#"{"kind":"all","packages":[]}"#[..]).is_err());
}

#[test]
fn only_a_command_the_server_signed_for_this_machine_runs_and_only_once() {
    let ca = Ca::new();
    let host = Host::pinned(&ca);
    let signed = ca.sign(command("c1"));

    let unsigned = command("c2");
    assert!(host
        .run(&unsigned, CONFIGURE_SOURCES)
        .unwrap_err()
        .contains("not signed"));
    let forged = Ca::new().sign(command("c2"));
    assert!(host
        .run(&forged, CONFIGURE_SOURCES)
        .unwrap_err()
        .contains("does not verify"));
    let mut tampered = signed.clone();
    tampered.payload.push(4);
    assert!(host
        .run(&tampered, CONFIGURE_SOURCES)
        .unwrap_err()
        .contains("does not verify"));
    let mut other = command("c2");
    other.agent_id = "web-02".into();
    assert!(host
        .run(&ca.sign(other), CONFIGURE_SOURCES)
        .unwrap_err()
        .contains("is for web-02"));
    assert!(host
        .run(&signed, SELF_UPDATE)
        .unwrap_err()
        .contains("this helper runs self_update"));
    let mut expired = command("c2");
    expired.expires_at_unix_ms = NOW;
    assert!(host
        .run(&ca.sign(expired), CONFIGURE_SOURCES)
        .unwrap_err()
        .contains("expired"));
    let mut bad_id = command("../c2");
    bad_id = ca.sign(bad_id);
    assert!(host
        .run(&bad_id, CONFIGURE_SOURCES)
        .unwrap_err()
        .contains("invalid command ID"));
    assert!(authorize(
        &host.place,
        &HelperRequest { command: None },
        CONFIGURE_SOURCES,
        NOW
    )
    .is_err());

    // None of those refusals used the command up.
    assert_eq!(host.run(&signed, CONFIGURE_SOURCES).unwrap(), signed);
    assert!(host
        .run(&signed, CONFIGURE_SOURCES)
        .unwrap_err()
        .contains("already run"));
    assert_eq!(
        host.run(&ca.sign(command("c2")), CONFIGURE_SOURCES)
            .unwrap()
            .command_id,
        "c2"
    );
}

#[test]
fn used_commands_are_forgotten_once_they_expired() {
    let ca = Ca::new();
    let host = Host::pinned(&ca);
    host.run(&ca.sign(command("c1")), CONFIGURE_SOURCES)
        .unwrap();
    let used = host.place.state_dir.join(USED_DIR);
    assert!(used.join("c1").exists());

    let later = NOW + 120_000;
    let mut next = command("c2");
    next.expires_at_unix_ms = later + 60_000;
    let request = HelperRequest::of(&ca.sign(next));
    authorize(&host.place, &request, CONFIGURE_SOURCES, later).unwrap();
    assert!(!used.join("c1").exists());
    assert!(used.join("c2").exists());
}

#[test]
fn the_pin_decides_which_ca_is_trusted_whatever_the_service_holds() {
    let (ca, intruder) = (Ca::new(), Ca::new());
    let host = Host::pinned(&ca);
    // The service's user adds a CA of its own next to the real one.
    fs::write(
        &host.place.ca_cert,
        format!("{}{}", intruder.0.pem(), ca.0.pem()),
    )
    .unwrap();
    assert!(host
        .run(&intruder.sign(command("c1")), CONFIGURE_SOURCES)
        .unwrap_err()
        .contains("does not verify"));
    let anchor = host.anchor();
    assert_eq!(anchor.origin, Origin::Pinned);
    assert_eq!(anchor.ca_certs.len(), 1);
    assert_eq!(anchor.pin, Some(ca.pin()));
    assert_eq!(anchor.peer_id, "web-01");

    // Once made, the anchor no longer reads the service's files.
    fs::write(&host.place.ca_cert, intruder.0.pem()).unwrap();
    fs::write(&host.place.cert, intruder.issue(PeerRole::Agent, "web-02")).unwrap();
    host.run(&ca.sign(command("c1")), CONFIGURE_SOURCES)
        .unwrap();
    assert!(host
        .run(&intruder.sign(command("c2")), CONFIGURE_SOURCES)
        .is_err());
}

#[test]
fn a_pin_needs_its_ca_and_a_certificate_it_issued() {
    let (ca, intruder) = (Ca::new(), Ca::new());
    let refused = |host: &Host| host.run(&ca.sign(command("c1")), CONFIGURE_SOURCES);

    let host = Host::pinned(&ca);
    fs::write(&host.place.ca_cert, intruder.0.pem()).unwrap();
    assert!(refused(&host).unwrap_err().contains("is not in"));

    let host = Host::pinned(&ca);
    fs::write(&host.place.cert, intruder.issue(PeerRole::Agent, "web-01")).unwrap();
    assert!(refused(&host).unwrap_err().contains("was not issued by"));

    let host = Host::pinned(&ca);
    fs::write(&host.place.cert, ca.issue(PeerRole::Relay, "web-01")).unwrap();
    assert!(refused(&host)
        .unwrap_err()
        .contains("certificate of a relay"));

    let host = Host::pinned(&ca);
    fs::remove_file(&host.place.cert).unwrap();
    assert!(refused(&host).unwrap_err().contains("is missing"));

    let host = Host::pinned(&ca);
    fs::write(&host.place.pin_file, "not a pin").unwrap();
    assert!(refused(&host).unwrap_err().contains("not a SHA-256"));
    // Nothing was stored by any of them.
    assert!(!host.place.state_dir.join(ANCHOR_FILE).exists());
}

#[test]
fn a_machine_without_a_pin_adopts_once_and_a_later_pin_replaces_it() {
    let (ca, intruder) = (Ca::new(), Ca::new());
    let host = Host::enrolled(&ca);
    host.run(&ca.sign(command("c1")), CONFIGURE_SOURCES)
        .unwrap();
    assert_eq!(host.anchor().origin, Origin::Adopted);
    assert_eq!(host.anchor().pin, None);

    // Adopted once: what the service's user writes later changes nothing.
    fs::write(&host.place.ca_cert, intruder.0.pem()).unwrap();
    fs::write(&host.place.cert, intruder.issue(PeerRole::Agent, "web-01")).unwrap();
    assert!(host
        .run(&intruder.sign(command("c2")), CONFIGURE_SOURCES)
        .is_err());
    host.run(&ca.sign(command("c3")), CONFIGURE_SOURCES)
        .unwrap();

    // The install command run again names its CA: the anchor follows it.
    let next = Ca::new();
    fs::write(&host.place.ca_cert, next.0.pem()).unwrap();
    fs::write(&host.place.cert, next.issue(PeerRole::Agent, "web-09")).unwrap();
    fs::write(&host.place.pin_file, next.pin()).unwrap();
    let mut renamed = command("c4");
    renamed.agent_id = "web-09".into();
    host.run(&next.sign(renamed), CONFIGURE_SOURCES).unwrap();
    assert_eq!(host.anchor().origin, Origin::Pinned);
    assert!(host
        .run(&ca.sign(command("c5")), CONFIGURE_SOURCES)
        .is_err());
}

#[test]
fn the_anchor_follows_a_change_of_ca_on_signed_bundles_only() {
    let (old, new) = (Ca::new(), Ca::new());
    let host = Host::pinned(&old);
    let runs = |ca: &Ca, id: &str| host.run(&ca.sign(command(id)), CONFIGURE_SOURCES).is_ok();
    assert!(runs(&old, "c1") && !runs(&new, "c2"));

    // Introduced: the old CA signs for the new one. Then switched, and
    // retired: the new CA alone.
    let introduced = Ca::renewal(2, &[&old, &new], &[&old, &new]);
    let switched = Ca::renewal(3, &[&new, &old], &[&new, &old]);
    let retired = Ca::renewal(4, &[&new], &[&new]);
    host.keeps(&[&introduced]);
    assert!(runs(&new, "c3") && runs(&old, "c4"));
    assert_eq!(host.anchor().ca_certs.len(), 2);
    host.keeps(&[&introduced, &switched, &retired]);
    assert!(runs(&new, "c5") && !runs(&old, "c6"));
    assert_eq!(host.anchor().ca_certs.len(), 1);

    // The service's user keeps the earlier bundles and shows them again:
    // their certificates are older than the last one accepted.
    host.keeps(&[&introduced]);
    assert!(!runs(&old, "c7"));
    host.keeps(&[&switched]);
    assert!(!runs(&old, "c8") && runs(&new, "c9"));
}

#[test]
fn a_machine_that_was_away_follows_the_whole_change_at_once() {
    let (old, new) = (Ca::new(), Ca::new());
    let host = Host::pinned(&old);
    // It never saw the introduction: the switched bundle carries the old
    // CA's signature too, then the retirement the new one's.
    let switched = Ca::renewal(3, &[&new, &old], &[&new, &old]);
    let retired = Ca::renewal(4, &[&new], &[&new]);
    host.keeps(&[&switched, &retired]);
    host.run(&new.sign(command("c1")), CONFIGURE_SOURCES)
        .unwrap();
    assert!(host
        .run(&old.sign(command("c2")), CONFIGURE_SOURCES)
        .is_err());
}

#[test]
fn bundles_nobody_trusted_signed_change_nothing() {
    let (old, new, intruder) = (Ca::new(), Ca::new(), Ca::new());
    let host = Host::pinned(&old);
    let refused = |bundle: SignedBundle, id: &str| {
        host.keeps(&[&bundle]);
        let command = intruder.sign(command(id));
        assert!(host.run(&command, CONFIGURE_SOURCES).is_err(), "{id}");
        assert_eq!(host.anchor().ca_certs.len(), 1, "{id}");
    };
    // A CA of the service user's own, signed by itself or by nobody.
    refused(Ca::renewal(2, &[&old, &intruder], &[&intruder]), "c1");
    refused(Ca::renewal(2, &[&intruder, &old], &[]), "c2");
    refused(Ca::renewal(2, &[&intruder], &[&intruder]), "c3");
    // The server's signature for another bundle, or another certificate.
    let mut swapped = Ca::renewal(2, &[&old, &new], &[&old]);
    swapped.ca_bundle_pem = format!("{}{}", old.0.pem(), intruder.0.pem());
    refused(swapped, "c4");
    let mut other_cert = Ca::renewal(2, &[&old, &new], &[&old]);
    other_cert.cert_pem = old.issue_on(PeerRole::Agent, "web-01", 3);
    refused(other_cert, "c5");

    // A bundle the server signed for another machine does not move this
    // one's anchor, nor does a file that is not bundles at all.
    let neighbour = Ca::renewal_of("web-02", 2, &[&old, &new], &[&old]);
    host.keeps(&[&neighbour]);
    assert!(host
        .run(&new.sign(command("c6")), CONFIGURE_SOURCES)
        .is_err());
    fs::write(ca_bundle::kept_path(&host.place.ca_cert), b"\xff garbage").unwrap();
    host.run(&old.sign(command("c7")), CONFIGURE_SOURCES)
        .unwrap();
}

#[test]
fn the_installer_pins_the_ca_of_its_token() {
    let (ca, old) = (Ca::new(), Ca::new());
    let token = |ca: &Ca| tls::EnrollmentToken::new([7; 32], tls::fingerprint(ca.0.der()));

    // Connected with `ca`: its token pins it, an older CA's does not.
    let host = Host::enrolled(&ca);
    host.run(&ca.sign(command("c1")), CONFIGURE_SOURCES)
        .unwrap();
    assert_eq!(host.anchor().origin, Origin::Adopted);
    assert_eq!(write_pin(&host.place, &token(&old)).unwrap(), Pinned::Stale);
    assert!(!host.place.pin_file.exists());
    assert_eq!(
        write_pin(&host.place, &token(&ca)).unwrap(),
        Pinned::Written
    );
    host.run(&ca.sign(command("c2")), CONFIGURE_SOURCES)
        .unwrap();
    assert_eq!(host.anchor().origin, Origin::Pinned);

    // Installed again after its identity was removed: the anchor of the
    // earlier install goes, and the next one names the new certificate.
    fs::remove_file(&host.place.cert).unwrap();
    assert_eq!(
        write_pin(&host.place, &token(&ca)).unwrap(),
        Pinned::Written
    );
    assert!(!host.place.state_dir.join(ANCHOR_FILE).exists());
    fs::write(&host.place.cert, ca.issue(PeerRole::Agent, "web-09")).unwrap();
    let mut renamed = command("c3");
    renamed.agent_id = "web-09".into();
    host.run(&ca.sign(renamed), CONFIGURE_SOURCES).unwrap();
    assert_eq!(host.anchor().peer_id, "web-09");
}

#[cfg(unix)]
#[test]
fn a_state_directory_others_can_write_is_refused() {
    use std::os::unix::fs::PermissionsExt;

    let ca = Ca::new();
    let host = Host::pinned(&ca);
    fs::create_dir(&host.place.state_dir).unwrap();
    fs::set_permissions(&host.place.state_dir, fs::Permissions::from_mode(0o777)).unwrap();
    assert!(host
        .run(&ca.sign(command("c1")), CONFIGURE_SOURCES)
        .unwrap_err()
        .contains("writable by it only"));

    // A link in its place is not followed either.
    let host = Host::pinned(&ca);
    let elsewhere = host.place.state_dir.with_file_name("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &host.place.state_dir).unwrap();
    assert!(host
        .run(&ca.sign(command("c1")), CONFIGURE_SOURCES)
        .is_err());
}
