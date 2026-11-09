//! Several server instances on one database (ADR-0089): two instances in
//! one process, each with its own copy of the CA, mTLS listener, command
//! dispatcher, threshold engine and cluster task, as two hosts would run
//! them. A crash stops an instance's streams and heartbeat without leaving.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use chrono::{TimeDelta, Utc};
use monitorflow_common::command;
use monitorflow_common::config::TlsPaths;
use monitorflow_common::proto::v1::agent_service_client::AgentServiceClient;
use monitorflow_common::proto::v1::envelope::Payload;
use monitorflow_common::proto::v1::{
    Command, CommandResult, CommandStatus, Envelope, Hello, Sample, SampleBatch,
};
use monitorflow_common::tls::{self, Identity, PeerIdentity, PeerRole};
use monitorflow_common::PROTOCOL_VERSION;
use monitorflow_server::alerting::offline::{self, OfflineSettings};
use monitorflow_server::alerting::{AlertStore, Alerts, EngineSettings, LogNotifier};
use monitorflow_server::ca::{CaOptions, CertificateAuthority, RevocationReason, Serial};
use monitorflow_server::cluster::{self, Cluster, Links};
use monitorflow_server::commands::{CommandStore, Commands, DispatcherSettings, NewCommand, State};
use monitorflow_server::crl::CrlPublisher;
use monitorflow_server::enroll::server_cert;
use monitorflow_server::grpc::{
    self, AgentStreamService, DbInventory, DbRelayPolicy, Registry, StreamSettings,
};
use monitorflow_server::notify::Dispatcher;
use rcgen::{CertificateParams, KeyPair};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_stream::wrappers::ReceiverStream;
use tonic::Streaming;
use uuid::Uuid;

const TIMEOUT: Duration = Duration::from_secs(10);

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

fn settings() -> cluster::Settings {
    cluster::Settings {
        heartbeat: Duration::from_millis(100),
        dead_after: Duration::from_secs(1),
        leader_ttl: Duration::from_secs(1),
    }
}

/// The database and the CA every instance copies, as the second server's
/// installer would.
struct Harness {
    pool: PgPool,
    dir: TempDir,
    closed: bool,
}

impl Harness {
    async fn start(test_pool: &PgPool) -> Self {
        // A pool of its own, closed at the end, so the tasks below do not
        // hold the test pool's permits (see remediation_loop.rs).
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .connect_with((*test_pool.connect_options()).clone())
            .await
            .unwrap();
        let dir = TempDir::new().unwrap();
        CertificateAuthority::open_or_create(&dir.path().join("ca"), CaOptions::default()).unwrap();
        Self {
            pool,
            dir,
            closed: false,
        }
    }

    async fn close(mut self) {
        self.pool.close().await;
        self.closed = true;
    }

    async fn instance(&mut self, name: &str) -> Instance {
        let dir = self.dir.path().join(name);
        copy_dir(&self.dir.path().join("ca"), &dir.join("ca"));
        let ca = Arc::new(
            CertificateAuthority::open_or_create(&dir.join("ca"), CaOptions::default()).unwrap(),
        );
        let paths = tls_paths(&dir, "server");
        server_cert::ensure(
            &ca,
            &paths,
            &server_cert::parse_names(&["localhost".into()]),
        )
        .unwrap();
        let (crl, tls_config) = CrlPublisher::start(ca.clone(), Identity::load(&paths).unwrap())
            .await
            .unwrap();
        let crl = Arc::new(crl);
        let pool = self.pool.clone();
        let registry = Registry::default();
        // Polls far apart: a command reaches its agent by notification.
        let commands = Commands::spawn(
            CommandStore::new(pool.clone()),
            registry.clone(),
            ca.clone(),
            DispatcherSettings {
                poll_interval: Duration::from_secs(600),
                ..DispatcherSettings::default()
            },
        );
        let alerts = Alerts::start(
            AlertStore::new(pool.clone()),
            EngineSettings::default(),
            Arc::new(LogNotifier),
        )
        .await
        .unwrap();
        let service = AgentStreamService::new(
            registry.clone(),
            Arc::new(DbRelayPolicy::new(pool.clone())),
            StreamSettings {
                heartbeat_timeout: TIMEOUT,
                ..StreamSettings::default()
            },
        )
        .with_inventory(Arc::new(DbInventory::new(pool.clone())))
        .with_commands(commands.clone())
        .with_alerts(alerts.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "https://localhost:{}",
            listener.local_addr().unwrap().port()
        );
        let serve = tokio::spawn(async move {
            let _ = grpc::serve_all(
                listener,
                tls_config,
                service,
                None,
                None,
                std::future::pending(),
            )
            .await;
        });
        let cluster = Cluster::join(
            pool.clone(),
            registry.clone(),
            Links {
                commands: Some(commands.clone()),
                relay_commands: None,
                crl: Some(crl.clone()),
            },
            settings(),
        )
        .await
        .unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let run = tokio::spawn(cluster.clone().run(async {
            let _ = stopped.await;
        }));
        Instance {
            dir,
            url,
            ca,
            crl,
            registry,
            alerts,
            cluster,
            serve,
            run: Some(run),
            stop: Some(stop),
        }
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if !self.closed && !std::thread::panicking() {
            panic!("end the test with Harness::close");
        }
    }
}

