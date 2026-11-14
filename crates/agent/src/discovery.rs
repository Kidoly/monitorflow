//! What runs on this machine, reported to the server so it can recommend
//! checks (ADR-0134): running systemd services, listening TCP ports and
//! mounted file systems. A look every [`SCAN_EVERY`] that is cheap (one
//! `systemctl` and three files of `/proc`), sent only when it changed, and
//! at least every [`REFRESH_EVERY`] so the server can say how fresh it is.
//! The server treats all of it as untrusted and bounds it again.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use crate::shutdown::Shutdown;
use monitorflow_common::proto::v1::{DiscoveredMount, DiscoveredPort, DiscoveryReport};
use tokio::process::Command;
use tokio::sync::watch;
use tokio::time::Instant;

/// How often the agent looks.
pub const SCAN_EVERY: Duration = Duration::from_secs(300);
/// Longest time without a report, so "last checked" stays meaningful.
pub const REFRESH_EVERY: Duration = Duration::from_secs(3600);
/// Longest `systemctl list-units` may take.
const SYSTEMCTL_TIMEOUT: Duration = Duration::from_secs(10);

/// Caps, the same the server enforces (ADR-0134).
pub const MAX_SERVICES: usize = 256;
pub const MAX_PORTS: usize = 128;
pub const MAX_MOUNTS: usize = 64;
const MAX_UNIT_LEN: usize = 255;
const MAX_PATH_LEN: usize = 256;

/// File systems that hold data, as opposed to `proc`, `tmpfs` and the like.
const DATA_FILE_SYSTEMS: &[&str] = &[
    "ext2",
    "ext3",
    "ext4",
    "xfs",
    "btrfs",
    "zfs",
    "f2fs",
    "jfs",
    "reiserfs",
    "vfat",
    "exfat",
    "ntfs",
    "ntfs3",
    "nfs",
    "nfs4",
    "cifs",
    "smb3",
    "glusterfs",
    "ceph",
    "fuse.sshfs",
];

/// Running services from `systemctl list-units --type=service
/// --state=running --no-legend --plain`: the first word of each line,
/// when it is a unit name systemd allows.
pub fn parse_units(output: &str) -> Vec<String> {
    let mut units: Vec<String> = output
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|unit| valid_unit(unit))
        .map(str::to_owned)
        .collect();
    units.sort();
    units.dedup();
    units.truncate(MAX_SERVICES);
    units
}

/// systemd's unit name charset (ADR-0021), ending in `.service`.
pub fn valid_unit(unit: &str) -> bool {
    unit.len() <= MAX_UNIT_LEN
        && unit.ends_with(".service")
        && !unit.starts_with(['-', '.', '@'])
        && unit
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b":_.\\@-".contains(&b))
}

/// Listening TCP ports from `/proc/net/tcp` and `/proc/net/tcp6`, merged:
/// a port is loopback only when every socket on it listens on loopback.
pub fn parse_listening(tables: &[&str]) -> Vec<DiscoveredPort> {
    // Per port: every socket on loopback, some socket reachable on 127.0.0.1.
    let mut ports: BTreeMap<u32, (bool, bool)> = BTreeMap::new();
    for table in tables {
        for line in table.lines().skip(1) {
            let mut fields = line.split_whitespace();
            let (Some(_), Some(local), Some(_), Some(state)) =
                (fields.next(), fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            // 0A is TCP_LISTEN.
            if state != "0A" {
                continue;
            }
            let Some((address, port)) = local.split_once(':') else {
                continue;
            };
            let Ok(port) = u32::from_str_radix(port, 16) else {
                continue;
            };
            let Some(address) = address_of(address) else {
                continue;
            };
            if port == 0 || port > 65_535 {
                continue;
            }
            let loopback = is_loopback(address);
            let reachable = loopback || address.is_unspecified();
            ports
                .entry(port)
                .and_modify(|(all, any)| {
                    *all &= loopback;
                    *any |= reachable;
                })
                .or_insert((loopback, reachable));
        }
    }
    ports
        .into_iter()
        .take(MAX_PORTS)
        .map(|(port, (loopback_only, local))| DiscoveredPort {
            port,
            loopback_only,
            local,
        })
        .collect()
}

fn is_loopback(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => {
            v6.is_loopback() || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        }
    }
}

/// A `/proc/net/tcp*` address in hex, in 32-bit words of host order
/// (little endian on every platform the agent ships for): `0100007F` is
/// 127.0.0.1.
fn address_of(hex: &str) -> Option<IpAddr> {
    let word = |i: usize| -> Option<[u8; 4]> {
        let w = u32::from_str_radix(hex.get(i * 8..i * 8 + 8)?, 16).ok()?;
        Some(w.to_le_bytes())
    };
    match hex.len() {
        8 => Some(IpAddr::V4(Ipv4Addr::from(word(0)?))),
        32 => {
            let mut bytes = [0u8; 16];
            for i in 0..4 {
                bytes[i * 4..i * 4 + 4].copy_from_slice(&word(i)?);
            }
            Some(IpAddr::V6(Ipv6Addr::from(bytes)))
        }
        _ => None,
    }
}

