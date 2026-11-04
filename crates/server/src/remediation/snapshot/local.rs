//! Local snapshots taken by the agent itself when `/` is on LVM or ZFS
//! (ADR-0094), for servers without a Proxmox VM mapping.
//!
//! The agent reports its system volumes and MonitorFlow's snapshots with
//! each package report, stored in `server_storage`. [`support`] reads that
//! with the same rules the agent's helper applies, so the interface can
//! say in advance what a change gets. [`LocalSnapshots::take`] queues a
//! `take_snapshot` command and waits for its result. [`prune`] deletes
//! what retention no longer keeps, with `delete_snapshot` commands.

use std::collections::BTreeMap;
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use monitorflow_common::command::{DELETE_SNAPSHOT, TAKE_SNAPSHOT};
use monitorflow_common::proto::v1::{
    DeleteSnapshot, StorageReport, TakeSnapshot, Volume, VolumeKind,
};
use monitorflow_common::snapshot::{self, bytes, Refusal};
use prost::Message;
use serde::{Deserialize, Serialize};
use sqlx::{PgExecutor, PgPool};
use uuid::Uuid;

use crate::api::audit::{self, Event};
use crate::commands::{Commands, NewCommand, State};

/// A snapshot's command must start within this long.
pub const TAKE_TTL: Duration = Duration::from_secs(10 * 60);
/// How long [`LocalSnapshots::take`] waits past the TTL for a result.
const RESULT_GRACE: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_secs(1);
/// Retention: a classic LVM snapshot slows every write to its origin and
/// becomes invalid once full, so it goes after a day...
pub const KEEP_CLASSIC: chrono::Duration = chrono::Duration::days(1);
/// ...others after a week...
pub const KEEP: chrono::Duration = chrono::Duration::days(7);
/// ...and only the newest 3 stay, as for Proxmox (ADR-0044).
pub const KEEP_NEWEST: usize = 3;
/// How often [`prune`] runs, and how long a delete is given before it is
/// asked again.
pub const PRUNE_EVERY: Duration = Duration::from_secs(60 * 60);
const RETRY_DELETE_AFTER: chrono::Duration = chrono::Duration::hours(6);
/// Volumes and snapshots kept from one report.
const MAX_ITEMS: usize = 200;
const MAX_TEXT: usize = 256;

/// One system volume, as stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredVolume {
    pub mount_point: String,
    pub fs_type: String,
    /// `plain`, `lvm`, `lvm_thin` or `zfs`.
    pub kind: String,
    pub source: String,
    pub size_bytes: u64,
    pub free_bytes: u64,
    pub pool_bytes: u64,
    pub metadata_percent: f64,
}

/// One volume's snapshot, as stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredSnapshot {
    pub name: String,
    pub mount_point: String,
    pub kind: String,
    pub source: String,
    pub origin: String,
    pub created_at: DateTime<Utc>,
    pub size_bytes: u64,
    pub used_percent: f64,
    pub invalid: bool,
}

/// What `server_storage.report` holds.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Storage {
    pub volumes: Vec<StoredVolume>,
    pub snapshots: Vec<StoredSnapshot>,
    /// Why the agent could not inspect its storage.
    pub error: Option<String>,
}

fn kind_name(kind: VolumeKind) -> &'static str {
    match kind {
        VolumeKind::Lvm => "lvm",
        VolumeKind::LvmThin => "lvm_thin",
        VolumeKind::Zfs => "zfs",
        VolumeKind::Plain | VolumeKind::Unspecified => "plain",
    }
}

fn kind_of(name: &str) -> VolumeKind {
    match name {
        "lvm" => VolumeKind::Lvm,
        "lvm_thin" => VolumeKind::LvmThin,
        "zfs" => VolumeKind::Zfs,
        _ => VolumeKind::Plain,
    }
}

fn text(s: &str) -> Option<String> {
    (s.len() <= MAX_TEXT && !s.contains('\0')).then(|| s.to_owned())
}

