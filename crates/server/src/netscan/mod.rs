//! Network discovery (ADR-0154): scans of a site's addresses from the
//! central server or a relay, run as one-off `discover` tests of 16
//! addresses at a time on the scan's probe.
//!
//! [`run`] works on every instance: it queues the runs the schedule asks
//! for, fails runs left behind by an instance that stopped, and takes the
//! queued runs whose probe it reaches (the central server, or a relay
//! whose stream it holds), one run per probe at a time. What each run
//! found is kept per address ([`results`]); proposals are computed when
//! read ([`proposals`]).

pub mod proposals;
pub mod results;
pub mod targets;

use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use monitorflow_common::proto::v1::Sample as ProtoSample;
use monitorflow_common::tls::PeerRole;
use monitorflow_probe::{discover, Check, CENTRAL};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::grpc::Registry;
use crate::probes::{Probes, TestError};
use crate::secrets::{self, Column};
use crate::snmp;

/// Time between two looks for work.
pub const POLL: Duration = Duration::from_secs(5);
/// A running run that has not moved for this long was on an instance
/// that stopped.
pub const STALE: Duration = Duration::from_secs(120);
/// Addresses per test.
pub const CHUNK: usize = discover::MAX_TARGETS;
/// Runs kept per scan.
pub const KEEP_RUNS: i64 = 10;
/// Most sets of SNMP credentials a scan tries.
pub const MAX_SETS: usize = discover::MAX_SNMP;
/// Longest a test of one chunk may take.
const TEST_TIMEOUT: Duration = Duration::from_secs(25);

/// Why a run failed, as stored: a code the web interface words, or the
/// probe's own message.
pub mod failure {
    pub const INTERRUPTED: &str = "interrupted";
    pub const RELAY_DISCONNECTED: &str = "relay_disconnected";
    pub const RELAY_OUTDATED: &str = "relay_outdated";
    pub const RELAY_REMOVED: &str = "relay_removed";
}

/// Runs one-off tests on a probe: [`Live`] in the server, a fake in
/// tests.
#[async_trait::async_trait]
pub trait Tester: Send + Sync + 'static {
    /// Whether this instance can run tests on `probe` now.
    fn reaches(&self, probe: &str) -> bool;
    async fn test(&self, probe: &str, check: Check) -> Result<Vec<ProtoSample>, TestError>;
}

/// The central server's probes and the relays' streams it holds.
#[derive(Clone)]
pub struct Live {
    pub probes: Option<Probes>,
    pub registry: Registry,
}

#[async_trait::async_trait]
impl Tester for Live {
    fn reaches(&self, probe: &str) -> bool {
        probe == CENTRAL || self.registry.get(PeerRole::Relay, probe).is_some()
    }

    async fn test(&self, probe: &str, check: Check) -> Result<Vec<ProtoSample>, TestError> {
        match &self.probes {
            Some(probes) => probes.test(&self.registry, probe, check).await,
            None if probe == CENTRAL => {
                monitorflow_probe::test_once(&monitorflow_probe::Builtin, &check)
                    .await
                    .map(|s| {
                        s.into_iter()
                            .map(monitorflow_probe::sample_to_proto)
                            .collect()
                    })
                    .map_err(TestError::Failed)
            }
            None => Err(TestError::NotConnected),
        }
    }
}

/// SNMP credentials as a scan keeps them, sealed as a whole.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredCredentials {
    pub version: String,
    pub port: u16,
    pub user: Option<String>,
    pub auth_protocol: Option<String>,
    pub priv_protocol: Option<String>,
    pub secrets: snmp::Secrets,
}

impl std::fmt::Debug for StoredCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredCredentials")
            .field("version", &self.version)
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

impl From<&snmp::Credentials> for StoredCredentials {
    fn from(c: &snmp::Credentials) -> Self {
        Self {
            version: c.version.clone(),
            port: c.port,
            user: c.user.clone(),
            auth_protocol: c.auth_protocol.clone(),
            priv_protocol: c.priv_protocol.clone(),
            secrets: c.secrets.clone(),
        }
    }
}

impl From<StoredCredentials> for snmp::Credentials {
    fn from(c: StoredCredentials) -> Self {
        Self {
            version: c.version,
            port: c.port,
            user: c.user,
            auth_protocol: c.auth_protocol,
            priv_protocol: c.priv_protocol,
            secrets: c.secrets,
        }
    }
}

