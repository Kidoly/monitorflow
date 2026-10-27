use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use tempfile::TempDir;

use super::*;
use crate::packages::Tools;
use crate::self_update::tests::{elf, ReleaseFiles, Signer};

const ASSET: &str = "monitorflow-agent-linux-x86_64";

/// Serves `/<dir>/<file>` from memory.
async fn serve(files: HashMap<String, Vec<u8>>) -> String {
    async fn get(
        State(files): State<Arc<HashMap<String, Vec<u8>>>>,
        UrlPath((dir, file)): UrlPath<(String, String)>,
    ) -> Result<Vec<u8>, StatusCode> {
        files
            .get(&format!("{dir}/{file}"))
            .cloned()
            .ok_or(StatusCode::NOT_FOUND)
    }
    let app = axum::Router::new()
        .route("/{dir}/{file}", axum::routing::get(get))
        .with_state(Arc::new(files));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

fn add(files: &mut HashMap<String, Vec<u8>>, dir: &str, release: &ReleaseFiles) {
    for (name, bytes) in [
        ("SHA256SUMS", release.sums.as_bytes().to_vec()),
        ("SHA256SUMS.sig", release.signature.clone()),
        ("VERSION", release.version_file.as_bytes().to_vec()),
        (ASSET, release.binary.clone()),
    ] {
        files.insert(format!("{dir}/{name}"), bytes);
    }
}

/// A `sudo` that saves its arguments and stdin, prints `reply` and exits
/// with `code`.
fn fake_sudo(dir: &TempDir, reply: &str, code: i32) -> Tools {
    let script = dir.path().join("sudo");
    let out = dir.path().display();
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >{out}/args\ncat >{out}/stdin\necho '{reply}'\necho 'Error: refused' >&2\nexit {code}\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    Tools {
        sudo: script,
        agent: PathBuf::from("/usr/bin/monitorflow-agent"),
        ..Tools::default()
    }
}

fn handler(tools: Tools, signer: &Signer) -> SelfUpdateHandler {
    SelfUpdateHandler::with(
        SudoHelper { tools },
        vec![signer.public()],
        "0.2.0".parse().unwrap(),
        Some(Platform::LinuxX86_64),
    )
}

fn payload(version: &str, urls: &[String]) -> Vec<u8> {
    SelfUpdate {
        version: version.into(),
        base_urls: urls.to_vec(),
        allow_downgrade: false,
        health_timeout_seconds: 60,
    }
    .encode_to_vec()
}

#[tokio::test]
async fn takes_the_first_source_with_a_valid_release_and_hands_it_to_the_helper() {
    let signer = Signer::new();
    let good = signer.release("0.3.0", ASSET, &elf("0.3.0"));
    let mut files = HashMap::new();
    add(
        &mut files,
        "foreign",
        &Signer::new().release("0.3.0", ASSET, &elf("evil")),
    );
    add(
        &mut files,
        "stale",
        &signer.release("0.2.5", ASSET, &elf("0.2.5")),
    );
    add(&mut files, "good", &good);
    let base = serve(files).await;
    let dir = TempDir::new().unwrap();
    let handler = handler(fake_sudo(&dir, "already running 0.3.0", 0), &signer);
    let urls = ["missing", "foreign", "stale", "good"].map(|d| format!("{base}/{d}"));

    let output = handler.run_command("c-1", &payload("0.3.0", &urls)).await;
    assert_eq!(
        (output.exit_code, output.error.as_deref()),
        (0, None),
        "{output:?}"
    );
    let log = String::from_utf8(output.stdout).unwrap();
    assert_eq!(log.matches("skipped").count(), 3, "{log}");
    assert!(log.contains("does not verify"), "{log}");
    assert!(log.contains("serves 0.2.5, not 0.3.0"), "{log}");
    assert!(
        log.contains(&format!("downloaded 0.3.0 from {base}/good/")),
        "{log}"
    );

    let args = std::fs::read_to_string(dir.path().join("args")).unwrap();
    assert_eq!(args.trim(), "-n /usr/bin/monitorflow-agent self-update");
    assert_eq!(
        std::fs::read(dir.path().join("stdin")).unwrap(),
        good.input("c-1", 60_000)
    );
}

#[tokio::test]
async fn fails_when_no_source_is_valid_and_reports_helper_errors() {
    let signer = Signer::new();
    let mut files = HashMap::new();
    let mut truncated = signer.release("0.3.0", ASSET, &elf("0.3.0"));
    truncated.binary.truncate(10);
    add(&mut files, "truncated", &truncated);
    add(
        &mut files,
        "good",
        &signer.release("0.3.0", ASSET, &elf("0.3.0")),
    );
    let base = serve(files).await;
    let dir = TempDir::new().unwrap();

    let none = handler(fake_sudo(&dir, "", 0), &signer)
        .run_command("c-1", &payload("0.3.0", &[format!("{base}/truncated")]))
        .await;
    assert_eq!(
        none.error.as_deref(),
        Some("no source served a valid 0.3.0 release")
    );
    assert!(!dir.path().join("stdin").exists(), "the helper never ran");

    let refused = handler(fake_sudo(&dir, "", 3), &signer)
        .run_command("c-1", &payload("0.3.0", &[format!("{base}/good")]))
        .await;
    assert_eq!(
        (refused.exit_code, refused.error.as_deref()),
        (3, Some("Error: refused"))
    );
}

#[tokio::test]
async fn refuses_bad_payloads_without_downloading() {
    let signer = Signer::new();
    let dir = TempDir::new().unwrap();
    let handler = handler(fake_sudo(&dir, "", 0), &signer);
    let url = vec!["http://127.0.0.1:9/r".to_string()];
    for (name, payload) in [
        ("downgrade", payload("0.1.0", &url)),
        ("version", payload("latest", &url)),
        ("no url", payload("0.3.0", &[])),
        ("scheme", payload("0.3.0", &["file:///etc".into()])),
        (
            "credentials",
            payload("0.3.0", &["https://u:p@example.com/".into()]),
        ),
        ("garbage", b"\xff\xff".to_vec()),
    ] {
        let output = handler.run_command("c-1", &payload).await;
        assert!(output.rejected, "{name}: {output:?}");
    }
    let same = handler.run_command("c-1", &payload("0.2.0", &url)).await;
    assert_eq!(same.stdout, b"already running 0.2.0\n");
}

#[test]
fn base_urls_get_a_trailing_slash() {
    let spec = SelfUpdate {
        base_urls: vec![
            "https://example.com/agent/0.3.0".into(),
            "http://relay:7480/agent/0.3.0/".into(),
        ],
        ..SelfUpdate::default()
    };
    let urls: Vec<String> = base_urls(&spec)
        .unwrap()
        .iter()
        .map(Url::to_string)
        .collect();
    assert_eq!(
        urls,
        [
            "https://example.com/agent/0.3.0/",
            "http://relay:7480/agent/0.3.0/"
        ]
    );
}