impl Storage {
    /// From an agent's report; entries that do not fit are dropped.
    pub fn from_report(report: &StorageReport) -> Self {
        let volumes = report
            .volumes
            .iter()
            .take(MAX_ITEMS)
            .filter_map(|v| {
                Some(StoredVolume {
                    mount_point: text(&v.mount_point)?,
                    fs_type: text(&v.fs_type)?,
                    kind: kind_name(v.kind()).to_owned(),
                    source: text(&v.source)?,
                    size_bytes: v.size_bytes,
                    free_bytes: v.free_bytes,
                    pool_bytes: v.pool_bytes,
                    metadata_percent: finite(v.metadata_percent),
                })
            })
            .collect();
        let snapshots = report
            .snapshots
            .iter()
            .take(MAX_ITEMS)
            .filter_map(|s| {
                Some(StoredSnapshot {
                    name: Some(s.name.clone()).filter(|n| snapshot::valid_name(n))?,
                    mount_point: text(&s.mount_point)?,
                    kind: kind_name(s.kind()).to_owned(),
                    source: text(&s.source)?,
                    origin: text(&s.origin)?,
                    created_at: Utc.timestamp_opt(s.created_at_unix, 0).single()?,
                    size_bytes: s.size_bytes,
                    used_percent: finite(s.used_percent),
                    invalid: s.invalid,
                })
            })
            .collect();
        let error = (!report.error.is_empty()).then(|| {
            report
                .error
                .chars()
                .take(1024)
                .collect::<String>()
                .replace('\0', "")
        });
        Self {
            volumes,
            snapshots,
            error,
        }
    }

    /// The volumes as the shared rules read them.
    pub fn proto_volumes(&self) -> Vec<Volume> {
        self.volumes
            .iter()
            .map(|v| Volume {
                mount_point: v.mount_point.clone(),
                fs_type: v.fs_type.clone(),
                kind: kind_of(&v.kind).into(),
                source: v.source.clone(),
                size_bytes: v.size_bytes,
                free_bytes: v.free_bytes,
                pool_bytes: v.pool_bytes,
                metadata_percent: v.metadata_percent,
            })
            .collect()
    }
}

/// JSON has no NaN or infinity.
fn finite(value: f64) -> f64 {
    if value.is_finite() {
        value
    } else {
        0.0
    }
}

/// Stores `report` for `server`, or forgets the previous one when the
/// agent sent none (Windows, or an agent before ADR-0094).
pub async fn store(
    db: impl PgExecutor<'_>,
    server: Uuid,
    report: Option<&StorageReport>,
) -> Result<(), sqlx::Error> {
    match report {
        Some(report) => {
            sqlx::query(
                "INSERT INTO server_storage (server_id, report) VALUES ($1, $2)
                 ON CONFLICT (server_id) DO UPDATE SET report = $2, reported_at = now()",
            )
            .bind(server)
            .bind(sqlx::types::Json(Storage::from_report(report)))
            .execute(db)
            .await?;
        }
        None => {
            sqlx::query("DELETE FROM server_storage WHERE server_id = $1")
                .bind(server)
                .execute(db)
                .await?;
        }
    }
    Ok(())
}

/// The latest storage report of `server` and when it came.
pub async fn load(
    db: impl PgExecutor<'_>,
    server: Uuid,
) -> Result<Option<(Storage, DateTime<Utc>)>, sqlx::Error> {
    let row: Option<(sqlx::types::Json<Storage>, DateTime<Utc>)> =
        sqlx::query_as("SELECT report, reported_at FROM server_storage WHERE server_id = $1")
            .bind(server)
            .fetch_optional(db)
            .await?;
    Ok(row.map(|(report, at)| (report.0, at)))
}

/// Why a server gets no local snapshot.
#[derive(Debug, Clone, PartialEq)]
pub enum LocalUnavailable {
    /// Windows: VSS is not used (ADR-0094).
    Windows,
    /// No storage report: an agent before ADR-0094, or none yet.
    NotReported,
    /// The sudo rules lack the `snapshots` lines.
    SudoRules,
    /// The helper failed otherwise.
    Inspect { error: String },
    /// The shared rules refuse.
    Refused(Refusal),
}

