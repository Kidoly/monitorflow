//! Remote terminals and desktops end to end (ADR-0190): the server's hub
//! and the agent's sessions over a real mTLS stream, with PostgreSQL, a
//! real shell on a pseudo-terminal, and a fake RDP service behind the
//! central server's RDCleanPath proxy.

// The real server only builds on Unix (ADR-0047).
#![cfg(unix)]

mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use monitorflow_agent::backoff::Backoff;
use monitorflow_agent::commands::{self, Dispatcher, DispatcherSettings, Handlers, Journal};
use monitorflow_agent::os::{HostInfo, OsFamily};
use monitorflow_agent::remote::{self, Allowed, RemoteSessionHandler, Spawners};
use monitorflow_agent::shutdown::{self, Trigger};
use monitorflow_agent::stream::{Agent, StreamSettings};
use monitorflow_common::command::{REMOTE_DESKTOP, REMOTE_SESSION};
use monitorflow_common::tls;
use monitorflow_server::commands::{CommandStore, Commands, DispatcherSettings as ServerDispatch};
use monitorflow_server::grpc::{self, AgentStreamService, DbInventory, DbRelayPolicy};
use monitorflow_server::remote::desktop::{
    self,
    ironrdp_rdcleanpath::{RDCleanPath, RDCleanPathPdu},
};
use monitorflow_server::remote::rdp::sample;
use monitorflow_server::remote::{token_hash, FromBrowser, RemoteHub, ToBrowser};
use sqlx::PgPool;
use support::{agent_config, record, Pki};
use tempfile::TempDir;
use tokio::sync::{mpsc, watch};
use uuid::Uuid;

const TIMEOUT: Duration = Duration::from_secs(15);
const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

struct Setup {
    _pki: Pki,
    _data: TempDir,
    pool: PgPool,
    hub: RemoteHub,
    server_id: Uuid,
    remote: remote::Remote,
    announced: Arc<Mutex<Vec<String>>>,
    trigger: Trigger,
}

impl Setup {
    async fn start(pool: &PgPool, allowed: bool) -> Self {
        let allowed = Allowed {
            terminal: allowed,
            desktop: false,
        };
        let unused = remote::desktop::connector("127.0.0.1:9".parse().unwrap());
        Self::start_with(pool, allowed, unused).await
    }

