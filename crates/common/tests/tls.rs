//! A real tonic server and client over mTLS on 127.0.0.1.

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use monitorflow_common::config::TlsPaths;
use monitorflow_common::proto::v1::agent_service_client::AgentServiceClient;
use monitorflow_common::proto::v1::agent_service_server::{AgentService, AgentServiceServer};
use monitorflow_common::proto::v1::envelope::Payload;
use monitorflow_common::proto::v1::{Envelope, Hello};
use monitorflow_common::tls::{self, error_chain, AcceptOptions, Identity, PeerRole};
use rustls::pki_types::CertificateRevocationListDer;
use rustls::{ClientConfig, RootCertStore};
use support::{Ca, Leaf};
use tempfile::TempDir;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tonic::transport::Server;
use tonic::{Request, Response, Status, Streaming};

const TIMEOUT: Duration = Duration::from_secs(5);

/// Answers every stream with one `Hello` carrying the caller's identity URI.
/// With `expect` set, it plays an endpoint reserved to that role.
struct WhoAmI {
    expect: Option<PeerRole>,
}

#[tonic::async_trait]
impl AgentService for WhoAmI {
    type StreamStream = tokio_stream::Once<Result<Envelope, Status>>;

    async fn stream(
        &self,
        request: Request<Streaming<Envelope>>,
    ) -> Result<Response<Self::StreamStream>, Status> {
        let peer = tls::peer_identity(&request)?;
        if let Some(expected) = self.expect.filter(|&role| role != peer.role()) {
            return Err(Status::permission_denied(format!(
                "{expected} expected, got {peer}"
            )));
        }
        let hello = Hello {
            agent_id: peer.uri(),
            ..Default::default()
        };
        Ok(Response::new(tokio_stream::once(Ok(Envelope {
            sequence: 1,
            payload: Some(Payload::Hello(hello)),
        }))))
    }
}

fn identity(ca: &Ca, leaf: &Leaf) -> Identity {
    let dir = TempDir::new().unwrap();
    let (ca_cert, cert, key) = support::write(dir.path(), "id", ca, leaf);
    Identity::load(&TlsPaths { ca_cert, cert, key }).unwrap()
}

struct Pki {
    ca: Ca,
    server: Identity,
}

#[derive(Default)]
struct Serve {
    crls: Vec<CertificateRevocationListDer<'static>>,
    expect: Option<PeerRole>,
    options: AcceptOptions,
}

impl Pki {
    fn new() -> Self {
        let ca = Ca::new("monitorflow test CA");
        let server = identity(&ca, &ca.server());
        Self { ca, server }
    }

    fn client(&self, leaf: &Leaf) -> Arc<ClientConfig> {
        identity(&self.ca, leaf).client_config().unwrap()
    }

    async fn serve(&self, serve: Serve) -> String {
        let config = self.server.server_config(serve.crls).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (_tx, rx) = watch::channel(config);
        let service = AgentServiceServer::new(WhoAmI {
            expect: serve.expect,
        });
        tokio::spawn(
            Server::builder()
                .add_service(service)
                .serve_with_incoming(tls::incoming(listener, rx, serve.options)),
        );
        format!("https://127.0.0.1:{port}")
    }
}

/// Our own timeout and tonic's `connect_timeout` race; either is fine.
fn is_timeout(err: &str) -> bool {
    err.contains("timed out") || err.contains("deadline has elapsed")
}

/// Connects and opens a stream; returns the identity URI the server saw.
/// With TLS 1.3 the server's verdict on the client certificate arrives after
/// the client's handshake, so a rejection may surface on the call.
async fn who_am_i(url: &str, config: Arc<ClientConfig>) -> Result<String, String> {
    let channel = tls::connect(url, config, TIMEOUT)
        .await
        .map_err(|e| e.to_string())?;
    let mut client = AgentServiceClient::new(channel);
    let mut stream = client
        .stream(tokio_stream::empty())
        .await
        .map_err(|e| error_chain(&e))?
        .into_inner();
    match stream.message().await.map_err(|e| error_chain(&e))? {
        Some(Envelope {
            payload: Some(Payload::Hello(hello)),
            ..
        }) => Ok(hello.agent_id),
        other => Err(format!("unexpected reply {other:?}")),
    }
}

#[tokio::test]
async fn valid_client_is_identified() {
    let pki = Pki::new();
    let url = pki.serve(Serve::default()).await;
    let id = who_am_i(&url, pki.client(&pki.ca.client("agent-7", 10))).await;
    assert_eq!(id.unwrap(), "monitorflow://agent/agent-7");
}

