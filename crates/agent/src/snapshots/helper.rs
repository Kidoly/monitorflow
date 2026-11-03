//! The `snapshots` root helper (ADR-0094): `inspect` reads the system
//! volumes and MonitorFlow's snapshots, `take` and `delete` read a
//! [`Request`] on stdin. The request only names the snapshot: which
//! volumes it covers, its size and the tools' arguments are decided here,
//! so sudoers can allow these exact lines and nothing else.
//!
//! Ownership lives on the host: LVM snapshots carry the `monitorflow` tag
//! and ZFS ones the `monitorflow:managed=yes` property. Only those are
//! listed, so `delete` can never touch a snapshot someone else made, even
//! one named `monitorflow-…`.

use std::io;
use std::path::PathBuf;
use std::process::Command;

use monitorflow_common::proto::v1::{LocalSnapshot, StorageReport, Volume, VolumeKind};
use monitorflow_common::snapshot::{self, valid_name, Refusal, SYSTEM_MOUNTS};
use serde::{Deserialize, Serialize};

use super::parse::{self, Lv, ZfsSnapshot, LVS_FIELDS, VGS_FIELDS, ZFS_PROPERTY};

/// Longest request read on stdin.
pub const MAX_REQUEST_BYTES: usize = 4096;
/// The LVM tag on MonitorFlow's snapshots.
pub const LVM_TAG: &str = "monitorflow";
/// LVM's longest logical volume name.
const MAX_LV_NAME: usize = 127;
const MIB: u64 = 1 << 20;

/// What `take` and `delete` read on stdin: `{"name": "monitorflow-…"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub name: String,
}

impl Request {
    /// Refuses anything but a JSON object with a valid name.
    pub fn parse(bytes: &[u8]) -> Result<Self, HelperError> {
        if bytes.len() > MAX_REQUEST_BYTES {
            return Err(HelperError::Request(format!(
                "larger than {MAX_REQUEST_BYTES} bytes"
            )));
        }
        let request: Self =
            serde_json::from_slice(bytes).map_err(|e| HelperError::Request(e.to_string()))?;
        if !valid_name(&request.name) {
            return Err(HelperError::Request(format!(
                "invalid snapshot name `{}`",
                request.name
            )));
        }
        Ok(request)
    }

    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum HelperError {
    #[error("the request is not a snapshot request: {0}")]
    Request(String),
    #[error("cannot read the mounts: {0}")]
    Mounts(String),
    #[error("{0}")]
    Parse(#[from] parse::ParseError),
    #[error("no snapshot: {0}")]
    Refused(Refusal),
    #[error("`{line}` exited with {code}: {stderr}")]
    Failed {
        line: String,
        code: i32,
        stderr: String,
    },
    #[error("cannot run {0}")]
    Spawn(String),
    #[error("the snapshot name {0} is too long for LVM")]
    TooLong(String),
}

/// Storage tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Lvm,
    Zfs,
}

/// A finished tool.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ran {
    pub line: String,
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Ran {
    fn failed(&self) -> HelperError {
        let stderr = self.stderr.lines().find(|l| !l.trim().is_empty());
        HelperError::Failed {
            line: self.line.clone(),
            code: self.code,
            stderr: stderr.unwrap_or("").trim().chars().take(300).collect(),
        }
    }
}

/// The host as the helper sees it. [`Host`] is the real one; tests use
/// fixtures.
pub trait System {
    fn mountinfo(&self) -> io::Result<String>;
    fn has(&self, tool: Tool) -> bool;
    fn run(&self, tool: Tool, args: &[String]) -> io::Result<Ran>;
}

/// The real host: tools at fixed paths, in the C locale.
#[derive(Debug, Clone)]
pub struct Host {
    pub lvm: PathBuf,
    pub zfs: PathBuf,
}

impl Default for Host {
    fn default() -> Self {
        let first = |paths: [&str; 2]| {
            PathBuf::from(
                paths
                    .iter()
                    .find(|p| std::path::Path::new(p).exists())
                    .unwrap_or(&paths[0]),
            )
        };
        Self {
            lvm: first(["/usr/sbin/lvm", "/sbin/lvm"]),
            zfs: first(["/usr/sbin/zfs", "/sbin/zfs"]),
        }
    }
}

impl System for Host {
    fn mountinfo(&self) -> io::Result<String> {
        std::fs::read_to_string("/proc/self/mountinfo")
    }

