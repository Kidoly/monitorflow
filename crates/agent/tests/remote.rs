//! Remote terminals end to end (ADR-0190): the server's hub and the
//! agent's sessions over a real mTLS stream, with PostgreSQL and a real
//! shell on a pseudo-terminal.

// The real server only builds on Unix (ADR-0047).
#![cfg(unix)]

mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use monitorflow_agent::backoff::Backoff;
use monitorflow_agent::commands::{self, Dispatcher, DispatcherSettings, Handlers, Journal};
use monitorflow_agent::os::{HostInfo, OsFamily};
use monitorflow_agent::remote::{self, RemoteSessionHandler};
use monitorflow_agent::shutdown::{self, Trigger};
use monitorflow_agent::stream::{Agent, StreamSettings};
use monitorflow_common::command::REMOTE_SESSION;
use monitorflow_common::tls;
use monitorflow_server::commands::{CommandStore, Commands, DispatcherSettings as ServerDispatch};
use monitorflow_server::grpc::{self, AgentStreamService, DbInventory, DbRelayPolicy};
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
            remote::platform_spawner(None),
            Arc::new(move |m| said.lock().unwrap().push(m)),
        );
        let mut handlers = Handlers::default();
        handlers.register(
            REMOTE_SESSION,
            Arc::new(RemoteSessionHandler::new(remote.clone())),
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
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO remote_sessions (id, server_id, kind, username, token_sha256,
                 token_expires_at, recorded, idle_timeout_seconds)
             VALUES ($1, $2, 'terminal', 'alice', $3, now() + interval '1 minute', true, 900)",
        )
        .bind(id)
        .bind(self.server_id)
        .bind(token_hash(TOKEN))
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