    /// The agent with `allowed`, reaching its RDP service with `desktop`.
    async fn start_with(pool: &PgPool, allowed: Allowed, desktop: remote::Spawner) -> Self {
        let site: Uuid =
            sqlx::query_scalar("INSERT INTO sites (name) VALUES ('paris') RETURNING id")
                .fetch_one(pool)
                .await
                .unwrap();
        let server_id: Uuid = sqlx::query_scalar(
            "INSERT INTO servers (peer_id, site_id, hostname, os_family)
             VALUES ('web-01', $1, 'web-01', 'debian') RETURNING id",
        )
        .bind(site)
        .fetch_one(pool)
        .await
        .unwrap();
        let pki = Pki::new();
        let ca = pki.ca.clone();
        let hub_slot: Arc<Mutex<Option<RemoteHub>>> = Arc::default();
        let slot = hub_slot.clone();
        let upstream = pki
            .serve_service(|registry| {
                let settings = ServerDispatch {
                    poll_interval: Duration::from_millis(100),
                    ..ServerDispatch::default()
                };
                let commands = Commands::spawn(
                    CommandStore::new(pool.clone()),
                    registry.clone(),
                    ca.clone(),
                    settings,
                );
                let hub =
                    RemoteHub::new(pool.clone(), registry.clone(), ca, Some(commands.clone()));
                *slot.lock().unwrap() = Some(hub.clone());
                AgentStreamService::new(
                    registry,
                    Arc::new(DbRelayPolicy::new(pool.clone())),
                    grpc::StreamSettings {
                        heartbeat_timeout: TIMEOUT,
                        ..grpc::StreamSettings::default()
                    },
                )
                .with_inventory(Arc::new(DbInventory::new(pool.clone())))
                .with_commands(commands)
                .with_remote(hub)
            })
            .await;
        let hub = hub_slot.lock().unwrap().clone().unwrap();
        let data = TempDir::new().unwrap();
        let config = agent_config(data.path().into(), &upstream.url, "https://localhost:1");
        let (identity, issued) = pki.agent_identity_valid_for(&config, "web-01", None);
        record(pool, "web-01", &issued).await;

        let announced: Arc<Mutex<Vec<String>>> = Arc::default();
        let said = announced.clone();
        let (_, proof) = watch::channel(identity.clone());
        let (remote, remote_end) = remote::link(
            allowed,
            Box::new(proof),
            Spawners {
                terminal: remote::platform_spawner(None),
                desktop,
            },
            Arc::new(move |m| said.lock().unwrap().push(m)),
        );
        let mut handlers = Handlers::default();
        handlers.register(
            REMOTE_SESSION,
            Arc::new(RemoteSessionHandler::new(remote.clone())),
        );
        handlers.register(
            REMOTE_DESKTOP,
            Arc::new(RemoteSessionHandler::desktop(remote.clone())),
        );
        let dispatcher = Dispatcher::new(
            "web-01",
            tls::load_certs(&config.tls_paths().ca_cert).unwrap(),
            handlers,
            Journal::in_dir(data.path()),
            DispatcherSettings::default(),
        );
        let (stream_end, dispatcher_end) = commands::link();
        let (trigger, stop) = shutdown::channel();
        tokio::spawn(dispatcher.run(dispatcher_end, stop.clone()));
        let settings = StreamSettings {
            heartbeat_interval: Duration::from_millis(200),
            backoff: Backoff::new(Duration::from_millis(20), Duration::from_millis(200)),
            ..StreamSettings::from_config(&config)
        };
        let host = HostInfo {
            hostname: "web-01".into(),
            os_family: OsFamily::Debian,
            os_version: "debian 12".into(),
            package_manager: true,
            hardware: Default::default(),
        };
        let agent = Agent::new(settings, identity, host)
            .with_commands(stream_end)
            .with_remote(remote_end);
        tokio::spawn(async move { agent.run(stop).await.unwrap() });
        Self {
            _pki: pki,
            _data: data,
            pool: pool.clone(),
            hub,
            server_id,
            remote,
            announced,
            trigger,
        }
    }

    /// A session asked by alice, as `POST /api/servers/{id}/remote-sessions` stores it.
    async fn session(&self) -> Uuid {
        self.session_of("terminal").await
    }

    async fn session_of(&self, kind: &str) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO remote_sessions (id, server_id, kind, username, token_sha256,
                 token_expires_at, recorded, idle_timeout_seconds)
             VALUES ($1, $2, $4, 'alice', $3, now() + interval '1 minute', $4 = 'terminal', 900)",
        )
        .bind(id)
        .bind(self.server_id)
        .bind(token_hash(TOKEN))
        .bind(kind)
        .execute(&self.pool)
        .await
        .unwrap();
        id
    }

    /// Runs the session as a browser would; returns its two ends.
    async fn run(
        &self,
        id: Uuid,
    ) -> (
        mpsc::Sender<FromBrowser>,
        mpsc::Receiver<ToBrowser>,
        tokio::task::JoinHandle<String>,
    ) {
        let attached = self.hub.attach(id, TOKEN, 100, 30).await.unwrap();
        let (from_tx, from_rx) = mpsc::channel(16);
        let (to_tx, to_rx) = mpsc::channel(64);
        let hub = self.hub.clone();
        let task = tokio::spawn(async move { hub.run(attached, from_rx, to_tx).await });
        (from_tx, to_rx, task)
    }
}

impl Drop for Setup {
    fn drop(&mut self) {
        self.trigger.fire();
    }
}

async fn next(to: &mut mpsc::Receiver<ToBrowser>) -> ToBrowser {
    tokio::time::timeout(TIMEOUT, to.recv())
        .await
        .expect("the session answers")
        .expect("the session runs")
}

