use std::cell::RefCell;
use std::io;

use monitorflow_common::snapshot::Refusal;

use super::helper::{self, HelperError, Kind, Ran, System, Tool};
use super::parse;

const MOUNTINFO_THIN: &str = include_str!("fixtures/mountinfo-thin.txt");
const LVS_THIN: &str = include_str!("fixtures/lvs-thin.json");
const VGS: &str = include_str!("fixtures/vgs.json");
const MOUNTINFO_CLASSIC: &str = include_str!("fixtures/mountinfo-classic.txt");
const LVS_CLASSIC: &str = include_str!("fixtures/lvs-classic.json");
const VGS_CLASSIC: &str = include_str!("fixtures/vgs-classic.json");
const MOUNTINFO_ZFS: &str = include_str!("fixtures/mountinfo-zfs.txt");
const ZFS_FILESYSTEMS: &str = include_str!("fixtures/zfs-filesystems.txt");
const ZFS_SNAPSHOTS: &str = include_str!("fixtures/zfs-snapshots.txt");
const GIB: u64 = 1 << 30;

/// A host from fixtures that records what the helper runs. Commands that
/// change something answer `fail_on` with exit code 5.
#[derive(Default)]
struct Fake {
    mountinfo: &'static str,
    lvs: Option<String>,
    vgs: &'static str,
    zfs_filesystems: Option<&'static str>,
    zfs_snapshots: &'static str,
    fail_on: Option<&'static str>,
    ran: RefCell<Vec<String>>,
}

impl Fake {
    fn thin() -> Self {
        Self {
            mountinfo: MOUNTINFO_THIN,
            lvs: Some(LVS_THIN.into()),
            vgs: VGS,
            ..Self::default()
        }
    }

    fn classic() -> Self {
        Self {
            mountinfo: MOUNTINFO_CLASSIC,
            lvs: Some(LVS_CLASSIC.into()),
            vgs: VGS_CLASSIC,
            ..Self::default()
        }
    }

    fn zfs() -> Self {
        Self {
            mountinfo: MOUNTINFO_ZFS,
            zfs_filesystems: Some(ZFS_FILESYSTEMS),
            zfs_snapshots: ZFS_SNAPSHOTS,
            ..Self::default()
        }
    }

    /// What changed the host: every run but the listings.
    fn changes(&self) -> Vec<String> {
        self.ran
            .borrow()
            .iter()
            .filter(|l| !l.starts_with("lvm lvs") && !l.starts_with("lvm vgs"))
            .filter(|l| !l.starts_with("zfs list"))
            .cloned()
            .collect()
    }
}

impl System for Fake {
    fn mountinfo(&self) -> io::Result<String> {
        Ok(self.mountinfo.to_owned())
    }

    fn has(&self, tool: Tool) -> bool {
        match tool {
            Tool::Lvm => self.lvs.is_some(),
            Tool::Zfs => self.zfs_filesystems.is_some(),
        }
    }

    fn run(&self, tool: Tool, args: &[String]) -> io::Result<Ran> {
        let line = format!(
            "{} {}",
            if tool == Tool::Lvm { "lvm" } else { "zfs" },
            args.join(" ")
        );
        self.ran.borrow_mut().push(line.clone());
        let stdout = match (tool, args[0].as_str(), args.get(4).map(String::as_str)) {
            (Tool::Lvm, "lvs", _) => self.lvs.clone().unwrap_or_default(),
            (Tool::Lvm, "vgs", _) => self.vgs.to_owned(),
            (Tool::Zfs, "list", Some("filesystem")) => {
                self.zfs_filesystems.unwrap_or_default().to_owned()
            }
            (Tool::Zfs, "list", _) => self.zfs_snapshots.to_owned(),
            _ => String::new(),
        };
        let fails = self.fail_on.is_some_and(|f| line.contains(f));
        Ok(Ran {
            line,
            code: if fails { 5 } else { 0 },
            stdout,
            stderr: if fails {
                "  Insufficient free space\n".into()
            } else {
                String::new()
            },
        })
    }
}

