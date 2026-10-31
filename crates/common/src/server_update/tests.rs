use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};
use tempfile::TempDir;

use super::*;
use crate::release::sha256;

const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn signer() -> (Ed25519KeyPair, PublicKey) {
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
    let key = release_key(&pair);
    (pair, key)
}

fn images_json(version: &str) -> Vec<u8> {
    format!(
        r#"{{"version": "{version}", "images": {{
            "server": "ghcr.io/kidoly/monitorflow-server@sha256:{DIGEST}",
            "web": "localhost:5000/monitorflow-web@sha256:{DIGEST}",
            "updater": "ghcr.io/kidoly/monitorflow-updater@sha256:{DIGEST}",
            "mirror": "ghcr.io/kidoly/monitorflow-mirror@sha256:{DIGEST}",
            "later": "ignored"}}}}"#
    )
    .into_bytes()
}

/// The four files of a release signed by `pair`, with `images` as
/// `images.json`.
fn release_files(pair: &Ed25519KeyPair, version: &str, images: &[u8]) -> [Vec<u8>; 4] {
    let version_file = format!("{version}\n").into_bytes();
    let hex =
        |bytes: &[u8]| -> String { sha256(bytes).iter().map(|b| format!("{b:02x}")).collect() };
    let sums = format!(
        "{}  VERSION\n{}  images.json\n",
        hex(&version_file),
        hex(images)
    )
    .into_bytes();
    let signature = pair.sign(&sums).as_ref().to_vec();
    [sums, signature, version_file, images.to_vec()]
}

fn v(text: &str) -> Version {
    text.parse().unwrap()
}

#[test]
fn a_signed_release_names_its_images() {
    let (pair, key) = signer();
    let files = release_files(&pair, "0.2.0", &images_json("0.2.0"));
    let candidate =
        Candidate::verify("https://x/".into(), files, &[key], Some(&v("0.2.0"))).unwrap();
    assert_eq!(candidate.release.version, v("0.2.0"));
    assert_eq!(
        candidate.images.server.as_str(),
        format!("ghcr.io/kidoly/monitorflow-server@sha256:{DIGEST}")
    );
    assert_eq!(
        candidate.images.web.as_str(),
        format!("localhost:5000/monitorflow-web@sha256:{DIGEST}")
    );
}

#[test]
fn images_must_be_covered_by_the_signature() {
    let (pair, key) = signer();
    // Another images.json than the one SHA256SUMS lists.
    let mut files = release_files(&pair, "0.2.0", &images_json("0.2.0"));
    files[3] = images_json("0.2.0")
        .into_iter()
        .map(|b| if b == b'0' { b'1' } else { b })
        .collect();
    assert!(matches!(
        Candidate::verify("b".into(), files, std::slice::from_ref(&key), None),
        Err(UpdateError::Release(ReleaseError::Mismatch(_)))
    ));
    // Signed by a key this build does not trust.
    let (other, _) = signer();
    let files = release_files(&other, "0.2.0", &images_json("0.2.0"));
    assert!(matches!(
        Candidate::verify("b".into(), files, std::slice::from_ref(&key), None),
        Err(UpdateError::Release(ReleaseError::BadSignature))
    ));
    // A release without images.json.
    let [sums, signature, version_file, _] = release_files(&pair, "0.2.0", &images_json("0.2.0"));
    let release = release::verify(&sums, &signature, &version_file, &[key]).unwrap();
    let no_images = {
        let version_file = b"0.2.0\n".to_vec();
        let hex: String = sha256(&version_file)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let sums = format!("{hex}  VERSION\n").into_bytes();
        let signature = pair.sign(&sums).as_ref().to_vec();
        [sums, signature, version_file, images_json("0.2.0")]
    };
    assert!(matches!(
        Candidate::verify("b".into(), no_images, &[release_key(&pair)], None),
        Err(UpdateError::Release(ReleaseError::NotListed(_)))
    ));
    assert!(verify_images(&release, &images_json("0.2.0")).is_ok());
}