db_tests! {
    #[sqlx::test(migrations = "../server/migrations")]
    async fn a_terminal_runs_through_the_stream_sealed_audited_and_recorded(pool: PgPool) {
        let s = Setup::start(&pool, true).await;
        let id = s.session().await;
        let (from, mut to, task) = s.run(id).await;
        assert_eq!(next(&mut to).await, ToBrowser::Waiting);
        assert_eq!(next(&mut to).await, ToBrowser::Opened);
        assert_eq!(s.remote.open_sessions(), 1);
        assert_eq!(s.hub.local_sessions(), 1);

        from.send(FromBrowser::Input(b"echo mf-$((40+2)) $MONITORFLOW_REMOTE_USER\r".to_vec()))
            .await
            .unwrap();
        let mut seen = String::new();
        while !seen.contains("mf-42 alice") {
            match next(&mut to).await {
                ToBrowser::Output(bytes) => seen.push_str(&String::from_utf8_lossy(&bytes)),
                other => panic!("{other:?} before the output; seen {seen:?}"),
            }
        }
        from.send(FromBrowser::Close).await.unwrap();
        let reason = tokio::time::timeout(TIMEOUT, task).await.unwrap().unwrap();
        assert_eq!(reason, "closed by alice");

        let row: (Option<chrono::DateTime<chrono::Utc>>, Option<String>, i64, i64) = sqlx::query_as(
            "SELECT opened_at, close_reason, bytes_in, bytes_out FROM remote_sessions WHERE id = $1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(row.0.is_some());
        assert_eq!(row.1.as_deref(), Some("closed by alice"));
        assert!(row.2 > 0 && row.3 > 0, "{row:?}");
        let recorded: Vec<Vec<u8>> =
            sqlx::query_scalar("SELECT output FROM remote_session_recordings WHERE session_id = $1 ORDER BY seq")
                .bind(id)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert!(String::from_utf8_lossy(&recorded.concat()).contains("mf-42 alice"));
        let actions: Vec<String> = sqlx::query_scalar(
            "SELECT action FROM audit_log WHERE action LIKE 'remote_session.%' ORDER BY id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(actions, ["remote_session.open", "remote_session.close"]);
        let command: String = sqlx::query_scalar(
            "SELECT c.state FROM commands c JOIN remote_sessions r ON r.command_id = c.id WHERE r.id = $1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(command, "succeeded");

        // The agent ends its side too, and says so on the host.
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        while s.remote.open_sessions() > 0 {
            assert!(tokio::time::Instant::now() < deadline, "the agent kept the session");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(s.hub.local_sessions(), 0);
        let said = s.announced.lock().unwrap().clone();
        assert_eq!(said.len(), 2, "{said:?}");
        assert!(said[0].contains("alice") && said[0].contains("recorded"), "{said:?}");
    }

    #[sqlx::test(migrations = "../server/migrations")]
    async fn a_host_that_did_not_allow_terminals_refuses_and_the_browser_hears_why(pool: PgPool) {
        let s = Setup::start(&pool, false).await;
        let id = s.session().await;
        let (_from, mut to, task) = s.run(id).await;
        assert_eq!(next(&mut to).await, ToBrowser::Waiting);
        let reason = tokio::time::timeout(TIMEOUT, task).await.unwrap().unwrap();
        assert!(reason.contains("remote.terminal"), "{reason}");
        assert_eq!(next(&mut to).await, ToBrowser::Closed(reason));
        assert_eq!(s.remote.open_sessions(), 0);
        assert!(s.announced.lock().unwrap().is_empty());
    }
}

mod rdp {
    //! A fake RDP service: it answers the X.224 request choosing TLS, does
    //! TLS with a certificate of its own, as Windows does, then reads the
    //! client's first PDU and answers with the PDUs it was given.

    use std::net::SocketAddr;
    use std::sync::Arc;

    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::oneshot;

    /// An X.224 connection request asking for TLS and CredSSP, as IronRDP
    /// sends it.
    pub const REQUEST: [u8; 19] = [3, 0, 0, 19, 14, 0xe0, 0, 0, 0, 0, 0, 1, 0, 8, 0, 3, 0, 0, 0];
    /// The service's confirm: TLS.
    pub const CONFIRM: [u8; 19] = [
        3, 0, 0, 19, 14, 0xd0, 0, 0, 0x12, 0x34, 0, 2, 0, 8, 0, 1, 0, 0, 0,
    ];

    pub struct Service {
        pub address: SocketAddr,
        pub certificate: Vec<u8>,
        /// The X.224 request and the client's first PDU through TLS.
        pub seen: oneshot::Receiver<(Vec<u8>, Vec<u8>)>,
    }

    /// Fast-path PDUs of `total` bytes in all, which no recorder decodes:
    /// for a desktop that is not recorded.
    pub fn burst(total: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(total);
        while out.len() < total {
            let length = (total - out.len()).clamp(4, 0x4000);
            out.extend_from_slice(&[0, 0x80 | (length >> 8) as u8, length as u8]);
            out.extend((3..length).map(|n| (n % 251) as u8));
        }
        out
    }

    /// Answers the client's first PDU with `answer`, then keeps the
    /// connection open.
    pub async fn start(answer: Vec<u8>) -> Service {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = rcgen::CertificateParams::new(vec!["web-01".to_owned()])
            .unwrap()
            .self_signed(&key)
            .unwrap();
        let certificate = cert.der().to_vec();
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(certificate.clone())],
            PrivateKeyDer::Pkcs8(key.serialize_der().into()),
        )
        .unwrap();
        let (seen_tx, seen) = oneshot::channel();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0u8; REQUEST.len()];
            socket.read_exact(&mut request).await.unwrap();
            socket.write_all(&CONFIRM).await.unwrap();
            let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
            let mut tls = acceptor.accept(socket).await.unwrap();
            // One TPKT: its header gives its length.
            let mut first = vec![0u8; 4];
            tls.read_exact(&mut first).await.unwrap();
            first.resize(usize::from(u16::from_be_bytes([first[2], first[3]])), 0);
            tls.read_exact(&mut first[4..]).await.unwrap();
            let _ = seen_tx.send((request, first));
            tls.write_all(&answer).await.unwrap();
            tls.flush().await.unwrap();
            // Until the client goes.
            let mut rest = Vec::new();
            let _ = tls.read_to_end(&mut rest).await;
        });
        Service {
            address,
            certificate,
            seen,
        }
    }
}