struct Instance {
    dir: std::path::PathBuf,
    url: String,
    ca: Arc<CertificateAuthority>,
    crl: Arc<CrlPublisher>,
    registry: Registry,
    alerts: Alerts,
    cluster: Cluster,
    serve: JoinHandle<()>,
    run: Option<JoinHandle<()>>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Instance {
    /// The process dies: its streams end, its heartbeat stops, it never
    /// leaves.
    fn crash(&mut self) {
        self.serve.abort();
        self.registry.close_all();
        if let Some(run) = self.run.take() {
            run.abort();
        }
    }

    /// The process stops cleanly and leaves.
    async fn stop(&mut self) {
        self.serve.abort();
        self.registry.close_all();
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(run) = self.run.take() {
            run.await.unwrap();
        }
    }

    fn running(&self) -> bool {
        self.run.is_some()
    }

    /// An agent certificate from this instance's copy of the CA, recorded
    /// as enrollment does.
    async fn agent(&self, pool: &PgPool, id: &str) -> (Identity, Serial) {
        let key = KeyPair::generate().unwrap();
        let csr = CertificateParams::default()
            .serialize_request(&key)
            .unwrap()
            .pem()
            .unwrap();
        let identity = PeerIdentity::new(PeerRole::Agent, id).unwrap();
        let issued = self.ca.issue(&csr, &identity, None, &[]).unwrap();
        sqlx::query(
            "INSERT INTO certificates (serial, peer_role, peer_id, not_before, not_after)
             VALUES ($1, 'agent', $2, now() - interval '1 minute', now() + interval '1 day')",
        )
        .bind(issued.serial.as_bytes())
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
        let paths = tls_paths(&self.dir, id);
        std::fs::write(&paths.ca_cert, self.ca.ca_cert_pem()).unwrap();
        std::fs::write(&paths.cert, &issued.cert_pem).unwrap();
        std::fs::write(&paths.key, key.serialize_pem()).unwrap();
        (Identity::load(&paths).unwrap(), issued.serial)
    }

    async fn open(&self, identity: &Identity, id: &str) -> Result<Agent, tonic::Status> {
        let channel = tls::connect(&self.url, identity.client_config().unwrap(), TIMEOUT)
            .await
            .map_err(|e| tonic::Status::unavailable(e.to_string()))?;
        let (tx, rx) = mpsc::channel(8);
        tx.send(Envelope {
            sequence: 1,
            payload: Some(Payload::Hello(Hello {
                agent_id: id.into(),
                protocol_version: PROTOCOL_VERSION.into(),
                agent_version: "0.1.0".into(),
                hostname: format!("{id}.example"),
                os_family: "debian".into(),
                os_version: "debian 12".into(),
                ..Hello::default()
            })),
        })
        .await
        .unwrap();
        let rx = AgentServiceClient::new(channel)
            .stream(ReceiverStream::new(rx))
            .await?
            .into_inner();
        Ok(Agent {
            tx,
            rx,
            sequence: 1,
        })
    }
}

struct Agent {
    tx: mpsc::Sender<Envelope>,
    rx: Streaming<Envelope>,
    sequence: u64,
}

impl Agent {
    async fn send(&mut self, payload: Payload) {
        self.sequence += 1;
        self.tx
            .send(Envelope {
                sequence: self.sequence,
                payload: Some(payload),
            })
            .await
            .unwrap();
    }

    /// The next command, skipping anything else.
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
        while let Ok(Ok(Some(envelope))) =
            tokio::time::timeout_at(deadline, self.rx.message()).await
        {
            if let Some(Payload::Command(c)) = envelope.payload {
                panic!("unexpected command {}", c.command_id);
            }
        }
    }

