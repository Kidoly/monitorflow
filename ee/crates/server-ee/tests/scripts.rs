//! The owner's tooling, `scripts/license-keypair.sh` and
//! `scripts/license-sign.sh`, makes license keys this crate accepts. With a
//! throwaway key pair in a temporary directory; skipped without OpenSSL 3
//! or jq.

use std::path::{Path, PathBuf};
use std::process::Command;

use monitorflow_server_ee::Verifier;

fn scripts() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../scripts")
}

fn has(tool: &str, arg: &str) -> bool {
    Command::new(tool)
        .arg(arg)
        .output()
        .is_ok_and(|o| o.status.success())
}

#[test]
fn signed_by_the_scripts_verifies() {
    if !has("jq", "--version") || !has("openssl", "version") {
        eprintln!("skipped: needs openssl and jq");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let keys = dir.path().join("keys");
    let made = Command::new(scripts().join("license-keypair.sh"))
        .arg(&keys)
        .output()
        .unwrap();
    assert!(
        made.status.success(),
        "{}",
        String::from_utf8_lossy(&made.stderr)
    );
    let public = String::from_utf8(made.stdout).unwrap();
    assert!(public.contains("BEGIN PUBLIC KEY"));
    assert!(
        !public.contains("PRIVATE"),
        "the private key is never printed"
    );
    // Never replaces a signing key.
    let again = Command::new(scripts().join("license-keypair.sh"))
        .arg(&keys)
        .output()
        .unwrap();
    assert!(!again.status.success());

    let out = dir.path().join("license.txt");
    let signed = Command::new(scripts().join("license-sign.sh"))
        .args(["--key"])
        .arg(keys.join("license-signing.pem"))
        .args(["--customer", "Example \"Corp\" Ltd", "--servers", "3"])
        .args(["--expires", "2099-12-31", "--out"])
        .arg(&out)
        .output()
        .unwrap();
    assert!(
        signed.status.success(),
        "{}",
        String::from_utf8_lossy(&signed.stderr)
    );
    let text = std::fs::read_to_string(&out).unwrap();
    let license = Verifier::from_pem(&public).verify(&text).unwrap();
    assert_eq!(license.customer, "Example \"Corp\" Ltd");
    assert_eq!(license.server_instances, 3);
    assert_eq!(license.expires.to_string(), "2099-12-31");
    assert_eq!(license.edition, "enterprise");
}
