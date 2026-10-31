//! Sensor configuration and sample upload against the real server pieces
//! in-process over mTLS: batching by size and by time, the saved
//! configuration at start-up, and, with a database, a configured sensor's
//! samples reaching a fake VictoriaMetrics with the agent's identity.

// The real server and the Linux helpers only build on Unix (ADR-0047).
#![cfg(unix)]

#[path = "../../server/tests/fake_http/mod.rs"]
mod fake_http;
mod support;

use std::sync::Arc;
use std::time::Duration;

use fake_http::FakeHttp;
use monitorflow_agent::os::{HostInfo, OsFamily};
use monitorflow_agent::sensor_config::{self, ConfigFile, FactoryError, SensorFactory};
use monitorflow_agent::sensors::Sensors;
use monitorflow_agent::shutdown::{self, Trigger};
use monitorflow_agent::stream::{Agent, StreamSettings};
use monitorflow_agent::upload::{Uplink, UploadSettings};
use monitorflow_common::proto::v1::{SensorConfig, SensorSpec};
use monitorflow_sensors::testing::FakeSensor;
use monitorflow_sensors::{SchedulerOptions, Sensor, SensorConfig as Config};
use monitorflow_server::grpc::{
    self, AgentStreamService, DbInventory, DbRelayPolicy, DenyRelayedAgents, Registry,
};
use monitorflow_server::ingest::metrics::decode;
use monitorflow_server::ingest::{Ingest, RemoteWrite, Writer, WriterSettings};
use monitorflow_server::sensor_config::{SensorConfigStore, SensorConfigs, WatcherSettings};
use sqlx::PgPool;
use support::{agent_config, Pki, Upstream};
use tempfile::TempDir;

const TIMEOUT: Duration = Duration::from_secs(10);

/// Builds a [`FakeSensor`] for the kind `fake`; knows no other kind.
struct Fakes;

impl SensorFactory for Fakes {
    fn build(&self, kind: &str, _config: &Config) -> Result<Arc<dyn Sensor>, FactoryError> {
        match kind {
            "fake" => Ok(Arc::new(FakeSensor::new())),
            other => Err(FactoryError::UnknownKind(other.into())),
        }
    }
}

fn server_settings() -> grpc::StreamSettings {
    grpc::StreamSettings {
        heartbeat_timeout: TIMEOUT,
        ..grpc::StreamSettings::default()
    }
}

/// `n` fake sensors collecting every second.
fn fakes(n: usize) -> SensorConfig {
    SensorConfig {
        revision: 1,
        sensors: (0..n)
            .map(|i| SensorSpec {
                kind: "fake".into(),
                name: format!("fake_{i}"),
                interval_ms: 1_000,
                ..SensorSpec::default()
            })
            .collect(),
    }
}

/// A running agent: sensors task and stream.
struct Running {
    dir: TempDir,
    trigger: Trigger,
}

impl Running {
    async fn start(pki: &Pki, upstream: &Upstream, id: &str, upload: UploadSettings) -> Self {
        Self::start_with(pki, upstream, id, upload, None).await
    }

    /// Starts an agent `id`; `saved` is written to its data directory first.
    async fn start_with(
        pki: &Pki,
        upstream: &Upstream,
        id: &str,
        upload: UploadSettings,
        saved: Option<SensorConfig>,
    ) -> Self {
        let dir = TempDir::new().unwrap();
        let config = agent_config(dir.path().into(), &upstream.url, "https://127.0.0.1:1");
        let (identity, _) = pki.agent_identity(&config, id);
        let file = ConfigFile::in_dir(&config.data_dir);
        if let Some(saved) = saved {
            file.save(&saved).await.unwrap();
        }
        let uplink = Uplink::new(upload);
        let (stream_end, sensors_end) = sensor_config::link();
        let (trigger, stop) = shutdown::channel();
        let sensors =
            Sensors::new(Arc::new(Fakes), file, uplink.clone()).with_options(SchedulerOptions {
                jitter: false,
                ..SchedulerOptions::default()
            });
        tokio::spawn(sensors.run(sensors_end, stop.clone()));
        let host = HostInfo {
            hostname: format!("{id}.example"),
            os_family: OsFamily::Debian,
            os_version: "debian 12".into(),
            package_manager: true,
        };
        let agent = Agent::new(StreamSettings::from_config(&config), identity, host)
            .with_sensors(stream_end)
            .with_upload(uplink);
        tokio::spawn(agent.run(stop));
        Self { dir, trigger }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.trigger.fire();
    }
}

