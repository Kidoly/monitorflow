//! The snap inventory against a fake snapd on a Unix socket (ADR-0140):
//! the exact requests the agent sends, and what it makes of the answers.

#![cfg(unix)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use monitorflow_agent::packages::snap::Snap;
use monitorflow_agent::packages::Tools;
use monitorflow_common::proto::v1::{PackageManager, PackageScope};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;

/// Answers each request line with the body mapped to its path, or a 404,
/// as snapd does, then closes the connection; records the request lines.
fn fake_snapd(
    dir: &TempDir,
    answers: HashMap<&'static str, (u16, &'static str)>,
) -> (PathBuf, Arc<Mutex<Vec<String>>>) {
    let socket = dir.path().join("snapd.socket");
    let listener = UnixListener::bind(&socket).unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buf = [0u8; 1024];
            while !request.ends_with(b"\r\n\r\n") {
                let n = stream.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
            }
            let text = String::from_utf8(request).unwrap();
            let line = text.lines().next().unwrap_or("").to_owned();
            log.lock().unwrap().push(text);
            let path = line.split(' ').nth(1).unwrap_or("");
            let (status, body) = answers.get(path).copied().unwrap_or((
                404,
                r#"{"type":"error","status-code":404,"result":{"message":"not found","kind":"not-found"}}"#,
            ));
            let answer = format!(
                "HTTP/1.0 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(answer.as_bytes()).await.unwrap();
            stream.shutdown().await.unwrap();
        }
    });
    (socket, seen)
}

fn tools(dir: &TempDir, socket: PathBuf) -> Tools {
    let snap = dir.path().join("snap");
    std::fs::write(&snap, "").unwrap();
    Tools {
        snap,
        snapd_socket: socket,
        ..Tools::default()
    }
}

const SNAPS: &str = r#"{"type":"sync","status-code":200,"result":[{"name":"example-app","version":"1.2.0","revision":"57","tracking-channel":"2/stable","publisher":{"username":"example-dev"}},{"name":"example-base","version":"1","revision":"3","tracking-channel":"latest/stable"}]}"#;
const REFRESH: &str = r#"{"type":"sync","status-code":200,"result":[{"name":"example-app","version":"1.3.1","revision":"61"}]}"#;
const SYSTEM_INFO: &str = r#"{"type":"sync","status-code":200,"result":{"version":"2.67","refresh":{"timer":"00:00~24:00/4","last":"2026-11-15T21:14:00Z","next":"2026-11-16T03:10:00Z"}}}"#;

#[tokio::test]
async fn inventory_and_schedule_from_snapd() {
    let dir = TempDir::new().unwrap();
    let (socket, seen) = fake_snapd(
        &dir,
        HashMap::from([
            ("/v2/snaps", (200, SNAPS)),
            ("/v2/find?select=refresh", (200, REFRESH)),
            ("/v2/system-info", (200, SYSTEM_INFO)),
        ]),
    );
    let snap = Snap::new(tools(&dir, socket));
    assert!(snap.present());

    let apps = snap.inventory().await.unwrap();
    let summary: Vec<_> = apps
        .iter()
        .map(|a| {
            assert_eq!(a.manager, PackageManager::Snap as i32);
            assert_eq!(a.scope, PackageScope::Machine as i32);
            (
                a.id.as_str(),
                a.origin.as_str(),
                a.version.as_str(),
                a.available_version.as_str(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("example-app", "2/stable", "1.2.0", "1.3.1"),
            ("example-base", "latest/stable", "1", ""),
        ]
    );

    let schedule = snap.system().await.unwrap();
    assert_eq!(schedule.version, "2.67");
    assert_eq!(schedule.timer, "00:00~24:00/4");
    assert_eq!(schedule.hold, "");
    assert_eq!(schedule.next, "2026-11-16T03:10:00Z");

    let requests = seen.lock().unwrap().clone();
    let lines: Vec<_> = requests
        .iter()
        .map(|r| r.lines().next().unwrap().to_owned())
        .collect();
    assert_eq!(
        lines,
        [
            "GET /v2/snaps HTTP/1.0",
            "GET /v2/find?select=refresh HTTP/1.0",
            "GET /v2/system-info HTTP/1.0",
        ]
    );
    assert!(requests.iter().all(|r| r.contains("\r\nHost: snapd\r\n")));
}

#[tokio::test]
async fn no_refresh_and_refusals() {
    let dir = TempDir::new().unwrap();
    // Some snapd versions answer "no updates" with a not-found error.
    let (socket, _) = fake_snapd(
        &dir,
        HashMap::from([
            ("/v2/snaps", (200, SNAPS)),
            (
                "/v2/find?select=refresh",
                (
                    404,
                    r#"{"type":"error","status-code":404,"result":{"message":"no snap updates available","kind":"snap-not-found"}}"#,
                ),
            ),
        ]),
    );
    let snap = Snap::new(tools(&dir, socket));
    let apps = snap.inventory().await.unwrap();
    assert!(apps.iter().all(|a| a.available_version.is_empty()));
    let error = snap.system().await.unwrap_err().to_string();
    assert!(error.contains("not found (not-found)"), "{error}");
}

#[tokio::test]
async fn store_errors_fail_the_inventory() {
    let dir = TempDir::new().unwrap();
    let (socket, _) = fake_snapd(
        &dir,
        HashMap::from([
            ("/v2/snaps", (200, SNAPS)),
            (
                "/v2/find?select=refresh",
                (
                    400,
                    r#"{"type":"error","status-code":400,"result":{"message":"unable to contact snap store","kind":"network-timeout"}}"#,
                ),
            ),
        ]),
    );
    let error = Snap::new(tools(&dir, socket))
        .inventory()
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("unable to contact snap store"), "{error}");
}

#[tokio::test]
async fn absent_snapd() {
    let dir = TempDir::new().unwrap();
    let tools = Tools {
        snap: dir.path().join("snap"),
        snapd_socket: dir.path().join("snapd.socket"),
        ..Tools::default()
    };
    let snap = Snap::new(tools);
    assert!(!snap.present());
    let error = snap.inventory().await.unwrap_err().to_string();
    assert!(error.contains("cannot reach snapd"), "{error}");
}
