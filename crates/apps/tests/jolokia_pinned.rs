//! The Java check against a Jolokia stand-in over HTTPS with a
//! self-signed certificate (ADR-0183): trusted by its pinned fingerprint
//! only, and the password never sent to a certificate that is not.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use monitorflow_apps::{build, Place, JMX};
use monitorflow_common::tls::trust::{fingerprint, show_fingerprint};
use monitorflow_sensors::Sample;
use rcgen::KeyPair;
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use serde_json::{json, Value};
use tokio::net::TcpListener;

const PASSWORD: &str = "hunter22-jolokia";

/// What reached the stand-in: the `Authorization` header of each request.
type Seen = Arc<Mutex<Vec<Option<String>>>>;

/// A bulk answer: the heap, the rest absent.
async fn answer(
    request: Request<Incoming>,
    seen: Seen,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let auth = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    seen.lock().unwrap().push(auth);
    let body = request.into_body().collect().await.unwrap().to_bytes();
    let reads: Value = serde_json::from_slice(&body).unwrap();
    let count = reads.as_array().map_or(0, Vec::len);
    let mut answers = vec![json!({"status": 404, "error": "absent"}); count];
    answers[0] = json!({"status": 200, "value": {
        "HeapMemoryUsage": {"used": 800, "committed": 900, "max": 1000},
    }});
    Ok(Response::new(Full::new(Bytes::from(
        Value::Array(answers).to_string(),
    ))))
}

/// A Jolokia stand-in on HTTPS: its port, its certificate's fingerprint,
/// and what reached it.
async fn serve() -> (u16, String, Seen) {
    let key = KeyPair::generate().unwrap();
    let cert = rcgen::CertificateParams::new(vec!["jvm.example.test".to_owned()])
        .unwrap()
        .self_signed(&key)
        .unwrap();
    let print = show_fingerprint(&fingerprint(cert.der()));
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![cert.der().clone()], key)
    .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Seen::default();
    let requests = seen.clone();
    tokio::spawn(async move {
        loop {
            let (tcp, _) = listener.accept().await.unwrap();
            let acceptor = acceptor.clone();
            let seen = requests.clone();
            tokio::spawn(async move {
                if let Ok(stream) = acceptor.accept(tcp).await {
                    let service = hyper::service::service_fn(move |r| answer(r, seen.clone()));
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
                }
            });
        }
    });
    (port, print, seen)
}

async fn collect(port: u16, extra: &[(&str, &str)]) -> Vec<Sample> {
    let mut params = BTreeMap::from([
        ("host".to_owned(), "127.0.0.1".to_owned()),
        ("port".to_owned(), port.to_string()),
        ("tls".to_owned(), "verify".to_owned()),
        ("user".to_owned(), "monitorflow".to_owned()),
        ("password".to_owned(), PASSWORD.to_owned()),
        ("groups".to_owned(), "none".to_owned()),
    ]);
    params.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
    let sensor = build(JMX, &params, Place::Relay).unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    sensor.collect(deadline).await.unwrap()
}

fn find<'a>(samples: &'a [Sample], name: &str) -> &'a Sample {
    samples
        .iter()
        .find(|s| s.sensor == name)
        .unwrap_or_else(|| panic!("no `{name}` in {samples:?}"))
}

#[tokio::test]
async fn the_pinned_certificate_is_trusted() {
    let (port, print, seen) = serve().await;
    let samples = collect(port, &[("fingerprint", &print)]).await;
    assert_eq!(find(&samples, "up").value, 1.0, "{samples:?}");
    assert_eq!(find(&samples, "heap_used_bytes").value, 800.0);
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert!(seen[0].as_deref().is_some_and(|a| a.starts_with("Basic ")));
}

#[tokio::test]
async fn another_certificate_is_refused_before_any_password() {
    let (port, print, seen) = serve().await;
    let wrong = show_fingerprint(&[0xab; 32]);
    let samples = collect(port, &[("fingerprint", &wrong)]).await;
    let up = find(&samples, "up");
    assert_eq!(up.value, 0.0);
    assert_eq!(up.labels["reason"], "tls");
    assert_eq!(
        up.labels["error"],
        format!("the certificate changed: its fingerprint is now {print}")
    );
    // Without a pin, the machine's roots do not trust it either.
    let samples = collect(port, &[]).await;
    let up = find(&samples, "up");
    assert_eq!(up.labels["reason"], "tls");
    assert!(up.labels["error"].contains(&print), "{up:?}");
    assert!(up.labels["error"].contains("not trusted"), "{up:?}");
    // The handshake failed: no request, so no password, reached Jolokia.
    assert!(seen.lock().unwrap().is_empty());
    assert!(!format!("{samples:?}").contains(PASSWORD));
}

#[tokio::test]
async fn a_test_shows_the_certificate_and_reads_once_trusted() {
    let (port, print, seen) = serve().await;
    let samples = collect(port, &[("what", "test")]).await;
    assert_eq!(find(&samples, "answers").value, 1.0, "{samples:?}");
    let certificate = find(&samples, "certificate_trusted");
    assert_eq!(certificate.value, 0.0);
    assert_eq!(certificate.labels["fingerprint"], print);
    assert_eq!(certificate.labels["by_roots"], "false");
    assert_eq!(certificate.labels["subject"], "rcgen self signed cert");
    assert!(certificate.labels["not_after"].parse::<i64>().unwrap() > 0);
    // Not trusted: only a handshake, nothing read, no password sent.
    assert!(samples.iter().all(|s| s.sensor != "up"));
    assert!(seen.lock().unwrap().is_empty());

    let samples = collect(port, &[("what", "test"), ("fingerprint", &print)]).await;
    assert_eq!(find(&samples, "certificate_trusted").value, 1.0);
    assert_eq!(find(&samples, "up").value, 1.0, "{samples:?}");
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_test_of_what_does_not_speak_tls_says_so() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut tcp, _)) = listener.accept().await {
            use tokio::io::AsyncWriteExt;
            let _ = tcp.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n").await;
        }
    });
    let samples = collect(port, &[("what", "test")]).await;
    let answers = find(&samples, "answers");
    assert_eq!(answers.value, 0.0);
    assert_eq!(answers.labels["reason"], "tls");
    assert_eq!(samples.len(), 1);
}
