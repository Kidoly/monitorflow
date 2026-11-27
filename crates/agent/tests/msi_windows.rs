//! The update service's MSI installs on a real Windows (ADR-0188): a tiny
//! package (`deploy/windows/test-msi/Tiny.wxs`) downloaded from a local
//! HTTP listener, checked, installed with a public property, then removed,
//! as LocalSystem would. Runs on the windows CI job, which builds the
//! package and names it in `MF_TEST_MSI`; skipped without it, since it
//! needs an administrator and changes the machine.

#![cfg(windows)]

use std::net::SocketAddr;
use std::sync::Arc;

use monitorflow_agent::packages::msi::run::{install, is_installed, remove};
use monitorflow_agent::packages::msi::MsiPackage;
use monitorflow_agent::packages::windows_update::Outcome;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// The ProductCode of `Tiny.wxs`.
const CODE: &str = "{6F1C2E7A-3B9D-4C55-A0E2-8D4F7B1A9C30}";
const MARKER: &str = r"set by MonitorFlow, C:\x";

/// Serves `body` to every request.
async fn serve(body: Arc<Vec<u8>>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let body = body.clone();
            tokio::spawn(async move {
                let mut request = [0; 4096];
                let _ = stream.read(&mut request).await;
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes()).await;
                let _ = stream.write_all(&body).await;
            });
        }
    });
    addr
}

fn marker() -> Option<String> {
    windows_registry::LOCAL_MACHINE
        .open(r"SOFTWARE\MonitorFlowMsiTest")
        .and_then(|key| key.get_string("Marker"))
        .ok()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_msi_package_is_checked_installed_and_removed() {
    let Some(path) = std::env::var_os("MF_TEST_MSI") else {
        eprintln!("MF_TEST_MSI is not set: skipped");
        return;
    };
    let path = std::path::PathBuf::from(path);
    let bytes = Arc::new(std::fs::read(&path).unwrap());
    let sha256: String = ring::digest::digest(&ring::digest::SHA256, &bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let addr = serve(bytes.clone()).await;
    // Next to the package, where the job uploads msiexec's logs from on
    // a failure.
    let work = path.with_file_name("work");
    let dir = work.as_path();
    let package = MsiPackage {
        product_code: CODE.into(),
        name: "MSI test".into(),
        sha256,
        size: bytes.len() as u64,
        urls: vec![format!("http://{addr}/software/tiny.msi")],
        properties: vec![("MARKER".into(), MARKER.into())],
    };
    assert!(!is_installed(CODE), "the test package is installed already");

    // A file that is not the command's never reaches msiexec.
    let wrong = MsiPackage {
        sha256: "0".repeat(64),
        ..package.clone()
    };
    let done = install(&[wrong], dir).await;
    assert_eq!(done.results.len(), 1, "{done:?}");
    assert_eq!(done.results[0].result, Outcome::Failed, "{done:?}");
    assert!(done.results[0].title.contains("SHA-256"), "{done:?}");
    assert!(!is_installed(CODE));

    let done = install(std::slice::from_ref(&package), dir).await;
    assert_eq!(done.results.len(), 1, "{done:?}");
    assert!(done.results[0].result.succeeded(), "{done:?}");
    assert!(is_installed(CODE));
    assert_eq!(marker().as_deref(), Some(MARKER));
    assert!(!package.file_in(dir).exists(), "the file stays");
    assert!(package.log_in(dir).is_file(), "no msiexec log");

    let again = install(std::slice::from_ref(&package), dir).await;
    assert!(again.results.is_empty(), "{again:?}");
    assert_eq!(again.unchanged, [CODE]);

    let removal = MsiPackage {
        sha256: String::new(),
        size: 0,
        urls: vec![],
        properties: vec![],
        ..package
    };
    let done = remove(std::slice::from_ref(&removal), dir).await;
    assert!(done.results[0].result.succeeded(), "{done:?}");
    assert!(!is_installed(CODE));
    assert_eq!(marker(), None);
    let again = remove(std::slice::from_ref(&removal), dir).await;
    assert_eq!(again.unchanged, [CODE]);
}