fn release_key(pair: &Ed25519KeyPair) -> PublicKey {
    let mut der = vec![
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ];
    der.extend_from_slice(pair.public_key().as_ref());
    PublicKey::from_spki(&der).unwrap()
}

#[test]
fn images_json_must_name_the_release_version_and_digests() {
    let (pair, key) = signer();
    let files = release_files(&pair, "0.2.0", &images_json("0.1.0"));
    let error = Candidate::verify("b".into(), files, std::slice::from_ref(&key), None).unwrap_err();
    assert!(error.to_string().contains("version 0.1.0"), "{error}");

    let tagged =
        br#"{"version": "0.2.0", "images": {"server": "ghcr.io/kidoly/monitorflow-server:0.2.0",
        "web": "w@sha256:0", "updater": "u"}}"#;
    let files = release_files(&pair, "0.2.0", tagged);
    assert!(matches!(
        Candidate::verify("b".into(), files, &[key], None),
        Err(UpdateError::BadImages(_))
    ));
}

#[test]
fn older_releases_have_no_mirror_image() {
    let (pair, key) = signer();
    let older = format!(
        r#"{{"version": "0.2.0", "images": {{"server": "s@sha256:{DIGEST}",
        "web": "w@sha256:{DIGEST}", "updater": "u@sha256:{DIGEST}"}}}}"#
    );
    let files = release_files(&pair, "0.2.0", older.as_bytes());
    let candidate = Candidate::verify("b".into(), files, std::slice::from_ref(&key), None).unwrap();
    assert_eq!(candidate.images.mirror, None);

    let tagged = older.replace(r#""updater""#, r#""mirror": "m:0.2.0", "updater""#);
    let files = release_files(&pair, "0.2.0", tagged.as_bytes());
    assert!(matches!(
        Candidate::verify("b".into(), files, &[key], None),
        Err(UpdateError::BadImages(_))
    ));
}

#[test]
fn a_source_serving_another_version_is_refused() {
    let (pair, key) = signer();
    let files = release_files(&pair, "0.2.0", &images_json("0.2.0"));
    assert!(matches!(
        Candidate::verify("b".into(), files, &[key], Some(&v("0.3.0"))),
        Err(UpdateError::WrongVersion { .. })
    ));
}

#[test]
fn image_references_are_digests_with_plain_names() {
    for good in [
        format!("ghcr.io/kidoly/monitorflow-server@sha256:{DIGEST}"),
        format!("localhost:5000/mf_web.x@sha256:{DIGEST}"),
        format!("server@sha256:{DIGEST}"),
    ] {
        assert!(ImageRef::parse(&good).is_ok(), "{good}");
    }
    for bad in [
        "ghcr.io/kidoly/monitorflow-server:0.2.0".to_owned(),
        format!("-v@sha256:{DIGEST}"),
        format!("GHCR.io/x@sha256:{DIGEST}"),
        format!("x y@sha256:{DIGEST}"),
        format!("a//b@sha256:{DIGEST}"),
        format!("@sha256:{DIGEST}"),
        format!("x@sha256:{}", &DIGEST[1..]),
        format!("x@sha256:{}", DIGEST.to_uppercase()),
        format!("x:tag@sha256:{DIGEST}\n"),
    ] {
        assert!(ImageRef::parse(&bad).is_err(), "{bad:?}");
    }
}

#[test]
fn sources_and_release_pages() {
    let github = "https://github.com/Kidoly/monitorflow/releases/latest/download";
    assert_eq!(source(None, github), github);
    assert_eq!(source(Some(""), github), github);
    assert_eq!(
        source(Some("http://files:8080/mf"), github),
        "http://files:8080/mf"
    );
    assert_eq!(
        release_base(github, None),
        "https://github.com/Kidoly/monitorflow/releases/latest/download/"
    );
    assert_eq!(
        release_base(&format!("{github}/"), Some(&v("0.2.0"))),
        "https://github.com/Kidoly/monitorflow/releases/download/v0.2.0/"
    );
    assert_eq!(
        release_base("https://files.example/mf/", Some(&v("0.2.0"))),
        "https://files.example/mf/"
    );
    assert_eq!(
        release_notes_url(github, &v("0.2.0")).as_deref(),
        Some("https://github.com/Kidoly/monitorflow/releases/tag/v0.2.0")
    );
    assert_eq!(
        release_notes_url("https://files.example/mf", &v("0.2.0")),
        None
    );
}

