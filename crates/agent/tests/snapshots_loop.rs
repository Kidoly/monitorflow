//! The snapshots helper against real LVM and ZFS on loop devices
//! (ADR-0094). Needs root and creates volume groups and pools, so it is
//! ignored by default; CI runs it with sudo on a throwaway runner. Never
//! run it on a machine you care about.
//!
//! The helper looks at the system mount points only, so the test shows it
//! a mountinfo where its loop volumes are mounted at `/` and `/var`; every
//! tool runs for real.
#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::Command;

use monitorflow_agent::snapshots::helper::{self, Host, Kind, Ran, System, Tool};
use monitorflow_common::snapshot::{Refusal, SYSTEM_MOUNTS};
use tempfile::TempDir;

/// Runs a setup command, failing the test when it fails.
fn sh(program: &str, args: &[&str]) -> String {
    let output = Command::new(program).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{program} {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// The real host, but with `mounts` (real mount point, shown as) moved
/// where the helper looks, and the real system mounts moved away.
struct Remapped {
    host: Host,
    mounts: Vec<(PathBuf, &'static str)>,
}

impl System for Remapped {
    fn mountinfo(&self) -> std::io::Result<String> {
        let text = std::fs::read_to_string("/proc/self/mountinfo")?;
        Ok(text
            .lines()
            .map(|line| {
                let mut fields: Vec<String> = line.split(' ').map(str::to_owned).collect();
                if let Some(point) = fields.get_mut(4) {
                    if let Some((_, shown)) = self
                        .mounts
                        .iter()
                        .find(|(real, _)| Path::new(point.as_str()) == real)
                    {
                        *point = (*shown).to_owned();
                    } else if SYSTEM_MOUNTS.contains(&point.as_str()) {
                        *point = format!("/hidden{point}");
                    }
                }
                fields.join(" ")
            })
            .collect::<Vec<_>>()
            .join("\n"))
    }

    fn has(&self, tool: Tool) -> bool {
        self.host.has(tool)
    }

    fn run(&self, tool: Tool, args: &[String]) -> std::io::Result<Ran> {
        self.host.run(tool, args)
    }
}

/// A loop device over a sparse file, detached when dropped.
struct Loop {
    device: String,
    _dir: TempDir,
}

impl Loop {
    fn new(size: &str) -> Self {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("disk");
        sh("truncate", &["-s", size, file.to_str().unwrap()]);
        let device = sh("losetup", &["--find", "--show", file.to_str().unwrap()]);
        Self { device, _dir: dir }
    }
}

impl Drop for Loop {
    fn drop(&mut self) {
        let _ = Command::new("losetup").args(["-d", &self.device]).status();
    }
}

/// A volume group on a loop device, with what is mounted from it; all of
/// it removed when dropped.
struct Group {
    name: String,
    mounts: Vec<TempDir>,
    _disk: Loop,
}

impl Group {
    fn new() -> Self {
        let disk = Loop::new("3G");
        let name = format!("mftest{}", std::process::id());
        sh("pvcreate", &["-q", &disk.device]);
        sh("vgcreate", &["-q", &name, &disk.device]);
        Self {
            name,
            mounts: Vec::new(),
            _disk: disk,
        }
    }

    /// Makes an ext4 filesystem on `lv` and mounts it.
    fn mount(&mut self, lv: &str) -> PathBuf {
        let device = format!("/dev/{}/{lv}", self.name);
        sh("mkfs.ext4", &["-q", &device]);
        let dir = TempDir::new().unwrap();
        sh("mount", &[&device, dir.path().to_str().unwrap()]);
        let path = dir.path().to_owned();
        self.mounts.push(dir);
        path
    }

    fn lvs(&self) -> Vec<String> {
        sh("lvs", &["--noheadings", "-o", "lv_name", &self.name])
            .lines()
            .map(|l| l.trim().to_owned())
            .collect()
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        for dir in &self.mounts {
            let _ = Command::new("umount").arg(dir.path()).status();
        }
        let _ = Command::new("vgremove")
            .args(["-f", "-q", &self.name])
            .status();
        let _ = Command::new("pvremove")
            .args(["-q", &self._disk.device])
            .status();
    }
}

#[test]
#[ignore = "needs root, and creates LVM volumes; CI runs it with sudo"]
fn lvm_on_loop_devices() {
    let mut group = Group::new();
    let vg = group.name.clone();
    sh("lvcreate", &["-q", "-L", "1G", "-T", &format!("{vg}/pool")]);
    sh(
        "lvcreate",
        &[
            "-q",
            "-V",
            "512M",
            "-T",
            &format!("{vg}/pool"),
            "-n",
            "root",
        ],
    );
    sh("lvcreate", &["-q", "-L", "300M", "-n", "var", &vg]);
    let root = group.mount("root");
    let var = group.mount("var");
    let system = Remapped {
        host: Host::default(),
        mounts: vec![(root, "/"), (var, "/var")],
    };

    let inventory = helper::inspect(&system).unwrap();
    let kinds: Vec<(&str, Kind)> = inventory
        .volumes
        .iter()
        .map(|v| (v.mount_point.as_str(), v.kind))
        .collect();
    assert_eq!(kinds, [("/", Kind::LvmThin), ("/var", Kind::Lvm)]);

    // Both, in one take; again is a no-op.
    let outcome = helper::take(&system, "monitorflow-ci-1");
    assert!(outcome.error.is_none(), "{:?}", outcome.error);
    let inventory = helper::inspect(&system).unwrap();
    let taken: Vec<(&str, Kind, u64)> = inventory
        .snapshots
        .iter()
        .map(|s| (s.mount_point.as_str(), s.kind, s.size_bytes))
        .collect();
    // The classic one is the whole 300 MiB origin: 20 % is under 1 GiB.
    assert_eq!(
        taken,
        [("/", Kind::LvmThin, 0), ("/var", Kind::Lvm, 300 << 20)]
    );
    assert!(helper::take(&system, "monitorflow-ci-1").note.is_some());

    // Someone else's snapshot with a MonitorFlow name is never deleted.
    sh(
        "lvcreate",
        &[
            "-q",
            "-s",
            "-n",
            "monitorflow-ci-2_root",
            &format!("{vg}/root"),
        ],
    );
    assert!(helper::delete(&system, "monitorflow-ci-2").note.is_some());
    assert!(group.lvs().contains(&"monitorflow-ci-2_root".to_owned()));

    let outcome = helper::delete(&system, "monitorflow-ci-1");
    assert!(outcome.error.is_none(), "{:?}", outcome.error);
    assert!(helper::inspect(&system).unwrap().snapshots.is_empty());

    // A group without room for the classic snapshot: nothing is taken,
    // not even the thin one.
    sh("lvcreate", &["-q", "-l", "100%FREE", "-n", "filler", &vg]);
    let outcome = helper::take(&system, "monitorflow-ci-3");
    assert!(
        matches!(
            outcome.error,
            Some(helper::HelperError::Refused(Refusal::NoSpace { .. }))
        ),
        "{:?}",
        outcome.error
    );
    assert!(!group
        .lvs()
        .iter()
        .any(|lv| lv.starts_with("monitorflow-ci-3")));
}

/// A ZFS pool on a loop device, destroyed when dropped.
struct Pool {
    name: String,
    _disk: Loop,
    _mount: TempDir,
}

impl Drop for Pool {
    fn drop(&mut self) {
        let _ = Command::new("zpool")
            .args(["destroy", "-f", &self.name])
            .status();
    }
}

#[test]
#[ignore = "needs root and the ZFS module; CI runs it with sudo"]
fn zfs_on_a_loop_device() {
    if !Command::new("modprobe")
        .arg("zfs")
        .status()
        .is_ok_and(|s| s.success())
    {
        eprintln!("no ZFS module on this kernel; skipped");
        return;
    }
    let disk = Loop::new("1G");
    let name = format!("mftest{}", std::process::id());
    let mount = TempDir::new().unwrap();
    let root = mount.path().join("root");
    sh("zpool", &["create", "-m", "none", &name, &disk.device]);
    let pool = Pool {
        name: name.clone(),
        _disk: disk,
        _mount: mount,
    };
    let dataset = format!("{name}/ROOT/debian");
    sh(
        "zfs",
        &[
            "create",
            "-p",
            "-o",
            &format!("mountpoint={}", root.display()),
            &dataset,
        ],
    );
    sh("zfs", &["create", &format!("{dataset}/lib")]);
    let system = Remapped {
        host: Host::default(),
        mounts: vec![(root, "/")],
    };

    let inventory = helper::inspect(&system).unwrap();
    assert_eq!(inventory.volumes[0].kind, Kind::Zfs);
    assert_eq!(inventory.volumes[0].source, dataset);

    let outcome = helper::take(&system, "monitorflow-ci-1");
    assert!(outcome.error.is_none(), "{:?}", outcome.error);
    let origins: Vec<String> = helper::inspect(&system)
        .unwrap()
        .snapshots
        .iter()
        .map(|s| s.origin.clone())
        .collect();
    // Recursive: the child has its snapshot, with the property.
    assert_eq!(origins.len(), 2, "{origins:?}");
    assert!(origins.contains(&format!("{dataset}/lib")));

    sh("zfs", &["snapshot", &format!("{dataset}@monitorflow-ci-2")]);
    assert!(helper::delete(&system, "monitorflow-ci-2").note.is_some());

    let outcome = helper::delete(&system, "monitorflow-ci-1");
    assert!(outcome.error.is_none(), "{:?}", outcome.error);
    assert!(helper::inspect(&system).unwrap().snapshots.is_empty());
    drop(pool);
}