#[tokio::test]
async fn agent_cert_where_a_relay_is_expected_is_told_apart() {
    let pki = Pki::new();
    let url = pki
        .serve(Serve {
            expect: Some(PeerRole::Relay),
            ..Serve::default()
        })
        .await;
    let relay = who_am_i(&url, pki.client(&pki.ca.relay("relay-1", 10))).await;
    assert_eq!(relay.unwrap(), "monitorflow://relay/relay-1");
    let err = who_am_i(&url, pki.client(&pki.ca.client("relay-1", 11)))
        .await
        .unwrap_err();
    assert!(err.contains("relay expected, got agent relay-1"), "{err}");
}

#[tokio::test]
async fn client_without_certificate_is_rejected() {
    let pki = Pki::new();
    let url = pki.serve(Serve::default()).await;
    let mut roots = RootCertStore::empty();
    roots.add(pki.ca.cert.der().clone()).unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec()];
    let err = who_am_i(&url, Arc::new(config)).await.unwrap_err();
    assert!(err.contains("CertificateRequired"), "{err}");
}

#[tokio::test]
async fn client_from_another_ca_is_rejected() {
    let pki = Pki::new();
    let url = pki.serve(Serve::default()).await;
    // Trusts the server's CA, but its own certificate comes from elsewhere.
    let rogue = Ca::new("rogue CA");
    let config = pki.client(&rogue.client("agent-7", 10));
    let err = who_am_i(&url, config).await.unwrap_err();
    assert!(err.contains("UnknownCA"), "{err}");
}

#[tokio::test]
async fn revoked_client_is_rejected() {
    let pki = Pki::new();
    let url = pki
        .serve(Serve {
            crls: vec![pki.ca.crl(&[10])],
            ..Serve::default()
        })
        .await;
    let revoked = who_am_i(&url, pki.client(&pki.ca.client("agent-revoked", 10))).await;
    assert!(revoked.unwrap_err().contains("CertificateRevoked"));
    let good = who_am_i(&url, pki.client(&pki.ca.client("agent-ok", 11))).await;
    assert_eq!(good.unwrap(), "monitorflow://agent/agent-ok");
}

#[tokio::test]
async fn expired_crl_rejects_every_client() {
    let pki = Pki::new();
    let url = pki
        .serve(Serve {
            crls: vec![pki.ca.crl_until(&[], 2021)],
            ..Serve::default()
        })
        .await;
    let err = who_am_i(&url, pki.client(&pki.ca.client("agent-ok", 11)))
        .await
        .unwrap_err();
    // rustls reports an expired CRL to the client as an unknown CA.
    assert!(err.contains("UnknownCA"), "{err}");
}

#[tokio::test]
async fn client_that_does_not_trust_the_server_is_rejected() {
    let pki = Pki::new();
    let url = pki.serve(Serve::default()).await;
    let other = Ca::new("other CA");
    let config = identity(&other, &other.client("agent-7", 10))
        .client_config()
        .unwrap();
    let err = who_am_i(&url, config).await.unwrap_err();
    assert!(err.starts_with("cannot connect to"), "{err}");
    assert!(err.contains("UnknownIssuer"), "{err}");
}

#[tokio::test]
async fn concurrent_handshakes_are_bounded() {
    let pki = Pki::new();
    let url = pki
        .serve(Serve {
            options: AcceptOptions {
                max_handshakes: 2,
                handshake_timeout: Duration::from_secs(60),
            },
            ..Serve::default()
        })
        .await;
    let addr = url.trim_start_matches("https://");
    // Two silent clients take both handshake slots.
    let idle = [
        TcpStream::connect(addr).await.unwrap(),
        TcpStream::connect(addr).await.unwrap(),
    ];
    let config = pki.client(&pki.ca.client("agent-7", 10));
    let channel = tls::connect(&url, config.clone(), Duration::from_millis(500)).await;
    let err = channel.unwrap_err().to_string();
    assert!(is_timeout(&err), "{err}");
    // Freeing the slots lets the next client through.
    drop(idle);
    let id = who_am_i(&url, config).await;
    assert_eq!(id.unwrap(), "monitorflow://agent/agent-7");
}

#[tokio::test]
async fn connect_gives_up_on_a_silent_server() {
    // Accepts TCP (kernel backlog) but never speaks TLS.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "https://127.0.0.1:{}",
        listener.local_addr().unwrap().port()
    );
    let pki = Pki::new();
    let config = pki.client(&pki.ca.client("agent-7", 10));
    let started = Instant::now();
    let err = tls::connect(&url, config, Duration::from_millis(300))
        .await
        .unwrap_err()
        .to_string();
    assert!(started.elapsed() < Duration::from_secs(3), "{err}");
    assert!(is_timeout(&err), "{err}");
}
