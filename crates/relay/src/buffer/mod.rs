//! Store-and-forward buffer (ADR-0027): `SampleBatch` envelopes kept on
//! disk while an agent has no working upstream stream, replayed oldest
//! first once it has one.
//!
//! [`Buffer`] runs the synchronous [`Store`] on the blocking pool, and
//! [`Buffer::run`] syncs it to disk every `fsync_interval`, drops expired
//! segments and logs its counters.

use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use monitorflow_common::client::shutdown::Shutdown;
use monitorflow_common::config::RelayConfig;
use monitorflow_common::proto::v1::Envelope;
use prost::Message;
use tokio::time::{interval, MissedTickBehavior};

pub mod store;

pub use store::{Limits, Stats, Store};

/// How often expired segments are dropped and counters logged.
const HOUSEKEEPING_EVERY: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub struct BufferSettings {
    pub dir: PathBuf,
    pub limits: Limits,
    /// Zero syncs after every record.
    pub fsync_interval: Duration,
}

impl BufferSettings {
    pub fn from_config(config: &RelayConfig) -> Self {
        Self {
            dir: config.buffer_dir(),
            limits: Limits::new(
                config.buffer.max_bytes,
                Duration::from_secs(config.buffer.max_age),
            ),
            fsync_interval: Duration::from_millis(config.buffer.fsync_interval_ms),
        }
    }
}

/// Shared handle on the store. Cheap to clone.
#[derive(Clone)]
pub struct Buffer {
    store: Arc<Mutex<Store>>,
    fsync_interval: Duration,
}

impl Buffer {
    /// Opens the store, recovering torn records, on the blocking pool.
    pub async fn open(settings: &BufferSettings) -> io::Result<Self> {
        let (dir, limits) = (settings.dir.clone(), settings.limits);
        let store = tokio::task::spawn_blocking(move || Store::open(&dir, limits))
            .await
            .map_err(io::Error::other)??;
        Ok(Self {
            store: Arc::new(Mutex::new(store)),
            fsync_interval: settings.fsync_interval,
        })
    }

    async fn blocking<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Store) -> io::Result<T> + Send + 'static,
    ) -> io::Result<T> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            let mut store = store.lock().unwrap_or_else(|e| e.into_inner());
            f(&mut store)
        })
        .await
        .map_err(io::Error::other)?
    }

    pub async fn set_hello(&self, agent: &str, hello: &Envelope) -> io::Result<()> {
        let (agent, bytes) = (agent.to_owned(), hello.encode_to_vec());
        self.blocking(move |s| s.set_hello(&agent, &bytes)).await
    }

    /// The agent's stored `Hello`, unless missing or unreadable.
    pub async fn hello(&self, agent: &str) -> io::Result<Option<Envelope>> {
        let agent = agent.to_owned();
        let bytes = self
            .blocking(move |s| Ok(s.hello(&agent).map(<[u8]>::to_vec)))
            .await?;
        Ok(bytes.and_then(|b| Envelope::decode(b.as_slice()).ok()))
    }

    /// Appends `envelope`; false if it was too large to keep.
    pub async fn append(&self, agent: &str, envelope: &Envelope) -> io::Result<bool> {
        let (agent, bytes) = (agent.to_owned(), envelope.encode_to_vec());
        let sync = self.fsync_interval.is_zero();
        self.blocking(move |s| {
            let kept = s.append(&agent, unix_ms(), &bytes)?;
            if sync {
                s.sync()?;
            }
            Ok(kept)
        })
        .await
    }

    /// Up to `max` envelopes of `agent` after its cursor, oldest first.
    pub async fn read(&self, agent: &str, max: usize) -> io::Result<Vec<Envelope>> {
        let id = agent.to_owned();
        let records = self.blocking(move |s| s.read(&id, unix_ms(), max)).await?;
        Ok(records
            .into_iter()
            .filter_map(|bytes| match Envelope::decode(bytes.as_slice()) {
                Ok(envelope) => Some(envelope),
                Err(error) => {
                    tracing::warn!(agent_id = agent, %error, "skipping an undecodable record in the buffer");
                    None
                }
            })
            .collect())
    }

    pub async fn commit(&self, agent: &str) -> io::Result<()> {
        let agent = agent.to_owned();
        self.blocking(move |s| s.commit(&agent)).await
    }

    pub async fn rewind(&self, agent: &str) -> io::Result<()> {
        let agent = agent.to_owned();
        self.blocking(move |s| {
            s.rewind(&agent);
            Ok(())
        })
        .await
    }

    /// Deletes the agent's records; returns how many there were.
    pub async fn discard(&self, agent: &str) -> io::Result<u64> {
        let agent = agent.to_owned();
        self.blocking(move |s| s.discard(&agent)).await
    }

    pub async fn has_pending(&self, agent: &str) -> io::Result<bool> {
        let agent = agent.to_owned();
        self.blocking(move |s| Ok(s.has_pending(&agent))).await
    }

    /// Agents with records to send.
    pub async fn pending(&self) -> io::Result<Vec<String>> {
        self.blocking(|s| Ok(s.pending())).await
    }

    pub async fn stats(&self) -> io::Result<Stats> {
        self.blocking(|s| Ok(s.stats())).await
    }

    /// Syncs, expires and logs until `shutdown`, then syncs once more.
    pub async fn run(self, mut shutdown: Shutdown) {
        let every = if self.fsync_interval.is_zero() {
            HOUSEKEEPING_EVERY
        } else {
            self.fsync_interval
        };
        let mut sync = interval(every);
        sync.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut housekeeping = interval(HOUSEKEEPING_EVERY);
        housekeeping.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut logged = Stats::default();
        loop {
            tokio::select! {
                () = shutdown.wait() => break,
                _ = sync.tick() => {
                    if let Err(error) = self.blocking(Store::sync).await {
                        tracing::error!(%error, "cannot sync the buffer to disk");
                    }
                }
                _ = housekeeping.tick() => {
                    let expired = self.blocking(|s| s.expire(unix_ms())).await;
                    if let Err(error) = expired {
                        tracing::error!(%error, "cannot drop expired buffer segments");
                    }
                    if let Ok(stats) = self.stats().await {
                        log_stats(&stats, &logged);
                        logged = stats;
                    }
                }
            }
        }
        if let Err(error) = self.blocking(Store::sync).await {
            tracing::error!(%error, "cannot sync the buffer to disk at shutdown");
        }
    }
}