    fn has(&self, tool: Tool) -> bool {
        self.path(tool).exists()
    }

    fn run(&self, tool: Tool, args: &[String]) -> io::Result<Ran> {
        let path = self.path(tool);
        let output = Command::new(path)
            .args(args)
            .env("LC_ALL", "C")
            // lvm warns about every descriptor sudo leaves open.
            .env("LVM_SUPPRESS_FD_WARNINGS", "1")
            .stdin(std::process::Stdio::null())
            .output()?;
        Ok(Ran {
            line: format!("{} {}", path.display(), args.join(" ")),
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

impl Host {
    fn path(&self, tool: Tool) -> &std::path::Path {
        match tool {
            Tool::Lvm => &self.lvm,
            Tool::Zfs => &self.zfs,
        }
    }
}

/// Kind of a volume or snapshot, as `inspect` prints it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Plain,
    Lvm,
    LvmThin,
    Zfs,
}

impl From<Kind> for VolumeKind {
    fn from(kind: Kind) -> Self {
        match kind {
            Kind::Plain => Self::Plain,
            Kind::Lvm => Self::Lvm,
            Kind::LvmThin => Self::LvmThin,
            Kind::Zfs => Self::Zfs,
        }
    }
}

/// A system volume, as `inspect` prints it; see the `Volume` message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VolumeInfo {
    pub mount_point: String,
    pub fs_type: String,
    pub kind: Kind,
    pub source: String,
    pub size_bytes: u64,
    pub free_bytes: u64,
    pub pool_bytes: u64,
    #[serde(default)]
    pub metadata_percent: f64,
}

/// One volume's snapshot, as `inspect` prints it; see `LocalSnapshot`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotInfo {
    pub name: String,
    #[serde(default)]
    pub mount_point: String,
    pub kind: Kind,
    pub source: String,
    pub origin: String,
    pub created_at_unix: i64,
    pub size_bytes: u64,
    #[serde(default)]
    pub used_percent: f64,
    #[serde(default)]
    pub invalid: bool,
}

/// What `inspect` prints, as JSON.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Inventory {
    pub volumes: Vec<VolumeInfo>,
    pub snapshots: Vec<SnapshotInfo>,
}

impl Inventory {
    pub fn to_report(&self) -> StorageReport {
        StorageReport {
            volumes: self
                .volumes
                .iter()
                .map(|v| Volume {
                    mount_point: v.mount_point.clone(),
                    fs_type: v.fs_type.clone(),
                    kind: VolumeKind::from(v.kind).into(),
                    source: v.source.clone(),
                    size_bytes: v.size_bytes,
                    free_bytes: v.free_bytes,
                    pool_bytes: v.pool_bytes,
                    metadata_percent: v.metadata_percent,
                })
                .collect(),
            snapshots: self
                .snapshots
                .iter()
                .map(|s| LocalSnapshot {
                    name: s.name.clone(),
                    mount_point: s.mount_point.clone(),
                    kind: VolumeKind::from(s.kind).into(),
                    source: s.source.clone(),
                    origin: s.origin.clone(),
                    created_at_unix: s.created_at_unix,
                    size_bytes: s.size_bytes,
                    used_percent: s.used_percent,
                    invalid: s.invalid,
                })
                .collect(),
            error: String::new(),
        }
    }
}

/// Runs `tool args`; a non-zero exit is an error.
fn run(system: &dyn System, tool: Tool, args: &[&str]) -> Result<Ran, HelperError> {
    let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    let ran = system
        .run(tool, &args)
        .map_err(|e| HelperError::Spawn(format!("{tool:?}: {e}").to_lowercase()))?;
    if ran.code != 0 {
        return Err(ran.failed());
    }
    Ok(ran)
}

/// The system volumes and MonitorFlow's snapshots. LVM or ZFS tools that
/// are missing or fail, as inside a container, leave their volumes plain:
/// a snapshot is then refused, never attempted on a guess.
pub fn inspect(system: &dyn System) -> Result<Inventory, HelperError> {
    let mounts = parse::mountinfo(
        &system
            .mountinfo()
            .map_err(|e| HelperError::Mounts(e.to_string()))?,
        &SYSTEM_MOUNTS,
    );
    let mut lvs = Vec::new();
    let mut vgs = Vec::new();
    if system.has(Tool::Lvm) {
        let report = ["--reportformat", "json", "--units", "b", "--nosuffix", "-o"];
        let lvs_args: Vec<&str> = std::iter::once("lvs")
            .chain(report)
            .chain([LVS_FIELDS])
            .collect();
        let vgs_args: Vec<&str> = std::iter::once("vgs")
            .chain(report)
            .chain([VGS_FIELDS])
            .collect();
        if let (Ok(l), Ok(v)) = (
            run(system, Tool::Lvm, &lvs_args),
            run(system, Tool::Lvm, &vgs_args),
        ) {
            lvs = parse::lvs(&l.stdout)?;
            vgs = parse::vgs(&v.stdout)?;
        }
    }
    let mut datasets = Vec::new();
    let mut zfs_snapshots = Vec::new();
    if system.has(Tool::Zfs) {
        let filesystems = [
            "list",
            "-H",
            "-p",
            "-t",
            "filesystem",
            "-o",
            "name,used,avail",
        ];
        let properties = format!("name,creation,used,{ZFS_PROPERTY}");
        let snapshots = ["list", "-H", "-p", "-t", "snapshot", "-o", &properties];
        if let (Ok(f), Ok(s)) = (
            run(system, Tool::Zfs, &filesystems),
            run(system, Tool::Zfs, &snapshots),
        ) {
            datasets = parse::zfs_filesystems(&f.stdout)?;
            zfs_snapshots = parse::zfs_snapshots(&s.stdout)?;
        }
    }

    let volumes: Vec<VolumeInfo> = mounts
        .iter()
        .map(|mount| {
            let mut volume = VolumeInfo {
                mount_point: mount.mount_point.clone(),
                fs_type: mount.fs_type.clone(),
                kind: Kind::Plain,
                source: mount.source.clone(),
                size_bytes: 0,
                free_bytes: 0,
                pool_bytes: 0,
                metadata_percent: 0.0,
            };
            if mount.fs_type == "zfs" {
                if let Some(d) = datasets.iter().find(|d| d.name == mount.source) {
                    volume.kind = Kind::Zfs;
                    volume.size_bytes = d.used;
                    volume.free_bytes = d.available;
                    volume.pool_bytes = d.used + d.available;
                }
                return volume;
            }
            let Some(lv) = lvs
                .iter()
                .find(|lv| lv.major == mount.major && lv.minor == mount.minor && lv.major >= 0)
            else {
                return volume;
            };
            volume.source = lv.full_name();
            volume.size_bytes = lv.size;
            match lv.kind_letter() {
                'V' => {
                    if let Some(pool) = lvs.iter().find(|p| p.vg == lv.vg && p.name == lv.pool) {
                        let used = pool.data_percent.unwrap_or(100.0).clamp(0.0, 100.0);
                        volume.kind = Kind::LvmThin;
                        volume.pool_bytes = pool.size;
                        volume.free_bytes = (pool.size as f64 * (100.0 - used) / 100.0) as u64;
                        volume.metadata_percent = pool.metadata_percent.unwrap_or(100.0);
                    }
                }
                '-' | 'o' => {
                    if let Some(vg) = vgs.iter().find(|vg| vg.name == lv.vg) {
                        volume.kind = Kind::Lvm;
                        volume.pool_bytes = vg.size;
                        volume.free_bytes = vg.free;
                    }
                }
                _ => {}
            }
            volume
        })
        .collect();

    let mount_of = |origin: &str| {
        volumes
            .iter()
            .find(|v| v.kind != Kind::Plain && v.source == origin)
            .map(|v| v.mount_point.clone())
            .unwrap_or_default()
    };
    let mut snapshots: Vec<SnapshotInfo> = lvs
        .iter()
        .filter_map(lvm_snapshot)
        .map(|mut s| {
            s.mount_point = mount_of(&s.origin);
            s
        })
        .collect();
    snapshots.extend(zfs_snapshots.iter().filter_map(|z| {
        let mut s = zfs_snapshot(z)?;
        s.mount_point = mount_of(&s.origin);
        Some(s)
    }));
    snapshots.sort_by(|a, b| {
        (a.created_at_unix, &a.name, &a.source).cmp(&(b.created_at_unix, &b.name, &b.source))
    });
    Ok(Inventory { volumes, snapshots })
}

/// `lv` as MonitorFlow's snapshot: tagged, named `<name>_<origin>`.
fn lvm_snapshot(lv: &Lv) -> Option<SnapshotInfo> {
    if !lv.tags.iter().any(|t| t == LVM_TAG) || lv.origin.is_empty() {
        return None;
    }
    let (name, _) = lv.name.split_once('_')?;
    if !valid_name(name) {
        return None;
    }
    let thin = lv.thin();
    let invalid = lv.kind_letter() == 'S' || lv.attr.chars().nth(4) == Some('I');
    Some(SnapshotInfo {
        name: name.to_owned(),
        mount_point: String::new(),
        kind: if thin { Kind::LvmThin } else { Kind::Lvm },
        source: lv.full_name(),
        origin: format!("{}/{}", lv.vg, lv.origin),
        created_at_unix: lv.created_unix.unwrap_or(0),
        // A thin snapshot shares the pool and holds no space of its own.
        size_bytes: if thin { 0 } else { lv.size },
        used_percent: if thin {
            0.0
        } else {
            lv.data_percent.unwrap_or(0.0)
        },
        invalid,
    })
}

fn zfs_snapshot(z: &ZfsSnapshot) -> Option<SnapshotInfo> {
    (z.managed && valid_name(&z.name)).then(|| SnapshotInfo {
        name: z.name.clone(),
        mount_point: String::new(),
        kind: Kind::Zfs,
        source: format!("{}@{}", z.dataset, z.name),
        origin: z.dataset.clone(),
        created_at_unix: z.created_unix,
        size_bytes: z.used,
        used_percent: 0.0,
        invalid: false,
    })
}

/// What `take` or `delete` ran, and what it ended with.
#[derive(Debug, Default)]
pub struct Outcome {
    pub runs: Vec<Ran>,
    /// Said on stdout when nothing needed running.
    pub note: Option<String>,
    pub error: Option<HelperError>,
}

impl Outcome {
    fn note(note: impl Into<String>) -> Self {
        Self {
            note: Some(note.into()),
            ..Self::default()
        }
    }