/// Seals a scan's own credentials.
pub fn seal(credentials: &snmp::Credentials) -> Result<String, String> {
    let plain =
        serde_json::to_string(&StoredCredentials::from(credentials)).map_err(|e| e.to_string())?;
    secrets::seal(Column::NetworkScans, &plain).map_err(|e| e.to_string())
}

/// Opens a scan's own credentials.
pub fn open(sealed: &str) -> Result<snmp::Credentials, String> {
    let plain = secrets::open(Column::NetworkScans, sealed).map_err(|e| e.to_string())?;
    serde_json::from_str::<StoredCredentials>(plain.expose())
        .map(Into::into)
        .map_err(|_| "the scan's SNMP credentials do not read".to_string())
}

/// Where a set of credentials came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Scan,
    Device(Uuid),
}

/// The sets a scan tries: its own, then each distinct set of the site's
/// devices, most recently changed first, at most [`MAX_SETS`].
pub async fn credential_sets(
    db: &mut PgConnection,
    site: Uuid,
    own: Option<&str>,
) -> Result<Vec<(Source, snmp::Credentials)>, sqlx::Error> {
    let mut sets: Vec<(Source, snmp::Credentials)> = Vec::new();
    if let Some(Ok(credentials)) = own.map(open) {
        sets.push((Source::Scan, credentials));
    }
    let devices: Vec<Uuid> = sqlx::query_scalar(
        "SELECT c.server_id FROM snmp_credentials c JOIN servers s ON s.id = c.server_id
         WHERE s.site_id = $1 AND s.revoked_at IS NULL ORDER BY c.updated_at DESC, s.id",
    )
    .bind(site)
    .fetch_all(&mut *db)
    .await?;
    for device in devices {
        if sets.len() >= MAX_SETS {
            break;
        }
        if let Some(Ok(credentials)) = snmp::load(&mut *db, device).await? {
            if !sets.iter().any(|(_, c)| *c == credentials) {
                sets.push((Source::Device(device), credentials));
            }
        }
    }
    Ok(sets)
}

/// The `discover` test of `chunk`.
pub fn chunk_check(chunk: &[IpAddr], ports: &[u16], sets: &[snmp::Credentials]) -> Check {
    let mut params = BTreeMap::new();
    let join = |v: Vec<String>| v.join(",");
    params.insert(
        "targets".to_owned(),
        join(chunk.iter().map(IpAddr::to_string).collect()),
    );
    params.insert(
        "ports".to_owned(),
        join(ports.iter().map(u16::to_string).collect()),
    );
    for (n, set) in sets.iter().enumerate() {
        for (key, value) in set.params() {
            params.insert(format!("snmp{}_{key}", n + 1), value);
        }
    }
    let mut config = monitorflow_sensors::SensorConfig::new("discover", Duration::from_secs(60));
    config.timeout = Some(TEST_TIMEOUT);
    config.params = params;
    Check {
        device: "discover".into(),
        kind: discover::KIND.into(),
        config,
    }
}

/// The stored reason of a test that failed.
fn failed(error: TestError) -> String {
    match error {
        TestError::NotConnected | TestError::NoAnswer => failure::RELAY_DISCONNECTED.into(),
        TestError::Failed(message) if message.contains("does not run `discover`") => {
            failure::RELAY_OUTDATED.into()
        }
        TestError::Failed(message) => message.chars().take(500).collect(),
    }
}

/// Queues the runs the schedule asks for: a scan with `every` whose last
/// run was queued longer ago than that, and none waiting or running.
pub async fn schedule(pool: &PgPool) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query(
        "INSERT INTO network_scan_runs (scan_id, trigger)
         SELECT s.id, 'schedule' FROM network_scans s
         WHERE s.every IS NOT NULL
           AND coalesce((SELECT max(queued_at) FROM network_scan_runs r WHERE r.scan_id = s.id),
                        '-infinity')
               <= now() - CASE s.every WHEN 'hour' THEN interval '1 hour'
                                       WHEN 'day' THEN interval '1 day'
                                       ELSE interval '7 days' END
         ON CONFLICT (scan_id) WHERE state IN ('queued', 'running') DO NOTHING",
    )
    .execute(pool)
    .await?
    .rows_affected())
}

/// Fails runs that stopped moving: their instance stopped.
pub async fn fail_stale(pool: &PgPool) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query(
        "UPDATE network_scan_runs SET state = 'failed', error = $1, finished_at = now()
         WHERE state = 'running' AND progress_at < now() - make_interval(secs => $2)",
    )
    .bind(failure::INTERRUPTED)
    .bind(STALE.as_secs_f64())
    .execute(pool)
    .await?
    .rows_affected())
}

