//! The agent stream over real mTLS on 127.0.0.1.

mod fake_http;
#[path = "../../common/tests/support/mod.rs"]
mod support;

use std::sync::Arc;
use std::time::Duration;

use fake_http::FakeHttp;
use monitorflow_common::config::TlsPaths;
use monitorflow_common::proto::v1::agent_service_client::AgentServiceClient;
use monitorflow_common::proto::v1::envelope::Payload;
use monitorflow_common::proto::v1::{Command, Envelope, Heartbeat, Hello, Sample, SampleBatch};
use monitorflow_common::tls::{self, Identity, PeerRole};
use monitorflow_common::PROTOCOL_VERSION;
use monitorflow_server::grpc::{
    self, AcceptedHello, AgentStreamService, AllowAllRelayedAgents, DbInventory, DbRelayPolicy,
    DenyRelayedAgents, Inventory, InventoryError, NoInventory, Registry, RelayPolicy,
    StreamSettings,
};
use monitorflow_server::ingest::metrics::decode;
use monitorflow_server::ingest::{
    Ingest, JsonLines, LogRecord, RemoteWrite, Series, Writer, WriterSettings,
};
use sqlx::PgPool;
use support::{uri_san, Ca, Leaf};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Code, Status, Streaming};
use uuid::Uuid;

const TIMEOUT: Duration = Duration::from_secs(5);

fn identity(ca: &Ca, leaf: &Leaf) -> Identity {
    let dir = TempDir::new().unwrap();
    let (ca_cert, cert, key) = support::write(dir.path(), "id", ca, leaf);
    Identity::load(&TlsPaths { ca_cert, cert, key }).unwrap()
}

struct Harness {
    ca: Ca,
    url: String,
    registry: Registry,
    shutdown: tokio::sync::oneshot::Sender<()>,
    server: tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
}

fn settings() -> StreamSettings {
    StreamSettings {
        hello_timeout: Duration::from_millis(200),
        heartbeat_timeout: TIMEOUT,
        outbound_capacity: 8,
        last_seen_interval: Duration::from_secs(3600),
    }
}

impl Harness {
    async fn start(settings: StreamSettings, policy: Arc<dyn RelayPolicy>) -> Self {
        Self::start_with(settings, policy, Arc::new(NoInventory)).await
    }

    async fn start_with(
        settings: StreamSettings,
        policy: Arc<dyn RelayPolicy>,
        inventory: Arc<dyn Inventory>,
    ) -> Self {
        Self::start_with_ingest(settings, policy, inventory, Ingest::default()).await
    }