    fn fail(mut self, error: HelperError) -> Self {
        self.error = Some(error);
        self
    }

    /// Records `run` and stops at its failure.
    fn step(&mut self, system: &dyn System, tool: Tool, args: &[String]) -> bool {
        match system.run(tool, args) {
            Ok(ran) => {
                let failed = (ran.code != 0).then(|| ran.failed());
                self.runs.push(ran);
                if let Some(error) = failed {
                    self.error = Some(error);
                    return false;
                }
                true
            }
            Err(e) => {
                self.error = Some(HelperError::Spawn(format!("{tool:?}: {e}").to_lowercase()));
                false
            }
        }
    }
}

/// What `take` created, to remove again when a later step fails.
enum Created {
    Lv(String),
    Zfs(Vec<String>),
}

/// Snapshots every system volume on LVM or ZFS as `name`, after
/// [`snapshot::plan`] agreed. A name already taken is success without a
/// second snapshot, so a retried step takes one. When a step fails, what
/// this call created is removed, and the host is as before.
pub fn take(system: &dyn System, name: &str) -> Outcome {
    if !valid_name(name) {
        return Outcome::default().fail(HelperError::Request(format!(
            "invalid snapshot name `{name}`"
        )));
    }
    let inventory = match inspect(system) {
        Ok(inventory) => inventory,
        Err(error) => return Outcome::default().fail(error),
    };
    if inventory.snapshots.iter().any(|s| s.name == name) {
        return Outcome::note(format!("{name} already exists"));
    }
    let report = inventory.to_report();
    let plan = match snapshot::plan(&report.volumes) {
        Ok(plan) => plan,
        Err(refusal) => return Outcome::default().fail(HelperError::Refused(refusal)),
    };
    let mut outcome = Outcome::default();
    let mut created = Vec::new();
    let mut datasets: Vec<&str> = Vec::new();
    for volume in &plan.covered {
        match volume.kind() {
            VolumeKind::Lvm | VolumeKind::LvmThin => {
                let Some((vg, lv)) = volume.source.split_once('/') else {
                    continue;
                };
                let snapshot = format!("{name}_{lv}");
                if snapshot.len() > MAX_LV_NAME {
                    undo(system, &mut outcome, created);
                    return outcome.fail(HelperError::TooLong(snapshot));
                }
                let mut args: Vec<String> = [
                    "lvcreate",
                    "--snapshot",
                    "--name",
                    &snapshot,
                    "--addtag",
                    LVM_TAG,
                ]
                .map(str::to_owned)
                .to_vec();
                if volume.kind() == VolumeKind::Lvm {
                    let size = snapshot::classic_size(volume.size_bytes).div_ceil(MIB);
                    args.extend(["--size".to_owned(), format!("{size}m")]);
                }
                args.push(volume.source.clone());
                if !outcome.step(system, Tool::Lvm, &args) {
                    let error = outcome.error.take();
                    undo(system, &mut outcome, created);
                    outcome.error = error;
                    return outcome;
                }
                created.push(Created::Lv(format!("{vg}/{snapshot}")));
            }
            VolumeKind::Zfs => datasets.push(&volume.source),
            _ => {}
        }
    }
    // A recursive snapshot of a dataset covers its children.
    let tops: Vec<String> = datasets
        .iter()
        .filter(|d| {
            !datasets
                .iter()
                .any(|other| d.starts_with(&format!("{other}/")))
        })
        .map(|d| format!("{d}@{name}"))
        .collect();
    if !tops.is_empty() {
        let mut args: Vec<String> = ["snapshot", "-r", "-o"].map(str::to_owned).to_vec();
        args.push(format!("{ZFS_PROPERTY}=yes"));
        args.extend(tops.iter().cloned());
        // One `zfs snapshot` is atomic: all of them or none.
        if !outcome.step(system, Tool::Zfs, &args) {
            let error = outcome.error.take();
            undo(system, &mut outcome, created);
            outcome.error = error;
            return outcome;
        }
        created.push(Created::Zfs(tops));
    }
    outcome
}

/// Removes what a failed `take` created; failures here are only recorded
/// in the output, as the take's own error is the one to report.
fn undo(system: &dyn System, outcome: &mut Outcome, created: Vec<Created>) {
    for item in created.into_iter().rev() {
        match item {
            Created::Lv(lv) => {
                let args = ["lvremove", "--yes", &lv].map(str::to_owned);
                outcome.step(system, Tool::Lvm, &args);
            }
            Created::Zfs(snapshots) => {
                for snapshot in snapshots {
                    let args = ["destroy", "-r", &snapshot].map(str::to_owned);
                    outcome.step(system, Tool::Zfs, &args);
                }
            }
        }
    }
}

/// Deletes MonitorFlow's snapshots named `name`, on every volume. Nothing
/// to delete is success. Every one is attempted; the first failure is the
/// error.
pub fn delete(system: &dyn System, name: &str) -> Outcome {
    if !valid_name(name) {
        return Outcome::default().fail(HelperError::Request(format!(
            "invalid snapshot name `{name}`"
        )));
    }
    let inventory = match inspect(system) {
        Ok(inventory) => inventory,
        Err(error) => return Outcome::default().fail(error),
    };
    let targets: Vec<&SnapshotInfo> = inventory
        .snapshots
        .iter()
        .filter(|s| s.name == name)
        .collect();
    if targets.is_empty() {
        return Outcome::note(format!("no snapshot {name}"));
    }
    let mut outcome = Outcome::default();
    let mut first_error = None;
    for target in targets {
        let (tool, args) = match target.kind {
            Kind::Zfs => (Tool::Zfs, ["destroy", target.source.as_str()]),
            _ => (Tool::Lvm, ["lvremove", "--yes"]),
        };
        let mut args: Vec<String> = args.map(str::to_owned).to_vec();
        if tool == Tool::Lvm {
            args.push(target.source.clone());
        }
        if !outcome.step(system, tool, &args) {
            first_error = first_error.or(outcome.error.take());
        }
    }
    outcome.error = first_error;
    outcome
}