/// Takes the queued runs this instance can run: their probe reachable
/// and running no other run. Runs of a removed relay fail.
pub async fn claim(pool: &PgPool, tester: &dyn Tester) -> Result<Vec<Uuid>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('monitorflow.network_scans'))")
        .execute(&mut *tx)
        .await?;
    let busy: BTreeSet<String> = sqlx::query_scalar(
        "SELECT CASE WHEN s.probe = 'central' THEN 'central' ELSE rl.peer_id END
         FROM network_scan_runs r JOIN network_scans s ON s.id = r.scan_id
         LEFT JOIN relays rl ON rl.id = s.probe_relay_id WHERE r.state = 'running'",
    )
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .collect();
    let queued: Vec<(Uuid, String, bool)> = sqlx::query_as(
        "SELECT r.id, CASE WHEN s.probe = 'central' THEN 'central' ELSE rl.peer_id END,
                rl.revoked_at IS NOT NULL
         FROM network_scan_runs r JOIN network_scans s ON s.id = r.scan_id
         LEFT JOIN relays rl ON rl.id = s.probe_relay_id
         WHERE r.state = 'queued' ORDER BY r.queued_at FOR UPDATE OF r SKIP LOCKED",
    )
    .fetch_all(&mut *tx)
    .await?;
    let mut taken = Vec::new();
    let mut busy = busy;
    for (run, probe, removed) in queued {
        if removed {
            sqlx::query(
                "UPDATE network_scan_runs SET state = 'failed', error = $2, finished_at = now()
                 WHERE id = $1",
            )
            .bind(run)
            .bind(failure::RELAY_REMOVED)
            .execute(&mut *tx)
            .await?;
            continue;
        }
        if busy.contains(&probe) || !tester.reaches(&probe) {
            continue;
        }
        sqlx::query(
            "UPDATE network_scan_runs SET state = 'running', started_at = now(), progress_at = now()
             WHERE id = $1",
        )
        .bind(run)
        .execute(&mut *tx)
        .await?;
        busy.insert(probe);
        taken.push(run);
    }
    tx.commit().await?;
    Ok(taken)
}

/// What a run needs of its scan.
struct Scan {
    id: Uuid,
    site: Uuid,
    targets: Vec<String>,
    ports: Vec<u16>,
    probe: String,
    snmp: Option<String>,
    notify: bool,
}

async fn load_scan(pool: &PgPool, run: Uuid) -> Result<Option<Scan>, sqlx::Error> {
    type Row = (
        Uuid,
        Uuid,
        Vec<String>,
        Vec<i32>,
        String,
        Option<String>,
        bool,
    );
    let row: Option<Row> = sqlx::query_as(
        "SELECT s.id, s.site_id, s.targets, s.ports,
                CASE WHEN s.probe = 'central' THEN 'central' ELSE rl.peer_id END,
                s.snmp_secret, s.notify
         FROM network_scan_runs r JOIN network_scans s ON s.id = r.scan_id
         LEFT JOIN relays rl ON rl.id = s.probe_relay_id WHERE r.id = $1",
    )
    .bind(run)
    .fetch_optional(pool)
    .await?;
    Ok(
        row.map(|(id, site, targets, ports, probe, snmp, notify)| Scan {
            id,
            site,
            targets,
            ports: ports
                .into_iter()
                .filter_map(|p| u16::try_from(p).ok())
                .collect(),
            probe,
            snmp,
            notify,
        }),
    )
}

async fn fail(pool: &PgPool, run: Uuid, error: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE network_scan_runs SET state = 'failed', error = $2, finished_at = now()
         WHERE id = $1 AND state = 'running'",
    )
    .bind(run)
    .bind(error.chars().take(500).collect::<String>())
    .execute(pool)
    .await?;
    Ok(())
}

