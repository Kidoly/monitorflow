//! The renewal client against an upstream that answers like a compromised
//! relay (ADR-0107): a real `CertificateService` over mTLS on 127.0.0.1,
//! with a server certificate of the real CA, as a relay has, that tries to
//! give the peer a CA of its own.

mod support;

use std::time::Duration;

use monitorflow_common::ca_bundle;
use monitorflow_common::client::identity::{self, Enrolled, StoredIdentity};
use monitorflow_common::client::renew::{RenewError, Renewal, RenewalSettings};
use monitorflow_common::command;
use monitorflow_common::config::TlsPaths;
use monitorflow_common::proto::v1::certificate_service_server::{
    CertificateService, CertificateServiceServer,
};
use monitorflow_common::proto::v1::{Command, ForwardedRenewRequest, RenewRequest, RenewResponse};
use monitorflow_common::redact::Secret;
use monitorflow_common::tls::{self, AcceptOptions, Identity, PeerRole};
use rcgen::{CertificateSigningRequestParams, ExtendedKeyUsagePurpose, KeyPair};
use ring::rand::SystemRandom;
use ring::signature::{EcdsaKeyPair, ECDSA_P256_SHA256_ASN1_SIGNING};
use support::{uri_san, Ca};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tonic::transport::Server;
use tonic::{Request, Response, Status};

fn sign(key: &KeyPair, message: &[u8]) -> Vec<u8> {
    let rng = SystemRandom::new();
    EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &key.serialize_der(), &rng)
        .unwrap()
        .sign(&rng, message)
        .unwrap()
        .as_ref()
        .to_vec()
}

/// Whose signatures the upstream sends with its bundle.
#[derive(Clone, Copy)]
enum SignedBy {
    /// None, as a server or relay older than ADR-0107 would.
    Nobody,
    /// The new CA's own.
    TheNewCa,
    /// The CA the peer trusts: only the central server can.
    TheTrustedCa,
}

/// Answers every `Renew` with a certificate for the caller's key and
/// identity from `new`, and the bundle `[new, trusted]`.
struct Upstream {
    new: Ca,
    trusted: Ca,
    signed: SignedBy,
}

#[tonic::async_trait]
impl CertificateService for Upstream {
    async fn renew(
        &self,
        request: Request<RenewRequest>,
    ) -> Result<Response<RenewResponse>, Status> {
        let peer = tls::peer_identity(&request)?;
        let mut csr = CertificateSigningRequestParams::from_pem(&request.get_ref().csr_pem)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        csr.params.subject_alt_names = vec![uri_san(&peer.uri())];
        csr.params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        let cert = csr.signed_by(&self.new.cert).unwrap();
        let fingerprint = |ca: &Ca| tls::fingerprint(ca.cert.der());
        let bundle = [fingerprint(&self.new), fingerprint(&self.trusted)];
        let message = ca_bundle::signed_bytes(&tls::fingerprint(cert.der()), &bundle);
        let ca_bundle_signatures = match self.signed {
            SignedBy::Nobody => vec![],
            SignedBy::TheNewCa => vec![sign(self.new.cert.key(), &message)],
            SignedBy::TheTrustedCa => vec![
                sign(self.new.cert.key(), &message),
                sign(self.trusted.cert.key(), &message),
            ],
        };
        Ok(Response::new(RenewResponse {
            cert_pem: cert.pem(),
            ca_cert_pem: self.new.cert.pem(),
            peer_uri: peer.uri(),
            not_after_unix: 0,
            ca_bundle_pem: format!("{}{}", self.new.cert.pem(), self.trusted.cert.pem()),
            ca_bundle_signatures,
        }))
    }

    async fn renew_forwarded(
        &self,
        _: Request<ForwardedRenewRequest>,
    ) -> Result<Response<RenewResponse>, Status> {
        Err(Status::unimplemented("not used"))
    }
}

struct Setup {
    dir: TempDir,
    paths: TlsPaths,
    agent: StoredIdentity,
    url: String,
    /// The key of the CA the upstream tries to add.
    new_key: KeyPair,
}

