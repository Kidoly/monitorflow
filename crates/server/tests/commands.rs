//! Commands (ADR-0031): the `commands` table's transitions, and dispatch
//! on agents' streams over real mTLS.

#[path = "../../common/tests/support/mod.rs"]
mod support;

use std::sync::Arc;
use std::time::Duration;

use monitorflow_common::command;
use monitorflow_common::config::TlsPaths;
use monitorflow_common::proto::v1::agent_service_client::AgentServiceClient;
use monitorflow_common::proto::v1::envelope::Payload;
use monitorflow_common::proto::v1::{Command, CommandResult, CommandStatus, Envelope, Hello};
use monitorflow_common::tls::{self, Identity};
use monitorflow_common::PROTOCOL_VERSION;
use monitorflow_server::ca::{CaOptions, CertificateAuthority};
use monitorflow_server::commands::{CommandStore, Commands, DispatcherSettings, NewCommand, State};
use monitorflow_server::grpc::{
    self, AgentStreamService, DbInventory, DbRelayPolicy, Registry, StreamSettings,
};
use rustls::pki_types::CertificateDer;
use sqlx::PgPool;
use support::{Ca, Leaf};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};
use tokio_stream::wrappers::ReceiverStream;
use tonic::Streaming;
use uuid::Uuid;

const TIMEOUT: Duration = Duration::from_secs(5);

/// Marks every test in the block as ignored when `DATABASE_URL` is unset.
macro_rules! db_tests {
    ($($item:item)*) => {
        $(
            #[cfg_attr(
                not(db_tests),
                ignore = "DATABASE_URL is not set; run `docker compose -f deploy/docker-compose.dev.yml up -d postgres` and export DATABASE_URL from .env.example"
            )]
            $item
        )*
    };
}