    async fn start_with_ingest(
        settings: StreamSettings,
        policy: Arc<dyn RelayPolicy>,
        inventory: Arc<dyn Inventory>,
        ingest: Ingest,
    ) -> Self {
        let ca = Ca::new("monitorflow test CA");
        let server = identity(&ca, &ca.server());
        let (_tx, tls_config) = watch::channel(server.server_config(Vec::new()).unwrap());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "https://127.0.0.1:{}",
            listener.local_addr().unwrap().port()
        );
        let registry = Registry::default();
        let service = AgentStreamService::new(registry.clone(), policy, settings)
            .with_inventory(inventory)
            .with_ingest(ingest);
        let (shutdown, stop) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(grpc::serve(listener, tls_config, service, async {
            let _ = stop.await;
        }));
        Self {
            ca,
            url,
            registry,
            shutdown,
            server,
        }
    }

    async fn default() -> Self {
        Self::start(settings(), Arc::new(DenyRelayedAgents)).await
    }

    // `Status` is boxed to keep the `Err` variant small (clippy::result_large_err).
    async fn open(&self, leaf: &Leaf, first: Option<Envelope>) -> Result<Peer, Box<Status>> {
        let config = identity(&self.ca, leaf).client_config().unwrap();
        let channel = tls::connect(&self.url, config, TIMEOUT).await.unwrap();
        let (tx, rx) = mpsc::channel(8);
        if let Some(first) = first {
            tx.send(first).await.unwrap();
        }
        let mut client = AgentServiceClient::new(channel);
        let call = client.stream(ReceiverStream::new(rx));
        let rx = tokio::time::timeout(TIMEOUT, call)
            .await
            .unwrap()
            .map_err(Box::new)?
            .into_inner();
        Ok(Peer { tx, rx })
    }

    async fn wait_until(&self, what: &str, done: impl Fn(&Registry) -> bool) {
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        while !done(&self.registry) {
            assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

struct Peer {
    tx: mpsc::Sender<Envelope>,
    rx: Streaming<Envelope>,
}

impl Peer {
    async fn send(&self, sequence: u64, payload: Payload) {
        let envelope = Envelope {
            sequence,
            payload: Some(payload),
        };
        self.tx.send(envelope).await.unwrap();
    }

    /// Next message from the server, or the status that ended the stream.
    async fn next(&mut self) -> Result<Option<Envelope>, Box<Status>> {
        tokio::time::timeout(TIMEOUT, self.rx.message())
            .await
            .unwrap()
            .map_err(Box::new)
    }
}

fn hello_v(agent_id: &str, version: &str) -> Envelope {
    Envelope {
        sequence: 1,
        payload: Some(Payload::Hello(Hello {
            agent_id: agent_id.into(),
            protocol_version: version.into(),
            agent_version: "0.1.0".into(),
            hostname: format!("{agent_id}.example"),
            os_family: "debian".into(),
            os_version: "debian 12".into(),
        })),
    }
}

fn hello(agent_id: &str) -> Option<Envelope> {
    Some(hello_v(agent_id, PROTOCOL_VERSION))
}

fn heartbeat() -> Payload {
    Payload::Heartbeat(Heartbeat { sent_at_unix_ms: 0 })
}

fn refused(result: Result<Peer, Box<Status>>) -> Box<Status> {
    match result {
        Ok(_) => panic!("stream unexpectedly accepted"),
        Err(status) => status,
    }
}

#[tokio::test]
async fn agent_registers_and_disappears_on_disconnect() {
    let h = Harness::default().await;
    let mut peer = h
        .open(&h.ca.client("agent-1", 10), hello("agent-1"))
        .await
        .unwrap();
    let info = h.registry.get("agent-1").unwrap();
    assert_eq!(info.role, PeerRole::Agent);
    assert_eq!(info.via_relay, None);
    assert_eq!(info.hostname, "agent-1.example");
    assert_eq!(info.agent_version, "0.1.0");
    assert_eq!(info.last_sequence, 1);
    assert_eq!(h.registry.connected().len(), 1);

    // Closing our side ends the stream cleanly.
    drop(peer.tx);
    assert_eq!(peer.rx.message().await.unwrap(), None);
    assert!(h.registry.get("agent-1").is_none());
}

#[tokio::test]
async fn agent_claiming_another_id_is_rejected() {
    let h = Harness::default().await;
    let status = refused(h.open(&h.ca.client("agent-1", 10), hello("agent-2")).await);
    assert_eq!(status.code(), Code::PermissionDenied);
    assert!(status.message().contains("agent-2"), "{status}");
    assert!(h.registry.connected().is_empty());
}

#[tokio::test]
async fn server_role_is_rejected() {
    let h = Harness::default().await;
    let leaf =
        h.ca.leaf("central", vec![uri_san("monitorflow://server/central")], 10);
    let status = refused(h.open(&leaf, hello("central")).await);
    assert_eq!(status.code(), Code::PermissionDenied);
}

#[tokio::test]
async fn missing_or_wrong_first_envelope_is_rejected() {
    let h = Harness::default().await;
    let leaf = h.ca.client("agent-1", 10);
    let status = refused(h.open(&leaf, None).await);
    assert_eq!(status.code(), Code::DeadlineExceeded);
    let not_hello = Envelope {
        sequence: 1,
        payload: Some(heartbeat()),
    };
    let status = refused(h.open(&leaf, Some(not_hello)).await);
    assert_eq!(status.code(), Code::InvalidArgument);
}

#[tokio::test]
async fn other_protocol_major_is_rejected() {
    let h = Harness::default().await;
    let leaf = h.ca.client("agent-1", 10);
    let status = refused(h.open(&leaf, Some(hello_v("agent-1", "2.0"))).await);
    assert_eq!(status.code(), Code::FailedPrecondition);
    assert!(status.message().contains("2.0"), "{status}");
    assert!(status.message().contains(PROTOCOL_VERSION), "{status}");
    // A newer minor of our major is fine.
    h.open(&leaf, Some(hello_v("agent-1", "1.99")))
        .await
        .unwrap();
}

#[tokio::test]
async fn duplicate_connection_replaces_the_old_one() {
    let h = Harness::default().await;
    let mut old = h
        .open(&h.ca.client("agent-1", 10), hello("agent-1"))
        .await
        .unwrap();
    let first = h.registry.get("agent-1").unwrap().connection;
    let mut new = h
        .open(&h.ca.client("agent-1", 11), hello("agent-1"))
        .await
        .unwrap();
    let second = h.registry.get("agent-1").unwrap().connection;
    assert_ne!(first, second);

    // The old stream is closed, and its teardown left the new entry alone.
    assert_eq!(old.next().await.unwrap_err().code(), Code::Aborted);
    assert_eq!(h.registry.get("agent-1").unwrap().connection, second);
    assert_eq!(h.registry.connected().len(), 1);

    // The new stream is the one that receives.
    let command = Envelope {
        sequence: 1,
        payload: Some(Payload::Command(Command::default())),
    };
    h.registry.send("agent-1", command.clone()).await.unwrap();
    assert_eq!(new.next().await.unwrap(), Some(command));
}

#[tokio::test]
async fn silent_peer_is_removed_after_heartbeat_timeout() {
    let settings = StreamSettings {
        heartbeat_timeout: Duration::from_millis(300),
        ..settings()
    };
    let h = Harness::start(settings, Arc::new(DenyRelayedAgents)).await;
    let mut peer = h
        .open(&h.ca.client("agent-1", 10), hello("agent-1"))
        .await
        .unwrap();
    // Heartbeats keep it alive past the timeout...
    for sequence in 2..6 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        peer.send(sequence, heartbeat()).await;
    }
    h.wait_until("last heartbeat", |r| {
        r.get("agent-1").unwrap().last_sequence == 5
    })
    .await;
    // ...and silence closes it.
    assert_eq!(
        peer.next().await.unwrap_err().code(),
        Code::DeadlineExceeded
    );
    assert!(h.registry.get("agent-1").is_none());
}

#[tokio::test]
async fn stale_sequence_numbers_are_ignored() {
    let h = Harness::default().await;
    let peer = h
        .open(&h.ca.client("agent-1", 10), hello("agent-1"))
        .await
        .unwrap();
    let batch = |n| {
        Payload::Samples(SampleBatch {
            samples: vec![Sample::default(); n],
        })
    };
    peer.send(2, batch(3)).await;
    peer.send(2, batch(5)).await; // duplicate
    peer.send(1, heartbeat()).await; // older than the last one
    peer.send(4, heartbeat()).await;
    h.wait_until("sequence 4", |r| {
        r.get("agent-1").unwrap().last_sequence == 4
    })
    .await;
    let counters = h.registry.get("agent-1").unwrap().counters;
    assert_eq!(counters.stale_sequences, 2);
    assert_eq!(counters.sample_batches, 1);
    assert_eq!(counters.samples, 3);
}

#[tokio::test]
async fn registry_send_reaches_the_client() {
    let h = Harness::default().await;
    let mut peer = h
        .open(&h.ca.client("agent-1", 10), hello("agent-1"))
        .await
        .unwrap();
    let command = Envelope {
        sequence: 42,
        payload: Some(Payload::Command(Command {
            command_id: "cmd-1".into(),
            ..Command::default()
        })),
    };
    h.registry.send("agent-1", command.clone()).await.unwrap();
    assert_eq!(peer.next().await.unwrap(), Some(command));
}

#[tokio::test]
async fn relay_speaks_for_itself_and_others_only_if_allowed() {
    let h = Harness::default().await;
    let _relay = h
        .open(&h.ca.relay("relay-1", 10), hello("relay-1"))
        .await
        .unwrap();
    assert_eq!(h.registry.get("relay-1").unwrap().role, PeerRole::Relay);
    let status = refused(h.open(&h.ca.relay("relay-1", 11), hello("agent-9")).await);
    assert_eq!(status.code(), Code::PermissionDenied);

    let h = Harness::start(settings(), Arc::new(AllowAllRelayedAgents)).await;
    let _relay = h
        .open(&h.ca.relay("relay-1", 10), hello("agent-9"))
        .await
        .unwrap();
    let info = h.registry.get("agent-9").unwrap();
    assert_eq!(info.role, PeerRole::Agent);
    assert_eq!(info.via_relay.as_deref(), Some("relay-1"));
}

#[tokio::test]
async fn shutdown_closes_streams_and_stops_serving() {
    let h = Harness::default().await;
    let mut peer = h
        .open(&h.ca.client("agent-1", 10), hello("agent-1"))
        .await
        .unwrap();
    h.shutdown.send(()).unwrap();
    assert_eq!(peer.next().await.unwrap_err().code(), Code::Unavailable);
    assert!(h.registry.connected().is_empty());
    let _keep = peer;
    let served = tokio::time::timeout(TIMEOUT, h.server).await.unwrap();
    served.unwrap().unwrap();
}

#[tokio::test]
async fn agent_hello_without_a_valid_os_family_is_rejected() {
    let h = Harness::start(settings(), Arc::new(AllowAllRelayedAgents)).await;
    for os_family in ["", "windows"] {
        let mut envelope = hello_v("agent-1", PROTOCOL_VERSION);
        if let Some(Payload::Hello(hello)) = envelope.payload.as_mut() {
            hello.os_family = os_family.into();
        }
        let status = refused(
            h.open(&h.ca.client("agent-1", 10), Some(envelope.clone()))
                .await,
        );
        assert_eq!(status.code(), Code::InvalidArgument);
        assert!(status.message().contains("os_family"), "{status}");
        // Also when a relay forwards it...
        if let Some(Payload::Hello(hello)) = envelope.payload.as_mut() {
            hello.agent_id = "agent-9".into();
        }
        let status = refused(h.open(&h.ca.relay("relay-1", 10), Some(envelope)).await);
        assert_eq!(status.code(), Code::InvalidArgument);
    }
    // ...but a relay's own Hello has no OS to report.
    let mut own = hello_v("relay-1", PROTOCOL_VERSION);
    if let Some(Payload::Hello(hello)) = own.payload.as_mut() {
        hello.os_family.clear();
    }
    h.open(&h.ca.relay("relay-1", 11), Some(own)).await.unwrap();
    assert!(h.registry.get("agent-1").is_none());
}

/// Puts every peer in one site, without a database.
struct OneSite(&'static str);

#[tonic::async_trait]
impl Inventory for OneSite {
    async fn hello(&self, _hello: AcceptedHello<'_>) -> Result<String, InventoryError> {
        Ok(self.0.to_owned())
    }

    async fn seen(&self, _role: PeerRole, _id: &str) -> Result<(), InventoryError> {
        Ok(())
    }
}

/// Writers to a fake VictoriaMetrics and VictoriaLogs.
struct Storage {
    fake: FakeHttp,
    metrics: Writer<Series>,
    events: Writer<LogRecord>,
}

impl Storage {
    async fn start() -> Self {
        let fake = FakeHttp::start().await;
        let settings = WriterSettings {
            flush_interval: Duration::from_millis(20),
            ..WriterSettings::default()
        };
        let metrics = Writer::spawn(RemoteWrite::new(&fake.url).unwrap(), settings.clone());
        let events = Writer::spawn(JsonLines::new(&fake.url).unwrap(), settings);
        Self {
            fake,
            metrics,
            events,
        }
    }

    fn ingest(&self) -> Ingest {
        Ingest {
            metrics: Some(self.metrics.queue()),
            events: Some(self.events.queue()),
        }
    }

    /// Label sets of every series written so far.
    fn series(&self) -> Vec<Vec<(String, String)>> {
        let mut series = Vec::new();
        for request in self.fake.received() {
            if request.path_and_query == "/api/v1/write" {
                for ts in decode(&request.body).unwrap().timeseries {
                    series.push(ts.labels.into_iter().map(|l| (l.name, l.value)).collect());
                }
            }
        }
        series
    }

    async fn wait_for_series(&self, n: usize) -> Vec<Vec<(String, String)>> {
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        loop {
            let series = self.series();
            if series.len() >= n {
                return series;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "{n} series never arrived"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// The JSON log lines written so far.
    fn events(&self) -> Vec<serde_json::Value> {
        self.fake
            .received()
            .iter()
            .filter(|r| r.path_and_query.starts_with("/insert/jsonline"))
            .flat_map(|r| {
                std::str::from_utf8(&r.body)
                    .unwrap()
                    .lines()
                    .map(|l| serde_json::from_str(l).unwrap())
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    async fn wait_for_events(&self, n: usize) -> Vec<serde_json::Value> {
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        loop {
            let events = self.events();
            if events.len() >= n {
                return events;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "{n} events never arrived"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

fn samples(entries: &[(&str, &[(&str, &str)])]) -> Payload {
    let now = chrono::Utc::now().timestamp_millis();
    Payload::Samples(SampleBatch {
        samples: entries
            .iter()
            .map(|(sensor, labels)| Sample {
                sensor: sensor.to_string(),
                instance: "/".into(),
                timestamp_unix_ms: now,
                value: 1.0,
                labels: labels
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            })
            .collect(),
    })
}

fn owned(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[tokio::test]
async fn samples_are_written_with_the_identity_of_the_stream() {
    let storage = Storage::start().await;
    let h = Harness::start_with_ingest(
        settings(),
        Arc::new(AllowAllRelayedAgents),
        Arc::new(OneSite("paris")),
        storage.ingest(),
    )
    .await;
    let agent = h
        .open(&h.ca.client("agent-1", 10), hello("agent-1"))
        .await
        .unwrap();
    agent
        .send(
            2,
            samples(&[
                ("disk_free_bytes", &[("device", "sda")]),
                // Forged identity labels drop the sample.
                ("disk_free_bytes", &[("peer", "agent-9")]),
                ("disk_free_bytes", &[("site", "lyon")]),
                ("Not-A-Sensor", &[]),
            ]),
        )
        .await;
    let series = storage.wait_for_series(1).await;
    assert_eq!(
        series,
        [owned(&[
            ("__name__", "monitorflow_disk_free_bytes"),
            ("device", "sda"),
            ("instance", "/"),
            ("peer", "agent-1"),
            ("site", "paris"),
        ])]
    );
    h.wait_until("invalid samples counted", |r| {
        r.get("agent-1").unwrap().counters.invalid_samples == 3
    })
    .await;
    let counters = h.registry.get("agent-1").unwrap().counters;
    assert_eq!((counters.samples, counters.dropped_samples), (4, 0));

    // Behind a relay, the series belong to the agent the relay speaks for.
    let relayed = h
        .open(&h.ca.relay("relay-1", 11), hello("agent-2"))
        .await
        .unwrap();
    relayed.send(2, samples(&[("cpu", &[])])).await;
    let series = storage.wait_for_series(2).await;
    assert!(
        series[1].contains(&("peer".to_string(), "agent-2".to_string())),
        "{series:?}"
    );

    drop(agent);
    let events = storage.wait_for_events(3).await;
    assert_eq!(events[0]["event"], "peer.connect");
    assert_eq!(events[0]["peer"], "agent-1");
    assert_eq!(events[0]["site"], "paris");
    assert_eq!(events[0]["role"], "agent");
    assert_eq!(events[1]["relay"], "relay-1");
    assert_eq!(events[2]["event"], "peer.disconnect");
    assert_eq!(events[2]["peer"], "agent-1");
    assert_eq!(events[2]["reason"], "closed");

    let stats = storage.metrics.shutdown(TIMEOUT).await;
    assert_eq!((stats.written, stats.rejected), (2, 3));
}

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

/// A consumed agent enrollment token, as `Enroller::enroll` leaves it.
async fn enrolled(pool: &PgPool, peer_id: &str, site_id: Uuid) {
    sqlx::query(
        "INSERT INTO enrollment_tokens
             (token_hash, role, peer_id, site_id, expires_at, used_at, used_by_peer_id)
         VALUES (sha256(gen_random_uuid()::text::bytea), 'agent', $1, $2,
                 now() + interval '1 hour', clock_timestamp(), $1)",
    )
    .bind(peer_id)
    .bind(site_id)
    .execute(pool)
    .await
    .unwrap();
}

#[derive(Debug, sqlx::FromRow)]
struct ServerRow {
    id: Uuid,
    site_id: Uuid,
    relay_id: Option<Uuid>,
    hostname: String,
    os_family: String,
    os_version: Option<String>,
    agent_version: Option<String>,
    last_seen_at: Option<chrono::DateTime<chrono::Utc>>,
}

async fn server_row(pool: &PgPool, peer_id: &str) -> Option<ServerRow> {
    sqlx::query_as(
        "SELECT id, site_id, relay_id, hostname, os_family, os_version, agent_version, last_seen_at
         FROM servers WHERE peer_id = $1",
    )
    .bind(peer_id)
    .fetch_optional(pool)
    .await
    .unwrap()
}

async fn audit_count(pool: &PgPool, action: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = $1")
        .bind(action)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn db_harness(pool: &PgPool, settings: StreamSettings) -> Harness {
    Harness::start_with(
        settings,
        Arc::new(DbRelayPolicy::new(pool.clone())),
        Arc::new(DbInventory::new(pool.clone())),
    )
    .await
}

fn with_hello(agent_id: &str, edit: impl FnOnce(&mut Hello)) -> Option<Envelope> {
    let mut envelope = hello_v(agent_id, PROTOCOL_VERSION);
    if let Some(Payload::Hello(hello)) = envelope.payload.as_mut() {
        edit(hello);
    }
    Some(envelope)
}

db_tests! {
    #[sqlx::test]
    async fn hello_creates_then_updates_the_server_row_in_its_site(pool: PgPool) {
        let paris = site(&pool, "paris").await;
        let lyon = site(&pool, "lyon").await;
        enrolled(&pool, "agent-1", paris).await;
        let h = db_harness(&pool, settings()).await;

        let peer = h.open(&h.ca.client("agent-1", 10), hello("agent-1")).await.unwrap();
        let row = server_row(&pool, "agent-1").await.unwrap();
        assert_eq!(row.site_id, paris);
        assert_eq!(row.relay_id, None);
        assert_eq!(row.hostname, "agent-1.example");
        assert_eq!(row.os_family, "debian");
        assert_eq!(row.os_version.as_deref(), Some("debian 12"));
        assert_eq!(row.agent_version.as_deref(), Some("0.1.0"));
        assert!(row.last_seen_at.is_some());
        assert_eq!(audit_count(&pool, "server.create").await, 1);
        drop(peer);

        // A later token for another site does not move an existing row.
        enrolled(&pool, "agent-1", lyon).await;
        let second = with_hello("agent-1", |hello| {
            hello.hostname = "web-1".into();
            hello.os_family = "rhel".into();
            hello.os_version = String::new();
            hello.agent_version = "0.2.0".into();
        });
        let _peer = h.open(&h.ca.client("agent-1", 11), second).await.unwrap();
        let updated = server_row(&pool, "agent-1").await.unwrap();
        assert_eq!(updated.id, row.id);
        assert_eq!(updated.site_id, paris);
        assert_eq!(updated.hostname, "web-1");
        assert_eq!(updated.os_family, "rhel");
        assert_eq!(updated.os_version, None);
        assert_eq!(updated.agent_version.as_deref(), Some("0.2.0"));
        assert!(updated.last_seen_at >= row.last_seen_at);
        assert_eq!(audit_count(&pool, "server.create").await, 1);
    }

    #[sqlx::test]
    async fn unenrolled_or_invalid_agents_get_no_row(pool: PgPool) {
        let paris = site(&pool, "paris").await;
        let h = db_harness(&pool, settings()).await;
        let status = refused(h.open(&h.ca.client("agent-1", 10), hello("agent-1")).await);
        assert_eq!(status.code(), Code::PermissionDenied);
        assert!(status.message().contains("not enrolled"), "{status}");

        enrolled(&pool, "agent-1", paris).await;
        let bad = with_hello("agent-1", |hello| hello.os_family = "arch".into());
        let status = refused(h.open(&h.ca.client("agent-1", 10), bad).await);
        assert_eq!(status.code(), Code::InvalidArgument);
        assert!(server_row(&pool, "agent-1").await.is_none());
        assert!(h.registry.connected().is_empty());
    }

    #[sqlx::test]
    async fn revoked_rows_refuse_valid_certificates(pool: PgPool) {
        let paris = site(&pool, "paris").await;
        enrolled(&pool, "agent-1", paris).await;
        sqlx::query("INSERT INTO relays (peer_id, site_id, name) VALUES ('relay-1', $1, 'relay-1')")
            .bind(paris)
            .execute(&pool)
            .await
            .unwrap();
        let h = db_harness(&pool, settings()).await;
        drop(h.open(&h.ca.client("agent-1", 10), hello("agent-1")).await.unwrap());
        drop(h.open(&h.ca.relay("relay-1", 10), hello("relay-1")).await.unwrap());

        sqlx::query("UPDATE servers SET revoked_at = now()").execute(&pool).await.unwrap();
        sqlx::query("UPDATE relays SET revoked_at = now()").execute(&pool).await.unwrap();
        for (leaf, id) in [(h.ca.client("agent-1", 11), "agent-1"), (h.ca.relay("relay-1", 11), "relay-1")] {
            let status = refused(h.open(&leaf, hello(id)).await);
            assert_eq!(status.code(), Code::PermissionDenied);
            assert!(status.message().contains("revoked"), "{status}");
        }
        // A relay with no row at all is not enrolled.
        let status = refused(h.open(&h.ca.relay("relay-2", 10), hello("relay-2")).await);
        assert!(status.message().contains("not enrolled"), "{status}");
    }

    #[sqlx::test]
    async fn last_seen_at_is_written_at_most_once_per_interval(pool: PgPool) {
        let paris = site(&pool, "paris").await;
        enrolled(&pool, "agent-1", paris).await;
        enrolled(&pool, "agent-2", paris).await;
        let set_old = || async {
            sqlx::query("UPDATE servers SET last_seen_at = '2020-01-01T00:00:00Z'")
                .execute(&pool)
                .await
                .unwrap();
        };
        let old = chrono::DateTime::parse_from_rfc3339("2020-01-01T00:00:00Z").unwrap();

        // An hour between writes: heartbeats do not touch the row.
        let h = db_harness(&pool, settings()).await;
        let peer = h.open(&h.ca.client("agent-1", 10), hello("agent-1")).await.unwrap();
        set_old().await;
        for sequence in 2..12 {
            peer.send(sequence, heartbeat()).await;
        }
        h.wait_until("heartbeats", |r| r.get("agent-1").unwrap().last_sequence == 11).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(server_row(&pool, "agent-1").await.unwrap().last_seen_at, Some(old.into()));

        // No minimum interval: the next heartbeat writes it.
        let h = db_harness(&pool, StreamSettings { last_seen_interval: Duration::ZERO, ..settings() }).await;
        let peer = h.open(&h.ca.client("agent-2", 10), hello("agent-2")).await.unwrap();
        set_old().await;
        peer.send(2, heartbeat()).await;
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        while server_row(&pool, "agent-2").await.unwrap().last_seen_at == Some(old.into()) {
            assert!(tokio::time::Instant::now() < deadline, "last_seen_at never updated");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[sqlx::test]
    async fn series_get_the_site_of_the_inventory_row(pool: PgPool) {
        let paris = site(&pool, "paris").await;
        enrolled(&pool, "agent-1", paris).await;
        let storage = Storage::start().await;
        let h = Harness::start_with_ingest(
            settings(),
            Arc::new(DbRelayPolicy::new(pool.clone())),
            Arc::new(DbInventory::new(pool.clone())),
            storage.ingest(),
        )
        .await;
        // First Hello creates the row, the second reads it back.
        for sequence in [10, 20] {
            let peer = h.open(&h.ca.client("agent-1", 10), hello("agent-1")).await.unwrap();
            peer.send(sequence, samples(&[("cpu", &[])])).await;
            let series = storage.wait_for_series(sequence as usize / 10).await;
            let last = series.last().unwrap();
            assert!(last.contains(&("site".to_string(), "paris".to_string())), "{last:?}");
        }
    }

    #[sqlx::test]
    async fn relayed_agent_gets_its_row_with_the_relay(pool: PgPool) {
        let paris = site(&pool, "paris").await;
        let lyon = site(&pool, "lyon").await;
        let relay_id: Uuid = sqlx::query_scalar(
            "INSERT INTO relays (peer_id, site_id, name) VALUES ('relay-1', $1, 'relay-1') RETURNING id",
        )
        .bind(paris)
        .fetch_one(&pool)
        .await
        .unwrap();
        enrolled(&pool, "agent-1", paris).await;
        enrolled(&pool, "agent-2", lyon).await;
        let h = db_harness(&pool, settings()).await;

        // First Hello through the relay: the agent has no row yet.
        let _peer = h.open(&h.ca.relay("relay-1", 10), hello("agent-1")).await.unwrap();
        let row = server_row(&pool, "agent-1").await.unwrap();
        assert_eq!((row.site_id, row.relay_id), (paris, Some(relay_id)));
        assert_eq!(h.registry.get("agent-1").unwrap().via_relay.as_deref(), Some("relay-1"));

        // An agent of another site is refused and gets no row.
        let status = refused(h.open(&h.ca.relay("relay-1", 11), hello("agent-2")).await);
        assert_eq!(status.code(), Code::PermissionDenied);
        assert!(server_row(&pool, "agent-2").await.is_none());

        // Connecting directly clears the relay.
        let _direct = h.open(&h.ca.client("agent-1", 10), hello("agent-1")).await.unwrap();
        assert_eq!(server_row(&pool, "agent-1").await.unwrap().relay_id, None);
    }
}