    /// Waits for the server to end the stream; returns its status.
    async fn closed(&mut self) -> Option<tonic::Status> {
        loop {
            match tokio::time::timeout(TIMEOUT, self.rx.message())
                .await
                .expect("stream still open")
            {
                Ok(Some(_)) => {}
                Ok(None) => return None,
                Err(status) => return Some(status),
            }
        }
    }
}

fn disk(at: i64, value: f64) -> Payload {
    Payload::Samples(SampleBatch {
        samples: vec![Sample {
            sensor: "disk_used_ratio".into(),
            instance: "/".into(),
            timestamp_unix_ms: at,
            value,
            labels: Default::default(),
        }],
    })
}

fn tls_paths(dir: &Path, name: &str) -> TlsPaths {
    TlsPaths {
        ca_cert: dir.join(format!("{name}-ca.pem")),
        cert: dir.join(format!("{name}.pem")),
        key: dir.join(format!("{name}-key.pem")),
    }
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
    }
}

/// An enrolled server of a new site, as its first `Hello` finds it.
async fn enrolled(pool: &PgPool, peer_id: &str) -> Uuid {
    let site: Uuid =
        sqlx::query_scalar("INSERT INTO sites (name) VALUES ($1 || '-site') RETURNING id")
            .bind(peer_id)
            .fetch_one(pool)
            .await
            .unwrap();
    sqlx::query(
        "INSERT INTO enrollment_tokens
             (token_hash, role, peer_id, site_id, expires_at, used_at, used_by_peer_id)
         VALUES (sha256(gen_random_uuid()::text::bytea), 'agent', $1, $2,
                 now() + interval '1 hour', clock_timestamp(), $1)",
    )
    .bind(peer_id)
    .bind(site)
    .execute(pool)
    .await
    .unwrap();
    site
}