/// An agent enrolled with a CA, and an upstream holding a server
/// certificate of that CA.
async fn setup(signed: SignedBy) -> Setup {
    let dir = TempDir::new().unwrap();
    let ca = Ca::new("the real CA");
    let (ca_cert, cert, key) = support::write(dir.path(), "upstream", &ca, &ca.server());
    let upstream = Identity::load(&TlsPaths { ca_cert, cert, key }).unwrap();

    let paths = TlsPaths {
        ca_cert: dir.path().join("agent/ca.pem"),
        cert: dir.path().join("agent/cert.pem"),
        key: dir.path().join("agent/key.pem"),
    };
    let leaf = ca.client("web-01", 7);
    let enrolled = Enrolled {
        key_pem: Secret::new(leaf.key.serialize_pem()),
        cert_pem: leaf.cert.pem(),
        ca_cert_pem: ca.cert.pem(),
    };
    identity::store(&paths, &enrolled).unwrap();
    let agent = identity::load(&paths, PeerRole::Agent).unwrap().unwrap();

    let new = Ca::new("another CA");
    let new_key = KeyPair::try_from(new.cert.key().serialize_der()).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "https://127.0.0.1:{}",
        listener.local_addr().unwrap().port()
    );
    let (tx, rx) = watch::channel(upstream.server_config(vec![ca.crl(&[])]).unwrap());
    let service = CertificateServiceServer::new(Upstream {
        new,
        trusted: ca,
        signed,
    });
    tokio::spawn(async move {
        let _keep = tx;
        Server::builder()
            .add_service(service)
            .serve_with_incoming(tls::incoming(listener, rx, AcceptOptions::default()))
            .await
    });
    Setup {
        dir,
        paths,
        agent,
        url,
        new_key,
    }
}

impl Setup {
    async fn renew(&self) -> Result<StoredIdentity, RenewError> {
        let settings = RenewalSettings {
            connect_timeout: Duration::from_secs(5),
            ..RenewalSettings::for_upstream(&self.url)
        };
        let (renewal, _rx) = Renewal::new(settings, self.paths.clone(), self.agent.clone());
        renewal.renew(&self.agent).await
    }

    fn files(&self) -> [Vec<u8>; 3] {
        [&self.paths.ca_cert, &self.paths.cert, &self.paths.key].map(|p| std::fs::read(p).unwrap())
    }

    /// Whether `identity` runs a command signed with the new CA's key.
    fn obeys_the_new_ca(&self, identity: &StoredIdentity) -> bool {
        let mut cmd = Command {
            command_id: "c1".into(),
            agent_id: "web-01".into(),
            kind: command::SHELL.into(),
            ..Command::default()
        };
        cmd.signature = sign(&self.new_key, &command::signed_bytes(&cmd));
        command::verify(&cmd, identity.tls.ca_certs()).is_ok()
    }
}

#[tokio::test]
async fn a_relay_cannot_add_its_own_ca() {
    for (signed, why) in [
        (SignedBy::Nobody, "without the server's signature"),
        (
            SignedBy::TheNewCa,
            "no certificate authority this peer trusts",
        ),
    ] {
        let setup = setup(signed).await;
        let before = setup.files();
        let error = setup.renew().await.unwrap_err();
        assert!(matches!(error, RenewError::BadResponse(_)), "{error}");
        assert!(error.to_string().contains(why), "{error}");

        // Nothing was written: the same key, certificate and CA, and no
        // leftover of a renewal.
        assert_eq!(setup.files(), before);
        let stored = identity::load(&setup.paths, PeerRole::Agent)
            .unwrap()
            .unwrap();
        assert_eq!(stored.ca_fingerprints, setup.agent.ca_fingerprints);
        assert_eq!(stored.tls.leaf(), setup.agent.tls.leaf());
        assert_eq!(
            std::fs::read_dir(setup.dir.path().join("agent"))
                .unwrap()
                .count(),
            3
        );
        assert!(!setup.obeys_the_new_ca(&stored));
    }
}

/// The same answer with the trusted CA's signature is a change of
/// certificate authority, which only the central server can make.
#[tokio::test]
async fn a_new_ca_signed_by_the_trusted_one_is_stored() {
    let setup = setup(SignedBy::TheTrustedCa).await;
    assert!(!setup.obeys_the_new_ca(&setup.agent));
    let renewed = setup.renew().await.unwrap();
    assert_eq!(renewed.id, setup.agent.id);
    assert_eq!(renewed.ca_fingerprints.len(), 2);
    assert_eq!(renewed.ca_fingerprints[1], setup.agent.ca_fingerprints[0]);
    assert!(setup.obeys_the_new_ca(&renewed));
}
