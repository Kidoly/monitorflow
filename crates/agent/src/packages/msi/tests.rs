use std::io::Write;
use std::net::SocketAddr;

use monitorflow_common::proto::v1::software_change::Action;
use monitorflow_common::proto::v1::{PackageManager, SoftwareChange};
use monitorflow_common::software::check_change;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::*;

const CODE: &str = "{0D3D4C9E-6C7B-4F5B-9F65-2B8F1E4A7C11}";
const BODY: &[u8] = b"not really an MSI, but bytes with a digest";

fn sha256(bytes: &[u8]) -> String {
    hex(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
}

fn package(urls: Vec<String>) -> MsiPackage {
    MsiPackage {
        product_code: CODE.into(),
        name: "Tiny".into(),
        sha256: sha256(BODY),
        size: BODY.len() as u64,
        urls,
        properties: vec![("MARKER".into(), r"C:\Program Files\x".into())],
    }
}

#[test]
fn packages_round_trip_through_the_command() {
    let msi = package(vec!["http://relay.example:7480/software/x.msi".into()]);
    let proto = msi.to_package();
    assert_eq!(MsiPackage::of(&proto), Some(msi.clone()));
    let change = SoftwareChange {
        action: Action::Install.into(),
        manager: PackageManager::Winget.into(),
        packages: vec![proto],
        allow_reboot: false,
    };
    assert_eq!(check_change(&change, Some("windows")), Ok(()));
    // Not an MSI package.
    let winget = SoftwarePackage {
        id: "7zip.7zip".into(),
        source: "winget".into(),
        msi: None,
    };
    assert_eq!(MsiPackage::of(&winget), None);
    let json = serde_json::to_string(&MsiPackage {
        sha256: String::new(),
        size: 0,
        urls: vec![],
        properties: vec![],
        ..msi
    })
    .unwrap();
    assert_eq!(
        json,
        format!(r#"{{"product_code":"{CODE}","name":"Tiny"}}"#)
    );
}

#[test]
fn command_lines_are_fixed() {
    let msi = package(vec![]);
    let dir = Path::new(r"C:\ProgramData\MonitorFlow\update\msi");
    let file = msi.file_in(dir);
    let log = msi.log_in(dir);
    assert!(log
        .display()
        .to_string()
        .ends_with("msiexec-0D3D4C9E-6C7B-4F5B-9F65-2B8F1E4A7C11.log"));
    let args = install_args(&file, &msi.properties, &log);
    assert_eq!(
        args[..4],
        [
            "/i",
            &format!("\"{}\"", file.display()),
            "/qn",
            "/norestart"
        ]
    );
    assert_eq!(args[4], "/l*v");
    assert_eq!(args[6], r#"MARKER="C:\Program Files\x""#);
    assert_eq!(args.len(), 7);
    assert_eq!(
        remove_args(CODE, &log)[..4],
        ["/x", CODE, "/qn", "/norestart"]
    );
}

#[test]
fn exit_codes_say_what_happened() {
    assert_eq!(exit(0), (true, false, ""));
    assert_eq!(exit(3010), (true, true, ""));
    assert!(!exit(1603).0);
    assert_eq!(exit(1618).2, "another installation is in progress");
    let msi = package(vec![]);
    let (line, reboot) = result(&msi, 1603);
    assert!(!reboot);
    assert_eq!(line.result, Outcome::Failed);
    assert_eq!(line.hresult, 1603);
    assert_eq!(
        line.title,
        "Tiny: fatal error during installation (msiexec exit code 1603)"
    );
    let (line, reboot) = result(&msi, 3010);
    assert!(reboot);
    assert_eq!((line.name.as_str(), line.title.as_str()), (CODE, "Tiny"));
    assert_eq!(failed(&msi, "nope").title, "Tiny: nope");
}

#[test]
fn files_are_checked_by_size_and_digest() {
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(BODY).unwrap();
    let digest = sha256(BODY);
    assert_eq!(verify(&mut file, &digest, BODY.len() as u64), Ok(()));
    assert!(verify(&mut file, &digest, BODY.len() as u64 - 1).is_err());
    assert!(verify(&mut file, &digest, BODY.len() as u64 + 1).is_err());
    assert!(verify(&mut file, &sha256(b"other"), BODY.len() as u64).is_err());
}

/// Answers every request with `status` and `body`, as many times as asked.
async fn serve(status: &'static str, body: &'static [u8]) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut request = [0; 1024];
            let _ = stream.read(&mut request).await;
            let head = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes()).await;
            let _ = stream.write_all(body).await;
        }
    });
    addr
}

#[tokio::test]
async fn downloads_try_each_url_until_one_matches() {
    let dir = tempfile::tempdir().unwrap();
    let missing = serve("404 Not Found", b"").await;
    let wrong = serve("200 OK", b"something else of a length").await;
    let good = serve("200 OK", BODY).await;
    let msi = package(vec![
        format!("http://{missing}/software/a.msi"),
        format!("http://{wrong}/software/a.msi"),
        format!("http://{good}/software/a.msi"),
    ]);
    let path = download(&msi, dir.path()).await.unwrap();
    assert_eq!(path, msi.file_in(dir.path()));
    assert_eq!(std::fs::read(&path).unwrap(), BODY);
    assert!(!path.with_extension("part").exists());

    let only_bad = package(vec![
        format!("http://{missing}/software/a.msi"),
        format!("http://{wrong}/software/a.msi"),
    ]);
    std::fs::remove_file(&path).unwrap();
    let error = download(&only_bad, dir.path()).await.unwrap_err();
    assert!(error.contains("HTTP 404"), "{error}");
    assert!(error.contains("not the size"), "{error}");
    assert!(!path.exists());
}