async fn server_id(pool: &PgPool, peer_id: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM servers WHERE peer_id = $1")
        .bind(peer_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Polls `check` until it holds.
async fn eventually<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while !check().await {
        assert!(tokio::time::Instant::now() < deadline, "{what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn count(pool: &PgPool, sql: &'static str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
}

db_tests! {
    #[sqlx::test]
    async fn a_command_reaches_its_agent_on_whichever_instance_holds_the_stream(pool: PgPool) {
        let mut h = Harness::start(&pool).await;
        let pool = h.pool.clone();
        let (mut a, mut b) = (h.instance("a").await, h.instance("b").await);
        enrolled(&pool, "web-01").await;
        let (identity, _) = a.agent(&pool, "web-01").await;
        let mut agent = b.open(&identity, "web-01").await.unwrap();
        eventually("a sees the stream b holds", || async {
            a.registry.live(PeerRole::Agent, "web-01").is_some_and(|p| p.connection == 0)
        })
        .await;

        // Queued as the API, a campaign or the AI would, on any instance.
        let store = CommandStore::new(pool.clone());
        let queued = store
            .enqueue(&NewCommand {
                server_id: server_id(&pool, "web-01").await,
                kind: command::APPLY_UPDATES.into(),
                payload: b"1".to_vec(),
                ttl: Duration::from_secs(3600),
                requested_by: None,
                campaign_id: None,
            })
            .await
            .unwrap();
        // The dispatchers poll every ten minutes: the notification sends it.
        let sent = agent.command().await;
        assert_eq!(sent.command_id, queued.id.to_string());
        agent.no_command(Duration::from_millis(300)).await;
        assert_eq!(store.get(queued.id).await.unwrap().unwrap().state, State::Sent);

        // b dies before the result; the agent comes back on a, which sends
        // the command in flight again, and the agent answers it once from
        // its journal.
        b.crash();
        assert!(agent.closed().await.is_some());
        let mut agent = a.open(&identity, "web-01").await.unwrap();
        assert_eq!(agent.command().await.command_id, queued.id.to_string());
        agent
            .send(Payload::CommandResult(CommandResult {
                command_id: queued.id.to_string(),
                status: CommandStatus::Succeeded as i32,
                ..CommandResult::default()
            }))
            .await;
        eventually("the result is recorded", || async {
            store.get(queued.id).await.unwrap().unwrap().state == State::Succeeded
        })
        .await;
        agent.no_command(Duration::from_millis(300)).await;
        assert_eq!(
            count(&pool, "SELECT count(*) FROM audit_log WHERE action = 'command.finish'").await,
            1
        );
        drop(agent);
        a.stop().await;
        h.close().await;
    }

    #[sqlx::test]
    async fn one_instance_leads_and_another_takes_over_when_it_dies(pool: PgPool) {
        let mut h = Harness::start(&pool).await;
        let mut instances = vec![h.instance("a").await, h.instance("b").await, h.instance("c").await];
        let leaders = |instances: &[Instance]| -> Vec<usize> {
            instances
                .iter()
                .enumerate()
                .filter(|(_, i)| i.running() && i.cluster.is_leader())
                .map(|(n, _)| n)
                .collect()
        };
        eventually("one instance leads", || {
            let found = leaders(&instances).len() == 1;
            async move { found }
        })
        .await;
        // Never two at once while they all run.
        for _ in 0..20 {
            assert_eq!(leaders(&instances).len(), 1);
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        // A crash: the lease expires, then someone else leads.
        let first = leaders(&instances)[0];
        let crashed_at = tokio::time::Instant::now();
        instances[first].crash();
        eventually("another instance leads after the crash", || {
            let found = leaders(&instances).len() == 1;
            async move { found }
        })
        .await;
        assert!(crashed_at.elapsed() >= Duration::from_millis(500), "took the lease before it expired");
        let second = leaders(&instances)[0];
        assert_ne!(second, first);

        // A clean stop hands over within a few heartbeats, not a lease.
        let stopped_at = tokio::time::Instant::now();
        instances[second].stop().await;
        eventually("the last instance leads after the stop", || {
            let found = leaders(&instances).len() == 1;
            async move { found }
        })
        .await;
        assert!(stopped_at.elapsed() < Duration::from_millis(900));
        let rows = count(&h.pool, "SELECT count(*) FROM server_instances").await;
        assert!(rows <= 2, "the stopped instance left: {rows} rows");
        for i in &mut instances {
            if i.running() {
                i.stop().await;
            }
        }
        h.close().await;
    }

    #[sqlx::test]
    async fn a_stream_on_another_instance_is_online_until_that_instance_dies(pool: PgPool) {
        let mut h = Harness::start(&pool).await;
        let pool = h.pool.clone();
        let (mut a, mut b) = (h.instance("a").await, h.instance("b").await);
        enrolled(&pool, "web-01").await;
        let (identity, _) = a.agent(&pool, "web-01").await;
        let agent = b.open(&identity, "web-01").await.unwrap();
        eventually("a sees the stream b holds", || async { a.registry.live(PeerRole::Agent, "web-01").is_some() }).await;
        // Seen long ago as far as the database knows: only the stream on b
        // keeps it online for a.
        sqlx::query("UPDATE servers SET last_seen_at = now() - interval '1 day'")
            .execute(&pool)
            .await
            .unwrap();
        let online = |registry: &Registry| -> std::collections::HashMap<_, _> {
            registry
                .live_all()
                .into_iter()
                .map(|p| ((p.role, p.id), p.connected_at))
                .collect()
        };
        let long_ago = Utc::now() - TimeDelta::days(2);
        let settings = OfflineSettings::default();
        assert!(offline::check(&pool, &online(&a.registry), long_ago, &settings).await.unwrap().is_empty());

        b.crash();
        drop(agent);
        eventually("a forgets the streams of the dead instance", || async {
            a.registry.live(PeerRole::Agent, "web-01").is_none()
        })
        .await;
        // Two leaders for a moment write one incident.
        let seen = online(&a.registry);
        let (one, two) = tokio::join!(
            offline::check(&pool, &seen, long_ago, &settings),
            offline::check(&pool, &seen, long_ago, &settings),
        );
        assert_eq!(one.unwrap().len() + two.unwrap().len(), 1);
        assert_eq!(
            count(&pool, "SELECT count(*) FROM incidents WHERE metric = 'offline' AND state = 'open'").await,
            1
        );
        assert_eq!(count(&pool, "SELECT count(*) FROM incident_events").await, 1);
        a.stop().await;
        h.close().await;
    }

    #[sqlx::test]
    async fn an_agent_moving_between_instances_is_evaluated_from_what_was_stored(pool: PgPool) {
        let mut h = Harness::start(&pool).await;
        let pool = h.pool.clone();
        let (mut a, mut b) = (h.instance("a").await, h.instance("b").await);
        let site = enrolled(&pool, "web-01").await;
        sqlx::query(
            "INSERT INTO thresholds (site_id, metric, instance, comparison, warning, duration_s)
             VALUES ($1, 'monitorflow_disk_used_ratio', '/', '>', 0.8, 1)",
        )
        .bind(site)
        .execute(&pool)
        .await
        .unwrap();
        let (identity, _) = a.agent(&pool, "web-01").await;
        let states = || async {
            sqlx::query_scalar::<_, String>("SELECT state FROM incidents ORDER BY opened_at")
                .fetch_all(&pool)
                .await
                .unwrap()
        };
        let start = Utc::now().timestamp_millis() - 20_000;

        // On a: the disk fills, an incident opens.
        let mut agent = a.open(&identity, "web-01").await.unwrap();
        agent.send(disk(start, 0.9)).await;
        agent.send(disk(start + 1_500, 0.92)).await;
        eventually("a opens the incident", || async { states().await == ["open"] }).await;
        drop(agent);

        // On b: it recovers, b resolves the incident a opened.
        let mut agent = b.open(&identity, "web-01").await.unwrap();
        agent.send(disk(start + 3_000, 0.5)).await;
        eventually("b resolves it", || async { states().await == ["resolved"] }).await;
        drop(agent);

        // Back on a, which still runs: it fills again. a must not take the
        // incident for open still, as it was when the agent left.
        let mut agent = a.open(&identity, "web-01").await.unwrap();
        agent.send(disk(start + 4_500, 0.9)).await;
        agent.send(disk(start + 6_000, 0.95)).await;
        eventually("a opens a new incident", || async {
            states().await == ["resolved", "open"]
        })
        .await;
        a.alerts.flush().await;
        let kinds: Vec<String> =
            sqlx::query_scalar("SELECT kind FROM incident_events ORDER BY occurred_at, id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(kinds, ["opened", "resolved", "opened"]);
        drop(agent);
        a.stop().await;
        b.stop().await;
        h.close().await;
    }

    #[sqlx::test]
    async fn a_revocation_on_one_instance_reaches_the_other(pool: PgPool) {
        let mut h = Harness::start(&pool).await;
        let pool = h.pool.clone();
        let (mut a, mut b) = (h.instance("a").await, h.instance("b").await);
        enrolled(&pool, "web-01").await;
        let (identity, serial) = a.agent(&pool, "web-01").await;
        let mut agent = b.open(&identity, "web-01").await.unwrap();

        // A relay of the same ID is revoked on a: another peer (ADR-0105),
        // so b lists its certificate and leaves the agent connected.
        let relay = Serial::from_bytes(&[0x42; 8]);
        sqlx::query(
            "INSERT INTO certificates (serial, peer_role, peer_id, not_before, not_after,
                                       revoked_at, revocation_reason)
             VALUES ($1, 'relay', 'web-01', now() - interval '1 minute', now() + interval '1 day',
                     now(), 'key_compromise')",
        )
        .bind(relay.as_bytes())
        .execute(&pool)
        .await
        .unwrap();
        eventually("b lists the relay's certificate", || async { b.crl.is_revoked(&relay) }).await;
        agent.no_command(Duration::from_millis(200)).await;
        assert!(b.registry.get(PeerRole::Agent, "web-01").is_some());

        // What the revoke endpoint does on a.
        sqlx::query(
            "UPDATE certificates SET revoked_at = now(), revocation_reason = 'key_compromise'
             WHERE serial = $1",
        )
        .bind(serial.as_bytes())
        .execute(&pool)
        .await
        .unwrap();
        a.crl.revoke(serial.clone(), RevocationReason::KeyCompromise).await.unwrap();

        let status = agent.closed().await.expect("b closes the stream");
        assert_eq!(status.code(), tonic::Code::PermissionDenied);
        assert!(b.crl.is_revoked(&serial));
        assert!(b.open(&identity, "web-01").await.is_err(), "b still accepts the certificate");
        a.stop().await;
        b.stop().await;
        h.close().await;
    }

    #[sqlx::test]
    async fn two_notification_dispatchers_claim_each_event_once(pool: PgPool) {
        let h = Harness::start(&pool).await;
        let pool = h.pool.clone();
        sqlx::query(
            "INSERT INTO notification_outbox (event, details)
             SELECT 'update.failed', jsonb_build_object('n', n) FROM generate_series(1, 250) n",
        )
        .execute(&pool)
        .await
        .unwrap();
        let dispatcher = |pool: &PgPool| {
            Dispatcher::new(pool.clone(), "https://mf.example")
                .with_retries(vec![Duration::ZERO])
                .without_waits()
        };
        let (a, b) = (dispatcher(&pool), dispatcher(&pool));
        let mut claimed = 0;
        loop {
            let (one, two) = tokio::join!(a.pass(), b.pass());
            let pass = one.unwrap() + two.unwrap();
            if pass == 0 {
                break;
            }
            claimed += pass;
        }
        assert_eq!(claimed, 250);
        assert_eq!(count(&pool, "SELECT count(*) FROM notification_outbox WHERE sent_at IS NULL").await, 0);
        h.close().await;
    }
}
