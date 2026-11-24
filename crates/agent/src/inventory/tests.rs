use std::fs;
use std::path::Path;

use monitorflow_common::inventory::{self, fields};
use monitorflow_common::proto::v1::{InventoryField, InventoryItem};

use super::linux::{self, Outputs};
use super::*;

fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

const CPUINFO: &str =
    "processor\t: 0\nphysical id\t: 0\nmodel name\t: Intel(R) Xeon(R) E-2236\ncpu cores\t: 6\n\n\
processor\t: 1\nphysical id\t: 0\nmodel name\t: Intel(R) Xeon(R) E-2236\ncpu cores\t: 6\n\n";

const LSBLK: &str = r#"{"blockdevices": [
 {"name":"sda","type":"disk","size":480103981056,"model":"SAMSUNG MZ7LH480","serial":"S45NNA0M123456","vendor":"ATA     ","rev":"HXT7404Q","rota":false,"tran":"sata"},
 {"name":"sdb","type":"disk","size":"4000787030016","model":"WDC WD40EFRX","serial":null,"vendor":"ATA","rev":"0A82","rota":"1","tran":"sata"},
 {"name":"loop0","type":"loop","size":1000,"model":null,"serial":null,"vendor":null,"rev":null,"rota":false,"tran":null},
 {"name":"sr0","type":"rom","size":1073741312,"model":"DVD","serial":null,"vendor":null,"rev":null,"rota":true,"tran":"sata"}
]}"#;

const IP: &str = r#"[{"ifname":"lo","addr_info":[{"family":"inet","local":"127.0.0.1","prefixlen":8,"scope":"host"}]},
{"ifname":"eno1","addr_info":[{"family":"inet","local":"192.0.2.10","prefixlen":24,"scope":"global"},{"family":"inet6","local":"fe80::1","prefixlen":64,"scope":"link"}]}]"#;

const UNIT_FILES: &str = "nginx.service enabled enabled\nssh.service enabled enabled\n\
getty@.service enabled enabled\nsystemd-fsck-root.service static -\ntelnet.service masked enabled\n\
cron.service disabled enabled\nfoo.socket enabled enabled\n";

fn host(root: &Path) {
    write(root, "sys/class/dmi/id/sys_vendor", "Dell Inc.\n");
    write(root, "sys/class/dmi/id/product_name", "PowerEdge T140\n");
    write(root, "sys/class/dmi/id/chassis_type", "17\n");
    write(root, "sys/class/dmi/id/bios_version", "2.15.1\n");
    write(root, "sys/class/dmi/id/bios_date", "05/04/2023\n");
    write(
        root,
        "sys/class/dmi/id/board_serial",
        "To Be Filled By O.E.M.\n",
    );
    write(root, "run/monitorflow-agent/product_serial", "7XK2LM3\n");
    write(
        root,
        "etc/os-release",
        "NAME=\"Debian GNU/Linux\"\nVERSION_ID=\"12\"\nPRETTY_NAME=\"Debian GNU/Linux 12 (bookworm)\"\n",
    );
    write(root, "proc/sys/kernel/osrelease", "6.1.0-27-amd64\n");
    write(
        root,
        "proc/meminfo",
        "MemTotal:       16318412 kB\nMemFree: 1 kB\n",
    );
    write(root, "proc/cpuinfo", CPUINFO);
    write(
        root,
        "sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_max_freq",
        "4800000\n",
    );
    write(root, "sys/class/net/eno1/address", "b0:7b:25:00:11:22\n");
    write(root, "sys/class/net/eno1/speed", "1000\n");
    write(root, "sys/class/net/eno1/device/vendor", "0x8086\n");
    write(root, "sys/class/net/eno1/device/device", "0x1533\n");
    write(root, "sys/class/net/br0/address", "b0:7b:25:00:11:22\n");
    write(root, "sys/class/net/lo/address", "00:00:00:00:00:00\n");
}

fn outputs() -> Outputs {
    Outputs {
        lsblk: LSBLK.into(),
        ip_addr: IP.into(),
        unit_files: UNIT_FILES.into(),
    }
}

#[test]
fn a_linux_host_is_read() {
    let dir = tempfile::tempdir().unwrap();
    host(dir.path());
    let inventory = linux::collect_in(dir.path(), &outputs());
    let by = |kind: &str, key: &str| {
        inventory
            .items
            .iter()
            .find(|i| i.kind == kind && i.key == key)
            .unwrap_or_else(|| panic!("{kind} {key}"))
            .clone()
    };
    let system = by("system", "system");
    let system = fields(&system);
    assert_eq!(system["manufacturer"], "Dell Inc.");
    assert_eq!(system["model"], "PowerEdge T140");
    assert_eq!(system["chassis"], "main server chassis");
    assert_eq!(system["bios_date"], "2023-05-04");
    assert_eq!(system["serial"], "7XK2LM3", "the unit's copy is read");
    assert!(!system.contains_key("board_serial"), "placeholder dropped");
    assert_eq!(system["os"], "Debian GNU/Linux 12 (bookworm)");
    assert_eq!(system["kernel"], "6.1.0-27-amd64");
    assert_eq!(system["memory_bytes"], (16318412u64 * 1024).to_string());

    let cpu = by("processor", "0");
    let cpu = fields(&cpu);
    assert_eq!(
        (cpu["model"], cpu["cores"], cpu["threads"], cpu["max_mhz"]),
        ("Intel(R) Xeon(R) E-2236", "6", "2", "4800")
    );

    let ssd = by("disk", "sda");
    let ssd = fields(&ssd);
    assert_eq!(
        (ssd["media"], ssd["vendor"], ssd["size_bytes"]),
        ("ssd", "ATA", "480103981056")
    );
    assert_eq!(fields(&by("disk", "sdb"))["media"], "hdd");
    assert!(!inventory
        .items
        .iter()
        .any(|i| i.key == "loop0" || i.key == "sr0"));

    let nic = by("nic", "eno1");
    let nic = fields(&nic);
    assert_eq!(nic["pci_id"], "8086:1533");
    assert_eq!(nic["addresses"], "192.0.2.10/24");
    assert_eq!(nic["speed_mbps"], "1000");
    assert!(
        !inventory
            .items
            .iter()
            .any(|i| i.key == "br0" || i.key == "lo"),
        "only cards with a device"
    );

    let services: Vec<(&str, &str)> = inventory
        .items
        .iter()
        .filter(|i| i.kind == "service")
        .map(|i| (i.key.as_str(), i.fields[0].value.as_str()))
        .collect();
    assert_eq!(
        services,
        [
            ("cron.service", "disabled"),
            ("nginx.service", "enabled"),
            ("ssh.service", "enabled"),
            ("telnet.service", "masked"),
        ]
    );
}

