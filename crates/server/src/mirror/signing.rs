//! The server's OpenPGP signing key in `<mirror_dir>/gnupg` (ADR-0030):
//! exactly one secret key, without a passphrase since publishing is
//! unattended, whose public half is exported to `keys/` for clients.

use std::path::Path;

use super::layout::{self, Layout};
use super::runner::{Invocation, RunError, Runner, Tool};

/// User ID of a generated key.
pub const DEFAULT_UID: &str = "MonitorFlow archive signing key";

/// `gpg --list-secret-keys` in machine-readable form.
pub fn list_secret_keys() -> Invocation {
    Invocation::new(
        Tool::Gpg,
        ["--batch", "--with-colons", "--list-secret-keys"],
    )
}

/// Fingerprints of the primary secret keys in `--with-colons` output.
pub fn fingerprints(colons: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut after_sec = false;
    for line in colons.lines() {
        let mut fields = line.split(':');
        match fields.next() {
            Some("sec") => after_sec = true,
            Some("fpr") if after_sec => {
                if let Some(fpr) = fields.nth(8).filter(|f| !f.is_empty()) {
                    found.push(fpr.to_owned());
                }
                after_sec = false;
            }
            Some("ssb" | "sub") => after_sec = false,
            _ => {}
        }
    }
    found
}

/// Checks the detached signature `signature` of `file` against the
/// upstream archive keys, as aptly did before publishing (ADR-0142): only
/// that keyring, whatever trust the signing home holds, with the status
/// lines on stdout for [`verified`].
pub fn verify_upstream(layout: &Layout, signature: &Path, file: &Path) -> Invocation {
    let mut args: Vec<std::ffi::OsString> = [
        "--batch",
        "--no-default-keyring",
        "--trust-model",
        "always",
        "--status-fd",
        "1",
        "--keyring",
    ]
    .map(Into::into)
    .into();
    args.push(layout.upstream_keyring().into());
    args.push("--verify".into());
    args.push(signature.into());
    args.push(file.into());
    Invocation::new(Tool::Gpg, args)
}

/// Status lines that refuse a signature even with `VALIDSIG`: gpg reports
/// a correct signature by an expired or revoked key, or an expired one,
/// with both and ends well. apt refuses them too.
const REFUSED: [&str; 5] = [
    "[GNUPG:] BADSIG ",
    "[GNUPG:] ERRSIG ",
    "[GNUPG:] EXPKEYSIG ",
    "[GNUPG:] REVKEYSIG ",
    "[GNUPG:] EXPSIG ",
];

/// Whether the status lines of a successful [`verify_upstream`] report a
/// valid signature: gpg also ends well on some outcomes that are not one.
pub fn verified(status: &str) -> bool {
    status.lines().any(|l| l.starts_with("[GNUPG:] VALIDSIG "))
        && !status
            .lines()
            .any(|l| REFUSED.iter().any(|refused| l.starts_with(refused)))
}

/// Signs the apt `Release` file at `release` with `key` into `InRelease`
/// (clear-signed) next to it.
pub fn clearsign(release: &Path, key: &str) -> Invocation {
    let out = release.with_file_name("InRelease");
    let mut args: Vec<std::ffi::OsString> = [
        "--batch",
        "--yes",
        "--local-user",
        key,
        "--digest-algo",
        "SHA512",
        "--output",
    ]
    .map(Into::into)
    .into();
    args.extend([out.into(), "--clearsign".into(), release.into()]);
    Invocation::new(Tool::Gpg, args)
}

/// Signs the apt `Release` file at `release` with `key` into `Release.gpg`
/// (detached, armored) next to it.
pub fn detach_sign(release: &Path, key: &str) -> Invocation {
    let out = release.with_file_name("Release.gpg");
    let mut args: Vec<std::ffi::OsString> = [
        "--batch",
        "--yes",
        "--local-user",
        key,
        "--digest-algo",
        "SHA512",
        "--armor",
        "--output",
    ]
    .map(Into::into)
    .into();
    args.extend([out.into(), "--detach-sign".into(), release.into()]);
    Invocation::new(Tool::Gpg, args)
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    #[error("no signing key: create one with `monitorflow-server mirror key generate` or import one with `mirror key import`")]
    Missing,
    #[error("{0} secret keys in the mirror GNUPGHOME; keep exactly one")]
    Several(usize),
}

