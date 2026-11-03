//! Parsers for what the helper reads: `/proc/self/mountinfo`, `lvs` and
//! `vgs` in their JSON report format, and `zfs list -H -p`. Pure, so each
//! is tested on fixtures taken from real hosts.

use chrono::DateTime;
use serde::Deserialize;

/// One line of mountinfo, for a mount point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    pub major: i64,
    pub minor: i64,
    pub mount_point: String,
    pub fs_type: String,
    /// The device or, for ZFS, the dataset.
    pub source: String,
}

/// The mounts of `mount_points`, the last one of each winning as it hides
/// the others; lines that do not parse are skipped.
pub fn mountinfo(text: &str, mount_points: &[&str]) -> Vec<Mount> {
    let mut mounts: Vec<Mount> = Vec::new();
    for mount in text.lines().filter_map(mount_line) {
        if !mount_points.contains(&mount.mount_point.as_str()) {
            continue;
        }
        mounts.retain(|m| m.mount_point != mount.mount_point);
        mounts.push(mount);
    }
    mounts.sort_by_key(|m| {
        mount_points
            .iter()
            .position(|p| *p == m.mount_point)
            .unwrap_or(usize::MAX)
    });
    mounts
}

/// `id parent major:minor root mount-point options [optional...] - fstype
/// source super-options`.
fn mount_line(line: &str) -> Option<Mount> {
    let (head, tail) = line.split_once(" - ")?;
    let head: Vec<&str> = head.split(' ').collect();
    let mut tail = tail.split(' ');
    let (major, minor) = head.get(2)?.split_once(':')?;
    Some(Mount {
        major: major.parse().ok()?,
        minor: minor.parse().ok()?,
        mount_point: unescape(head.get(4)?),
        fs_type: tail.next()?.to_owned(),
        source: unescape(tail.next()?),
    })
}