#[test]
fn mountinfo_keeps_system_mounts() {
    let mounts = parse::mountinfo(MOUNTINFO_THIN, &["/", "/var", "/boot", "/srv/my data"]);
    let points: Vec<&str> = mounts.iter().map(|m| m.mount_point.as_str()).collect();
    assert_eq!(points, ["/", "/var", "/boot", "/srv/my data"]);
    assert_eq!((mounts[0].major, mounts[0].minor), (253, 1));
    assert_eq!(mounts[1].fs_type, "xfs");
    assert_eq!(mounts[1].source, "/dev/mapper/vg0-var");
    // A later mount on the same point hides the first.
    let over = "1 0 8:1 / / rw - ext4 /dev/sda1 rw\n2 1 0:30 / / rw - overlay overlay rw\n";
    assert_eq!(parse::mountinfo(over, &["/"])[0].fs_type, "overlay");
    assert!(parse::mountinfo("garbage\n", &["/"]).is_empty());
}

#[test]
fn lvs_and_vgs_parse() {
    let lvs = parse::lvs(LVS_THIN).unwrap();
    assert_eq!(lvs.len(), 9);
    let pool = lvs.iter().find(|l| l.name == "pool").unwrap();
    assert_eq!(pool.data_percent, Some(60.0));
    assert_eq!(pool.metadata_percent, Some(12.5));
    let snapshot = &lvs[2];
    assert_eq!(snapshot.tags, ["monitorflow"]);
    assert_eq!(snapshot.origin, "root");
    assert_eq!(snapshot.created_unix, Some(1_793_697_300));
    assert_eq!(snapshot.major, -1);
    let vgs = parse::vgs(VGS).unwrap();
    assert_eq!(vgs[0].free, 20 * GIB);
    assert!(parse::lvs("{\"report\": [{\"lv\": [{\"vg_name\": 1}]}]}").is_err());
    assert!(parse::lvs("not json").is_err());
}

#[test]
fn zfs_lists_parse() {
    let datasets = parse::zfs_filesystems(ZFS_FILESYSTEMS).unwrap();
    assert_eq!(datasets[5].name, "rpool/ROOT/debian");
    let snapshots = parse::zfs_snapshots(ZFS_SNAPSHOTS).unwrap();
    assert!(snapshots[0].managed);
    assert!(!snapshots[3].managed);
    assert_eq!(snapshots[0].dataset, "rpool/ROOT/debian");
    assert!(parse::zfs_snapshots("rpool@a\t1\n").is_err());
    assert!(parse::zfs_filesystems("rpool\tx\t1\n").is_err());
}

#[test]
fn inspects_a_thin_host() {
    let inventory = helper::inspect(&Fake::thin()).unwrap();
    let root = &inventory.volumes[0];
    assert_eq!(
        (root.mount_point.as_str(), root.kind, root.source.as_str()),
        ("/", Kind::LvmThin, "vg0/root")
    );
    // 40 % of a 100 GiB pool.
    assert_eq!(root.free_bytes, 40 * GIB);
    assert_eq!(root.pool_bytes, 100 * GIB);
    assert_eq!(root.metadata_percent, 12.5);
    let kinds: Vec<(&str, Kind)> = inventory
        .volumes
        .iter()
        .map(|v| (v.mount_point.as_str(), v.kind))
        .collect();
    assert_eq!(
        kinds,
        [
            ("/", Kind::LvmThin),
            ("/var", Kind::LvmThin),
            ("/boot", Kind::Plain)
        ]
    );
    // Only tagged snapshots with a MonitorFlow name: not the untagged
    // `monitorflow-…` one, nor the tagged `before-upgrade`.
    let snapshots: Vec<(&str, &str, &str)> = inventory
        .snapshots
        .iter()
        .map(|s| (s.name.as_str(), s.source.as_str(), s.mount_point.as_str()))
        .collect();
    assert_eq!(
        snapshots,
        [
            (
                "monitorflow-incident-1a2b3c4d5e6f-2",
                "vg0/monitorflow-incident-1a2b3c4d5e6f-2_root",
                "/"
            ),
            (
                "monitorflow-incident-1a2b3c4d5e6f-2",
                "vg0/monitorflow-incident-1a2b3c4d5e6f-2_var",
                "/var"
            ),
        ]
    );
    assert_eq!(inventory.snapshots[0].size_bytes, 0);
}

