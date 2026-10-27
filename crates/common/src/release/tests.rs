use base64::Engine;
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};

use super::*;

/// A signing key and its public half, as the release workflow has them.
struct Signer {
    pair: Ed25519KeyPair,
}

impl Signer {
    fn new() -> Self {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
        Self {
            pair: Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap(),
        }
    }

    fn public(&self) -> PublicKey {
        let mut der = SPKI_PREFIX.to_vec();
        der.extend_from_slice(self.pair.public_key().as_ref());
        PublicKey::from_spki(&der).unwrap()
    }

    fn sign(&self, bytes: &[u8]) -> Vec<u8> {
        self.pair.sign(bytes).as_ref().to_vec()
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `SHA256SUMS` over `files`, as `sha256sum` writes it.
fn sums(files: &[(&str, &[u8])]) -> Vec<u8> {
    files
        .iter()
        .map(|(name, bytes)| format!("{}  {name}\n", hex(&sha256(bytes))))
        .collect::<String>()
        .into_bytes()
}

const BINARY: &[u8] = b"\x7fELF\x02\x01\x01\0\0\0\0\0\0\0\0\0\x03\0\x3e\0rest";

fn release_files(version: &str) -> (Vec<u8>, Vec<u8>) {
    let version_file = format!("{version}\n").into_bytes();
    let sums = sums(&[
        ("VERSION", &version_file),
        ("monitorflow-agent-linux-x86_64", BINARY),
    ]);
    (sums, version_file)
}

fn v(text: &str) -> Version {
    text.parse().unwrap()
}

#[test]
fn a_signed_release_verifies_and_lists_its_files() {
    let signer = Signer::new();
    let (sums, version) = release_files("0.2.0");
    let release = verify(&sums, &signer.sign(&sums), &version, &[signer.public()]).unwrap();
    assert_eq!(release.version, v("0.2.0"));
    release
        .check("monitorflow-agent-linux-x86_64", BINARY)
        .unwrap();
    assert_eq!(
        release.check("monitorflow-agent-linux-aarch64", BINARY),
        Err(ReleaseError::NotListed(
            "monitorflow-agent-linux-aarch64".into()
        ))
    );
}

#[test]
fn any_trusted_key_verifies_for_rotation() {
    let (old, new) = (Signer::new(), Signer::new());
    let (sums, version) = release_files("0.2.0");
    let keys = [old.public(), new.public()];
    verify(&sums, &new.sign(&sums), &version, &keys).unwrap();
    verify(&sums, &old.sign(&sums), &version, &keys).unwrap();
}

#[test]
fn a_bad_signature_is_refused_before_anything_is_read() {
    let (signer, other) = (Signer::new(), Signer::new());
    let (sums, version) = release_files("0.2.0");
    let keys = [signer.public()];
    let signature = signer.sign(&sums);
    // Another key, a changed byte, a truncated signature, none at all.
    let mut flipped = signature.clone();
    flipped[10] ^= 1;
    for bad in [
        other.sign(&sums),
        flipped,
        signature[..63].to_vec(),
        Vec::new(),
    ] {
        assert_eq!(
            verify(&sums, &bad, &version, &keys),
            Err(ReleaseError::BadSignature)
        );
    }
    // SHA256SUMS changed after signing.
    let mut changed = sums.clone();
    changed[0] = if changed[0] == b'0' { b'1' } else { b'0' };
    assert_eq!(
        verify(&changed, &signature, &version, &keys),
        Err(ReleaseError::BadSignature)
    );
    assert_eq!(
        verify(&sums, &signature, &version, &[]),
        Err(ReleaseError::NoKeys)
    );
}

#[test]
fn the_version_file_must_match_its_signed_line() {
    let signer = Signer::new();
    let (sums, _) = release_files("0.2.0");
    let signature = signer.sign(&sums);
    assert_eq!(
        verify(&sums, &signature, b"9.9.9\n", &[signer.public()]),
        Err(ReleaseError::Mismatch("VERSION".into()))
    );
    // Signed, but not a version.
    for text in ["0.2\n", "0.2.0", "0.2.0\n0.3.0\n", "v0.2.0\n"] {
        let sums = sums_with_version(text);
        let result = verify(
            &sums,
            &signer.sign(&sums),
            text.as_bytes(),
            &[signer.public()],
        );
        assert!(
            matches!(result, Err(ReleaseError::BadVersion(_))),
            "{text:?}: {result:?}"
        );
    }
}

fn sums_with_version(text: &str) -> Vec<u8> {
    sums(&[("VERSION", text.as_bytes())])
}

#[test]
fn a_truncated_or_altered_binary_does_not_match() {
    let signer = Signer::new();
    let (sums, version) = release_files("0.2.0");
    let release = verify(&sums, &signer.sign(&sums), &version, &[signer.public()]).unwrap();
    let name = "monitorflow-agent-linux-x86_64";
    for bad in [&BINARY[..BINARY.len() - 1], &[], b"other".as_slice()] {
        assert_eq!(
            release.check(name, bad),
            Err(ReleaseError::Mismatch(name.into()))
        );
    }
}

#[test]
fn malformed_sums_are_refused_even_when_signed() {
    let signer = Signer::new();
    let digest = "a".repeat(64);
    for text in [
        format!("{digest}  ../etc/passwd\n"),
        format!("{digest}  dir/file\n"),
        format!("{digest} file\n"),
        format!("{}  file\n", "A".repeat(64)),
        format!("{}  file\n", "a".repeat(63)),
        format!("{digest}  VERSION\n{digest}  VERSION\n"),
    ] {
        let sums = text.into_bytes();
        let result = verify(&sums, &signer.sign(&sums), b"0.1.0\n", &[signer.public()]);
        assert!(
            matches!(result, Err(ReleaseError::BadSums(_))),
            "{result:?}"
        );
    }
    // `*name`, binary mode, is fine.
    let version = b"0.1.0\n";
    let sums = format!("{} *VERSION\n", hex(&sha256(version))).into_bytes();
    verify(&sums, &signer.sign(&sums), version, &[signer.public()]).unwrap();
}

#[test]
fn versions_parse_and_order_as_semver() {
    let ordered = [
        "0.1.0",
        "0.1.1-alpha",
        "0.1.1-alpha.1",
        "0.1.1-alpha.beta",
        "0.1.1-beta.2",
        "0.1.1-beta.11",
        "0.1.1-rc.1",
        "0.1.1",
        "0.2.0",
        "0.10.0",
        "1.0.0",
    ];
    for pair in ordered.windows(2) {
        assert!(v(pair[0]) < v(pair[1]), "{} < {}", pair[0], pair[1]);
    }
    assert_eq!(v("0.2.0-rc.1").to_string(), "0.2.0-rc.1");
    for bad in [
        "", "1", "1.2", "1.2.3.4", "01.2.3", "1.2.3-", "1.2.3-01", "1.2.3+b", "a.b.c", "1.2.3-é",
    ] {
        assert!(bad.parse::<Version>().is_err(), "{bad:?}");
    }
}

#[test]
fn downgrades_need_an_explicit_permission() {
    assert_eq!(check_upgrade(&v("0.1.0"), &v("0.2.0"), false), Ok(true));
    assert_eq!(check_upgrade(&v("0.2.0"), &v("0.2.0"), false), Ok(false));
    assert!(matches!(
        check_upgrade(&v("0.2.0"), &v("0.1.0"), false),
        Err(ReleaseError::Downgrade { .. })
    ));
    assert!(matches!(
        check_upgrade(&v("0.2.0"), &v("0.2.0-rc.1"), false),
        Err(ReleaseError::Downgrade { .. })
    ));
    assert_eq!(check_upgrade(&v("0.2.0"), &v("0.1.0"), true), Ok(true));
}

fn elf(machine: u16) -> Vec<u8> {
    let mut head = b"\x7fELF\x02\x01\x01\0\0\0\0\0\0\0\0\0\x02\0".to_vec();
    head.extend_from_slice(&machine.to_le_bytes());
    head.resize(64, 0);
    head
}

fn pe(machine: u16) -> Vec<u8> {
    let mut head = vec![0u8; 0x100];
    head[..2].copy_from_slice(b"MZ");
    head[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
    head[0x80..0x84].copy_from_slice(b"PE\0\0");
    head[0x84..0x86].copy_from_slice(&machine.to_le_bytes());
    head
}

#[test]
fn executables_must_be_for_the_platform() {
    check_executable(Platform::LinuxX86_64, &elf(62)).unwrap();
    check_executable(Platform::LinuxAarch64, &elf(183)).unwrap();
    check_executable(Platform::WindowsX86_64, &pe(0x8664)).unwrap();
    let wrong = [
        (Platform::LinuxX86_64, elf(183)),
        (Platform::LinuxAarch64, elf(62)),
        (Platform::LinuxX86_64, pe(0x8664)),
        (Platform::WindowsX86_64, pe(0xaa64)),
        (Platform::WindowsX86_64, elf(62)),
        (Platform::LinuxX86_64, b"#!/bin/sh\n".to_vec()),
        (Platform::LinuxX86_64, Vec::new()),
    ];
    for (platform, head) in wrong {
        assert!(
            matches!(
                check_executable(platform, &head),
                Err(ReleaseError::WrongBinary(_))
            ),
            "{platform:?}"
        );
    }
    // A 32-bit ELF of the right machine number.
    let mut elf32 = elf(62);
    elf32[4] = 1;
    assert!(check_executable(Platform::LinuxX86_64, &elf32).is_err());
}

#[test]
fn keys_parse_from_openssl_pem_and_skip_comments() {
    let signer = Signer::new();
    let mut der = SPKI_PREFIX.to_vec();
    der.extend_from_slice(signer.pair.public_key().as_ref());
    let b64 = base64::engine::general_purpose::STANDARD.encode(&der);
    let text = format!(
        "# comment\n-----BEGIN PUBLIC KEY-----\n{b64}\n-----END PUBLIC KEY-----\n\n\
         # From openssl pkey -pubout:\n-----BEGIN PUBLIC KEY-----\n\
         MCowBQYDK2VwAyEAKI/P13cKZQDMLKl/3uHZzztpqZTu0KBbcs/go2/qgWY=\n\
         -----END PUBLIC KEY-----\n"
    );
    let keys = parse_keys(&text).unwrap();
    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0], signer.public());
    assert!(parse_keys("only a comment\n").unwrap().is_empty());
    // An ECDSA P-256 key is not a release key.
    let p256 = "-----BEGIN PUBLIC KEY-----\n\
        MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEEVs/o5+uQbTjL3chynL4wXgUg2R9\n\
        q9UU8I5mEovUf86QZ7kOBIjJwqnzD1omageEHWwHdBO6B+dFabmdT9POxg==\n\
        -----END PUBLIC KEY-----\n";
    assert!(matches!(parse_keys(p256), Err(ReleaseError::BadKey(_))));
    assert!(matches!(
        parse_keys("-----BEGIN PUBLIC KEY-----\nAAAA\n"),
        Err(ReleaseError::BadKey(_))
    ));
}

#[test]
fn the_built_in_keys_file_parses() {
    // Empty until the project's key is committed (ADR-0053); either way
    // the file must parse.
    parse_keys(BUILT_IN_KEYS).unwrap();
}

#[test]
fn the_current_platform_has_an_asset_when_released() {
    if let Some(platform) = Platform::current() {
        assert!(platform.agent_asset().starts_with("monitorflow-agent-"));
        if let Some(relay) = platform.relay_asset() {
            assert!(relay.starts_with("monitorflow-relay-linux-"));
        }
    }
}