#[derive(Debug, thiserror::Error)]
pub enum SigningError {
    #[error(transparent)]
    Key(#[from] KeyError),
    #[error("a signing key exists already ({0}); rotation is not supported yet")]
    Exists(String),
    #[error("the file holds no secret key")]
    NoSecretKey,
    #[error(transparent)]
    Run(#[from] RunError),
    #[error("cannot write the public key: {0}")]
    Io(#[from] std::io::Error),
}

async fn run(runner: &dyn Runner, invocation: Invocation) -> Result<String, RunError> {
    Ok(runner.run(&invocation).await?.check(&invocation)?.stdout)
}

async fn secret_keys(runner: &dyn Runner) -> Result<String, RunError> {
    run(runner, list_secret_keys()).await
}

async fn refuse_existing(runner: &dyn Runner) -> Result<(), SigningError> {
    match the_key(&secret_keys(runner).await?) {
        Err(KeyError::Missing) => Ok(()),
        Ok(key) => Err(SigningError::Exists(key)),
        Err(other) => Err(other.into()),
    }
}

/// Creates the key (RSA 4096, sign only, no expiry) and exports it.
pub async fn generate(
    runner: &dyn Runner,
    layout: &Layout,
    uid: &str,
) -> Result<String, SigningError> {
    refuse_existing(runner).await?;
    let args = [
        "--batch",
        "--pinentry-mode",
        "loopback",
        "--passphrase",
        "",
        "--quick-generate-key",
        uid,
        "rsa4096",
        "sign",
        "never",
    ];
    run(runner, Invocation::new(Tool::Gpg, args)).await?;
    export(runner, layout)
        .await?
        .ok_or(KeyError::Missing.into())
}

/// The signing key, created on first use so that adding a mirror needs no
/// step before it.
pub async fn ensure(runner: &dyn Runner, layout: &Layout) -> Result<String, SigningError> {
    match export(runner, layout).await? {
        Some(key) => Ok(key),
        None => generate(runner, layout, DEFAULT_UID).await,
    }
}

/// Imports a secret key from `file` and exports its public half.
pub async fn import(
    runner: &dyn Runner,
    layout: &Layout,
    file: &Path,
) -> Result<String, SigningError> {
    refuse_existing(runner).await?;
    let args = [
        "--batch".into(),
        "--import".into(),
        file.as_os_str().to_owned(),
    ];
    run(runner, Invocation::new(Tool::Gpg, args)).await?;
    match export(runner, layout).await {
        Err(SigningError::Key(KeyError::Missing)) | Ok(None) => Err(SigningError::NoSecretKey),
        other => other?.ok_or(SigningError::NoSecretKey),
    }
}

/// Writes the public key to `keys/`; `None` without a key.
pub async fn export(runner: &dyn Runner, layout: &Layout) -> Result<Option<String>, SigningError> {
    let key = match the_key(&secret_keys(runner).await?) {
        Ok(key) => key,
        Err(KeyError::Missing) => return Ok(None),
        Err(other) => return Err(other.into()),
    };
    let armored = run(
        runner,
        Invocation::new(Tool::Gpg, ["--batch", "--armor", "--export", key.as_str()]),
    )
    .await?;
    let path = layout.public_key();
    tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(path.parent().expect("keys/ has a parent"))?;
        layout::write_atomically(&path, armored.as_bytes())
    })
    .await
    .map_err(std::io::Error::other)??;
    Ok(Some(key))
}

/// The one signing key's fingerprint.
pub fn the_key(colons: &str) -> Result<String, KeyError> {
    let mut keys = fingerprints(colons);
    match keys.len() {
        0 => Err(KeyError::Missing),
        1 => Ok(keys.remove(0)),
        n => Err(KeyError::Several(n)),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::mirror::Output;

    #[test]
    fn upstream_and_release_signature_command_lines() {
        let layout = Layout::new("/srv/m");
        let release = Path::new("/srv/m/w/dists/x/Release");
        assert_eq!(
            verify_upstream(&layout, Path::new("/s/Release.gpg"), Path::new("/s/Release")).to_string(),
            "gpg --batch --no-default-keyring --trust-model always --status-fd 1 --keyring /srv/m/upstream-keyring.gpg --verify /s/Release.gpg /s/Release"
        );
        assert_eq!(
            clearsign(release, "FPR").to_string(),
            "gpg --batch --yes --local-user FPR --digest-algo SHA512 --output /srv/m/w/dists/x/InRelease --clearsign /srv/m/w/dists/x/Release"
        );
        assert_eq!(
            detach_sign(release, "FPR").to_string(),
            "gpg --batch --yes --local-user FPR --digest-algo SHA512 --armor --output /srv/m/w/dists/x/Release.gpg --detach-sign /srv/m/w/dists/x/Release"
        );
        let good = "[GNUPG:] NEWSIG\n[GNUPG:] GOODSIG A B\n[GNUPG:] VALIDSIG ABC 2026-11-16\n";
        assert!(verified(good));
        assert!(!verified("[GNUPG:] NEWSIG\n[GNUPG:] NO_PUBKEY A\n"));
        assert!(!verified(&format!("{good}[GNUPG:] BADSIG C D\n")));
        assert!(!verified(""));
        // What gpg 2.4 prints, exiting 0, for a key that has expired since.
        let expired = "[GNUPG:] NEWSIG\n\
            [GNUPG:] KEY_CONSIDERED 901EB5B5AD854BC0A3258CD033EA85C4A4277EC5 0\n\
            [GNUPG:] KEYEXPIRED 1577923200\n\
            [GNUPG:] SIG_ID IH1WPp+o5AUm54/u9fX2CTi//P8 2020-01-01 1577837400\n\
            [GNUPG:] EXPKEYSIG 33EA85C4A4277EC5 Old Archive <old@example.com>\n\
            [GNUPG:] VALIDSIG 901EB5B5AD854BC0A3258CD033EA85C4A4277EC5 2020-01-01 1577837400 0 4 0 22 10 00 901EB5B5AD854BC0A3258CD033EA85C4A4277EC5\n";
        assert!(!verified(expired));
        for refused in ["REVKEYSIG 33EA85C4A4277EC5 A", "EXPSIG 33EA85C4A4277EC5 A"] {
            let status = good.replace("GOODSIG A B", refused);
            assert!(!verified(&status), "{status}");
        }
    }

    /// gpg holding at most one key, set by `--quick-generate-key` or by an
    /// `--import` of a file whose name says what it holds.
    #[derive(Default)]
    struct FakeGpg {
        key: Mutex<Option<String>>,
        calls: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl Runner for FakeGpg {
        async fn run(&self, invocation: &Invocation) -> Result<Output, RunError> {
            let line = invocation.to_string();
            self.calls.lock().unwrap().push(line.clone());
            let mut key = self.key.lock().unwrap();
            let stdout = if line.contains("--list-secret-keys") {
                key.iter()
                    .map(|fpr| format!("sec:u:4096:1:X:1::::::scESC:\nfpr:::::::::{fpr}:\n"))
                    .collect()
            } else if line.contains("--quick-generate-key") {
                *key = Some("GEN1".into());
                String::new()
            } else if line.contains("--import") {
                if line.ends_with("secret.asc") {
                    *key = Some("IMP1".into());
                }
                String::new()
            } else if line.contains("--export") {
                format!("PUBLIC {}", key.as_deref().unwrap_or("none"))
            } else {
                unreachable!("{line}")
            };
            Ok(Output {
                success: true,
                status: "exit status: 0".into(),
                stdout,
                stderr: String::new(),
            })
        }
    }

    #[tokio::test]
    async fn one_key_is_generated_or_imported_then_exported() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        let gpg = FakeGpg::default();
        assert_eq!(export(&gpg, &layout).await.unwrap(), None);
        assert!(!layout.public_key().exists());
        assert_eq!(ensure(&gpg, &layout).await.unwrap(), "GEN1");
        assert_eq!(ensure(&gpg, &layout).await.unwrap(), "GEN1");
        assert_eq!(
            std::fs::read_to_string(layout.public_key()).unwrap(),
            "PUBLIC GEN1"
        );
        assert!(gpg.calls.lock().unwrap().iter().any(|c| c
            == "gpg --batch --pinentry-mode loopback --passphrase '' --quick-generate-key 'MonitorFlow archive signing key' rsa4096 sign never"));
        assert!(matches!(
            generate(&gpg, &layout, DEFAULT_UID).await,
            Err(SigningError::Exists(k)) if k == "GEN1"
        ));
        assert!(matches!(
            import(&gpg, &layout, Path::new("/tmp/secret.asc")).await,
            Err(SigningError::Exists(_))
        ));

        let gpg = FakeGpg::default();
        assert!(matches!(
            import(&gpg, &layout, Path::new("/tmp/public.asc")).await,
            Err(SigningError::NoSecretKey)
        ));
        assert_eq!(
            import(&gpg, &layout, Path::new("/tmp/secret.asc"))
                .await
                .unwrap(),
            "IMP1"
        );
        assert_eq!(
            std::fs::read_to_string(layout.public_key()).unwrap(),
            "PUBLIC IMP1"
        );
    }

    const ONE: &str = "sec:u:4096:1:0123456789ABCDEF:1760000000:::u:::scESC:::+:::23::0:
fpr:::::::::AAAA0123456789ABCDEF0123456789ABCDEF0123:
grp:::::::::0011223344556677889900112233445566778899:
uid:u::::1760000000::HASH::MonitorFlow archive::::::::::0:
ssb:u:4096:1:FEDCBA9876543210:1760000000::::::e:::+:::23:
fpr:::::::::BBBB0123456789ABCDEF0123456789ABCDEF0123:
";

    #[test]
    fn the_primary_fingerprint_is_found() {
        assert_eq!(
            the_key(ONE).unwrap(),
            "AAAA0123456789ABCDEF0123456789ABCDEF0123"
        );
        assert_eq!(the_key(""), Err(KeyError::Missing));
        assert_eq!(the_key(&format!("{ONE}{ONE}")), Err(KeyError::Several(2)));
    }
}
