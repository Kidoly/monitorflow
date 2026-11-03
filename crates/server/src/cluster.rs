//! Several server instances on one database (ADR-0089).
//!
//! Each process joins with a row of `server_instances` and, every
//! [`Settings::heartbeat`]:
//!
//! - refreshes that row, and removes instances silent for
//!   [`Settings::dead_after`] with the streams they held;
//! - takes or renews the leader lease. The leader alone runs the loops
//!   that [`lead`] wraps; it stops them itself when it could not renew in
//!   time, before another instance may take the lease;
//! - writes the streams it holds to `peer_streams` and reads the others',
//!   so its registry sees the whole fleet ([`Registry::live`]);
//! - revokes in its own copy of the CA what another instance revoked, and
//!   closes those peers' streams.
//!
//! It also listens for commands queued anywhere, so the instance holding
//! the peer's stream sends them at once; the dispatchers' polls remain the
//! fallback. A single server is an instance that always leads.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use chrono::{DateTime, Utc};
use monitorflow_common::tls::PeerRole;
use sqlx::postgres::PgListener;
use sqlx::PgPool;
use tokio::sync::watch;
use tokio::time::{interval, sleep, sleep_until, Instant, MissedTickBehavior};
use uuid::Uuid;

use crate::ca::{RevocationReason, Serial};
use crate::commands::Commands;
use crate::crl::CrlPublisher;
use crate::grpc::{CloseReason, Counters, PeerInfo, Registry};
use crate::relay_commands::RelayCommands;

/// The lease of the loops [`lead`] runs.
const LEADER: &str = "leader";
/// Payload: the agent ID. Sent by a trigger on `commands`.
pub const COMMANDS_CHANNEL: &str = "monitorflow_commands";
/// Payload: the relay ID. Sent by a trigger on `relay_commands`.
pub const RELAY_COMMANDS_CHANNEL: &str = "monitorflow_relay_commands";

#[derive(Debug, Clone)]
pub struct Settings {
    /// Time between two ticks.
    pub heartbeat: Duration,
    /// An instance whose heartbeat is older is gone, and so are its streams.
    pub dead_after: Duration,
    /// How long the leader lease lasts without a renewal. The leader stops
    /// leading one heartbeat before.
    pub leader_ttl: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            heartbeat: Duration::from_secs(5),
            dead_after: Duration::from_secs(30),
            leader_ttl: Duration::from_secs(30),
        }
    }
}

/// What the instance wakes or updates besides its registry.
#[derive(Clone, Default)]
pub struct Links {
    pub commands: Option<Commands>,
    pub relay_commands: Option<RelayCommands>,
    pub crl: Option<Arc<CrlPublisher>>,
}

/// A stream as written to `peer_streams`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Stream {
    connected_at: DateTime<Utc>,
    via_relay: Option<String>,
    agent_version: String,
    hostname: String,
}

type StreamKey = (PeerRole, String);

/// Handle to this instance; clones share it.
#[derive(Clone)]
pub struct Cluster {
    inner: Arc<Inner>,
}

struct Inner {
    pool: PgPool,
    id: Uuid,
    settings: Settings,
    registry: Registry,
    links: Links,
    leading: watch::Sender<bool>,
    /// When to stop leading if no renewal confirms the lease before.
    lead_until: Mutex<Option<Instant>>,
    /// The streams last written to `peer_streams`.
    written: Mutex<HashMap<StreamKey, Stream>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // Every critical section is a single read or assignment.
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

fn role_of(name: &str) -> Option<PeerRole> {
    match name {
        "agent" => Some(PeerRole::Agent),
        "relay" => Some(PeerRole::Relay),
        _ => None,
    }
}

impl Cluster {
    /// Records this process as a new instance. Nothing runs until [`Self::run`]
    /// or [`Self::tick`].
    pub async fn join(
        pool: PgPool,
        registry: Registry,
        links: Links,
        settings: Settings,
    ) -> Result<Self, sqlx::Error> {
        let cluster = Self {
            inner: Arc::new(Inner {
                pool,
                id: Uuid::new_v4(),
                settings,
                registry,
                links,
                leading: watch::channel(false).0,
                lead_until: Mutex::new(None),
                written: Mutex::new(HashMap::new()),
            }),
        };
        cluster.insert().await?;
        tracing::info!(instance = %cluster.id(), "joined as a server instance");
        Ok(cluster)
    }

