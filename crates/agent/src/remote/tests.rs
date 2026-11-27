use std::sync::Mutex as StdMutex;

use monitorflow_common::proto::v1::RemoteResize;
use monitorflow_common::remote::ServerHandshake;
use rcgen::{CertificateParams, CertifiedIssuer, KeyPair, SanType};
use ring::rand::SystemRandom;
use ring::signature::{EcdsaKeyPair, ECDSA_P256_SHA256_ASN1_SIGNING};

use super::*;
use crate::commands::Handler;

const SESSION: &str = "0b5c8a52-6c43-4a6e-8f1e-9d8a3f1c2b7e";

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

fn proof(id: &str) -> TestProof {
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let ca = CertifiedIssuer::self_signed(params, KeyPair::generate().unwrap()).unwrap();
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.subject_alt_names = vec![SanType::URI(
        format!("monitorflow://agent/{id}").try_into().unwrap(),
    )];
    let key = KeyPair::generate().unwrap();
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

/// What the fake terminal saw.
#[derive(Default)]
struct Seen {
    specs: Vec<Spec>,
    resized: Vec<(u16, u16)>,
    killed: usize,
}

/// The test's end of the fake terminal.
struct Fake {
    seen: Arc<StdMutex<Seen>>,
    /// Every terminal's output, so none closes before the test says.
    output: Arc<StdMutex<Vec<mpsc::Sender<Vec<u8>>>>>,
    input: Arc<StdMutex<Option<std::sync::mpsc::Receiver<Vec<u8>>>>>,
}

struct FakeControl(Arc<StdMutex<Seen>>);

impl Control for FakeControl {
    fn resize(&self, cols: u16, rows: u16) -> std::io::Result<()> {
        self.0.lock().unwrap().resized.push((cols, rows));
        Ok(())
    }

    fn kill(&mut self) {
        self.0.lock().unwrap().killed += 1;
    }
}

fn fake() -> (Fake, Spawner) {
    let fake = Fake {
        seen: Arc::default(),
        output: Arc::default(),
        input: Arc::default(),
    };
    let (seen, output, input) = (fake.seen.clone(), fake.output.clone(), fake.input.clone());
    let spawner: Spawner = Arc::new(move |spec: &Spec| {
        seen.lock().unwrap().specs.push(spec.clone());
        let (output_tx, output_rx) = mpsc::channel(16);
        let (input_tx, input_rx) = std::sync::mpsc::channel();
        output.lock().unwrap().push(output_tx);
        *input.lock().unwrap() = Some(input_rx);
        Ok(Terminal {
            output: output_rx,
            input: input_tx,
            control: Box::new(FakeControl(seen.clone())),
        })
    });
    (fake, spawner)
}

struct Setup {
    handler: RemoteSessionHandler,
    remote: Remote,
    end: StreamEnd,
    fake: Fake,
    announced: Arc<StdMutex<Vec<String>>>,
}

fn setup(enabled: bool) -> Setup {
    with(
        Allowed {
            terminal: enabled,
            desktop: false,
        },
        Kind::Terminal,
    )
}

/// Sessions as `allowed` says, with the handler of `kind`; both kinds
/// start the fake.
fn with(allowed: Allowed, kind: Kind) -> Setup {
    let (fake, spawner) = fake();
    let announced: Arc<StdMutex<Vec<String>>> = Arc::default();
    let said = announced.clone();
    let (remote, end) = link(
        allowed,
        Box::new(proof("web-1")),
        Spawners {
            terminal: spawner.clone(),
            desktop: spawner,
        },
        Arc::new(move |m| said.lock().unwrap().push(m)),
    );
    let handler = match kind {
        Kind::Terminal => RemoteSessionHandler::new(remote.clone()),
        Kind::Desktop => RemoteSessionHandler::desktop(remote.clone()),
    };
    Setup {
        handler,
        remote,
        end,
        fake,
        announced,
    }
}

fn desktop(server: &ServerHandshake) -> RemoteSessionOpen {
    RemoteSessionOpen {
        kind: DESKTOP.into(),
        cols: 0,
        rows: 0,
        recorded: false,
        ..open(server)
    }
}

fn open(server: &ServerHandshake) -> RemoteSessionOpen {
    RemoteSessionOpen {
        session_id: SESSION.into(),
        kind: TERMINAL.into(),
        server_key: server.public_key().to_vec(),
        cols: 120,
        rows: 40,
        requested_by: "alice".into(),
        idle_timeout_seconds: 600,
        recorded: true,
    }
}

/// Opens a session and returns the server's channel.
async fn opened(setup: &mut Setup) -> Channel {
    let server = ServerHandshake::new().unwrap();
    let output = setup.handler.run(&open(&server).encode_to_vec()).await;
    assert_eq!(output.error, None, "{output:?}");
    let frame = setup.end.up.recv().await.unwrap();
    let Some(remote_frame::Body::Opened(opened)) = frame.body else {
        panic!("{frame:?}")
    };
    server
        .finish(SESSION, "web-1", &opened, |_| Ok(()))
        .unwrap()
}

fn sealed(channel: &mut Channel, data: RemoteData) -> RemoteFrame {
    let (counter, sealed) = channel.seal(&data);
    RemoteFrame {
        session_id: SESSION.into(),
        body: Some(remote_frame::Body::Sealed(sealed)),
        counter,
    }
}

/// The next sealed frame up, opened, or the closing reason.
async fn next(setup: &mut Setup, channel: &mut Channel) -> Result<RemoteData, String> {
    let frame = setup.end.up.recv().await.unwrap();
    match frame.body {
        Some(remote_frame::Body::Sealed(sealed)) => {
            Ok(channel.open(frame.counter, &sealed).unwrap())
        }
        Some(remote_frame::Body::Closed(closed)) => Err(closed.reason),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn refused_unless_the_host_allowed_it() {
    let setup = setup(false);
    assert_eq!(setup.handler.acceptance(), CommandAcceptance::Disabled);
    let server = ServerHandshake::new().unwrap();
    let output = setup.handler.run(&open(&server).encode_to_vec()).await;
    assert!(output.rejected, "{output:?}");
    assert!(output.error.unwrap().contains("remote.terminal"));
    assert!(setup.fake.seen.lock().unwrap().specs.is_empty());
    assert!(setup.announced.lock().unwrap().is_empty());
}

#[tokio::test]
async fn refuses_what_it_cannot_open() {
    let setup = setup(true);
    assert_eq!(setup.handler.acceptance(), CommandAcceptance::Accepted);
    let server = ServerHandshake::new().unwrap();
    type Change = fn(&mut RemoteSessionOpen);
    let cases: [(&str, Change); 5] = [
        ("kind", |o| o.kind = "desktop".into()),
        ("ID", |o| o.session_id = "../x".into()),
        ("size", |o| o.cols = 0),
        ("size", |o| o.rows = 100_000),
        ("key", |o| o.server_key = vec![1; 3]),
    ];
    for (what, change) in cases {
        let mut request = open(&server);
        change(&mut request);
        let output = setup.handler.run(&request.encode_to_vec()).await;
        assert!(output.rejected, "{what}: {output:?}");
    }
    assert!(setup.handler.run(b"\xff").await.rejected);
    assert!(setup.fake.seen.lock().unwrap().specs.is_empty());
}

#[tokio::test]
async fn a_session_carries_keys_resizes_and_output_sealed() {
    let mut setup = setup(true);
    let mut channel = opened(&mut setup).await;
    assert_eq!(setup.remote.open_sessions(), 1);
    let spec = Spec {
        request: Vec::new(),
        ..setup.fake.seen.lock().unwrap().specs[0].clone()
    };
    assert_eq!(
        spec,
        Spec {
            session_id: SESSION.into(),
            requested_by: "alice".into(),
            cols: 120,
            rows: 40,
            request: Vec::new(),
        }
    );
    // The signed command, framed for the root helper.
    let request = setup.fake.seen.lock().unwrap().specs[0].request.clone();
    let framed = crate::helper_trust::HelperRequest::read(&mut request.as_slice()).unwrap();
    assert!(framed.command.is_some());
    let said = setup.announced.lock().unwrap().clone();
    assert_eq!(said.len(), 1);
    assert!(
        said[0].contains("alice") && said[0].contains("recorded"),
        "{said:?}"
    );

    setup
        .end
        .deliver(sealed(&mut channel, sealing::input(b"uptime\r".to_vec())));
    let input = setup.fake.input.lock().unwrap().take().unwrap();
    let typed = tokio::task::spawn_blocking(move || input.recv().unwrap())
        .await
        .unwrap();
    assert_eq!(typed, b"uptime\r");

    setup.end.deliver(sealed(
        &mut channel,
        RemoteData {
            body: Some(remote_data::Body::Resize(RemoteResize {
                cols: 90,
                rows: 30,
            })),
        },
    ));
    let printed = setup.fake.output.lock().unwrap()[0].clone();
    printed.send(b" 10:00 up 3 days\n".to_vec()).await.unwrap();
    assert_eq!(
        next(&mut setup, &mut channel).await.unwrap(),
        sealing::output(b" 10:00 up 3 days\n".to_vec())
    );
    assert_eq!(setup.fake.seen.lock().unwrap().resized, [(90, 30)]);

    // Closed from the web interface.
    setup
        .end
        .deliver(sealed(&mut channel, sealing::close("closed by alice")));
    assert_eq!(
        next(&mut setup, &mut channel).await.unwrap_err(),
        "closed by alice"
    );
    assert_eq!(setup.remote.open_sessions(), 0);
    assert_eq!(setup.fake.seen.lock().unwrap().killed, 1);
    let said = setup.announced.lock().unwrap().clone();
    assert!(said[1].contains("closed"), "{said:?}");
}

#[tokio::test]
async fn a_forged_or_replayed_frame_ends_the_session() {
    let mut setup = setup(true);
    let mut channel = opened(&mut setup).await;
    let frame = sealed(&mut channel, sealing::input(b"id\r".to_vec()));
    setup.end.deliver(frame.clone());
    setup.end.deliver(frame);
    let reason = next(&mut setup, &mut channel).await.unwrap_err();
    assert!(reason.contains("out of order"), "{reason}");
    assert_eq!(setup.remote.open_sessions(), 0);
}

#[tokio::test]
async fn the_session_ends_with_the_stream_or_the_shell() {
    let mut setup = setup(true);
    let mut channel = opened(&mut setup).await;
    setup.end.disconnected();
    let reason = next(&mut setup, &mut channel).await.unwrap_err();
    assert!(reason.contains("connection"), "{reason}");

    let mut setup = self::setup(true);
    let mut channel = opened(&mut setup).await;
    // The shell exits: its output closes.
    setup.fake.output.lock().unwrap().pop();
    assert_eq!(
        next(&mut setup, &mut channel).await.unwrap(),
        sealing::close("the shell exited")
    );
    assert_eq!(
        next(&mut setup, &mut channel).await.unwrap_err(),
        "the shell exited"
    );
}

#[tokio::test(start_paused = true)]
async fn the_session_closes_when_idle() {
    let mut setup = setup(true);
    let mut channel = opened(&mut setup).await;
    // 600 seconds without input.
    let closing = next(&mut setup, &mut channel).await.unwrap();
    assert_eq!(
        closing,
        sealing::close("closed after 10 minutes without input")
    );
    assert!(next(&mut setup, &mut channel).await.is_err());
}

#[tokio::test]
async fn sessions_are_bounded_and_never_opened_twice() {
    let mut setup = setup(true);
    opened(&mut setup).await;
    let server = ServerHandshake::new().unwrap();
    let output = setup.handler.run(&open(&server).encode_to_vec()).await;
    assert!(output.rejected, "{output:?}");
    assert!(output.error.unwrap().contains("already open"));
    for i in 1..MAX_SESSIONS {
        let mut request = open(&server);
        request.session_id = format!("0b5c8a52-6c43-4a6e-8f1e-9d8a3f1c2b7{i}");
        let output = setup.handler.run(&request.encode_to_vec()).await;
        assert_eq!(output.error, None, "{output:?}");
    }
    let mut request = open(&server);
    request.session_id = "0b5c8a52-6c43-4a6e-8f1e-9d8a3f1c2b70".into();
    let output = setup.handler.run(&request.encode_to_vec()).await;
    assert!(!output.rejected && output.exit_code == 1, "{output:?}");
    assert_eq!(setup.remote.open_sessions(), MAX_SESSIONS);
}

#[cfg(unix)]
#[tokio::test]
async fn a_real_shell_runs_on_a_pseudo_terminal() {
    let spec = Spec {
        session_id: SESSION.into(),
        requested_by: "alice".into(),
        cols: 100,
        rows: 30,
        request: Vec::new(),
    };
    let mut command = std::process::Command::new("/bin/sh");
    command.arg("-i");
    let mut terminal = pty_unix::spawn_command(command, &spec, None).unwrap();
    terminal
        .input
        .send(b"stty size; echo \"$MONITORFLOW_REMOTE_USER-$((40+2))\"; [ -z \"$MONITORFLOW_ENROLLMENT_TOKEN\" ] && echo clean\n".to_vec())
        .unwrap();
    let mut seen = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !(seen.contains("alice-42") && seen.contains("clean") && seen.contains("30 100")) {
        let chunk = tokio::time::timeout_at(deadline, terminal.output.recv())
            .await
            .expect("the shell answers")
            .expect("the shell runs");
        seen.push_str(&String::from_utf8_lossy(&chunk));
    }
    terminal.control.resize(80, 24).unwrap();
    terminal.input.send(b"stty size\n".to_vec()).unwrap();
    while !seen.contains("24 80") {
        let chunk = tokio::time::timeout_at(deadline, terminal.output.recv())
            .await
            .expect("the shell answers")
            .expect("the shell runs");
        seen.push_str(&String::from_utf8_lossy(&chunk));
    }
    terminal.control.kill();
    // Once the shell is gone, its output closes.
    while tokio::time::timeout_at(deadline, terminal.output.recv())
        .await
        .expect("the output closes")
        .is_some()
    {}
}

#[cfg(unix)]
#[tokio::test]
async fn a_helper_gets_the_request_on_a_pipe_and_the_terminal_on_stdout() {
    let spec = Spec {
        session_id: SESSION.into(),
        requested_by: "alice".into(),
        cols: 90,
        rows: 20,
        request: Vec::new(),
    };
    // What `remote-shell` does: read the request to its end, then take the
    // terminal of stdout as stdin.
    let mut command = std::process::Command::new("/bin/sh");
    command.args([
        "-c",
        "req=$(od -An -tx1 | tr -d ' \\n'); exec 0<&1; echo \"got:$req\"; stty size; exec /bin/sh -i",
    ]);
    let mut terminal = pty_unix::spawn_command(command, &spec, Some(b"\x00\x01ok")).unwrap();
    let mut seen = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !(seen.contains("got:00016f6b") && seen.contains("20 90")) {
        let chunk = tokio::time::timeout_at(deadline, terminal.output.recv())
            .await
            .expect("the helper answers")
            .expect("the helper runs");
        seen.push_str(&String::from_utf8_lossy(&chunk));
    }
    terminal
        .input
        .send(b"echo typed-$((1+1))\n".to_vec())
        .unwrap();
    while !seen.contains("typed-2") {
        let chunk = tokio::time::timeout_at(deadline, terminal.output.recv())
            .await
            .expect("the shell answers")
            .expect("the shell runs");
        seen.push_str(&String::from_utf8_lossy(&chunk));
    }
    terminal.control.kill();
}

#[tokio::test]
async fn a_desktop_needs_its_own_consent_and_its_own_command() {
    let terminal_only = Allowed {
        terminal: true,
        desktop: false,
    };
    let setup = with(terminal_only, Kind::Desktop);
    assert_eq!(setup.handler.acceptance(), CommandAcceptance::Disabled);
    let server = ServerHandshake::new().unwrap();
    let output = setup.handler.run(&desktop(&server).encode_to_vec()).await;
    assert!(output.rejected, "{output:?}");
    assert!(output.error.unwrap().contains("remote.desktop"));

    // A desktop's consent opens no terminal, and a terminal's command no
    // desktop: the kind is signed with the command.
    let desktop_only = Allowed {
        terminal: false,
        desktop: true,
    };
    let setup = with(desktop_only, Kind::Terminal);
    assert_eq!(setup.handler.acceptance(), CommandAcceptance::Disabled);
    assert!(
        setup
            .handler
            .run(&open(&server).encode_to_vec())
            .await
            .rejected
    );
    let both = Allowed {
        terminal: true,
        desktop: true,
    };
    let setup = with(both, Kind::Desktop);
    assert_eq!(setup.handler.acceptance(), CommandAcceptance::Accepted);
    let output = setup.handler.run(&open(&server).encode_to_vec()).await;
    assert!(output.rejected, "{output:?}");
    let setup = with(both, Kind::Terminal);
    let output = setup.handler.run(&desktop(&server).encode_to_vec()).await;
    assert!(output.rejected, "{output:?}");
    assert!(setup.fake.seen.lock().unwrap().specs.is_empty());
}

#[tokio::test]
async fn a_desktop_waits_for_the_server_once_its_window_is_full() {
    let mut setup = with(
        Allowed {
            terminal: false,
            desktop: true,
        },
        Kind::Desktop,
    );
    let server = ServerHandshake::new().unwrap();
    let output = setup.handler.run(&desktop(&server).encode_to_vec()).await;
    assert_eq!(output.error, None, "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("remote desktop"));
    let frame = setup.end.up.recv().await.unwrap();
    let Some(remote_frame::Body::Opened(opened)) = frame.body else {
        panic!("{frame:?}")
    };
    let mut channel = server
        .finish(SESSION, "web-1", &opened, |_| Ok(()))
        .unwrap();
    let said = setup.announced.lock().unwrap().clone();
    assert!(said[0].contains("remote desktop") && !said[0].contains("recorded"));

    // The service sends more than the window: the agent stops at it.
    let service = setup.fake.output.lock().unwrap()[0].clone();
    let total = DESKTOP_WINDOW + 5;
    tokio::spawn(async move {
        for i in 0..total {
            service.send(vec![i as u8]).await.unwrap();
        }
    });
    for i in 0..DESKTOP_WINDOW {
        assert_eq!(
            next(&mut setup, &mut channel).await.unwrap(),
            sealing::output(vec![i as u8])
        );
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(setup.end.up.try_recv().is_err(), "sent beyond the window");
    // A `delivered` beyond what was sent counts for what was sent.
    setup
        .end
        .deliver(sealed(&mut channel, sealing::delivered(u64::MAX)));
    for i in DESKTOP_WINDOW..total {
        assert_eq!(
            next(&mut setup, &mut channel).await.unwrap(),
            sealing::output(vec![i as u8])
        );
    }

    // The service closes its side.
    setup.fake.output.lock().unwrap().clear();
    assert_eq!(
        next(&mut setup, &mut channel).await.unwrap(),
        sealing::close("the remote desktop service closed the connection")
    );
}