/// Mounted data file systems from `/proc/self/mounts`, with the host of
/// a network share (`nas:/export`, `//nas/share`).
pub fn parse_mounts(mounts: &str) -> Vec<DiscoveredMount> {
    let mut found: Vec<DiscoveredMount> = Vec::new();
    for line in mounts.lines() {
        let mut fields = line.split(' ');
        let (Some(source), Some(path), Some(fs_type)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if !DATA_FILE_SYSTEMS.contains(&fs_type) {
            continue;
        }
        let path = unescape(path);
        if path.len() > MAX_PATH_LEN || found.iter().any(|m| m.path == path) {
            continue;
        }
        found.push(DiscoveredMount {
            path,
            fs_type: fs_type.to_owned(),
            source_host: share_host(fs_type, &unescape(source)).unwrap_or_default(),
        });
        if found.len() == MAX_MOUNTS {
            break;
        }
    }
    found
}

/// `\040` and the other octal escapes of `/proc/self/mounts`.
fn unescape(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && i + 3 < bytes.len()
            && bytes[i + 1..i + 4]
                .iter()
                .all(|b| (b'0'..=b'7').contains(b))
        {
            let n = (bytes[i + 1] - b'0') * 64 + (bytes[i + 2] - b'0') * 8 + (bytes[i + 3] - b'0');
            out.push(n);
            i += 4;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The file server of a network share, without a user name:
/// `nas:/export`, `[fd00::5]:/export`, `//user@nas/share`.
fn share_host(fs_type: &str, source: &str) -> Option<String> {
    let host = match fs_type {
        "nfs" | "nfs4" | "glusterfs" => match source.strip_prefix('[') {
            Some(v6) => v6.split_once("]:")?.0,
            None => source.split_once(':')?.0,
        },
        "cifs" | "smb3" => source.strip_prefix("//")?.split('/').next()?,
        _ => return None,
    };
    let host = host.rsplit('@').next()?;
    (!host.is_empty() && host.len() <= 253).then(|| host.to_owned())
}

/// Running services, or none on a machine without systemd.
async fn running_services() -> Vec<String> {
    if !Path::new("/run/systemd/system").exists() {
        return Vec::new();
    }
    let mut command = Command::new("systemctl");
    command
        .args([
            "list-units",
            "--type=service",
            "--state=running",
            "--no-legend",
            "--plain",
            "--no-pager",
        ])
        .env_clear()
        .env(
            "PATH",
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        )
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    match tokio::time::timeout(SYSTEMCTL_TIMEOUT, command.output()).await {
        Ok(Ok(output)) if output.status.success() => {
            parse_units(&String::from_utf8_lossy(&output.stdout))
        }
        Ok(Ok(output)) => {
            tracing::debug!(status = %output.status, "systemctl list-units failed; no services reported");
            Vec::new()
        }
        Ok(Err(error)) => {
            tracing::debug!(%error, "cannot run systemctl; no services reported");
            Vec::new()
        }
        Err(_) => {
            tracing::debug!("systemctl list-units timed out; no services reported");
            Vec::new()
        }
    }
}

/// One look at the machine.
pub async fn scan() -> DiscoveryReport {
    let read = |path: &'static str| async move {
        tokio::fs::read_to_string(path).await.unwrap_or_default()
    };
    let (services, tcp, tcp6, mounts) = tokio::join!(
        running_services(),
        read("/proc/net/tcp"),
        read("/proc/net/tcp6"),
        read("/proc/self/mounts"),
    );
    DiscoveryReport {
        collected_at_unix_ms: 0,
        services,
        ports: parse_listening(&[&tcp, &tcp6]),
        mounts: parse_mounts(&mounts),
    }
}

/// Publishes what [`scan`] finds: when it changed, and at least every
/// [`REFRESH_EVERY`]. The stream sends what is published.
pub struct Discovery {
    latest: watch::Sender<Option<DiscoveryReport>>,
}

impl Discovery {
    pub fn new() -> (Self, watch::Receiver<Option<DiscoveryReport>>) {
        let (latest, rx) = watch::channel(None);
        (Self { latest }, rx)
    }

    /// Publishes `found` unless it is what was published less than
    /// [`REFRESH_EVERY`] before `now`; true when it did.
    pub fn offer(
        &self,
        found: DiscoveryReport,
        now: Instant,
        last: &mut Option<(DiscoveryReport, Instant)>,
    ) -> bool {
        if let Some((previous, at)) = last {
            if *previous == found && now.duration_since(*at) < REFRESH_EVERY {
                return false;
            }
        }
        *last = Some((found.clone(), now));
        self.latest.send_replace(Some(DiscoveryReport {
            collected_at_unix_ms: unix_ms(),
            ..found
        }));
        true
    }

    /// Looks at once, then every [`SCAN_EVERY`], until `shutdown`.
    pub async fn run(self, mut shutdown: Shutdown) {
        let mut last = None;
        loop {
            let found = scan().await;
            if self.offer(found, Instant::now(), &mut last) {
                tracing::debug!("discovery report published");
            }
            tokio::select! {
                () = tokio::time::sleep(SCAN_EVERY) => {}
                () = shutdown.wait() => return,
            }
        }
    }
}

fn unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests;
