//! The mTLS listener's CRL is refreshed after a revocation and before it
//! expires, and relays fetch the same CRL, over real connections on
//! 127.0.0.1.

use std::sync::Arc;
use std::time::Duration;

use monitorflow_common::config::TlsPaths;
use monitorflow_common::proto::v1::agent_service_client::AgentServiceClient;
use monitorflow_common::proto::v1::envelope::Payload;
use monitorflow_common::proto::v1::revocation_service_client::RevocationServiceClient;
use monitorflow_common::proto::v1::{Envelope, GetCrlRequest, Hello};
use monitorflow_common::tls::{self, Identity, PeerIdentity, PeerRole};
use monitorflow_common::PROTOCOL_VERSION;
use monitorflow_server::ca::{CaOptions, CertificateAuthority, RevocationReason, Serial};
use monitorflow_server::crl::CrlPublisher;
use monitorflow_server::enroll::server_cert;
use monitorflow_server::grpc::{
    self, AgentStreamService, CrlService, DenyRelayedAgents, Registry, StreamSettings,
};
use rcgen::{CertificateParams, KeyPair};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

const TIMEOUT: Duration = Duration::from_secs(5);

struct Setup {
    dir: TempDir,
    ca: Arc<CertificateAuthority>,
    publisher: Arc<CrlPublisher>,
    url: String,
}

async fn setup(options: CaOptions) -> Setup {
    let dir = TempDir::new().unwrap();
    let ca =
        Arc::new(CertificateAuthority::open_or_create(&dir.path().join("ca"), options).unwrap());
    let paths = tls_paths(&dir, "server");
    let names = server_cert::parse_names(&["localhost".into()]);
    server_cert::ensure(&ca, &paths, &names).unwrap();
    let identity = Identity::load(&paths).unwrap();
    let (publisher, rx) = CrlPublisher::start(ca.clone(), identity).await.unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "https://localhost:{}",
        listener.local_addr().unwrap().port()
    );
    let service = AgentStreamService::new(
        Registry::default(),
        Arc::new(DenyRelayedAgents),
        StreamSettings::default(),
    );
    let crl = CrlService::new(publisher.subscribe());
    tokio::spawn(grpc::serve_all(
        listener,
        rx,
        service,
        None,
        Some(crl),
        std::future::pending(),
    ));
    Setup {
        dir,
        ca,
        publisher: Arc::new(publisher),
        url,
    }
}

fn tls_paths(dir: &TempDir, name: &str) -> TlsPaths {
    TlsPaths {
        ca_cert: dir.path().join(format!("{name}-ca.pem")),
        cert: dir.path().join(format!("{name}.pem")),
        key: dir.path().join(format!("{name}-key.pem")),
    }
}

impl Setup {
    /// An agent certificate from the CA, written next to the server's.
    fn agent(&self, id: &str) -> (Identity, Serial) {
        self.peer(PeerRole::Agent, id)
    }

    /// A certificate for `role`/`id` from the CA.
    fn peer(&self, role: PeerRole, id: &str) -> (Identity, Serial) {
        let key = KeyPair::generate().unwrap();
        let csr = CertificateParams::default()
            .serialize_request(&key)
            .unwrap()
            .pem()
            .unwrap();
        let identity = PeerIdentity::new(role, id).unwrap();
        let issued = self.ca.issue(&csr, &identity, None, &[]).unwrap();
        let paths = tls_paths(&self.dir, id);
        std::fs::write(&paths.ca_cert, self.ca.ca_cert_pem()).unwrap();
        std::fs::write(&paths.cert, &issued.cert_pem).unwrap();
        std::fs::write(&paths.key, key.serialize_pem()).unwrap();
        (Identity::load(&paths).unwrap(), issued.serial)
    }

