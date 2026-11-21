//! The PRTG reader through its HTTP client, against a local stand-in of
//! PRTG's API that serves recorded, anonymised answers. It checks the
//! credentials are sent, that only GET requests of reading endpoints are
//! made, and what a refused token gives.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use chrono::{TimeZone, Utc};
use monitorflow_import::prtg::Prtg;
use monitorflow_import::source::{Connection, Context, Input, ReadError, Source};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const TOKEN: &str = "recorded-token";
/// The only paths a reader may ask for.
const READING: &[&str] = &[
    "/api/status.json",
    "/api/table.json",
    "/api/getobjectproperty.htm",
    "/controls/editnotification.htm",
];

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap();
                out.push(u8::from_str_radix(hex, 16).unwrap());
                i += 2;
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8(out).unwrap()
}

/// What the stand-in saw: request lines, and requests refused.
#[derive(Default)]
struct Seen {
    requests: Vec<String>,
    refused: usize,
}

/// Answers a request line: `(status, body)`.
fn answer(recorded: &BTreeMap<String, Value>, line: &str, seen: &Mutex<Seen>) -> (u16, String) {
    let mut parts = line.split(' ');
    let (method, target) = (parts.next().unwrap(), parts.next().unwrap());
    seen.lock()
        .unwrap()
        .requests
        .push(format!("{method} {}", target.split('?').next().unwrap()));
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let mut pairs: Vec<(String, String)> = query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (decode(k), decode(v))
        })
        .collect();
    let token = pairs
        .iter()
        .position(|(k, _)| k == "apitoken")
        .map(|i| pairs.remove(i).1);
    if token.as_deref() != Some(TOKEN) {
        seen.lock().unwrap().refused += 1;
        return (401, "Unauthorized".into());
    }
    let query: Vec<String> = pairs.iter().map(|(k, v)| format!("{k}={v}")).collect();
    let key = if query.is_empty() {
        path.to_owned()
    } else {
        format!("{path}?{}", query.join("&"))
    };
    match recorded.get(&key) {
        Some(Value::String(body)) => (200, body.clone()),
        Some(value) => (200, value.to_string()),
        None if path == "/api/getobjectproperty.htm" => (
            200,
            "<prtg><result>(Property not found)</result></prtg>".into(),
        ),
        None => (404, format!("nothing recorded for {key}")),
    }
}

/// Starts the stand-in; returns its base URL.
async fn stand_in(seen: Arc<Mutex<Seen>>) -> String {
    let recorded: Arc<BTreeMap<String, Value>> =
        Arc::new(serde_json::from_str(include_str!("fixtures/prtg/api.json")).unwrap());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (recorded, seen) = (recorded.clone(), seen.clone());
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buf = [0u8; 4096];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = stream.read(&mut buf).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    request.extend_from_slice(&buf[..n]);
                }
                let text = String::from_utf8_lossy(&request);
                let line = text.lines().next().unwrap().to_owned();
                let (status, body) = answer(&recorded, &line, &seen);
                let response = format!(
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            });
        }
    });
    format!("http://{address}/")
}

fn connect(url: &str, token: &str) -> Input {
    Input::Connect(Connection(BTreeMap::from([
        ("url".to_owned(), url.to_owned()),
        ("apitoken".to_owned(), token.to_owned()),
    ])))
}

fn context() -> Context {
    Context::new(Utc.with_ymd_and_hms(2026, 11, 17, 12, 0, 0).unwrap())
}

#[tokio::test]
async fn reads_the_whole_install_with_get_requests_only() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let url = stand_in(seen.clone()).await;
    let inventory = Prtg.read(connect(&url, TOKEN), &context()).await.unwrap();
    let expected: Value =
        serde_json::from_str(include_str!("fixtures/prtg/api-inventory.json")).unwrap();
    assert_eq!(serde_json::to_value(&inventory).unwrap(), expected);
    let seen = seen.lock().unwrap();
    assert_eq!(seen.refused, 0);
    for request in &seen.requests {
        let (method, path) = request.split_once(' ').unwrap();
        assert_eq!(method, "GET", "{request}");
        assert!(READING.contains(&path), "{request}");
    }
}

#[tokio::test]
async fn a_refused_token_says_so() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let url = stand_in(seen.clone()).await;
    let error = Prtg
        .read(connect(&url, "wrong"), &context())
        .await
        .unwrap_err();
    assert!(matches!(error, ReadError::Refused(_)), "{error}");
    // The token is not in the message.
    assert!(!error.to_string().contains("wrong"));
}

/// Reading a file runs off the runtime: on a runtime of one thread,
/// another task goes on while a large file is read, where it once waited
/// for the whole reading.
#[tokio::test(flavor = "current_thread")]
async fn a_large_file_is_read_off_the_runtime() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let mut xml = String::from(
        "<prtg><nodes><group id=\"0\"><data><name>Root</name></data><nodes>\
         <probenode id=\"1\"><data><name>Probe</name></data><nodes>",
    );
    for i in 0..2000 {
        xml.push_str(&format!(
            "<device id=\"{}\"><data><name>d{i}</name><host>192.0.2.{}</host></data><nodes>\
             <sensor id=\"{}\"><data><name>Ping</name><sensortype>ping</sensortype></data></sensor>\
             </nodes></device>",
            10_000 + i,
            i % 250 + 1,
            100_000 + i,
        ));
    }
    xml.push_str("</nodes></probenode></nodes></group></nodes></prtg>");
    let ticks = Arc::new(AtomicUsize::new(0));
    let ticker = {
        let ticks = ticks.clone();
        tokio::spawn(async move {
            loop {
                ticks.fetch_add(1, Ordering::Relaxed);
                tokio::task::yield_now().await;
            }
        })
    };
    let files = vec![monitorflow_import::source::File {
        name: "PRTG Configuration.dat".into(),
        content: xml.into_bytes(),
    }];
    let inventory = Prtg
        .read(Input::Files(files), &Context::new(Utc::now()))
        .await
        .unwrap();
    let during = ticks.load(Ordering::Relaxed);
    ticker.abort();
    assert_eq!(inventory.hosts.len(), 2000);
    assert!(during > 0, "the runtime waited for the reading");
}