#[test]
fn inspects_a_classic_host() {
    let inventory = helper::inspect(&Fake::classic()).unwrap();
    let root = &inventory.volumes[0];
    assert_eq!((root.kind, root.free_bytes), (Kind::Lvm, 0));
    assert_eq!(root.source, "rl/root");
    let invalid: Vec<bool> = inventory.snapshots.iter().map(|s| s.invalid).collect();
    // Oldest first: the invalid one, full at 100 %, then the good one.
    assert_eq!(invalid, [true, false]);
    assert_eq!(inventory.snapshots[1].used_percent, 12.34);
    assert_eq!(inventory.snapshots[1].size_bytes, 8 * GIB);
}

#[test]
fn inspects_a_zfs_host() {
    let inventory = helper::inspect(&Fake::zfs()).unwrap();
    let kinds: Vec<(&str, Kind, &str)> = inventory
        .volumes
        .iter()
        .map(|v| (v.mount_point.as_str(), v.kind, v.source.as_str()))
        .collect();
    assert_eq!(
        kinds,
        [
            ("/", Kind::Zfs, "rpool/ROOT/debian"),
            ("/var", Kind::Zfs, "rpool/var"),
            ("/boot", Kind::Zfs, "bpool/BOOT/debian"),
        ]
    );
    let mounts: Vec<(&str, &str)> = inventory
        .snapshots
        .iter()
        .map(|s| (s.origin.as_str(), s.mount_point.as_str()))
        .collect();
    // /var/lib is not a system mount point of its own.
    assert_eq!(
        mounts,
        [
            ("rpool/ROOT/debian/var/lib", ""),
            ("rpool/ROOT/debian", "/"),
            ("rpool/var", "/var"),
        ]
    );
}

#[test]
fn without_tools_everything_is_plain() {
    let mut fake = Fake::thin();
    fake.lvs = None;
    let inventory = helper::inspect(&fake).unwrap();
    assert!(inventory.volumes.iter().all(|v| v.kind == Kind::Plain));
    let outcome = helper::take(&fake, "monitorflow-x");
    assert!(matches!(
        outcome.error,
        Some(HelperError::Refused(Refusal::RootNotCovered { .. }))
    ));
    assert!(fake.changes().is_empty());
}

#[test]
fn takes_thin_snapshots_without_a_size() {
    let fake = Fake::thin();
    let outcome = helper::take(&fake, "monitorflow-incident-0a0b0c0d0e0f-1");
    assert!(outcome.error.is_none(), "{:?}", outcome.error);
    assert_eq!(
        fake.changes(),
        [
            "lvm lvcreate --snapshot --name monitorflow-incident-0a0b0c0d0e0f-1_root --addtag monitorflow vg0/root",
            "lvm lvcreate --snapshot --name monitorflow-incident-0a0b0c0d0e0f-1_var --addtag monitorflow vg0/var",
        ]
    );
}

#[test]
fn a_name_already_taken_is_not_taken_twice() {
    let fake = Fake::thin();
    let outcome = helper::take(&fake, "monitorflow-incident-1a2b3c4d5e6f-2");
    assert!(outcome.error.is_none());
    assert_eq!(
        outcome.note.as_deref(),
        Some("monitorflow-incident-1a2b3c4d5e6f-2 already exists")
    );
    assert!(fake.changes().is_empty());
}

#[test]
fn classic_snapshots_need_free_space() {
    let fake = Fake::classic();
    let outcome = helper::take(&fake, "monitorflow-x");
    let Some(HelperError::Refused(refusal)) = outcome.error else {
        panic!("{:?}", outcome.error);
    };
    assert_eq!(
        refusal.to_string(),
        "volume group rl has 0 B free, and a snapshot of / needs 8.0 GiB"
    );
    assert!(fake.changes().is_empty());

    let mut fake = Fake::classic();
    fake.vgs = r#"{"report": [{"vg": [{"vg_name": "rl", "vg_size": "106296246272", "vg_free": "10737418240"}]}]}"#;
    let outcome = helper::take(&fake, "monitorflow-x");
    assert!(outcome.error.is_none(), "{:?}", outcome.error);
    // 20 % of 40 GiB.
    assert_eq!(
        fake.changes(),
        ["lvm lvcreate --snapshot --name monitorflow-x_root --addtag monitorflow --size 8192m rl/root"]
    );
}

