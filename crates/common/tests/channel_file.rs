//! `deploy/channel-file.sh`, which release.yml runs on a promotion, writes
//! a `tested.json` the server reads (ADR-0147). With a throwaway release
//! key; skipped without OpenSSL 3 or jq.

use std::path::Path;
use std::process::Command;

use monitorflow_common::release::parse_keys;
use monitorflow_common::server_update::channel::verify_promotion;

fn has(tool: &str, arg: &str) -> bool {
    Command::new(tool)
        .arg(arg)
        .output()
        .is_ok_and(|o| o.status.success())
}

#[test]
fn a_promotion_signed_by_the_script_verifies() {
    if !has("jq", "--version") || !has("openssl", "version") {
        eprintln!("skipped: needs openssl and jq");
        return;
    }
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/channel-file.sh");
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("release.pem");
    let made = Command::new("openssl")
        .args(["genpkey", "-algorithm", "ed25519", "-out"])
        .arg(&key)
        .status()
        .unwrap();
    assert!(made.success());
    let public = Command::new("openssl")
        .args(["pkey", "-pubout", "-in"])
        .arg(&key)
        .output()
        .unwrap()
        .stdout;
    let keys = dir.path().join("keys.pem");
    std::fs::write(&keys, &public).unwrap();
    let out = dir.path().join("out");
    let run = |args: &[&str], signing: &Path| {
        Command::new(&script)
            .args(args)
            .env("MF_RELEASE_KEYS", &keys)
            .env("MF_SIGNING_KEY", signing)
            .env("MF_PROMOTED", "2026-12-08")
            .output()
            .unwrap()
    };

    let signed = run(&["sign", "0.7.0", out.to_str().unwrap()], &key);
    assert!(
        signed.status.success(),
        "{}",
        String::from_utf8_lossy(&signed.stderr)
    );
    let bytes = std::fs::read(out.join("tested.json")).unwrap();
    let signature = std::fs::read(out.join("tested.json.sig")).unwrap();
    let keys_parsed = parse_keys(std::str::from_utf8(&public).unwrap()).unwrap();
    let promotion = verify_promotion(&bytes, &signature, &keys_parsed).unwrap();
    assert_eq!(promotion.version.to_string(), "0.7.0");
    assert_eq!(promotion.promoted.to_string(), "2026-12-08");
    assert!(run(&["verify", out.to_str().unwrap()], &key)
        .status
        .success());

    // A pre-release is never promoted.
    assert!(!run(&["sign", "0.8.0-rc.1", out.to_str().unwrap()], &key)
        .status
        .success());
    // A key the servers do not trust is refused after signing.
    let other = dir.path().join("other.pem");
    Command::new("openssl")
        .args(["genpkey", "-algorithm", "ed25519", "-out"])
        .arg(&other)
        .status()
        .unwrap();
    assert!(!run(&["sign", "0.7.1", out.to_str().unwrap()], &other)
        .status
        .success());
    assert!(!run(&["verify", out.to_str().unwrap()], &key)
        .status
        .success());
}