/// The browser's side of a desktop: its RDCleanPath request, then RDP in
/// the clear.
struct Browser {
    to_server: mpsc::Sender<Vec<u8>>,
    from_server: mpsc::Receiver<Vec<u8>>,
    task: tokio::task::JoinHandle<Result<String, desktop::DesktopError>>,
}

impl Setup {
    fn desktop(&self, id: Uuid) -> Browser {
        let first =
            RDCleanPathPdu::new_request(rdp::REQUEST.to_vec(), "web-01".into(), TOKEN.into(), None)
                .unwrap()
                .to_der()
                .unwrap();
        let (to_server, from_browser) = mpsc::channel(64);
        let (to_browser, from_server) = mpsc::channel(64);
        let hub = self.hub.clone();
        let task =
            tokio::spawn(
                async move { desktop::run(hub, id, &first, from_browser, to_browser).await },
            );
        Browser {
            to_server,
            from_server,
            task,
        }
    }
}

async fn received(browser: &mut Browser) -> Vec<u8> {
    tokio::time::timeout(TIMEOUT, browser.from_server.recv())
        .await
        .expect("the proxy answers")
        .expect("the proxy runs")
}

db_tests! {
    #[sqlx::test(migrations = "../server/migrations")]
    async fn a_desktop_reaches_the_rdp_service_through_the_tunnel_at_the_browsers_pace(pool: PgPool) {
        // More than the agent's window and than every buffer on the way.
        const BURST: usize = 4 * 1024 * 1024;
        let burst = rdp::burst(BURST);
        let service = rdp::start(burst.clone()).await;
        let allowed = Allowed { terminal: false, desktop: true };
        let s = Setup::start_with(&pool, allowed, remote::desktop::connector(service.address)).await;
        let id = s.session_of("desktop").await;
        // A terminal's hello cannot take a desktop's token.
        assert!(s.hub.attach(id, TOKEN, 80, 24).await.is_err());
        let mut browser = s.desktop(id);

        let answer = RDCleanPathPdu::from_der(&received(&mut browser).await).unwrap();
        let RDCleanPath::Response { x224_connection_response, server_cert_chain, server_addr } =
            answer.into_enum().unwrap()
        else {
            panic!("not a response");
        };
        assert_eq!(x224_connection_response.as_bytes(), rdp::CONFIRM);
        assert_eq!(server_cert_chain[0].as_bytes(), service.certificate.as_slice());
        assert_eq!(server_addr, "127.0.0.1:3389");

        // The proxy passes whole PDUs: this one in two halves.
        let initial = sample::connect_initial(&["cliprdr"], false);
        let (half, rest) = initial.split_at(10);
        browser.to_server.send(half.to_vec()).await.unwrap();
        browser.to_server.send(rest.to_vec()).await.unwrap();
        let (request, first) = tokio::time::timeout(TIMEOUT, service.seen).await.unwrap().unwrap();
        assert_eq!(request, rdp::REQUEST);
        assert_eq!(first, initial);
        // A slow browser: the agent waits for it instead of flooding.
        tokio::time::sleep(Duration::from_secs(2)).await;
        let mut got = Vec::with_capacity(BURST);
        while got.len() < BURST {
            got.extend(received(&mut browser).await);
        }
        assert!(got == burst, "the browser got other bytes than the service sent");

        drop(browser.to_server);
        let reason = tokio::time::timeout(TIMEOUT, browser.task).await.unwrap().unwrap().unwrap();
        assert_eq!(reason, "closed by alice");
        let row: (String, bool, Option<String>, i64, i64) = sqlx::query_as(
            "SELECT kind, recorded, close_reason, bytes_in, bytes_out FROM remote_sessions WHERE id = $1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!((row.0.as_str(), row.1, row.2.as_deref()), ("desktop", false, Some("closed by alice")));
        assert!(row.3 > 0 && row.4 > BURST as i64, "{row:?}");
        let recorded: i64 = sqlx::query_scalar("SELECT count(*) FROM remote_session_recordings")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(recorded, 0);
        let audit: Vec<(String, String)> = sqlx::query_as(
            "SELECT action, details->>'kind' FROM audit_log WHERE action LIKE 'remote_session.%' ORDER BY id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(audit, [("remote_session.open".into(), "desktop".into()),
                           ("remote_session.close".into(), "desktop".into())]);
        let command: (String, String) = sqlx::query_as(
            "SELECT c.kind, c.state FROM commands c JOIN remote_sessions r ON r.command_id = c.id WHERE r.id = $1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(command, ("remote_desktop".into(), "succeeded".into()));

        let deadline = tokio::time::Instant::now() + TIMEOUT;
        while s.remote.open_sessions() > 0 {
            assert!(tokio::time::Instant::now() < deadline, "the agent kept the session");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let said = s.announced.lock().unwrap().clone();
        assert_eq!(said.len(), 2, "{said:?}");
        assert!(said[0].contains("alice opened a remote desktop") && !said[0].contains("recorded"), "{said:?}");
    }

    #[sqlx::test(migrations = "../server/migrations")]
    async fn a_recorded_desktop_keeps_pictures_of_its_screen(pool: PgPool) {
        let mut answer = sample::connect_response(1);
        answer.extend(sample::demand_active(64, 48));
        answer.extend(sample::bitmap(4, 8, 2, 2, [200, 100, 50]));
        let service = rdp::start(answer.clone()).await;
        let allowed = Allowed { terminal: false, desktop: true };
        let s = Setup::start_with(&pool, allowed, remote::desktop::connector(service.address)).await;
        let id = s.session_of("desktop").await;
        sqlx::query("UPDATE remote_sessions SET recorded = true WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        let mut browser = s.desktop(id);
        received(&mut browser).await;
        browser.to_server.send(sample::connect_initial(&["cliprdr"], false)).await.unwrap();
        let mut got = Vec::new();
        while got.len() < answer.len() {
            got.extend(received(&mut browser).await);
        }
        assert!(got == answer, "the browser got other bytes than the service sent");
        drop(browser.to_server);
        let reason = tokio::time::timeout(TIMEOUT, browser.task).await.unwrap().unwrap().unwrap();
        assert_eq!(reason, "closed by alice");
        // The whole screen first; what the service painted then, in the
        // same picture or, a moment after, in one of its own.
        let frames: Vec<(i32, i32, i32, i32, i32, Vec<u8>)> = sqlx::query_as(
            "SELECT x, y, width, height, screen_width, image FROM remote_desktop_frames
             WHERE session_id = $1 ORDER BY seq",
        )
        .bind(id)
        .fetch_all(&pool)
        .await
        .unwrap();
        let parts: Vec<_> = frames.iter().map(|f| (f.0, f.1, f.2, f.3, f.4)).collect();
        assert!(
            parts == [(0, 0, 64, 48, 64)] || parts == [(0, 0, 64, 48, 64), (4, 8, 2, 2, 64)],
            "{parts:?}"
        );
        assert!(frames.iter().all(|f| f.5.starts_with(b"\x89PNG")));
    }

    #[sqlx::test(migrations = "../server/migrations")]
    async fn a_recorded_desktop_refuses_a_screen_it_cannot_read(pool: PgPool) {
        let service = rdp::start(sample::connect_response(0)).await;
        let allowed = Allowed { terminal: false, desktop: true };
        let s = Setup::start_with(&pool, allowed, remote::desktop::connector(service.address)).await;
        let id = s.session_of("desktop").await;
        sqlx::query("UPDATE remote_sessions SET recorded = true WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        let mut browser = s.desktop(id);
        received(&mut browser).await;
        // The graphics pipeline would bring the screen in a form the
        // recording cannot read.
        browser.to_server.send(sample::connect_initial(&[], true)).await.unwrap();
        let reason = tokio::time::timeout(TIMEOUT, browser.task).await.unwrap().unwrap().unwrap();
        assert!(reason.contains("graphics mode the recording cannot read"), "{reason}");
        let close: String = sqlx::query_scalar("SELECT close_reason FROM remote_sessions WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(close, reason);
    }

    #[sqlx::test(migrations = "../server/migrations")]
    async fn a_desktop_needs_the_hosts_consent_and_its_token(pool: PgPool) {
        // Terminals allowed, desktops not: the browser gets an RDCleanPath error.
        let service = rdp::start(Vec::new()).await;
        let terminal_only = Allowed { terminal: true, desktop: false };
        let s = Setup::start_with(&pool, terminal_only, remote::desktop::connector(service.address)).await;
        let id = s.session_of("desktop").await;
        let mut browser = s.desktop(id);
        let answer = RDCleanPathPdu::from_der(&received(&mut browser).await).unwrap();
        assert!(answer.error.is_some(), "{answer:?}");
        let reason = tokio::time::timeout(TIMEOUT, browser.task).await.unwrap().unwrap().unwrap();
        assert!(reason.contains("remote.desktop"), "{reason}");
        assert!(s.announced.lock().unwrap().is_empty());

        // A wrong token: refused before anything runs.
        let id = s.session_of("desktop").await;
        sqlx::query("UPDATE remote_sessions SET token_sha256 = sha256('other'::bytea) WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        let mut browser = s.desktop(id);
        let answer = RDCleanPathPdu::from_der(&received(&mut browser).await).unwrap();
        assert_eq!(answer.error.unwrap().http_status_code, Some(401));
        assert!(tokio::time::timeout(TIMEOUT, browser.task).await.unwrap().unwrap().is_err());
    }

    #[sqlx::test(migrations = "../server/migrations")]
    async fn a_desktop_ends_with_why_when_no_service_listens(pool: PgPool) {
        // Allowed, but nothing listens where the agent looks.
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let nowhere = closed.local_addr().unwrap();
        drop(closed);
        let allowed = Allowed { terminal: false, desktop: true };
        let s = Setup::start_with(&pool, allowed, remote::desktop::connector(nowhere)).await;
        let id = s.session_of("desktop").await;
        let mut browser = s.desktop(id);
        let answer = RDCleanPathPdu::from_der(&received(&mut browser).await).unwrap();
        assert!(answer.error.is_some(), "{answer:?}");
        let reason = tokio::time::timeout(TIMEOUT, browser.task).await.unwrap().unwrap().unwrap();
        assert!(reason.contains("cannot reach the remote desktop service"), "{reason}");
    }
}