#[test]
fn requests_are_checked() {
    let ok = br#"{"id": "4b3c-aa", "version": "0.2.0", "requested_by": "admin",
        "requested_at": "2026-10-27T10:00:00Z"}"#;
    let (request, version) = Request::parse(ok).unwrap();
    assert_eq!(request.requested_by, "admin");
    assert_eq!(version, v("0.2.0"));
    for bad in [
        r#"{"id": "../x", "version": "0.2.0", "requested_by": "a", "requested_at": "2026-10-27T10:00:00Z"}"#,
        r#"{"id": "x", "version": "latest", "requested_by": "a", "requested_at": "2026-10-27T10:00:00Z"}"#,
        r#"{"id": "x", "version": "0.2.0", "requested_by": "a\n", "requested_at": "2026-10-27T10:00:00Z"}"#,
        r#"{"id": "x", "version": "0.2.0", "requested_by": "a", "requested_at": "2026-10-27T10:00:00Z", "image": "evil"}"#,
    ] {
        assert!(Request::parse(bad.as_bytes()).is_err(), "{bad}");
    }
}

#[test]
fn states_and_status_round_trip() {
    assert!(UpdateState::RolledBack.is_final());
    assert!(!UpdateState::RollingBack.is_final());
    let status = Status {
        updater_version: "0.2.0".into(),
        heartbeat_at: Utc::now(),
        running: None,
        update: Some(UpdateRun {
            id: "x".into(),
            from: Some("0.1.0".into()),
            to: "0.2.0".into(),
            requested_by: "admin".into(),
            state: UpdateState::CachingAgents,
            steps: vec![],
            error: None,
            backup: None,
            migrated: false,
            started_at: Utc::now(),
            finished_at: None,
        }),
        backups: backup::Backups::default(),
    };
    let json = serde_json::to_string(&status).unwrap();
    assert!(json.contains(r#""state":"caching_agents""#), "{json}");
    assert_eq!(serde_json::from_str::<Status>(&json).unwrap(), status);
    // The status of an updater older than backups.
    let old = r#"{"updater_version": "0.1.0", "heartbeat_at": "2026-10-29T00:00:00Z", "running": null, "update": null}"#;
    assert_eq!(
        serde_json::from_str::<Status>(old).unwrap().backups,
        backup::Backups::default()
    );
}

#[test]
fn atomic_writes_replace_links_instead_of_following_them() {
    let dir = TempDir::new().unwrap();
    let victim = dir.path().join("victim");
    std::fs::write(&victim, "keep").unwrap();
    std::os::unix::fs::symlink(&victim, dir.path().join(STATUS_FILE)).unwrap();
    write_atomic(dir.path(), STATUS_FILE, b"{}", 0o644).unwrap();
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
    let path = dir.path().join(STATUS_FILE);
    assert!(std::fs::symlink_metadata(&path).unwrap().is_file());
    assert_eq!(read_small(&path, 10).unwrap().unwrap(), b"{}");
    // No temporary file is left behind.
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
}

#[test]
fn small_reads_refuse_links_and_large_files() {
    let dir = TempDir::new().unwrap();
    assert_eq!(read_small(&dir.path().join("none"), 10).unwrap(), None);
    std::fs::write(dir.path().join("big"), [0u8; 11]).unwrap();
    assert!(read_small(&dir.path().join("big"), 10).is_err());
    std::os::unix::fs::symlink(dir.path().join("big"), dir.path().join("link")).unwrap();
    assert!(read_small(&dir.path().join("link"), 100).is_err());
}