/// Undoes the kernel's octal escapes of space, tab, newline and `\`.
fn unescape(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let octal = bytes.get(i + 1..i + 4).and_then(|d| {
            d.iter()
                .all(|b| (b'0'..=b'7').contains(b))
                .then(|| u8::from_str_radix(std::str::from_utf8(d).ok()?, 8).ok())
                .flatten()
        });
        match (bytes[i], octal) {
            (b'\\', Some(byte)) => {
                out.push(byte);
                i += 4;
            }
            (byte, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A row of `lvs`.
#[derive(Debug, Clone, PartialEq)]
pub struct Lv {
    pub vg: String,
    pub name: String,
    /// `lv_attr`: the first letter is the type (`V` thin volume, `t` thin
    /// pool, `s` snapshot, `S` invalid snapshot, `o` origin, `-` plain),
    /// the fifth the state (`I` invalid snapshot).
    pub attr: String,
    pub size: u64,
    pub pool: String,
    pub origin: String,
    pub data_percent: Option<f64>,
    pub metadata_percent: Option<f64>,
    pub tags: Vec<String>,
    pub created_unix: Option<i64>,
    /// -1 when inactive.
    pub major: i64,
    pub minor: i64,
}

impl Lv {
    pub fn kind_letter(&self) -> char {
        self.attr.chars().next().unwrap_or('-')
    }

    pub fn thin(&self) -> bool {
        self.kind_letter() == 'V'
    }

    pub fn full_name(&self) -> String {
        format!("{}/{}", self.vg, self.name)
    }
}

/// The fields `lvs` is asked for, in its `-o` syntax.
pub const LVS_FIELDS: &str = "vg_name,lv_name,lv_attr,lv_size,pool_lv,origin,data_percent,\
                              metadata_percent,lv_tags,lv_time,lv_kernel_major,lv_kernel_minor";
/// The fields `vgs` is asked for.
pub const VGS_FIELDS: &str = "vg_name,vg_size,vg_free";

#[derive(Deserialize)]
struct Report<T> {
    report: Vec<T>,
}

#[derive(Deserialize)]
struct LvSection {
    #[serde(default)]
    lv: Vec<LvRow>,
}

#[derive(Deserialize)]
struct LvRow {
    vg_name: String,
    lv_name: String,
    lv_attr: String,
    lv_size: String,
    #[serde(default)]
    pool_lv: String,
    #[serde(default)]
    origin: String,
    #[serde(default)]
    data_percent: String,
    #[serde(default)]
    metadata_percent: String,
    #[serde(default)]
    lv_tags: String,
    #[serde(default)]
    lv_time: String,
    #[serde(default)]
    lv_kernel_major: String,
    #[serde(default)]
    lv_kernel_minor: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("cannot read the output of {tool}: {message}")]
pub struct ParseError {
    pub tool: &'static str,
    pub message: String,
}

impl ParseError {
    fn new(tool: &'static str, message: impl ToString) -> Self {
        Self {
            tool,
            message: message.to_string(),
        }
    }
}

/// `lvs --reportformat json --units b --nosuffix -o` [`LVS_FIELDS`].
pub fn lvs(json: &str) -> Result<Vec<Lv>, ParseError> {
    let report: Report<LvSection> =
        serde_json::from_str(json).map_err(|e| ParseError::new("lvs", e))?;
    report
        .report
        .into_iter()
        .flat_map(|section| section.lv)
        .map(|row| {
            let size = |s: &str| {
                s.parse::<u64>()
                    .map_err(|_| ParseError::new("lvs", format!("size `{s}`")))
            };
            let percent = |s: &str| s.trim().parse::<f64>().ok();
            let number = |s: &str| s.trim().parse::<i64>().unwrap_or(-1);
            Ok(Lv {
                size: size(&row.lv_size)?,
                vg: row.vg_name,
                name: row.lv_name,
                attr: row.lv_attr,
                pool: row.pool_lv,
                origin: row.origin,
                data_percent: percent(&row.data_percent),
                metadata_percent: percent(&row.metadata_percent),
                tags: row
                    .lv_tags
                    .split(',')
                    .filter(|t| !t.is_empty())
                    .map(str::to_owned)
                    .collect(),
                // LVM's default `report/time_format`.
                created_unix: DateTime::parse_from_str(&row.lv_time, "%Y-%m-%d %H:%M:%S %z")
                    .ok()
                    .map(|t| t.timestamp()),
                major: number(&row.lv_kernel_major),
                minor: number(&row.lv_kernel_minor),
            })
        })
        .collect()
}

/// A row of `vgs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vg {
    pub name: String,
    pub size: u64,
    pub free: u64,
}

#[derive(Deserialize)]
struct VgSection {
    #[serde(default)]
    vg: Vec<VgRow>,
}

#[derive(Deserialize)]
struct VgRow {
    vg_name: String,
    vg_size: String,
    vg_free: String,
}

/// `vgs --reportformat json --units b --nosuffix -o` [`VGS_FIELDS`].
pub fn vgs(json: &str) -> Result<Vec<Vg>, ParseError> {
    let report: Report<VgSection> =
        serde_json::from_str(json).map_err(|e| ParseError::new("vgs", e))?;
    report
        .report
        .into_iter()
        .flat_map(|section| section.vg)
        .map(|row| {
            let size = |s: &str| {
                s.parse::<u64>()
                    .map_err(|_| ParseError::new("vgs", format!("size `{s}`")))
            };
            Ok(Vg {
                size: size(&row.vg_size)?,
                free: size(&row.vg_free)?,
                name: row.vg_name,
            })
        })
        .collect()
}

/// A ZFS filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dataset {
    pub name: String,
    pub used: u64,
    pub available: u64,
}

/// `zfs list -H -p -t filesystem -o name,used,avail`.
pub fn zfs_filesystems(text: &str) -> Result<Vec<Dataset>, ParseError> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            let [name, used, available] = fields[..] else {
                return Err(ParseError::new("zfs list", format!("line `{line}`")));
            };
            let number = |s: &str| {
                s.parse::<u64>()
                    .map_err(|_| ParseError::new("zfs list", format!("number `{s}`")))
            };
            Ok(Dataset {
                name: name.to_owned(),
                used: number(used)?,
                available: number(available)?,
            })
        })
        .collect()
}

/// The user property MonitorFlow sets on its ZFS snapshots.
pub const ZFS_PROPERTY: &str = "monitorflow:managed";

/// A ZFS snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZfsSnapshot {
    pub dataset: String,
    pub name: String,
    pub created_unix: i64,
    pub used: u64,
    /// [`ZFS_PROPERTY`] is `yes`.
    pub managed: bool,
}

/// `zfs list -H -p -t snapshot -o name,creation,used,`[`ZFS_PROPERTY`].
pub fn zfs_snapshots(text: &str) -> Result<Vec<ZfsSnapshot>, ParseError> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            let [full, creation, used, managed] = fields[..] else {
                return Err(ParseError::new("zfs list", format!("line `{line}`")));
            };
            let (dataset, name) = full
                .split_once('@')
                .ok_or_else(|| ParseError::new("zfs list", format!("snapshot `{full}`")))?;
            Ok(ZfsSnapshot {
                dataset: dataset.to_owned(),
                name: name.to_owned(),
                created_unix: creation
                    .parse()
                    .map_err(|_| ParseError::new("zfs list", format!("time `{creation}`")))?,
                used: used
                    .parse()
                    .map_err(|_| ParseError::new("zfs list", format!("number `{used}`")))?,
                managed: managed == "yes",
            })
        })
        .collect()
}