async fn site(pool: &PgPool, name: &str) -> Uuid {
    sqlx::query_scalar("INSERT INTO sites (name) VALUES ($1) RETURNING id")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn server(pool: &PgPool, peer_id: &str, site_id: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO servers (peer_id, site_id, hostname, os_family)
         VALUES ($1, $2, $1, 'debian') RETURNING id",
    )
    .bind(peer_id)
    .bind(site_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

fn new(server_id: Uuid, payload: &[u8]) -> NewCommand {
    NewCommand {
        server_id,
        kind: command::APPLY_UPDATES.into(),
        payload: payload.to_vec(),
        ttl: Duration::from_secs(3600),
        requested_by: None,
        campaign_id: None,
    }
}

fn result(id: Uuid, status: CommandStatus, exit_code: i32) -> CommandResult {
    CommandResult {
        command_id: id.to_string(),
        exit_code,
        status: status as i32,
        stdout: b"done".to_vec(),
        ..CommandResult::default()
    }
}

async fn state(store: &CommandStore, id: Uuid) -> State {
    store.get(id).await.unwrap().unwrap().state
}

async fn audit_count(pool: &PgPool, action: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = $1")
        .bind(action)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// The server's stream service with a dispatcher, over mTLS from a test CA;
/// commands are signed by `signer`, a separate built-in CA.
struct Harness {
    ca: Ca,
    url: String,
    registry: Registry,
    signer: Arc<CertificateAuthority>,
    commands: Commands,
    _dir: TempDir,
}

impl Harness {
    async fn start(pool: &PgPool) -> Self {
        let dir = TempDir::new().unwrap();
        let signer = Arc::new(
            CertificateAuthority::open_or_create(&dir.path().join("ca"), CaOptions::default())
                .unwrap(),
        );
        let ca = Ca::new("monitorflow test CA");
        let server = identity(&ca, &ca.server());
        let (_tx, tls_config) = watch::channel(server.server_config(Vec::new()).unwrap());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "https://127.0.0.1:{}",
            listener.local_addr().unwrap().port()
        );
        let registry = Registry::default();
        let settings = DispatcherSettings {
            poll_interval: Duration::from_millis(100),
            ..DispatcherSettings::default()
        };
        let commands = Commands::spawn(
            CommandStore::new(pool.clone()),
            registry.clone(),
            signer.clone(),
            settings,
        );
        let stream = StreamSettings {
            heartbeat_timeout: TIMEOUT,
            ..StreamSettings::default()
        };
        let service = AgentStreamService::new(
            registry.clone(),
            Arc::new(DbRelayPolicy::new(pool.clone())),
            stream,
        )
        .with_inventory(Arc::new(DbInventory::new(pool.clone())))
        .with_commands(commands.clone());
        tokio::spawn(grpc::serve(
            listener,
            tls_config,
            service,
            std::future::pending(),
        ));
        Self {
            ca,
            url,
            registry,
            signer,
            commands,
            _dir: dir,
        }
    }

    async fn open(&self, agent_id: &str, serial: u64) -> Peer {
        let config = identity(&self.ca, &self.ca.client(agent_id, serial))
            .client_config()
            .unwrap();
        let channel = tls::connect(&self.url, config, TIMEOUT).await.unwrap();
        let (tx, rx) = mpsc::channel(8);
        let hello = Hello {
            agent_id: agent_id.into(),
            protocol_version: PROTOCOL_VERSION.into(),
            agent_version: "0.1.0".into(),
            hostname: format!("{agent_id}.example"),
            os_family: "debian".into(),
            os_version: "debian 12".into(),
        };
        tx.send(Envelope {
            sequence: 1,
            payload: Some(Payload::Hello(hello)),
        })
        .await
        .unwrap();
        let rx = AgentServiceClient::new(channel)
            .stream(ReceiverStream::new(rx))
            .await
            .unwrap()
            .into_inner();
        Peer {
            tx,
            rx,
            sequence: 1,
        }
    }

    fn signer_cert(&self) -> CertificateDer<'static> {
        let pem = pem::parse(self.signer.ca_cert_pem()).unwrap();
        CertificateDer::from(pem.into_contents())
    }
}

fn identity(ca: &Ca, leaf: &Leaf) -> Identity {
    let dir = TempDir::new().unwrap();
    let (ca_cert, cert, key) = support::write(dir.path(), "id", ca, leaf);
    Identity::load(&TlsPaths { ca_cert, cert, key }).unwrap()
}

struct Peer {
    tx: mpsc::Sender<Envelope>,
    rx: Streaming<Envelope>,
    sequence: u64,
}

impl Peer {
    /// The next command on the stream, skipping anything else.
    async fn command(&mut self) -> Command {
        loop {
            let envelope = tokio::time::timeout(TIMEOUT, self.rx.message())
                .await
                .expect("no command in time")
                .unwrap()
                .expect("stream ended");
            if let Some(Payload::Command(command)) = envelope.payload {
                return command;
            }
        }
    }

    /// Fails if a command arrives within `wait`.
    async fn no_command(&mut self, wait: Duration) {
        let deadline = tokio::time::Instant::now() + wait;
        while let Ok(message) = tokio::time::timeout_at(deadline, self.rx.message()).await {
            if let Some(Payload::Command(c)) = message.unwrap().and_then(|e| e.payload) {
                panic!("unexpected command {}", c.command_id);
            }
        }
    }

    async fn send(&mut self, result: CommandResult) {
        self.sequence += 1;
        self.tx
            .send(Envelope {
                sequence: self.sequence,
                payload: Some(Payload::CommandResult(result)),
            })
            .await
            .unwrap();
    }
}

async fn wait_for(store: &CommandStore, id: Uuid, expected: State) {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let now = state(store, id).await;
        if now == expected {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "command stayed {now:?}, expected {expected:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

db_tests! {
    #[sqlx::test]
    async fn one_command_in_flight_per_server_and_results_only_from_its_agent(pool: PgPool) {
        let paris = site(&pool, "paris").await;
        let (web1, web2) = (server(&pool, "web-01", paris).await, server(&pool, "web-02", paris).await);
        let store = CommandStore::new(pool.clone());
        let first = store.enqueue(&new(web1, b"1")).await.unwrap();
        let second = store.enqueue(&new(web1, b"2")).await.unwrap();
        let other = store.enqueue(&new(web2, b"3")).await.unwrap();
        assert_eq!(first.state, State::Queued);
        assert!(first.expires_at > first.issued_at);

        assert_eq!(store.next("web-01").await.unwrap().unwrap().id, first.id);
        assert!(store.mark_sent(first.id).await.unwrap());
        assert!(!store.mark_sent(first.id).await.unwrap(), "already sent");
        // The unique index keeps a second command of web-01 queued.
        assert!(!store.mark_sent(second.id).await.unwrap());
        assert!(store.mark_sent(other.id).await.unwrap());
        // The command in flight comes before anything queued.
        assert_eq!(store.next("web-01").await.unwrap().unwrap().id, first.id);

        // Another agent cannot report web-01's command.
        let done = result(first.id, CommandStatus::Succeeded, 0);
        assert_eq!(store.record("web-02", &done).await.unwrap(), None);
        assert_eq!(state(&store, first.id).await, State::Sent);
        let bogus = CommandResult { command_id: "not-a-uuid".into(), ..done.clone() };
        assert_eq!(store.record("web-01", &bogus).await.unwrap(), None);

        let running = result(first.id, CommandStatus::Running, 0);
        assert_eq!(store.record("web-01", &running).await.unwrap(), Some(State::Running));
        assert_eq!(store.record("web-01", &running).await.unwrap(), None);
        assert_eq!(store.record("web-01", &done).await.unwrap(), Some(State::Succeeded));
        // Final states never change again.
        let failed = result(first.id, CommandStatus::Failed, 1);
        assert_eq!(store.record("web-01", &failed).await.unwrap(), None);
        let row = store.get(first.id).await.unwrap().unwrap();
        assert_eq!((row.exit_code, row.stdout.as_deref()), (Some(0), Some("done")));
        assert!(row.started_at.is_some() && row.finished_at.is_some());
        assert_eq!(audit_count(&pool, "command.finish").await, 1);

        assert_eq!(store.next("web-01").await.unwrap().unwrap().id, second.id);
        assert!(store.mark_sent(second.id).await.unwrap());
        // A rejected command did not run: no start time.
        let mut rejected = result(second.id, CommandStatus::Rejected, 0);
        rejected.error = "unknown command kind".into();
        assert_eq!(store.record("web-01", &rejected).await.unwrap(), Some(State::Failed));
        let row = store.get(second.id).await.unwrap().unwrap();
        assert_eq!((row.started_at, row.error.as_deref()), (None, Some("unknown command kind")));
        assert_eq!(store.next("web-01").await.unwrap(), None);
    }

    #[sqlx::test]
    async fn stale_commands_expire_and_late_results_still_count(pool: PgPool) {
        let paris = site(&pool, "paris").await;
        let (web1, web2, web3) = (
            server(&pool, "web-01", paris).await,
            server(&pool, "web-02", paris).await,
            server(&pool, "web-03", paris).await,
        );
        let store = CommandStore::new(pool.clone());
        let queued = store.enqueue(&new(web1, b"")).await.unwrap();
        let sent = store.enqueue(&new(web2, b"")).await.unwrap();
        let running = store.enqueue(&new(web3, b"")).await.unwrap();
        for id in [sent.id, running.id] {
            assert!(store.mark_sent(id).await.unwrap());
        }
        store.record("web-03", &result(running.id, CommandStatus::Running, 0)).await.unwrap();
        let grace = Duration::from_secs(60);
        assert_eq!(store.expire(grace, Duration::from_secs(3600)).await.unwrap(), 0);

        sqlx::query(
            "UPDATE commands SET issued_at = issued_at - interval '3 hours',
                 expires_at = now() - interval '2 minutes',
                 started_at = started_at - interval '3 hours'",
        )
        .execute(&pool)
        .await
        .unwrap();
        // The queued one is past expiry, the sent one within its grace, the
        // running one past the run timeout.
        assert_eq!(store.expire(Duration::from_secs(300), Duration::from_secs(3600)).await.unwrap(), 2);
        assert_eq!(state(&store, queued.id).await, State::Expired);
        assert_eq!(state(&store, sent.id).await, State::Sent);
        let failed = store.get(running.id).await.unwrap().unwrap();
        assert_eq!((failed.state, failed.error.as_deref()), (State::Failed, Some("no result from the agent")));
        assert_eq!(store.expire(grace, Duration::from_secs(3600)).await.unwrap(), 1);
        assert_eq!(state(&store, sent.id).await, State::Expired);
        // Nothing queued past its expiry is ever sent.
        assert!(!store.mark_sent(queued.id).await.unwrap());
        assert_eq!(store.next("web-01").await.unwrap(), None);

        // The agent ran it before expiry by its clock: the result wins.
        let late = result(sent.id, CommandStatus::Succeeded, 0);
        assert_eq!(store.record("web-02", &late).await.unwrap(), Some(State::Succeeded));
    }

    #[sqlx::test]
    async fn commands_are_signed_sent_one_at_a_time_and_sent_again_after_a_reconnect(pool: PgPool) {
        let paris = site(&pool, "paris").await;
        let web1 = server(&pool, "web-01", paris).await;
        let h = Harness::start(&pool).await;
        let store = h.commands.store().clone();
        let first = store.enqueue(&new(web1, b"first")).await.unwrap();
        let second = store.enqueue(&new(web1, b"second")).await.unwrap();

        // Sent right after Hello, signed for this agent.
        let mut peer = h.open("web-01", 10).await;
        let cmd = peer.command().await;
        assert_eq!(cmd.command_id, first.id.to_string());
        assert_eq!((cmd.agent_id.as_str(), cmd.kind.as_str()), ("web-01", command::APPLY_UPDATES));
        assert_eq!(cmd.payload, b"first");
        assert_eq!(cmd.expires_at_unix_ms, first.expires_at.timestamp_millis());
        assert_eq!(command::verify(&cmd, &[h.signer_cert()]), Ok(()));
        assert_eq!(state(&store, first.id).await, State::Sent);

        // Nothing else while it is in flight, polls included.
        peer.send(result(first.id, CommandStatus::Running, 0)).await;
        wait_for(&store, first.id, State::Running).await;
        peer.no_command(Duration::from_millis(400)).await;

        // A new connection gets the command in flight again.
        drop(peer);
        let mut peer = h.open("web-01", 11).await;
        assert_eq!(peer.command().await.command_id, first.id.to_string());
        peer.no_command(Duration::from_millis(300)).await;

        // Its end sends the next one at once.
        peer.send(result(first.id, CommandStatus::Succeeded, 0)).await;
        let next = peer.command().await;
        assert_eq!(next.command_id, second.id.to_string());
        assert_eq!(state(&store, first.id).await, State::Succeeded);
        peer.send(result(second.id, CommandStatus::Failed, 100)).await;
        wait_for(&store, second.id, State::Failed).await;

        // Queued while connected: the API's dispatch call sends it.
        let third = store.enqueue(&new(web1, b"third")).await.unwrap();
        h.commands.dispatch("web-01");
        assert_eq!(peer.command().await.command_id, third.id.to_string());
        assert_eq!(h.registry.get("web-01").unwrap().counters.command_results, 2);
    }
}