    pub fn id(&self) -> Uuid {
        self.inner.id
    }

    pub fn is_leader(&self) -> bool {
        *self.inner.leading.borrow()
    }

    /// Follows whether this instance leads, for [`lead`].
    pub fn leadership(&self) -> watch::Receiver<bool> {
        self.inner.leading.subscribe()
    }

    async fn insert(&self) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO server_instances (id, hostname, version) VALUES ($1, $2, $3)
             ON CONFLICT (id) DO UPDATE SET heartbeat_at = now()",
        )
        .bind(self.inner.id)
        .bind(hostname())
        .bind(monitorflow_common::VERSION)
        .execute(&self.inner.pool)
        .await?;
        Ok(())
    }

    /// One heartbeat: the instance row, the leader lease, the streams and
    /// the revocations. Stops at the first database error; the next tick
    /// starts over.
    pub async fn tick(&self) -> Result<(), sqlx::Error> {
        self.heartbeat().await?;
        self.renew().await?;
        self.write_streams().await?;
        self.read_streams().await?;
        self.sync_revocations().await
    }

    async fn heartbeat(&self) -> Result<(), sqlx::Error> {
        let pool = &self.inner.pool;
        let beat = sqlx::query("UPDATE server_instances SET heartbeat_at = now() WHERE id = $1")
            .bind(self.inner.id)
            .execute(pool)
            .await?;
        if beat.rows_affected() == 0 {
            // Taken for dead after a pause, with the streams it wrote.
            tracing::warn!(instance = %self.id(), "this instance was taken for dead; joining again");
            self.insert().await?;
            lock(&self.inner.written).clear();
        }
        let gone = sqlx::query(
            "DELETE FROM server_instances
             WHERE id <> $1 AND heartbeat_at < now() - make_interval(secs => $2)",
        )
        .bind(self.inner.id)
        .bind(self.inner.settings.dead_after.as_secs_f64())
        .execute(pool)
        .await?;
        if gone.rows_affected() > 0 {
            tracing::warn!(
                instances = gone.rows_affected(),
                "server instances stopped answering; their streams are gone"
            );
        }
        Ok(())
    }

    /// Takes the leader lease when it is free or expired, or renews it.
    async fn renew(&self) -> Result<(), sqlx::Error> {
        let settings = &self.inner.settings;
        let asked = Instant::now();
        let held: Option<Uuid> = sqlx::query_scalar(
            "INSERT INTO cluster_leases (name, holder, expires_at)
             VALUES ($1, $2, now() + make_interval(secs => $3))
             ON CONFLICT (name) DO UPDATE
                 SET holder = EXCLUDED.holder, expires_at = EXCLUDED.expires_at
                 WHERE cluster_leases.holder = EXCLUDED.holder
                    OR cluster_leases.expires_at < now()
             RETURNING holder",
        )
        .bind(LEADER)
        .bind(self.inner.id)
        .bind(settings.leader_ttl.as_secs_f64())
        .fetch_optional(&self.inner.pool)
        .await?;
        if held.is_some() {
            // Counted from before the request: the database's expiry is
            // later than this, whatever the request's latency.
            let until = asked + settings.leader_ttl.saturating_sub(settings.heartbeat);
            *lock(&self.inner.lead_until) = Some(until);
            if !self.inner.leading.send_replace(true) {
                tracing::info!(instance = %self.id(), "this instance leads");
            }
        } else {
            self.step_down("another instance leads");
        }
        Ok(())
    }

    fn step_down(&self, why: &str) {
        *lock(&self.inner.lead_until) = None;
        if self.inner.leading.send_replace(false) {
            tracing::warn!(instance = %self.id(), why, "this instance stopped leading");
        }
    }

    /// Writes what changed in the registry since the last tick.
    async fn write_streams(&self) -> Result<(), sqlx::Error> {
        let now: HashMap<StreamKey, Stream> = self
            .inner
            .registry
            .connected()
            .into_iter()
            .map(|p| {
                let stream = Stream {
                    connected_at: p.connected_at,
                    via_relay: p.via_relay,
                    agent_version: p.agent_version,
                    hostname: p.hostname,
                };
                ((p.role, p.id), stream)
            })
            .filter(|((role, _), _)| matches!(role, PeerRole::Agent | PeerRole::Relay))
            .collect();
        let written = lock(&self.inner.written).clone();
        let changed: Vec<(&StreamKey, &Stream)> = now
            .iter()
            .filter(|(key, stream)| written.get(*key) != Some(*stream))
            .collect();
        let gone: Vec<&StreamKey> = written.keys().filter(|k| !now.contains_key(*k)).collect();
        let pool = &self.inner.pool;
        if !gone.is_empty() {
            sqlx::query(
                "DELETE FROM peer_streams p
                 USING UNNEST($2::text[], $3::text[]) AS t(peer_role, peer_id)
                 WHERE p.instance_id = $1 AND p.peer_role = t.peer_role
                   AND p.peer_id = t.peer_id",
            )
            .bind(self.inner.id)
            .bind(gone.iter().map(|(r, _)| r.as_str()).collect::<Vec<_>>())
            .bind(gone.iter().map(|(_, id)| id.as_str()).collect::<Vec<_>>())
            .execute(pool)
            .await?;
        }
        if !changed.is_empty() {
            sqlx::query(
                "INSERT INTO peer_streams (peer_role, peer_id, instance_id, connected_at,
                                           via_relay, agent_version, hostname)
                 SELECT t.peer_role, t.peer_id, $1, t.connected_at, t.via_relay,
                        t.agent_version, t.hostname
                 FROM UNNEST($2::text[], $3::text[], $4::timestamptz[], $5::text[],
                             $6::text[], $7::text[])
                      AS t(peer_role, peer_id, connected_at, via_relay, agent_version, hostname)
                 ON CONFLICT (peer_role, peer_id, instance_id) DO UPDATE
                     SET connected_at = EXCLUDED.connected_at, via_relay = EXCLUDED.via_relay,
                         agent_version = EXCLUDED.agent_version, hostname = EXCLUDED.hostname",
            )
            .bind(self.inner.id)
            .bind(changed.iter().map(|((r, _), _)| r.as_str()).collect::<Vec<_>>())
            .bind(changed.iter().map(|((_, id), _)| id.as_str()).collect::<Vec<_>>())
            .bind(changed.iter().map(|(_, s)| s.connected_at).collect::<Vec<_>>())
            .bind(
                changed
                    .iter()
                    .map(|(_, s)| s.via_relay.as_deref())
                    .collect::<Vec<_>>(),
            )
            .bind(
                changed
                    .iter()
                    .map(|(_, s)| s.agent_version.as_str())
                    .collect::<Vec<_>>(),
            )
            .bind(
                changed
                    .iter()
                    .map(|(_, s)| s.hostname.as_str())
                    .collect::<Vec<_>>(),
            )
            .execute(pool)
            .await?;
        }
        *lock(&self.inner.written) = now;
        Ok(())
    }

    /// Hands the registry the streams other live instances hold.
    async fn read_streams(&self) -> Result<(), sqlx::Error> {
        type Line = (
            String,
            String,
            DateTime<Utc>,
            Option<String>,
            String,
            String,
            Option<DateTime<Utc>>,
        );
        let lines: Vec<Line> = sqlx::query_as(
            "SELECT p.peer_role, p.peer_id, p.connected_at, p.via_relay, p.agent_version,
                    p.hostname,
                    CASE p.peer_role
                        WHEN 'agent' THEN (SELECT max(last_seen_at) FROM servers s
                                           WHERE s.peer_id = p.peer_id)
                        ELSE (SELECT max(last_seen_at) FROM relays r WHERE r.peer_id = p.peer_id)
                    END
             FROM peer_streams p
             JOIN server_instances i ON i.id = p.instance_id
             WHERE p.instance_id <> $1
               AND i.heartbeat_at >= now() - make_interval(secs => $2)
             ORDER BY p.connected_at",
        )
        .bind(self.inner.id)
        .bind(self.inner.settings.dead_after.as_secs_f64())
        .fetch_all(&self.inner.pool)
        .await?;
        let remote = lines
            .into_iter()
            .filter_map(
                |(role, id, connected_at, via_relay, agent_version, hostname, seen)| {
                    Some(PeerInfo {
                        id,
                        role: role_of(&role)?,
                        via_relay,
                        connection: 0,
                        cert_serial: None,
                        agent_version,
                        hostname,
                        connected_at,
                        last_seen: seen.map_or(connected_at, |s| s.max(connected_at)),
                        last_sequence: 0,
                        counters: Counters::default(),
                        sensor_revision_sent: None,
                        sensor_revision_applied: None,
                    })
                },
            )
            .collect();
        self.inner.registry.set_remote(remote);
        Ok(())
    }

    /// Revokes in this instance's CA the certificates revoked in the
    /// database that it does not list yet, and closes their streams here.
    async fn sync_revocations(&self) -> Result<(), sqlx::Error> {
        let Some(crl) = &self.inner.links.crl else {
            return Ok(());
        };
        let rows: Vec<(Vec<u8>, String, Option<String>)> = sqlx::query_as(
            "SELECT serial, peer_id, revocation_reason FROM certificates
             WHERE revoked_at IS NOT NULL AND not_after > now()",
        )
        .fetch_all(&self.inner.pool)
        .await?;
        for (serial, peer_id, reason) in rows {
            let serial = Serial::from_bytes(&serial);
            if crl.is_revoked(&serial) {
                continue;
            }
            let reason: RevocationReason = reason
                .and_then(|r| serde_json::from_value(serde_json::Value::String(r)).ok())
                .unwrap_or(RevocationReason::Unspecified);
            match crl.revoke(serial.clone(), reason).await {
                Ok(()) => {
                    let closed = self.inner.registry.close(&peer_id, CloseReason::Revoked);
                    tracing::info!(%serial, %peer_id, closed, "revoked by another instance");
                }
                Err(error) => {
                    tracing::error!(%serial, %peer_id, %error, "cannot revoke a certificate another instance revoked; will retry")
                }
            }
        }
        Ok(())
    }

    /// Ticks every heartbeat and listens for queued commands until `stop`
    /// resolves, then leaves.
    pub async fn run(self, stop: impl Future<Output = ()>) {
        tokio::pin!(stop);
        let listening = tokio::spawn(self.clone().listen());
        let mut ticks = interval(self.inner.settings.heartbeat);
        ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = &mut stop => break,
                _ = ticks.tick() => {}
            }
            let lead_until = *lock(&self.inner.lead_until);
            tokio::select! {
                _ = &mut stop => break,
                ticked = self.tick() => if let Err(error) = ticked {
                    tracing::warn!(%error, "server instance heartbeat failed; will retry");
                },
                // A tick stuck on the database must not keep a lease that
                // may have expired.
                () = sleep_until(lead_until.unwrap_or_else(Instant::now)), if lead_until.is_some() => {
                    self.step_down("the lease could not be renewed in time");
                }
            }
            if lock(&self.inner.lead_until).is_some_and(|until| until <= Instant::now()) {
                self.step_down("the lease could not be renewed in time");
            }
        }
        listening.abort();
        self.leave().await;
    }

    /// Gives up the lease and removes this instance and its streams, so
    /// another instance leads and shows the streams at once.
    pub async fn leave(&self) {
        self.step_down("stopping");
        let left = async {
            let mut tx = self.inner.pool.begin().await?;
            sqlx::query("DELETE FROM cluster_leases WHERE holder = $1")
                .bind(self.inner.id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM server_instances WHERE id = $1")
                .bind(self.inner.id)
                .execute(&mut *tx)
                .await?;
            tx.commit().await
        };
        if let Err(error) = left.await {
            tracing::warn!(%error, "could not leave cleanly; other instances wait for the heartbeat to age");
        }
    }

    /// Wakes the dispatcher of a peer whose stream is here when a command
    /// is queued for it, by any instance.
    async fn listen(self) {
        loop {
            let mut listener = match PgListener::connect_with(&self.inner.pool).await {
                Ok(listener) => listener,
                Err(sqlx::Error::PoolClosed) => return,
                Err(error) => {
                    tracing::warn!(%error, "cannot listen for queued commands; will retry");
                    sleep(self.inner.settings.heartbeat).await;
                    continue;
                }
            };
            if let Err(error) = listener
                .listen_all([COMMANDS_CHANNEL, RELAY_COMMANDS_CHANNEL])
                .await
            {
                tracing::warn!(%error, "cannot listen for queued commands; will retry");
                sleep(self.inner.settings.heartbeat).await;
                continue;
            }
            loop {
                match listener.try_recv().await {
                    Ok(Some(notification)) => {
                        self.wake(notification.channel(), notification.payload())
                    }
                    // Reconnected: what was missed goes out at the next poll.
                    Ok(None) => {}
                    Err(sqlx::Error::PoolClosed) => return,
                    Err(error) => {
                        tracing::warn!(%error, "lost the queued commands listener; will retry");
                        sleep(self.inner.settings.heartbeat).await;
                        break;
                    }
                }
            }
        }
    }

    fn wake(&self, channel: &str, peer_id: &str) {
        if self.inner.registry.get(peer_id).is_none() {
            return;
        }
        let links = &self.inner.links;
        match channel {
            COMMANDS_CHANNEL => links.commands.iter().for_each(|c| c.dispatch(peer_id)),
            RELAY_COMMANDS_CHANNEL => links
                .relay_commands
                .iter()
                .for_each(|c| c.dispatch(peer_id)),
            _ => {}
        }
    }
}