#[test]
fn a_container_reports_no_firmware() {
    let dir = tempfile::tempdir().unwrap();
    host(dir.path());
    write(dir.path(), "run/systemd/container", "lxc\n");
    let inventory = linux::collect_in(dir.path(), &outputs());
    let system = inventory.items.iter().find(|i| i.kind == "system").unwrap();
    let system = fields(system);
    assert!(!system.contains_key("model"));
    assert!(!system.contains_key("serial"));
    assert_eq!(system["kernel"], "6.1.0-27-amd64");
}

#[test]
fn unreadable_command_output_gives_no_items() {
    assert!(linux::disks("").is_empty());
    assert!(linux::addresses("not json").is_empty());
    assert!(linux::services("").is_empty());
    assert_eq!(linux::os_release("NAME=Arch\n").as_deref(), Some("Arch"));
}

#[test]
fn serial_placeholders_are_dropped() {
    for bad in [
        "",
        " ",
        "To Be Filled By O.E.M.",
        "Default string",
        "0000000",
        "...",
    ] {
        assert_eq!(clean_serial(bad), None, "{bad:?}");
    }
    assert_eq!(clean_serial(" CZ1234ABC ").as_deref(), Some("CZ1234ABC"));
}

fn snapshot(versions: &[(&str, &str)]) -> Arc<Snapshot> {
    let inventory = Inventory {
        items: versions
            .iter()
            .map(|(name, version)| InventoryItem {
                kind: "software".into(),
                key: (*name).into(),
                fields: vec![InventoryField {
                    name: "version".into(),
                    value: (*version).into(),
                }],
            })
            .collect(),
    };
    Arc::new(Snapshot::new(inventory, 1))
}

#[test]
fn the_link_sends_hashes_changes_and_whole_inventories() {
    let (collector, rx) = Collector::new();
    let mut link = Link::new(rx);
    assert_eq!(link.opened(), None, "nothing collected yet");
    assert_eq!(link.requested(b""), None);

    let first = snapshot(&[("a", "1"), ("b", "1"), ("c", "1")]);
    let mut last = None;
    assert!(collector.offer((*first).clone(), Instant::now(), &mut last));
    // A stream opens: the hash only.
    let opened = link.opened().unwrap();
    assert_eq!(
        (opened.hash.clone(), opened.body),
        (first.hash.clone(), None)
    );
    // The server holds nothing: the whole inventory.
    let whole = link.requested(b"").unwrap();
    assert!(matches!(whole.body, Some(inventory_report::Body::Full(ref i)) if i.items.len() == 3));

    // A change: the changes since what was sent.
    let second = snapshot(&[("a", "2"), ("b", "1"), ("c", "1")]);
    let report = link.report(second.clone());
    let Some(inventory_report::Body::Delta(delta)) = report.body else {
        panic!("expected changes");
    };
    assert_eq!(delta.base_hash, first.hash);
    assert_eq!(delta.upserted.len(), 1);
    assert_eq!(inventory::apply(&first.inventory, &delta), second.inventory);

    // The same again, as the daily refresh: the hash only.
    assert_eq!(link.report(second.clone()).body, None);

    // Once published, the collector's latest is the second one.
    assert!(collector.offer((*second).clone(), Instant::now(), &mut last));
    // The server lost the changes and still holds the first: it asks.
    // The agent no longer knows the first, so it sends the whole.
    let again = link.requested(&first.hash).unwrap();
    assert!(matches!(again.body, Some(inventory_report::Body::Full(_))));
    // The server holds the latest: nothing to send.
    assert_eq!(link.requested(&second.hash), None);
}

#[test]
fn the_collector_publishes_changes_and_a_daily_refresh() {
    let (collector, mut rx) = Collector::new();
    let mut last = None;
    let now = Instant::now();
    let one = snapshot(&[("a", "1")]);
    assert!(collector.offer((*one).clone(), now, &mut last));
    rx.mark_unchanged();
    assert!(!collector.offer((*one).clone(), now + Duration::from_secs(60), &mut last));
    assert!(!rx.has_changed().unwrap());
    assert!(collector.offer((*one).clone(), now + REFRESH_EVERY, &mut last));
    let two = snapshot(&[("a", "2")]);
    assert!(collector.offer((*two).clone(), now + REFRESH_EVERY, &mut last));
}
