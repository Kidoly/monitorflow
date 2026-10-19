//! Commands end to end (ADR-0031): the server's dispatcher and the agent's
//! dispatcher over a real mTLS stream, with PostgreSQL.

mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use monitorflow_agent::backoff::Backoff;
use monitorflow_agent::commands::{
    self, Dispatcher, DispatcherSettings, Handler, Handlers, Journal, Output,
};
use monitorflow_agent::os::{HostInfo, OsFamily};
use monitorflow_agent::shutdown::{self, Trigger};
use monitorflow_agent::stream::{Agent, StreamSettings};
use monitorflow_common::proto::v1::{Package, PackageReport};
use monitorflow_common::tls;
use monitorflow_server::commands::{
    CommandStore, Commands, DispatcherSettings as ServerDispatch, NewCommand, State,
};
use monitorflow_server::grpc::{
    self, AgentStreamService, CloseReason, DbInventory, DbRelayPolicy, Registry,
};
use sqlx::PgPool;
use support::{agent_config, Pki};
use tempfile::TempDir;
use tokio::sync::watch;
use uuid::Uuid;

const TIMEOUT: Duration = Duration::from_secs(10);

/// Counts its runs and echoes its payload.
#[derive(Default)]
struct Echo {
    runs: AtomicUsize,
}

#[async_trait::async_trait]
impl Handler for Echo {
    async fn run(&self, payload: &[u8]) -> Output {
        self.runs.fetch_add(1, Ordering::SeqCst);
        Output {
            stdout: payload.to_vec(),
            ..Output::default()
        }
    }
}

struct Setup {
    _pki: Pki,
    _data: TempDir,
    store: CommandStore,
    registry: Registry,
    server_id: Uuid,
    echo: Arc<Echo>,
    trigger: Trigger,
    reports: watch::Sender<Option<PackageReport>>,
}

impl Setup {
    async fn start(pool: &PgPool) -> Self {
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
        let store = CommandStore::new(pool.clone());
        let ca = pki.ca.clone();
        let upstream = pki
            .serve_service(|registry| {
                let settings = ServerDispatch {
                    poll_interval: Duration::from_millis(100),
                    ..ServerDispatch::default()
                };
                let commands = Commands::spawn(store.clone(), registry.clone(), ca, settings);
                let stream = grpc::StreamSettings {
                    heartbeat_timeout: TIMEOUT,
                    ..grpc::StreamSettings::default()
                };
                AgentStreamService::new(
                    registry,
                    Arc::new(DbRelayPolicy::new(pool.clone())),
                    stream,
                )
                .with_inventory(Arc::new(DbInventory::new(pool.clone())))
                .with_commands(commands)
            })
            .await;
        let data = TempDir::new().unwrap();
        let config = agent_config(data.path().into(), &upstream.url, "https://localhost:1");
        let (identity, _) = pki.agent_identity(&config, "web-01");

        let echo = Arc::new(Echo::default());
        let mut handlers = Handlers::default();
        handlers.register("echo", echo.clone());
        let ca_certs = tls::load_certs(&config.tls_paths().ca_cert).unwrap();
        let dispatcher = Dispatcher::new(
            "web-01",
            ca_certs,
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
        };
        let (reports, latest) = watch::channel(None);
        let agent = Agent::new(settings, identity, host)
            .with_commands(stream_end)
            .with_reports(latest);
        tokio::spawn(async move { agent.run(stop).await.unwrap() });
        Self {
            _pki: pki,
            _data: data,
            store,
            registry: upstream.registry,
            server_id,
            echo,
            trigger,
            reports,
        }
    }

    async fn enqueue(&self, kind: &str, payload: &[u8], ttl: Duration) -> Uuid {
        let new = NewCommand {
            server_id: self.server_id,
            kind: kind.into(),
            payload: payload.to_vec(),
            ttl,
            requested_by: None,
            campaign_id: None,
        };
        self.store.enqueue(&new).await.unwrap().id
    }

