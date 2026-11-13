//! The server's links to VictoriaMetrics and VictoriaLogs over TLS
//! (ADR-0087), against an in-process HTTPS endpoint that, like the stores,
//! reads its certificate files again for every connection.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use monitorflow_server::ca::{CaOptions, CertificateAuthority, ServerName};
use monitorflow_server::ingest::{RemoteWrite, Series, Sink, WriteError};
use monitorflow_server::storage_tls::{self, StorageAuth, CERT_FILE, KEY_FILE, PASSWORD_FILE};
use monitorflow_server::vm_query::{QueryError, VmQuery};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

/// An HTTPS endpoint answering 204 with `Connection: close`, so that every
/// request is a new handshake. Records the requests' heads.
struct Store {
    url: String,
    heads: Arc<Mutex<Vec<String>>>,
}

impl Store {
    /// Serves the certificate and key in `dir` on 127.0.0.1.
    async fn start(dir: PathBuf) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let heads = Arc::new(Mutex::new(Vec::new()));
        let seen = heads.clone();
        tokio::spawn(async move {
            loop {
                let (tcp, _) = listener.accept().await.unwrap();
                let acceptor = TlsAcceptor::from(Arc::new(server_config(&dir)));
                let seen = seen.clone();
                tokio::spawn(async move {
                    let Ok(mut tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    let mut head = Vec::new();
                    let mut buf = [0u8; 4096];
                    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                        match tls.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => head.extend_from_slice(&buf[..n]),
                        }
                    }
                    seen.lock()
                        .unwrap()
                        .push(String::from_utf8_lossy(&head).into_owned());
                    let _ = tls
                        .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                        .await;
                    let _ = tls.shutdown().await;
                });
            }
        });
        Self {
            url: format!("https://127.0.0.1:{port}"),
            heads,
        }
    }

    fn requests(&self) -> Vec<String> {
        self.heads.lock().unwrap().clone()
    }
}

fn server_config(dir: &Path) -> rustls::ServerConfig {
    let cert = pem::parse(std::fs::read(dir.join(CERT_FILE)).unwrap()).unwrap();
    let key = pem::parse(std::fs::read(dir.join(KEY_FILE)).unwrap()).unwrap();
    rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(cert.into_contents())],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.into_contents())),
        )
        .unwrap()
}

fn open_ca(dir: &TempDir, name: &str) -> CertificateAuthority {
    CertificateAuthority::open_or_create(&dir.path().join(name), CaOptions::default()).unwrap()
}

/// The host of the store's URL.
fn names() -> Vec<ServerName> {
    vec![ServerName::Ip("127.0.0.1".parse().unwrap())]
}

fn sink(url: &str, auth: &StorageAuth) -> RemoteWrite {
    RemoteWrite::with_auth(url, Some(auth)).unwrap()
}

fn series() -> Vec<Series> {
    vec![Series {
        labels: vec![("__name__".into(), "up".into())],
        timestamp_ms: 1_700_000_000_000,
        value: 1.0,
    }]
}

#[tokio::test]
async fn writes_over_tls_with_the_password() {
    let dir = TempDir::new().unwrap();
    let ca = open_ca(&dir, "ca");
    let tls = dir.path().join("tls");
    storage_tls::ensure(&ca, &tls, &names()).unwrap();
    let store = Store::start(tls.clone()).await;
    let auth = StorageAuth::load(ca.ca_cert_pem(), &tls).unwrap();

    sink(&store.url, &auth).write(&series()).await.unwrap();

    let password = std::fs::read_to_string(tls.join(PASSWORD_FILE)).unwrap();
    let expected =
        base64::engine::general_purpose::STANDARD.encode(format!("monitorflow:{password}"));
    let requests = store.requests();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0].to_ascii_lowercase().contains(&format!(
            "authorization: basic {}",
            expected.to_ascii_lowercase()
        )),
        "{}",
        requests[0]
    );
}

#[tokio::test]
async fn refuses_a_certificate_from_another_ca() {
    let dir = TempDir::new().unwrap();
    let ca = open_ca(&dir, "ca");
    let other = open_ca(&dir, "other");
    let tls = dir.path().join("tls");
    storage_tls::ensure(&other, &tls, &names()).unwrap();
    let store = Store::start(tls.clone()).await;
    let auth = StorageAuth::load(ca.ca_cert_pem(), &tls).unwrap();

    let err = sink(&store.url, &auth).write(&series()).await.unwrap_err();
    assert!(matches!(err, WriteError::Retryable(_)), "{err:?}");
    let query = VmQuery::with_auth(&store.url, Duration::from_secs(5), Some(&auth)).unwrap();
    let err = query.get_raw("/api/v1/query", &[]).await.unwrap_err();
    assert!(matches!(err, QueryError::Unavailable(_)), "{err:?}");
    // Nothing was sent: not the password, not the data.
    assert!(store.requests().is_empty());
}

#[tokio::test]
async fn refuses_a_certificate_for_another_name() {
    let dir = TempDir::new().unwrap();
    let ca = open_ca(&dir, "ca");
    let tls = dir.path().join("tls");
    storage_tls::ensure(&ca, &tls, &[ServerName::Dns("victorialogs".into())]).unwrap();
    // Served at 127.0.0.1, which the certificate does not name.
    let store = Store::start(tls.clone()).await;
    let auth = StorageAuth::load(ca.ca_cert_pem(), &tls).unwrap();

    let err = sink(&store.url, &auth).write(&series()).await.unwrap_err();
    assert!(matches!(err, WriteError::Retryable(_)), "{err:?}");
    assert!(store.requests().is_empty());
}