impl LocalUnavailable {
    /// For the API: `windows`, `not_reported`, `sudo_rules`, `inspect`,
    /// `no_root`, `root_not_covered`, `no_space` or `pool_full`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::NotReported => "not_reported",
            Self::SudoRules => "sudo_rules",
            Self::Inspect { .. } => "inspect",
            Self::Refused(Refusal::NoRoot) => "no_root",
            Self::Refused(Refusal::RootNotCovered { .. }) => "root_not_covered",
            Self::Refused(Refusal::NoSpace { .. }) => "no_space",
            Self::Refused(Refusal::PoolFull { .. }) => "pool_full",
        }
    }
}

impl std::fmt::Display for LocalUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Windows => f.write_str("Windows servers are not snapshotted by MonitorFlow"),
            Self::NotReported => f.write_str("the agent has not reported its storage"),
            Self::SudoRules => f.write_str(
                "the sudo rules on this server predate snapshots; run the install command again",
            ),
            Self::Inspect { error } => write!(f, "the agent cannot inspect its storage: {error}"),
            Self::Refused(refusal) => refusal.fmt(f),
        }
    }
}

/// What a local snapshot would cover now.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LocalPlan {
    /// `lvm` or `zfs`, from `/`.
    pub method: &'static str,
    /// Mount points snapshotted.
    pub covered: Vec<String>,
    /// System mount points left out, like a plain `/boot`.
    pub not_covered: Vec<String>,
    /// One sentence, like "LVM snapshot of / and /var (the thin pool has
    /// 40.0 GiB free)".
    pub summary: String,
}

/// Whether `storage` of a server of `os_family` can be snapshotted now.
pub fn plan_of(os_family: &str, storage: Option<&Storage>) -> Result<LocalPlan, LocalUnavailable> {
    if os_family == "windows" {
        return Err(LocalUnavailable::Windows);
    }
    let storage = storage.ok_or(LocalUnavailable::NotReported)?;
    if let Some(error) = &storage.error {
        return Err(if error.contains("password is required") {
            LocalUnavailable::SudoRules
        } else {
            LocalUnavailable::Inspect {
                error: error.clone(),
            }
        });
    }
    let volumes = storage.proto_volumes();
    let plan = snapshot::plan(&volumes).map_err(LocalUnavailable::Refused)?;
    let root = plan.covered[0];
    let method = if root.kind() == VolumeKind::Zfs {
        "zfs"
    } else {
        "lvm"
    };
    let covered: Vec<String> = plan.covered.iter().map(|v| v.mount_point.clone()).collect();
    let space = match root.kind() {
        VolumeKind::LvmThin => format!("the thin pool has {} free", bytes(root.free_bytes)),
        VolumeKind::Lvm => {
            let group = root.source.split('/').next().unwrap_or_default();
            format!(
                "volume group {group} has {} free; kept for a day",
                bytes(root.free_bytes)
            )
        }
        _ => format!("{} free", bytes(root.free_bytes)),
    };
    let summary = format!(
        "{} snapshot of {} ({space})",
        if method == "zfs" { "ZFS" } else { "LVM" },
        words(&covered)
    );
    Ok(LocalPlan {
        method,
        not_covered: plan
            .uncovered
            .iter()
            .map(|v| v.mount_point.clone())
            .collect(),
        covered,
        summary,
    })
}