    async fn wait_for(&self, id: Uuid, expected: State) {
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        loop {
            let state = self.store.get(id).await.unwrap().unwrap().state;
            if state == expected {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "command {id} stayed {state:?}, expected {expected:?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn runs(&self) -> usize {
        self.echo.runs.load(Ordering::SeqCst)
    }
}

impl Drop for Setup {
    fn drop(&mut self) {
        self.trigger.fire();
    }
}

db_tests! {
    #[sqlx::test(migrations = "../server/migrations")]
    async fn commands_run_on_the_agent_and_a_lost_result_is_recovered_without_running_again(pool: PgPool) {
        let s = Setup::start(&pool).await;
        let first = s.enqueue("echo", b"hello", Duration::from_secs(3600)).await;
        s.wait_for(first, State::Succeeded).await;
        let row = s.store.get(first).await.unwrap().unwrap();
        assert_eq!((row.exit_code, row.stdout.as_deref()), (Some(0), Some("hello")));
        assert!(row.started_at.is_some());
        assert_eq!(s.runs(), 1);

        // The result never reached the server: the command is still in
        // flight there when the agent reconnects.
        sqlx::query(
            "UPDATE commands SET state = 'sent', finished_at = NULL, exit_code = NULL, stdout = NULL
             WHERE id = $1",
        )
        .bind(first)
        .execute(&pool)
        .await
        .unwrap();
        let before = s.registry.get("web-01").unwrap().connection;
        s.registry.close("web-01", CloseReason::Replaced);
        s.wait_for(first, State::Succeeded).await;
        assert!(s.registry.get("web-01").unwrap().connection > before);
        let row = s.store.get(first).await.unwrap().unwrap();
        assert_eq!(row.stdout.as_deref(), Some("hello"));
        assert_eq!(s.runs(), 1, "answered from the journal, not run again");

        // Unknown kinds come back failed, with the agent's reason.
        let unknown = s.enqueue("reboot", b"", Duration::from_secs(3600)).await;
        s.wait_for(unknown, State::Failed).await;
        let row = s.store.get(unknown).await.unwrap().unwrap();
        assert_eq!(row.error.as_deref(), Some("unknown command kind `reboot`"));
        assert_eq!(row.started_at, None);
    }

    #[sqlx::test(migrations = "../server/migrations")]
    async fn a_command_queued_for_a_disconnected_agent_expires_unrun(pool: PgPool) {
        let s = Setup::start(&pool).await;
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        while s.registry.get("web-01").is_none() {
            assert!(tokio::time::Instant::now() < deadline, "agent never connected");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        s.trigger.fire();
        while s.registry.get("web-01").is_some() {
            assert!(tokio::time::Instant::now() < deadline, "agent never left");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let id = s.enqueue("echo", b"late", Duration::from_millis(300)).await;
        s.wait_for(id, State::Expired).await;
        assert_eq!(s.store.get(id).await.unwrap().unwrap().sent_at, None);
        assert_eq!(s.runs(), 0);
    }

    #[sqlx::test(migrations = "../server/migrations")]
    async fn package_reports_reach_the_server(pool: PgPool) {
        let s = Setup::start(&pool).await;
        let package = Package {
            name: "libc6".into(),
            version: "2.36-9".into(),
            arch: "amd64".into(),
            upgradable_version: "2.36-9+deb12u4".into(),
        };
        s.reports.send_replace(Some(PackageReport {
            packages: vec![package],
            reboot_required: true,
            collected_at_unix_ms: 1_800_000_000_000,
        }));
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        loop {
            let stored: Option<(String, Option<String>, Option<bool>)> = sqlx::query_as(
                "SELECT p.name, p.upgradable_version, s.reboot_required
                 FROM server_packages p JOIN servers s ON s.id = p.server_id",
            )
            .fetch_optional(&pool)
            .await
            .unwrap();
            if let Some(stored) = stored {
                assert_eq!(stored, ("libc6".into(), Some("2.36-9+deb12u4".into()), Some(true)));
                break;
            }
            assert!(tokio::time::Instant::now() < deadline, "report never stored");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}