/// Ends when the instance stops leading or the server stops.
pub type Until = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Runs `task` while this instance leads, again each time it leads anew,
/// until `stopped` turns true. `task` gets a future that resolves when it
/// must stop, as the loops take for a shutdown; its current pass then
/// ends normally. Every task run this way is also safe when two instances
/// run it for a moment: leadership only keeps the second one idle.
pub async fn lead<F, Fut>(
    mut leading: watch::Receiver<bool>,
    mut stopped: watch::Receiver<bool>,
    mut task: F,
) where
    F: FnMut(Until) -> Fut,
    Fut: Future<Output = ()>,
{
    loop {
        tokio::select! {
            _ = stopped.wait_for(|stop| *stop) => return,
            led = leading.wait_for(|lead| *lead) => if led.is_err() {
                return;
            },
        }
        let (mut lost, mut stop) = (leading.clone(), stopped.clone());
        task(Box::pin(async move {
            tokio::select! {
                _ = lost.wait_for(|lead| !*lead) => {}
                _ = stop.wait_for(|stop| *stop) => {}
            }
        }))
        .await;
    }
}

/// The host name, for operators reading `server_instances`.
fn hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::fs::read_to_string("/proc/sys/kernel/hostname").ok())
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_parse_from_the_column() {
        assert_eq!(role_of("agent"), Some(PeerRole::Agent));
        assert_eq!(role_of("relay"), Some(PeerRole::Relay));
        assert_eq!(role_of("server"), None);
    }

    #[test]
    fn the_leader_stops_one_heartbeat_before_its_lease_expires() {
        let settings = Settings::default();
        assert!(settings.leader_ttl > settings.heartbeat * 2);
        assert!(settings.dead_after > settings.heartbeat * 2);
    }

    #[tokio::test]
    async fn lead_runs_the_task_only_while_leading() {
        let (leading, leading_rx) = watch::channel(false);
        let (stop, stopped) = watch::channel(false);
        let (runs, mut runs_rx) = tokio::sync::mpsc::unbounded_channel();
        let led = tokio::spawn(lead(leading_rx, stopped, move |until| {
            let runs = runs.clone();
            async move {
                runs.send("started").unwrap();
                until.await;
                runs.send("stopped").unwrap();
            }
        }));
        tokio::task::yield_now().await;
        assert!(runs_rx.try_recv().is_err(), "ran before leading");
        leading.send_replace(true);
        assert_eq!(runs_rx.recv().await, Some("started"));
        leading.send_replace(false);
        assert_eq!(runs_rx.recv().await, Some("stopped"));
        leading.send_replace(true);
        assert_eq!(runs_rx.recv().await, Some("started"));
        stop.send_replace(true);
        assert_eq!(runs_rx.recv().await, Some("stopped"));
        led.await.unwrap();
    }
}