async fn eventually<T>(what: &str, mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        if let Some(value) = check() {
            return value;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// `(sample_batches, samples)` received from `id`.
fn received(registry: &Registry, id: &str) -> (u64, u64) {
    registry
        .get(id)
        .map_or((0, 0), |p| (p.counters.sample_batches, p.counters.samples))
}

async fn plain_upstream(pki: &Pki) -> Upstream {
    pki.serve_service(|registry| {
        AgentStreamService::new(registry, Arc::new(DenyRelayedAgents), server_settings())
    })
    .await
}

#[tokio::test]
async fn full_batches_go_at_once_and_the_saved_configuration_runs_at_start_up() {
    let pki = Pki::new();
    let upstream = plain_upstream(&pki).await;
    // Six samples a second, batches of three, a flush far away: only full
    // batches can go. The server sends no configuration: the agent runs
    // the one saved in its data directory.
    let upload = UploadSettings {
        batch_size: 3,
        flush_interval: Duration::from_secs(3600),
        capacity: 100,
    };
    let _agent = Running::start_with(&pki, &upstream, "web-01", upload, Some(fakes(6))).await;
    let (batches, samples) = eventually("12 samples", || {
        Some(received(&upstream.registry, "web-01")).filter(|&(_, s)| s >= 12)
    })
    .await;
    assert_eq!(
        samples,
        batches * 3,
        "{batches} batches for {samples} samples"
    );
}

#[tokio::test]
async fn partial_batches_go_at_the_flush_interval() {
    let pki = Pki::new();
    let upstream = plain_upstream(&pki).await;
    let upload = UploadSettings {
        batch_size: 500,
        flush_interval: Duration::from_millis(300),
        capacity: 1_000,
    };
    let _agent = Running::start_with(&pki, &upstream, "web-01", upload, Some(fakes(1))).await;
    let (batches, samples) = eventually("two batches", || {
        Some(received(&upstream.registry, "web-01")).filter(|&(b, _)| b >= 2)
    })
    .await;
    assert!((2..500).contains(&samples), "{samples} samples");
    assert!(batches <= samples);
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

db_tests! {
    #[sqlx::test(migrations = "../server/migrations")]
    async fn configured_samples_reach_victoriametrics_with_the_agent_identity(pool: PgPool) {
        sqlx::query("INSERT INTO sites (name) VALUES ('paris')").execute(&pool).await.unwrap();
        // A consumed token, as enrollment leaves it.
        sqlx::query(
            "INSERT INTO enrollment_tokens
                 (token_hash, role, peer_id, site_id, expires_at, used_at, used_by_peer_id)
             SELECT sha256('t'::bytea), 'agent', 'web-01', id, now() + interval '1 hour', now(), 'web-01'
             FROM sites",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO sensor_configs (site_id, kind, name, instance, interval_ms)
             SELECT id, 'fake', 'fake_metric', 'core0', 1000 FROM sites",
        )
        .execute(&pool)
        .await
        .unwrap();

        let fake = FakeHttp::start().await;
        let writer_settings = WriterSettings {
            flush_interval: Duration::from_millis(20),
            ..WriterSettings::default()
        };
        let metrics = Writer::spawn(RemoteWrite::new(&fake.url).unwrap(), writer_settings);
        let pki = Pki::new();
        let upstream = pki
            .serve_service(|registry| {
                let watcher = WatcherSettings {
                    poll_interval: Duration::from_millis(100),
                    ..WatcherSettings::default()
                };
                let configs =
                    SensorConfigs::spawn(SensorConfigStore::new(pool.clone()), registry.clone(), watcher);
                AgentStreamService::new(registry, Arc::new(DbRelayPolicy::new(pool.clone())), server_settings())
                    .with_inventory(Arc::new(DbInventory::new(pool.clone())))
                    .with_ingest(Ingest { metrics: Some(metrics.queue()), events: None })
                    .with_sensor_configs(configs)
            })
            .await;
        let upload = UploadSettings {
            flush_interval: Duration::from_millis(200),
            ..UploadSettings::default()
        };
        let agent = Running::start(&pki, &upstream, "web-01", upload).await;

        let labels = eventually("a series in VictoriaMetrics", || {
            fake.received()
                .iter()
                .filter(|r| r.path_and_query == "/api/v1/write")
                .flat_map(|r| decode(&r.body).unwrap().timeseries)
                .map(|ts| ts.labels.into_iter().map(|l| (l.name, l.value)).collect::<Vec<_>>())
                .next()
        })
        .await;
        let owned = |pairs: &[(&str, &str)]| {
            pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<Vec<_>>()
        };
        assert_eq!(
            labels,
            owned(&[
                ("__name__", "monitorflow_fake_metric"),
                ("instance", "core0"),
                ("peer", "web-01"),
                ("site", "paris"),
            ])
        );

        // The agent acknowledged the revision, refusing the built-in
        // defaults it has no implementation for, and saved the config.
        let revision: i64 = sqlx::query_scalar(
            "SELECT si.sensor_revision + s.sensor_revision FROM servers s JOIN sites si ON si.id = s.site_id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        loop {
            let applied: Option<i64> = sqlx::query_scalar("SELECT sensor_revision_applied FROM servers")
                .fetch_one(&pool)
                .await
                .unwrap();
            if applied == Some(revision) {
                break;
            }
            assert!(tokio::time::Instant::now() < deadline, "ack never recorded");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let saved = ConfigFile::in_dir(agent.dir.path()).load().await.unwrap();
        assert_eq!(saved.revision, revision as u64);
        let kinds: Vec<_> = saved.sensors.iter().map(|s| s.kind.as_str()).collect();
        assert_eq!(kinds, ["cpu", "disk", "fake", "memory"]);
    }
}