    /// Opens a stream; with TLS 1.3 a rejected client certificate shows up
    /// on the first call rather than on connect.
    /// Calls `GetCrl` as `peer`.
    async fn get_crl(&self, peer: &Identity) -> Result<Vec<u8>, Box<tonic::Status>> {
        let channel = tls::connect(&self.url, peer.client_config().unwrap(), TIMEOUT)
            .await
            .unwrap();
        let mut client = RevocationServiceClient::new(channel);
        let call = client.get_crl(GetCrlRequest {});
        let response = tokio::time::timeout(TIMEOUT, call)
            .await
            .unwrap()
            .map_err(Box::new)?;
        Ok(response.into_inner().crl_der)
    }

    async fn connects(&self, agent: &Identity, id: &str) -> bool {
        let Ok(channel) = tls::connect(&self.url, agent.client_config().unwrap(), TIMEOUT).await
        else {
            return false;
        };
        let (tx, rx) = mpsc::channel(1);
        let hello = Hello {
            agent_id: id.into(),
            protocol_version: PROTOCOL_VERSION.into(),
            hostname: format!("{id}.example"),
            os_family: "debian".into(),
            ..Default::default()
        };
        tx.send(Envelope {
            sequence: 1,
            payload: Some(Payload::Hello(hello)),
        })
        .await
        .unwrap();
        let mut client = AgentServiceClient::new(channel);
        let call = client.stream(ReceiverStream::new(rx));
        tokio::time::timeout(TIMEOUT, call).await.unwrap().is_ok()
    }
}

#[tokio::test]
async fn revoked_certificate_is_rejected_right_after_revoke() {
    let s = setup(CaOptions::default()).await;
    let (agent, serial) = s.agent("agent-1");
    let (other, _) = s.agent("agent-2");
    assert!(s.connects(&agent, "agent-1").await);

    s.publisher
        .revoke(serial, RevocationReason::KeyCompromise)
        .await
        .unwrap();
    assert!(!s.connects(&agent, "agent-1").await);
    assert!(s.connects(&other, "agent-2").await);
}

#[tokio::test]
async fn timer_refreshes_the_crl_before_it_expires() {
    // A 2 s CRL locks everyone out after 2 s unless it is refreshed.
    let options = CaOptions {
        crl_validity: Duration::from_secs(2),
        ..CaOptions::default()
    };
    let s = setup(options).await;
    let (agent, _) = s.agent("agent-1");
    let publisher = s.publisher.clone();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let timer = tokio::spawn(async move {
        publisher
            .run(Duration::from_millis(500), async {
                let _ = stopped.await;
            })
            .await;
    });
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(s.connects(&agent, "agent-1").await);

    // Without the timer the CRL goes stale and every client is refused.
    stop.send(()).unwrap();
    timer.await.unwrap();
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(!s.connects(&agent, "agent-1").await);
}

/// Serials listed in a DER CRL, without the sign byte of the DER integer.
fn revoked_serials(der: &[u8]) -> Vec<Vec<u8>> {
    use x509_parser::prelude::{CertificateRevocationList, FromDer};
    let (_, crl) = CertificateRevocationList::from_der(der).unwrap();
    crl.iter_revoked_certificates()
        .map(|r| unsigned(r.raw_serial()))
        .collect()
}

#[tokio::test]
async fn relays_fetch_the_crl_the_listener_enforces() {
    let s = setup(CaOptions::default()).await;
    let (relay, _) = s.peer(PeerRole::Relay, "relay-1");
    let (_, serial) = s.agent("agent-1");
    let before = s.get_crl(&relay).await.unwrap();
    assert!(revoked_serials(&before).is_empty());

    s.publisher
        .revoke(serial.clone(), RevocationReason::KeyCompromise)
        .await
        .unwrap();
    let after = s.get_crl(&relay).await.unwrap();
    assert_eq!(revoked_serials(&after), vec![unsigned(serial.as_bytes())]);

    // Agents get nothing: only relays verify other peers' certificates.
    let (other, _) = s.agent("agent-2");
    let status = s.get_crl(&other).await.unwrap_err();
    assert_eq!(status.code(), tonic::Code::PermissionDenied, "{status}");
}

fn unsigned(serial: &[u8]) -> Vec<u8> {
    let start = serial.iter().position(|&b| b != 0).unwrap_or(serial.len());
    serial[start..].to_vec()
}
