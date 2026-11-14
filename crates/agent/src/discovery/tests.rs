use super::*;

#[test]
fn running_units_are_the_first_word_of_valid_lines() {
    let output = "\
ssh.service                 loaded active running OpenBSD Secure Shell server
postgresql@16-main.service  loaded active running PostgreSQL Cluster 16-main
-bad.service loaded active running nope
weird$name.service loaded active running nope
cron.timer loaded active running not a service
ssh.service                 loaded active running duplicate
";
    assert_eq!(
        parse_units(output),
        ["postgresql@16-main.service", "ssh.service"]
    );
    let many: String = (0..300)
        .map(|i| format!("u{i:03}.service loaded active running x\n"))
        .collect();
    assert_eq!(parse_units(&many).len(), MAX_SERVICES);
}

#[test]
fn listening_ports_say_whether_they_are_local_or_loopback_only() {
    let tcp = "\
  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 00000000:0016 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 1 1
   1: 0100007F:1538 00000000:0000 0A 00000000:00000000 00:00000000 00000000   105        0 2 1
   2: 0A01010A:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 3 1
   3: 0A01010A:0016 0B01010A:C350 01 00000000:00000000 00:00000000 00000000     0        0 4 1
   4: 0100007F:0000 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 5 1
";
    // ::1 on 5432 and :: on 8080.
    let tcp6 = "\
  sl  local_address                         remote_address                        st
   0: 00000000000000000000000001000000:1538 00000000000000000000000000000000:0000 0A
   1: 00000000000000000000000000000000:1F90 00000000000000000000000000000000:0000 0A
   2: 0000000000000000FFFF00000100007F:18EB 00000000000000000000000000000000:0000 0A
";
    let port = |port, loopback_only, local| DiscoveredPort {
        port,
        loopback_only,
        local,
    };
    assert_eq!(
        parse_listening(&[tcp, tcp6]),
        [
            port(22, false, true),
            port(5432, true, true),
            // ::ffff:127.0.0.1.
            port(6379, true, true),
            // On 10.1.1.10, and on :: too.
            port(8080, false, true),
        ]
    );
    // A port bound to one outside address only is not reachable on 127.0.0.1.
    assert_eq!(
        parse_listening(&[tcp]).iter().find(|p| p.port == 8080),
        Some(&port(8080, false, false))
    );
    assert_eq!(
        parse_listening(&["header", "garbage line", "0: zz:zz 0 0A"]),
        []
    );
}

#[test]
fn mounts_keep_data_file_systems_and_the_host_of_shares() {
    let mounts = "\
/dev/mapper/vg0-root / ext4 rw,relatime 0 0
proc /proc proc rw 0 0
tmpfs /run tmpfs rw 0 0
nas.lan:/export/backup /mnt/backup nfs4 rw 0 0
//files.lan/share /mnt/my\\040share cifs rw 0 0
//user@files.lan/other /mnt/other cifs rw 0 0
[fd00::5]:/data /mnt/v6 nfs rw 0 0
/dev/sda1 / ext4 rw 0 0
";
    let mount = |path: &str, fs_type: &str, host: &str| DiscoveredMount {
        path: path.into(),
        fs_type: fs_type.into(),
        source_host: host.into(),
    };
    assert_eq!(
        parse_mounts(mounts),
        [
            mount("/", "ext4", ""),
            mount("/mnt/backup", "nfs4", "nas.lan"),
            mount("/mnt/my share", "cifs", "files.lan"),
            mount("/mnt/other", "cifs", "files.lan"),
            mount("/mnt/v6", "nfs", "fd00::5"),
        ]
    );
}

#[test]
fn a_report_goes_out_when_it_changes_or_has_not_for_an_hour() {
    let (discovery, rx) = Discovery::new();
    let report = |service: &str| DiscoveryReport {
        services: vec![service.into()],
        ..DiscoveryReport::default()
    };
    let start = Instant::now();
    let mut last = None;
    assert!(discovery.offer(report("a.service"), start, &mut last));
    assert!(rx.borrow().as_ref().unwrap().collected_at_unix_ms > 0);
    assert!(!discovery.offer(report("a.service"), start + SCAN_EVERY, &mut last));
    assert!(discovery.offer(report("b.service"), start + SCAN_EVERY * 2, &mut last));
    assert!(!discovery.offer(report("b.service"), start + SCAN_EVERY * 3, &mut last));
    assert!(discovery.offer(
        report("b.service"),
        start + SCAN_EVERY * 2 + REFRESH_EVERY,
        &mut last
    ));
    assert_eq!(rx.borrow().as_ref().unwrap().services, ["b.service"]);
}