/// A warning when records were dropped since `last`, otherwise the state
/// while the buffer holds anything.
fn log_stats(stats: &Stats, last: &Stats) {
    if stats.dropped() > last.dropped() {
        tracing::warn!(
            bytes = stats.bytes,
            dropped_full = stats.dropped_full,
            dropped_expired = stats.dropped_expired,
            dropped_refused = stats.dropped_refused,
            "buffer dropped records"
        );
    } else if stats.bytes > 0 || stats.appended > last.appended {
        tracing::info!(
            bytes = stats.bytes,
            segments = stats.segments,
            appended = stats.appended,
            "buffer"
        );
    }
}

fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use monitorflow_common::client::shutdown;
    use monitorflow_common::proto::v1::envelope::Payload;
    use monitorflow_common::proto::v1::{Hello, Sample, SampleBatch};

    use super::*;

    fn batch(sequence: u64) -> Envelope {
        Envelope {
            sequence,
            payload: Some(Payload::Samples(SampleBatch {
                samples: vec![Sample {
                    sensor: "load1".into(),
                    timestamp_unix_ms: sequence as i64,
                    ..Sample::default()
                }],
            })),
        }
    }

    #[test]
    fn settings_follow_the_config() {
        let mut config = RelayConfig::default();
        config.buffer.max_bytes = 8 * 1024 * 1024;
        config.buffer.max_age = 60;
        let settings = BufferSettings::from_config(&config);
        assert_eq!(settings.dir, Path::new("/var/lib/monitorflow/relay/buffer"));
        assert_eq!(settings.limits.segment_bytes, 1024 * 1024);
        assert_eq!(settings.limits.max_age, Duration::from_secs(60));
        assert_eq!(settings.fsync_interval, Duration::from_secs(1));
    }

    #[tokio::test]
    async fn envelopes_round_trip_through_the_disk() {
        let dir = tempfile::TempDir::new().unwrap();
        let settings = BufferSettings {
            dir: dir.path().join("buffer"),
            limits: Limits::new(1 << 20, Duration::from_secs(3600)),
            fsync_interval: Duration::ZERO,
        };
        let hello = Envelope {
            sequence: 1,
            payload: Some(Payload::Hello(Hello {
                agent_id: "web-01".into(),
                ..Hello::default()
            })),
        };
        {
            let buffer = Buffer::open(&settings).await.unwrap();
            buffer.set_hello("web-01", &hello).await.unwrap();
            for sequence in 2..5 {
                assert!(buffer.append("web-01", &batch(sequence)).await.unwrap());
            }
            let (trigger, stop) = shutdown::channel();
            let task = tokio::spawn(buffer.clone().run(stop));
            drop(trigger);
            task.await.unwrap();
        }
        let buffer = Buffer::open(&settings).await.unwrap();
        assert_eq!(buffer.pending().await.unwrap(), ["web-01"]);
        assert_eq!(buffer.hello("web-01").await.unwrap(), Some(hello));
        let read = buffer.read("web-01", 10).await.unwrap();
        assert_eq!(read, (2..5).map(batch).collect::<Vec<_>>());
        buffer.commit("web-01").await.unwrap();
        assert!(!buffer.has_pending("web-01").await.unwrap());
        assert_eq!(buffer.stats().await.unwrap().bytes, 0);
    }
}