/// "/", "/ and /var", "/, /usr and /var".
fn words(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// [`plan_of`] for `server`, from the database.
pub async fn support(
    pool: &PgPool,
    server: Uuid,
) -> Result<Result<LocalPlan, LocalUnavailable>, sqlx::Error> {
    let os_family: Option<String> =
        sqlx::query_scalar("SELECT os_family FROM servers WHERE id = $1")
            .bind(server)
            .fetch_optional(pool)
            .await?;
    let storage = load(pool, server).await?;
    Ok(plan_of(
        os_family.as_deref().unwrap_or_default(),
        storage.as_ref().map(|(s, _)| s),
    ))
}

/// The snapshots of one name, taken together.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SnapshotSet {
    pub name: String,
    /// `lvm`, `lvm_thin` or `zfs`, from the first volume.
    pub kind: String,
    pub created_at: DateTime<Utc>,
    /// When retention deletes it at the latest.
    pub delete_after: DateTime<Utc>,
    pub volumes: Vec<StoredSnapshot>,
}

impl SnapshotSet {
    /// Holds a classic LVM snapshot.
    pub fn classic(&self) -> bool {
        self.volumes.iter().any(|v| v.kind == "lvm")
    }

    pub fn invalid(&self) -> bool {
        self.volumes.iter().any(|v| v.invalid)
    }
}

/// `snapshots` grouped by name, oldest first.
pub fn sets(snapshots: &[StoredSnapshot]) -> Vec<SnapshotSet> {
    let mut by_name: BTreeMap<&str, Vec<&StoredSnapshot>> = BTreeMap::new();
    for snapshot in snapshots {
        by_name.entry(&snapshot.name).or_default().push(snapshot);
    }
    let mut sets: Vec<SnapshotSet> = by_name
        .into_iter()
        .map(|(name, volumes)| {
            let created_at = volumes
                .iter()
                .map(|v| v.created_at)
                .min()
                .unwrap_or_default();
            let mut set = SnapshotSet {
                name: name.to_owned(),
                kind: volumes[0].kind.clone(),
                created_at,
                delete_after: created_at,
                volumes: volumes.into_iter().cloned().collect(),
            };
            set.delete_after = created_at + if set.classic() { KEEP_CLASSIC } else { KEEP };
            set
        })
        .collect();
    sets.sort_by(|a, b| (a.created_at, &a.name).cmp(&(b.created_at, &b.name)));
    sets
}

/// Names retention deletes at `now`: invalid ones, those past their time,
/// and those beyond the newest [`KEEP_NEWEST`].
pub fn due(sets: &[SnapshotSet], now: DateTime<Utc>) -> Vec<String> {
    let beyond = sets.len().saturating_sub(KEEP_NEWEST);
    sets.iter()
        .enumerate()
        .filter(|(i, set)| *i < beyond || set.invalid() || set.delete_after <= now)
        .map(|(_, set)| set.name.clone())
        .collect()
}

/// The name of the snapshot before step `step` of remediation incident
/// `incident`: deterministic, so a retried step finds the one it took.
pub fn incident_name(incident: Uuid, step: u32) -> String {
    let hex = incident.simple().to_string();
    format!("monitorflow-incident-{}-{step}", &hex[..12])
}

/// The name of the snapshot update campaign `campaign` takes of each of
/// its servers before its update.
pub fn campaign_name(campaign: Uuid) -> String {
    let hex = campaign.simple().to_string();
    format!("monitorflow-campaign-{}", &hex[..12])
}

/// Takes local snapshots through agents' commands.
#[derive(Debug, Clone)]
pub struct LocalSnapshots {
    commands: Commands,
}

impl LocalSnapshots {
    pub fn new(commands: Commands) -> Self {
        Self { commands }
    }

    pub fn pool(&self) -> &PgPool {
        self.commands.store().pool()
    }