#[tokio::test]
async fn a_renewed_certificate_is_served_and_trusted() {
    let dir = TempDir::new().unwrap();
    let ca = open_ca(&dir, "ca");
    let tls = dir.path().join("tls");
    // Expiring in 20 days: inside the renewal margin.
    let key = rcgen::KeyPair::generate().unwrap();
    let csr = rcgen::CertificateParams::default()
        .serialize_request(&key)
        .unwrap()
        .pem()
        .unwrap();
    let short = ca
        .issue_tls_server(&csr, &names(), Some(Duration::from_secs(20 * 86_400)))
        .unwrap();
    storage_tls::ensure(&ca, &tls, &names()).unwrap();
    std::fs::write(tls.join(CERT_FILE), &short.cert_pem).unwrap();
    std::fs::write(tls.join(KEY_FILE), key.serialize_pem()).unwrap();
    let store = Store::start(tls.clone()).await;
    let auth = StorageAuth::load(ca.ca_cert_pem(), &tls).unwrap();
    let sink = sink(&store.url, &auth);
    sink.write(&series()).await.unwrap();

    assert_eq!(
        storage_tls::ensure(&ca, &tls, &names()).unwrap(),
        storage_tls::Outcome::Issued
    );
    assert_ne!(
        std::fs::read_to_string(tls.join(CERT_FILE)).unwrap(),
        short.cert_pem
    );
    // Same client, new handshake, new certificate.
    sink.write(&series()).await.unwrap();
    assert_eq!(store.requests().len(), 2);
}

#[tokio::test]
async fn the_stores_follow_a_change_of_certificate_authority() {
    let dir = TempDir::new().unwrap();
    let ca = Arc::new(open_ca(&dir, "ca"));
    let tls = dir.path().join("tls");
    storage_tls::ensure(&ca, &tls, &names()).unwrap();
    let old_cert = std::fs::read_to_string(tls.join(CERT_FILE)).unwrap();
    let old_key = std::fs::read_to_string(tls.join(KEY_FILE)).unwrap();
    let store = Store::start(tls.clone()).await;
    // One client for the whole change, as the running server has.
    let auth = StorageAuth::load(ca.ca_cert_pem(), &tls)
        .unwrap()
        .following(ca.clone());
    let sink = sink(&store.url, &auth);
    sink.write(&series()).await.unwrap();

    // Introduced: the old CA still issues, the certificate stays.
    ca.start_rotation(false).unwrap();
    assert_eq!(
        storage_tls::ensure(&ca, &tls, &names()).unwrap(),
        storage_tls::Outcome::Kept
    );
    sink.write(&series()).await.unwrap();

    // Switched: a new certificate from the new CA, which the same client
    // trusts at once.
    ca.switch().unwrap();
    assert_eq!(
        storage_tls::ensure(&ca, &tls, &names()).unwrap(),
        storage_tls::Outcome::Issued
    );
    sink.write(&series()).await.unwrap();

    // Retired: the new certificate still works; one from the old CA is
    // refused by the same client.
    ca.retire().unwrap();
    assert_eq!(
        storage_tls::ensure(&ca, &tls, &names()).unwrap(),
        storage_tls::Outcome::Kept
    );
    sink.write(&series()).await.unwrap();
    assert_eq!(store.requests().len(), 4);
    std::fs::write(tls.join(CERT_FILE), old_cert).unwrap();
    std::fs::write(tls.join(KEY_FILE), old_key).unwrap();
    let err = sink.write(&series()).await.unwrap_err();
    assert!(matches!(err, WriteError::Retryable(_)), "{err:?}");
    assert_eq!(store.requests().len(), 4);
}

#[tokio::test]
async fn refuses_a_relay_certificate_for_the_stores_name() {
    use monitorflow_common::tls::{PeerIdentity, PeerRole};

    let dir = TempDir::new().unwrap();
    let ca = Arc::new(open_ca(&dir, "ca"));
    let tls = dir.path().join("tls");
    storage_tls::ensure(&ca, &tls, &names()).unwrap();
    // What a customer administrator could get for a relay named after the
    // store: the same CA, serverAuth, the store's name.
    let key = rcgen::KeyPair::generate().unwrap();
    let csr = rcgen::CertificateParams::default()
        .serialize_request(&key)
        .unwrap()
        .pem()
        .unwrap();
    let relay = PeerIdentity::new(PeerRole::Relay, "relay-9").unwrap();
    let issued = ca.issue(&csr, &relay, None, &names()).unwrap();
    std::fs::write(tls.join(CERT_FILE), &issued.cert_pem).unwrap();
    std::fs::write(tls.join(KEY_FILE), key.serialize_pem()).unwrap();
    let store = Store::start(tls.clone()).await;

    // The one-off check's client and the running server's.
    let fixed = StorageAuth::load(ca.ca_cert_pem(), &tls).unwrap();
    let live = StorageAuth::load(ca.ca_cert_pem(), &tls)
        .unwrap()
        .following(ca.clone());
    for auth in [&fixed, &live] {
        let err = sink(&store.url, auth).write(&series()).await.unwrap_err();
        assert!(matches!(err, WriteError::Retryable(_)), "{err:?}");
        let query = VmQuery::with_auth(&store.url, Duration::from_secs(5), Some(auth)).unwrap();
        let err = query.get_raw("/api/v1/query", &[]).await.unwrap_err();
        assert!(matches!(err, QueryError::Unavailable(_)), "{err:?}");
    }
    // The password never left.
    assert!(store.requests().is_empty());
}