async fn store_hosts(
    pool: &PgPool,
    run: Uuid,
    hosts: &[results::Host],
    sources: &[Source],
) -> Result<(), sqlx::Error> {
    for host in hosts {
        let source = host.snmp_set.and_then(|n| sources.get(n - 1)).copied();
        sqlx::query(
            "INSERT INTO network_scan_hosts (run_id, address, ping, rtt_seconds, ports, name, mac,
                 snmp, snmp_from_scan, snmp_device_id, snmp_descr, snmp_name, snmp_object_id,
                 snmp_groups, template)
             VALUES ($1, $2::inet, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)
             ON CONFLICT DO NOTHING",
        )
        .bind(run)
        .bind(host.address.to_string())
        .bind(host.ping)
        .bind(host.rtt_seconds)
        .bind(host.ports.iter().map(|p| i32::from(*p)).collect::<Vec<_>>())
        .bind(&host.name)
        .bind(&host.mac)
        .bind(source.is_some())
        .bind(source == Some(Source::Scan))
        .bind(match source {
            Some(Source::Device(id)) => Some(id),
            _ => None,
        })
        .bind(&host.snmp_descr)
        .bind(&host.snmp_name)
        .bind(&host.snmp_object_id)
        .bind(&host.snmp_groups)
        .bind(&host.template)
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// Runs the claimed run `run` to its end.
pub async fn execute(pool: &PgPool, tester: &dyn Tester, run: Uuid) -> Result<(), sqlx::Error> {
    let Some(scan) = load_scan(pool, run).await? else {
        return Ok(());
    };
    let addresses = match targets::expand(&scan.targets) {
        Ok((_, addresses)) => addresses,
        Err(error) => return fail(pool, run, &error).await,
    };
    let sets = {
        let mut conn = pool.acquire().await?;
        credential_sets(&mut conn, scan.site, scan.snmp.as_deref()).await?
    };
    let (sources, credentials): (Vec<Source>, Vec<snmp::Credentials>) = sets.into_iter().unzip();
    sqlx::query("UPDATE network_scan_runs SET addresses = $2 WHERE id = $1")
        .bind(run)
        .bind(i32::try_from(addresses.len()).unwrap_or(i32::MAX))
        .execute(pool)
        .await?;
    for chunk in addresses.chunks(CHUNK) {
        let check = chunk_check(chunk, &scan.ports, &credentials);
        let samples = match tester.test(&scan.probe, check).await {
            Ok(samples) => samples,
            Err(error) => return fail(pool, run, &failed(error)).await,
        };
        let found = results::read(&samples, chunk, &scan.ports, credentials.len());
        store_hosts(pool, run, &found.hosts, &sources).await?;
        let moved = sqlx::query(
            "UPDATE network_scan_runs SET scanned = scanned + $2, dropped = dropped + $3,
                 ping_error = coalesce($4, ping_error), progress_at = now()
             WHERE id = $1 AND state = 'running'",
        )
        .bind(run)
        .bind(i32::try_from(chunk.len()).unwrap_or(0))
        .bind(i32::try_from(found.dropped).unwrap_or(i32::MAX))
        .bind(&found.ping_error)
        .execute(pool)
        .await?
        .rows_affected();
        // Deleted with its scan, or failed by another instance.
        if moved == 0 {
            return Ok(());
        }
    }
    finish(pool, &scan, run).await
}

/// Marks what is new and gone against the previous finished run, ends
/// the run, notifies when asked, and keeps the latest runs only.
async fn finish(pool: &PgPool, scan: &Scan, run: Uuid) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    let previous: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM network_scan_runs WHERE scan_id = $1 AND state = 'done' AND id <> $2
         ORDER BY finished_at DESC LIMIT 1",
    )
    .bind(scan.id)
    .bind(run)
    .fetch_optional(&mut *tx)
    .await?;
    let (new, gone): (Vec<String>, Vec<String>) = match previous {
        None => (Vec::new(), Vec::new()),
        Some(previous) => {
            let new: Vec<String> = sqlx::query_scalar(
                "UPDATE network_scan_hosts h SET is_new = true
                 WHERE h.run_id = $1 AND NOT EXISTS (SELECT 1 FROM network_scan_hosts p
                                                     WHERE p.run_id = $2 AND p.address = h.address)
                 RETURNING host(h.address)",
            )
            .bind(run)
            .bind(previous)
            .fetch_all(&mut *tx)
            .await?;
            let gone: Vec<String> = sqlx::query_scalar(
                "SELECT host(p.address) FROM network_scan_hosts p
                 WHERE p.run_id = $2 AND NOT EXISTS (SELECT 1 FROM network_scan_hosts h
                                                     WHERE h.run_id = $1 AND h.address = p.address)
                 ORDER BY p.address",
            )
            .bind(run)
            .bind(previous)
            .fetch_all(&mut *tx)
            .await?;
            (new, gone)
        }
    };
    let done = sqlx::query(
        "UPDATE network_scan_runs SET state = 'done', finished_at = now(), progress_at = now(),
             answered = (SELECT count(*) FROM network_scan_hosts WHERE run_id = $1),
             new_count = $2, gone = $3::text[]::inet[], compared = $4
         WHERE id = $1 AND state = 'running'",
    )
    .bind(run)
    .bind(i32::try_from(new.len()).unwrap_or(i32::MAX))
    .bind(&gone)
    .bind(previous.is_some())
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if done == 0 {
        return Ok(());
    }
    if scan.notify && (!new.is_empty() || !gone.is_empty()) {
        sqlx::query(
            "INSERT INTO notification_outbox (event, customer_id, details)
             SELECT 'network_scan.changed', c.id, jsonb_build_object(
                 'occurred_at', now(),
                 'customer', notification_ref(c.id, c.name),
                 'site', notification_ref(si.id, si.name),
                 'network_scan', jsonb_build_object('id', s.id, 'name', s.name, 'run_id', $2::uuid,
                                                    'new', $3::text[], 'gone', $4::text[]))
             FROM network_scans s JOIN sites si ON si.id = s.site_id
             LEFT JOIN customers c ON c.id = si.customer_id
             WHERE s.id = $1",
        )
        .bind(scan.id)
        .bind(run)
        .bind(&new)
        .bind(&gone)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query(
        "DELETE FROM network_scan_runs WHERE scan_id = $1 AND state IN ('done', 'failed')
           AND id NOT IN (SELECT id FROM network_scan_runs WHERE scan_id = $1
                          ORDER BY queued_at DESC LIMIT $2)",
    )
    .bind(scan.id)
    .bind(KEEP_RUNS)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    tracing::info!(scan = %scan.id, %run, new = new.len(), gone = gone.len(), "network scan done");
    Ok(())
}