    /// Queues `take_snapshot` of `name` on `server` and waits for its
    /// result: the command's error when it did not succeed.
    pub async fn take(&self, server: Uuid, name: &str) -> Result<(), String> {
        let pool = self.pool();
        let peer_id: String = sqlx::query_scalar("SELECT peer_id FROM servers WHERE id = $1")
            .bind(server)
            .fetch_one(pool)
            .await
            .map_err(|e| e.to_string())?;
        let payload = TakeSnapshot {
            name: name.to_owned(),
        }
        .encode_to_vec();
        let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
        let row = crate::commands::CommandStore::enqueue_in(
            &mut *tx,
            &NewCommand {
                server_id: server,
                kind: TAKE_SNAPSHOT.into(),
                payload,
                ttl: TAKE_TTL,
                requested_by: None,
                campaign_id: None,
            },
        )
        .await
        .map_err(|e| e.to_string())?;
        audit_snapshot(&mut *tx, "server.snapshot.take", server, name, row.id)
            .await
            .map_err(|e| e.to_string())?;
        tx.commit().await.map_err(|e| e.to_string())?;
        self.commands.dispatch(&peer_id);
        let store = self.commands.store();
        let limit = tokio::time::Instant::now() + TAKE_TTL + RESULT_GRACE;
        let mut row = row;
        while !row.state.is_final() {
            if tokio::time::Instant::now() >= limit {
                return Err(format!("no result from the agent for snapshot {name}"));
            }
            tokio::time::sleep(POLL).await;
            row = store
                .get(row.id)
                .await
                .map_err(|e| e.to_string())?
                .unwrap_or(row);
        }
        match row.state {
            State::Succeeded => Ok(()),
            state => Err(row
                .error
                .unwrap_or_else(|| format!("the snapshot command {}", state.as_str()))),
        }
    }
}

async fn audit_snapshot(
    db: impl PgExecutor<'_>,
    action: &str,
    server: Uuid,
    name: &str,
    command: Uuid,
) -> Result<(), sqlx::Error> {
    audit::record(
        db,
        Event {
            actor_kind: "system",
            actor_id: "snapshots",
            action,
            target: Some(("server", &server.to_string())),
            details: serde_json::json!({ "name": name, "command_id": command }),
        },
    )
    .await
}

/// One retention pass at `now`: queues `delete_snapshot` for every
/// snapshot set [`due`] on a server not revoked, unless one was asked in
/// the last hours. Returns the deletes queued. One server instance at a
/// time runs it, under a transaction lock.
pub async fn prune(commands: &Commands, now: DateTime<Utc>) -> Result<usize, sqlx::Error> {
    let pool = commands.store().pool();
    let mut tx = pool.begin().await?;
    let locked: bool = sqlx::query_scalar(
        "SELECT pg_try_advisory_xact_lock(hashtext('monitorflow.local_snapshots'))",
    )
    .fetch_one(&mut *tx)
    .await?;
    if !locked {
        return Ok(0);
    }
    let rows: Vec<(Uuid, String, sqlx::types::Json<Storage>)> = sqlx::query_as(
        "SELECT s.id, s.peer_id, st.report FROM server_storage st
         JOIN servers s ON s.id = st.server_id
         WHERE s.revoked_at IS NULL",
    )
    .fetch_all(&mut *tx)
    .await?;
    let mut queued = Vec::new();
    for (server, peer_id, storage) in rows {
        for name in due(&sets(&storage.snapshots), now) {
            let payload = DeleteSnapshot { name: name.clone() }.encode_to_vec();
            let recent: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM commands
                 WHERE server_id = $1 AND kind = $2 AND payload = $3 AND issued_at > $4)",
            )
            .bind(server)
            .bind(DELETE_SNAPSHOT)
            .bind(&payload)
            .bind(now - RETRY_DELETE_AFTER)
            .fetch_one(&mut *tx)
            .await?;
            if recent {
                continue;
            }
            let row = crate::commands::CommandStore::enqueue_in(
                &mut *tx,
                &NewCommand {
                    server_id: server,
                    kind: DELETE_SNAPSHOT.into(),
                    payload,
                    ttl: PRUNE_EVERY,
                    requested_by: None,
                    campaign_id: None,
                },
            )
            .await?;
            audit_snapshot(&mut *tx, "server.snapshot.delete", server, &name, row.id).await?;
            queued.push(peer_id.clone());
        }
    }
    tx.commit().await?;
    for peer_id in &queued {
        commands.dispatch(peer_id);
    }
    Ok(queued.len())
}

