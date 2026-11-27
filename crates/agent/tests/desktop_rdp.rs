//! A remote desktop against a real RDP service (ADR-0192): the agent's
//! own tunnel to the host's Remote Desktop on 127.0.0.1, sealed frames
//! and window included, with IronRDP's client in the central server's
//! place: X.224, TLS, network level authentication with a local account,
//! the connection sequence, then the first picture of the screen.
//!
//! Ignored by default, as it needs Remote Desktop turned on and an
//! account: CI's `windows` job turns it on with
//! `deploy/windows/rdp-test.ps1` and runs it with `--ignored`. See
//! [`support::rdp_client::Account`] for the variables it reads.

#[path = "support/rdp_client.rs"]
mod rdp_client;

use std::net::SocketAddr;
use std::sync::Arc;

use monitorflow_agent::commands::Handler;
use monitorflow_agent::remote::{self, Allowed, Proof, RemoteSessionHandler, Spawners};
use monitorflow_common::proto::v1::{remote_data, remote_frame, RemoteFrame, RemoteSessionOpen};
use monitorflow_common::remote::{self as sealing, Channel, ServerHandshake, MAX_CHUNK};
use prost::Message;
use ring::rand::SystemRandom;
use ring::signature::{EcdsaKeyPair, ECDSA_P256_SHA256_ASN1_SIGNING};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

const SESSION: &str = "0b5c8a52-6c43-4a6e-8f1e-9d8a3f1c2b7e";
const AGENT: &str = "rdp-test";
/// The server says what it passed on every this many output frames, as
/// the central server does.
const DELIVERED_EVERY: u64 = 32;

/// The agent's certificate and key, from a CA of the test's own.
struct TestProof {
    cert: Vec<u8>,
    key: EcdsaKeyPair,
}

impl Proof for TestProof {
    fn certificate(&self) -> Vec<u8> {
        self.cert.clone()
    }

    fn sign(&self, message: &[u8]) -> Option<Vec<u8>> {
        self.key
            .sign(&SystemRandom::new(), message)
            .ok()
            .map(|s| s.as_ref().to_vec())
    }
}

fn proof() -> TestProof {
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let ca =
        rcgen::CertifiedIssuer::self_signed(params, rcgen::KeyPair::generate().unwrap()).unwrap();
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    params.subject_alt_names = vec![rcgen::SanType::URI(
        format!("monitorflow://agent/{AGENT}").try_into().unwrap(),
    )];
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = params.signed_by(&key, &ca).unwrap();
    TestProof {
        cert: cert.der().to_vec(),
        key: EcdsaKeyPair::from_pkcs8(
            &ECDSA_P256_SHA256_ASN1_SIGNING,
            &key.serialize_der(),
            &SystemRandom::new(),
        )
        .unwrap(),
    }
}

fn sealed(channel: &mut Channel, data: &monitorflow_common::proto::v1::RemoteData) -> RemoteFrame {
    let (counter, sealed) = channel.seal(data);
    RemoteFrame {
        session_id: SESSION.into(),
        body: Some(remote_frame::Body::Sealed(sealed)),
        counter,
    }
}

#[tokio::test]
#[ignore = "needs Remote Desktop on 127.0.0.1 and an account; CI's windows job runs it"]
async fn a_real_remote_desktop_shows_its_screen_through_the_agents_tunnel() {
    let account = rdp_client::Account::from_env();
    assert!(
        !account.username.is_empty(),
        "set MF_RDP_USERNAME and MF_RDP_PASSWORD"
    );
    let port = rdp_client::port();
    // The agent's own connection when the service is where Windows puts
    // it; a test service's loopback port otherwise.
    let desktop = if port == 3389 {
        remote::desktop::spawner()
    } else {
        remote::desktop::connector(SocketAddr::from(([127, 0, 0, 1], port)))
    };
    let unused = remote::desktop::connector(SocketAddr::from(([127, 0, 0, 1], 9)));
    let (handle, mut end) = remote::link(
        Allowed {
            terminal: false,
            desktop: true,
        },
        Box::new(proof()),
        Spawners {
            terminal: unused,
            desktop,
        },
        Arc::new(|message| eprintln!("told the host: {message}")),
    );
    let handler = RemoteSessionHandler::desktop(handle.clone());
    let server = ServerHandshake::new().unwrap();
    let open = RemoteSessionOpen {
        session_id: SESSION.into(),
        kind: "desktop".into(),
        server_key: server.public_key().to_vec(),
        cols: 0,
        rows: 0,
        requested_by: "alice".into(),
        idle_timeout_seconds: 600,
        recorded: false,
    };
    let output = handler.run(&open.encode_to_vec()).await;
    assert_eq!(output.error, None, "{output:?}");
    let frame = end.up.recv().await.unwrap();
    let Some(remote_frame::Body::Opened(opened)) = frame.body else {
        panic!("the agent did not open the desktop: {frame:?}")
    };
    let mut channel = server
        .finish(SESSION, AGENT, &opened, |_| Ok(()))
        .expect("the agent proves its keys");

    // The central server's half of the tunnel: bytes of the client up as
    // sealed input, sealed output down as bytes, `delivered` as it goes.
    let (client, tunnel) = tokio::io::duplex(64 * 1024);
    let (mut from_client, mut to_client) = tokio::io::split(tunnel);
    let (input_tx, mut input) = mpsc::channel::<Vec<u8>>(16);
    tokio::spawn(async move {
        let mut buffer = vec![0u8; MAX_CHUNK];
        loop {
            match from_client.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if input_tx.send(buffer[..n].to_vec()).await.is_err() {
                        break;
                    }
                }
            }
        }
    });
    let pump = tokio::spawn(async move {
        let mut passed = 0u64;
        loop {
            tokio::select! {
                bytes = input.recv() => match bytes {
                    Some(bytes) => end.deliver(sealed(&mut channel, &sealing::input(bytes))),
                    None => break,
                },
                frame = end.up.recv() => {
                    let Some(frame) = frame else { break };
                    let Some(remote_frame::Body::Sealed(bytes)) = frame.body else {
                        panic!("the session ended: {frame:?}");
                    };
                    match channel.open(frame.counter, &bytes).unwrap().body {
                        Some(remote_data::Body::Output(output)) => {
                            if to_client.write_all(&output).await.is_err() {
                                break;
                            }
                            passed += 1;
                            if passed.is_multiple_of(DELIVERED_EVERY) {
                                end.deliver(sealed(&mut channel, &sealing::delivered(passed)));
                            }
                        }
                        Some(remote_data::Body::Close(reason)) => panic!("the agent closed: {reason}"),
                        _ => {}
                    }
                }
            }
        }
    });

    let screen = tokio::time::timeout(rdp_client::TIMEOUT, rdp_client::direct(client, &account))
        .await
        .expect("the first picture within the time")
        .expect("connected and drawn");
    eprintln!(
        "first picture: {} by {}, {} pixels lit",
        screen.width, screen.height, screen.lit
    );
    assert!(screen.width > 0 && screen.height > 0 && screen.lit > 0);
    pump.abort();
}