/// One look for work: schedule, fail stale runs, and start what this
/// instance can run.
pub async fn tick(
    pool: &PgPool,
    tester: &Arc<dyn Tester>,
    running: &mut tokio::task::JoinSet<()>,
) -> Result<(), sqlx::Error> {
    schedule(pool).await?;
    fail_stale(pool).await?;
    for run in claim(pool, &**tester).await? {
        let (pool, tester) = (pool.clone(), tester.clone());
        running.spawn(async move {
            if let Err(error) = execute(&pool, &*tester, run).await {
                tracing::warn!(%run, %error, "network scan stopped on a database error");
                let _ = fail(&pool, run, "the database was unavailable").await;
            }
        });
    }
    while running.try_join_next().is_some() {}
    Ok(())
}

/// Runs scans until `until` completes; see the module documentation.
pub async fn run(
    pool: PgPool,
    tester: Arc<dyn Tester>,
    until: impl std::future::Future<Output = ()>,
) {
    let mut running = tokio::task::JoinSet::new();
    let work = async {
        let mut ticks = tokio::time::interval(POLL);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticks.tick().await;
            if let Err(error) = tick(&pool, &tester, &mut running).await {
                tracing::warn!(%error, "network scans: cannot look for work");
            }
        }
    };
    tokio::select! {
        () = work => {}
        () = until => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_carry_their_credentials() {
        let v2c = snmp::check(
            &snmp::CredentialsRequest {
                version: "v2c".into(),
                community: Some("c0mmunity".into()),
                ..Default::default()
            },
            None,
        )
        .unwrap();
        let chunk = ["192.0.2.1".parse().unwrap(), "192.0.2.2".parse().unwrap()];
        let check = chunk_check(&chunk, &[22, 443], &[v2c]);
        let params = &check.config.params;
        assert_eq!(params["targets"], "192.0.2.1,192.0.2.2");
        assert_eq!(params["ports"], "22,443");
        assert_eq!(params["snmp1_community"], "c0mmunity");
        assert_eq!(params["snmp1_port"], "161");
        assert!(discover::parse(params).is_ok());
        assert_eq!(check.kind, "discover");
    }

    #[test]
    fn failures_are_worded() {
        assert_eq!(failed(TestError::NoAnswer), failure::RELAY_DISCONNECTED);
        assert_eq!(
            failed(TestError::Failed(
                "a probe does not run `discover` checks".into()
            )),
            failure::RELAY_OUTDATED
        );
        assert_eq!(failed(TestError::Failed("boom".into())), "boom");
    }
}