/// Runs [`prune`] every [`PRUNE_EVERY`] until the process ends.
pub async fn run_retention(commands: Commands) {
    let mut every = tokio::time::interval(PRUNE_EVERY);
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        every.tick().await;
        match prune(&commands, Utc::now()).await {
            Ok(0) => {}
            Ok(queued) => tracing::info!(queued, "local snapshots past retention to delete"),
            Err(error) => tracing::warn!(%error, "cannot prune local snapshots; will retry"),
        }
    }
}

#[cfg(test)]
mod tests {
    use monitorflow_common::proto::v1::LocalSnapshot;

    use super::*;

    const GIB: u64 = 1 << 30;

    fn volume(mount: &str, kind: &str, source: &str, free: u64) -> StoredVolume {
        StoredVolume {
            mount_point: mount.into(),
            fs_type: if kind == "zfs" { "zfs" } else { "ext4" }.into(),
            kind: kind.into(),
            source: source.into(),
            size_bytes: 20 * GIB,
            free_bytes: free,
            pool_bytes: 100 * GIB,
            metadata_percent: 10.0,
        }
    }

    fn storage(volumes: Vec<StoredVolume>) -> Storage {
        Storage {
            volumes,
            ..Storage::default()
        }
    }

    fn at(hours_ago: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 3, 12, 0, 0).unwrap() - chrono::Duration::hours(hours_ago)
    }

    fn snapshot(name: &str, kind: &str, hours_ago: i64) -> StoredSnapshot {
        StoredSnapshot {
            name: name.into(),
            mount_point: "/".into(),
            kind: kind.into(),
            source: format!("vg0/{name}_root"),
            origin: "vg0/root".into(),
            created_at: at(hours_ago),
            size_bytes: 0,
            used_percent: 0.0,
            invalid: false,
        }
    }

    #[test]
    fn says_what_a_change_gets() {
        let thin = storage(vec![
            volume("/", "lvm_thin", "vg0/root", 40 * GIB),
            volume("/var", "lvm_thin", "vg0/var", 40 * GIB),
            volume("/boot", "plain", "/dev/sda1", 0),
        ]);
        let plan = plan_of("debian", Some(&thin)).unwrap();
        assert_eq!(plan.method, "lvm");
        assert_eq!(
            plan.summary,
            "LVM snapshot of / and /var (the thin pool has 40.0 GiB free)"
        );
        assert_eq!(plan.not_covered, ["/boot"]);

        let classic = storage(vec![volume("/", "lvm", "rl/root", 10 * GIB)]);
        assert_eq!(
            plan_of("rhel", Some(&classic)).unwrap().summary,
            "LVM snapshot of / (volume group rl has 10.0 GiB free; kept for a day)"
        );
        let zfs = storage(vec![
            volume("/", "zfs", "rpool/ROOT/debian", 50 * GIB),
            volume("/usr", "zfs", "rpool/usr", 50 * GIB),
            volume("/var", "zfs", "rpool/var", 50 * GIB),
        ]);
        let plan = plan_of("debian", Some(&zfs)).unwrap();
        assert_eq!(plan.method, "zfs");
        assert_eq!(
            plan.summary,
            "ZFS snapshot of /, /usr and /var (50.0 GiB free)"
        );
    }

    #[test]
    fn says_why_not() {
        let plain = storage(vec![volume("/", "plain", "/dev/sda1", 0)]);
        let why = |os: &str, s: Option<&Storage>| {
            let why = plan_of(os, s).unwrap_err();
            (why.code(), why.to_string())
        };
        assert_eq!(
            why("debian", Some(&plain)),
            ("root_not_covered", "/ is on plain ext4".into())
        );
        assert_eq!(why("windows", Some(&plain)).0, "windows");
        assert_eq!(why("debian", None).0, "not_reported");
        let sudo = Storage {
            error: Some("sudo: a password is required".into()),
            ..Storage::default()
        };
        assert_eq!(why("debian", Some(&sudo)).0, "sudo_rules");
        let broken = Storage {
            error: Some("cannot read the mounts".into()),
            ..Storage::default()
        };
        assert_eq!(
            why("debian", Some(&broken)),
            (
                "inspect",
                "the agent cannot inspect its storage: cannot read the mounts".into()
            )
        );
        let full = storage(vec![volume("/", "lvm_thin", "vg0/root", 5 * GIB)]);
        assert_eq!(why("debian", Some(&full)).0, "pool_full");
        let no_room = storage(vec![volume("/", "lvm", "vg0/root", 0)]);
        assert_eq!(why("debian", Some(&no_room)).0, "no_space");
    }

    #[test]
    fn retention_deletes_by_age_validity_and_count() {
        let now = at(0);
        // Classic: a day. Thin: a week.
        let sets_of = |snapshots: Vec<StoredSnapshot>| sets(&snapshots);
        let classic = sets_of(vec![snapshot("monitorflow-a", "lvm", 25)]);
        assert_eq!(due(&classic, now), ["monitorflow-a"]);
        assert_eq!(classic[0].delete_after, at(1));
        let young = sets_of(vec![snapshot("monitorflow-a", "lvm", 23)]);
        assert!(due(&young, now).is_empty());
        let thin = sets_of(vec![
            snapshot("monitorflow-old", "lvm_thin", 7 * 24 + 1),
            snapshot("monitorflow-new", "lvm_thin", 6 * 24),
        ]);
        assert_eq!(due(&thin, now), ["monitorflow-old"]);

        let mut invalid = snapshot("monitorflow-full", "lvm", 1);
        invalid.invalid = true;
        assert_eq!(due(&sets_of(vec![invalid]), now), ["monitorflow-full"]);

        // The newest 3 names stay; a name's volumes count once.
        let mut many: Vec<StoredSnapshot> = (1..=4)
            .map(|i| snapshot(&format!("monitorflow-s{i}"), "zfs", 10 - i))
            .collect();
        let mut var = snapshot("monitorflow-s1", "zfs", 9);
        var.mount_point = "/var".into();
        many.push(var);
        let sets = sets(&many);
        assert_eq!(sets.len(), 4);
        assert_eq!(sets[0].volumes.len(), 2);
        assert_eq!(due(&sets, now), ["monitorflow-s1"]);
    }

    #[test]
    fn stores_only_what_fits() {
        let good = LocalSnapshot {
            name: "monitorflow-a".into(),
            mount_point: "/".into(),
            kind: VolumeKind::LvmThin.into(),
            source: "vg0/monitorflow-a_root".into(),
            origin: "vg0/root".into(),
            created_at_unix: 1_793_697_300,
            ..LocalSnapshot::default()
        };
        let report = StorageReport {
            volumes: vec![Volume {
                mount_point: "/".into(),
                fs_type: "ext4".into(),
                kind: VolumeKind::LvmThin.into(),
                source: "vg0/root".into(),
                metadata_percent: f64::NAN,
                ..Volume::default()
            }],
            snapshots: vec![
                good.clone(),
                LocalSnapshot {
                    name: "before-upgrade".into(),
                    ..good.clone()
                },
                LocalSnapshot {
                    source: "x".repeat(300),
                    ..good
                },
            ],
            error: String::new(),
        };
        let stored = Storage::from_report(&report);
        assert_eq!(stored.volumes[0].kind, "lvm_thin");
        assert_eq!(stored.volumes[0].metadata_percent, 0.0);
        assert_eq!(stored.snapshots.len(), 1);
        assert_eq!(stored.error, None);
        assert_eq!(stored.proto_volumes()[0].kind(), VolumeKind::LvmThin);
    }

    #[test]
    fn incident_names_are_valid_and_stable() {
        let incident = Uuid::parse_str("1a2b3c4d-5e6f-4000-8000-000000000000").unwrap();
        let name = incident_name(incident, 2);
        assert_eq!(name, "monitorflow-incident-1a2b3c4d5e6f-2");
        assert!(snapshot::valid_name(&name));
        assert!(snapshot::valid_name(&incident_name(incident, u32::MAX)));
    }
}
