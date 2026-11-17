//! The owner's tooling, `scripts/license-keypair.sh` and
//! `scripts/license-sign.sh`, makes license keys this crate accepts. With a
//! throwaway key pair in a temporary directory; skipped without OpenSSL 3
//! or jq.

use std::path::{Path, PathBuf};
use std::process::Command;

use monitorflow_server_ee::{Tier, Verifier};

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

    let verifier = Verifier::from_pem(&public);
    let sign = |args: &[&str]| {
        let out = dir.path().join("license.txt");
        let signed = Command::new(scripts().join("license-sign.sh"))
            .arg("--key")
            .arg(keys.join("license-signing.pem"))
            .args([
                "--customer",
                "Example \"Corp\" Ltd",
                "--expires",
                "2099-12-31",
            ])
            .args(args)
            .arg("--out")
            .arg(&out)
            .output()
            .unwrap();
        signed
            .status
            .success()
            .then(|| std::fs::read_to_string(&out).unwrap())
            .ok_or_else(|| String::from_utf8_lossy(&signed.stderr).into_owned())
    };

    // Every tier, in format 2 (ADR-0147).
    for (name, tier) in [
        ("basic", Tier::Basic),
        ("standard", Tier::Standard),
        ("premium", Tier::Premium),
    ] {
        let text = sign(&["--tier", name, "--managed-servers", "25"]).unwrap();
        let license = verifier.verify(&text).unwrap();
        assert_eq!(license.tier, tier);
        assert_eq!(license.format, 2);
        assert_eq!(license.customer, "Example \"Corp\" Ltd");
        assert_eq!(license.managed_servers, Some(25));
        assert_eq!(license.server_instances, 1);
        assert_eq!(license.support, None);
        assert_eq!(license.expires.to_string(), "2099-12-31");
        assert!(text.starts_with(&format!("MonitorFlow {} subscription for", tier.name())));
    }
    let text = sign(&[
        "--tier",
        "enterprise",
        "--managed-servers",
        "300",
        "--central-servers",
        "3",
        "--support",
        "support@example.com",
    ])
    .unwrap();
    let license = verifier.verify(&text).unwrap();
    assert_eq!(license.tier, Tier::Enterprise);
    assert_eq!(license.server_instances, 3);
    assert_eq!(license.edition, "enterprise");
    assert_eq!(license.support.as_deref(), Some("support@example.com"));

    // The format of ADR-0100, with its old option name, for older servers.
    let text = sign(&["--format", "1", "--servers", "3"]).unwrap();
    let license = verifier.verify(&text).unwrap();
    assert_eq!(license.format, 1);
    assert_eq!(license.server_instances, 3);
    assert_eq!(license.managed_servers, None);

    // What makes no sense is refused before anything is signed.
    for args in [
        &[
            "--tier",
            "premium",
            "--managed-servers",
            "5",
            "--central-servers",
            "2",
        ][..],
        &["--tier", "gold", "--managed-servers", "5"],
        &["--tier", "basic"],
        &["--tier", "basic", "--managed-servers", "0"],
        &[
            "--tier",
            "basic",
            "--managed-servers",
            "5",
            "--support",
            "javascript:alert(1)",
        ],
        &["--format", "1", "--tier", "basic"],
    ] {
        assert!(sign(args).is_err(), "{args:?}");
    }
}