#[test]
fn a_full_thin_pool_is_refused() {
    let mut fake = Fake::thin();
    fake.lvs = Some(LVS_THIN.replace("\"60.00\"", "\"95.00\""));
    let outcome = helper::take(&fake, "monitorflow-x");
    assert!(matches!(
        outcome.error,
        Some(HelperError::Refused(Refusal::PoolFull { .. }))
    ));
    assert!(fake.changes().is_empty());
}

#[test]
fn a_failed_step_removes_what_was_taken() {
    let mut fake = Fake::thin();
    fake.fail_on = Some("_var");
    let outcome = helper::take(&fake, "monitorflow-x");
    let error = outcome.error.unwrap().to_string();
    assert!(
        error.contains("exited with 5: Insufficient free space"),
        "{error}"
    );
    assert_eq!(
        fake.changes(),
        [
            "lvm lvcreate --snapshot --name monitorflow-x_root --addtag monitorflow vg0/root",
            "lvm lvcreate --snapshot --name monitorflow-x_var --addtag monitorflow vg0/var",
            "lvm lvremove --yes vg0/monitorflow-x_root",
        ]
    );
}

#[test]
fn zfs_snapshots_are_recursive_and_atomic() {
    let fake = Fake::zfs();
    let outcome = helper::take(&fake, "monitorflow-campaign-0123456789ab");
    assert!(outcome.error.is_none(), "{:?}", outcome.error);
    // /var/lib is under / in the tree, so the recursive snapshot of /
    // covers it; /var and /boot are datasets of their own.
    assert_eq!(
        fake.changes(),
        ["zfs snapshot -r -o monitorflow:managed=yes rpool/ROOT/debian@monitorflow-campaign-0123456789ab rpool/var@monitorflow-campaign-0123456789ab bpool/BOOT/debian@monitorflow-campaign-0123456789ab"]
    );
}

#[test]
fn deletes_only_its_own() {
    let fake = Fake::thin();
    // The untagged snapshot of that name stays.
    let outcome = helper::delete(&fake, "monitorflow-incident-ffffffffffff-1");
    assert_eq!(
        outcome.note.as_deref(),
        Some("no snapshot monitorflow-incident-ffffffffffff-1")
    );
    assert!(fake.changes().is_empty());

    let outcome = helper::delete(&fake, "monitorflow-incident-1a2b3c4d5e6f-2");
    assert!(outcome.error.is_none());
    assert_eq!(
        fake.changes(),
        [
            "lvm lvremove --yes vg0/monitorflow-incident-1a2b3c4d5e6f-2_root",
            "lvm lvremove --yes vg0/monitorflow-incident-1a2b3c4d5e6f-2_var",
        ]
    );

    let fake = Fake::zfs();
    assert!(helper::delete(&fake, "monitorflow-incident-ffffffffffff-1")
        .note
        .is_some());
    let outcome = helper::delete(&fake, "monitorflow-incident-1a2b3c4d5e6f-2");
    assert!(outcome.error.is_none());
    assert_eq!(fake.changes().len(), 3);
    assert!(fake.changes()[0].starts_with("zfs destroy rpool/"));
}

#[test]
fn refuses_anything_not_typed() {
    use monitorflow_common::proto::v1::{DeleteSnapshot, TakeSnapshot};
    use prost::Message;

    use super::handler::SnapshotAction;

    // The helper takes a name from the signed payload, and nothing else.
    let take =
        |name: &str| SnapshotAction::Take.name(&TakeSnapshot { name: name.into() }.encode_to_vec());
    for bad in [
        "vg0/home",
        "monitorflow-x; rm -rf /",
        "monitorflow--force",
        "-x",
        "",
    ] {
        assert!(take(bad).is_err(), "{bad}");
    }
    assert!(SnapshotAction::Take.name(b"\xff\xff").is_err());
    assert_eq!(take("monitorflow-x").unwrap(), "monitorflow-x");
    let delete = DeleteSnapshot {
        name: "monitorflow-y".into(),
    };
    assert_eq!(
        SnapshotAction::Delete
            .name(&delete.encode_to_vec())
            .unwrap(),
        "monitorflow-y"
    );
    // The functions refuse too, whoever calls them.
    let fake = Fake::thin();
    assert!(helper::take(&fake, "-x").error.is_some());
    assert!(helper::delete(&fake, "vg0/root").error.is_some());
    assert!(fake.ran.borrow().is_empty());
}
