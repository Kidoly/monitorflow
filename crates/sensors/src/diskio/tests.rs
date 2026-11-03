use super::*;

const DEBIAN: &str = include_str!("../../tests/fixtures/debian12/diskstats");
const RHEL: &str = include_str!("../../tests/fixtures/rhel9/diskstats");

fn names(devices: &[Device]) -> Vec<&str> {
    devices.iter().map(|d| d.name.as_str()).collect()
}

fn device(name: &str, id: &str, counters: Counters) -> Device {
    Device {
        name: name.into(),
        id: id.into(),
        counters,
    }
}

fn value(samples: &[Sample], instance: &str, metric: &str) -> f64 {
    samples
        .iter()
        .find(|s| s.instance == instance && s.sensor == metric)
        .unwrap_or_else(|| panic!("no {metric} for {instance}"))
        .value
}

fn instances(samples: &[Sample]) -> Vec<&str> {
    let mut seen: Vec<&str> = samples.iter().map(|s| s.instance.as_str()).collect();
    seen.dedup();
    seen
}

#[test]
fn parses_debian_and_rhel_diskstats() {
    let debian = parse_diskstats(DEBIAN).unwrap();
    assert_eq!(debian.len(), 12);
    let sda = debian.iter().find(|s| s.name == "sda").unwrap();
    assert_eq!((sda.major, sda.minor), (8, 0));
    assert_eq!(
        sda.counters,
        Counters {
            reads: 184310,
            writes: 532114,
            read_sectors: 12750234,
            written_sectors: 28411934,
            busy_ms: 612048,
            io_ms: 98321 + 1520034,
            queue_ms: 1702115,
            in_flight: 0,
        }
    );
    let rhel = parse_diskstats(RHEL).unwrap();
    let nvme = rhel.iter().find(|s| s.name == "nvme0n1").unwrap();
    assert_eq!(nvme.counters.in_flight, 3);
    assert_eq!(nvme.counters.queue_ms, 9562049);
}

#[test]
fn parses_old_kernels_and_refuses_garbage() {
    // 2.6 kernels gave partitions 4 fields and no flush or discard.
    let old = "   8    0 sda 1 2 3 4 5 6 7 8 0 10 11\n   8    1 sda1 1 2 3 4\n";
    let stats = parse_diskstats(old).unwrap();
    assert_eq!(stats.len(), 1);
    assert_eq!(stats[0].counters.busy_ms, 10);
    assert!(parse_diskstats("").unwrap().is_empty());
    for bad in ["x y sda 1 2 3 4 5 6 7 8 9 10 11\n", "8 0\n"] {
        assert!(parse_diskstats(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn whole_devices_are_selected_and_named() {
    let whole: HashSet<String> = [
        "loop0", "loop1", "sda", "sdb", "sr0", "md0", "dm-0", "dm-1", "nbd0",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let alias = |kernel: &str| match kernel {
        "dm-0" => Some("vg0-root".to_owned()),
        "dm-1" => Some(String::new()),
        _ => None,
    };
    let devices = select(parse_diskstats(DEBIAN).unwrap(), Some(&whole), alias);
    // No partitions, loop or optical drives, nor nbd0 that never did I/O;
    // dm-1 has no name of its own.
    assert_eq!(names(&devices), ["sda", "sdb", "md0", "vg0-root", "dm-1"]);
    assert_eq!(devices[3].id, "253:0");
    // Without /sys/block, partitions are told by their names.
    let devices = select(parse_diskstats(RHEL).unwrap(), None, |_| None);
    assert_eq!(names(&devices), ["nvme0n1", "dm-0", "dm-1", "vda"]);
}

#[test]
fn partition_names() {
    for p in [
        "sda1",
        "vdb12",
        "xvda1",
        "hdc3",
        "nvme0n1p1",
        "mmcblk0p2",
        "md127p1",
    ] {
        assert!(looks_like_partition(p), "{p}");
    }
    for d in [
        "sda", "nvme0n1", "mmcblk0", "md0", "md127", "dm-0", "sdaa", "nbd0", "vda",
    ] {
        assert!(!looks_like_partition(d), "{d}");
    }
}

#[test]
fn an_alias_taken_twice_falls_back_to_the_kernel_name() {
    let stats = parse_diskstats(DEBIAN).unwrap();
    let devices = select(stats, None, |k| {
        k.starts_with("dm-").then(|| "data".to_owned())
    });
    assert_eq!(names(&devices), ["sda", "sdb", "md0", "data", "dm-1"]);
}

#[test]
fn counters_wrap_at_32_bits_and_reset_beyond() {
    assert_eq!(increase(10, 15), Some(5));
    assert_eq!(increase(u64::from(u32::MAX) - 1, 3), Some(5));
    assert_eq!(increase(1 << 40, 5), None);
}

#[test]
fn rates_from_two_readings() {
    let prev = Counters {
        reads: 100,
        writes: 200,
        read_sectors: 1000,
        written_sectors: 4000,
        busy_ms: 10_000,
        io_ms: 5_000,
        queue_ms: 8_000,
        in_flight: 0,
    };
    let cur = Counters {
        reads: 700,
        writes: 1400,
        read_sectors: 1000 + 60 * 2048,
        written_sectors: 4000 + 60 * 4096,
        busy_ms: 10_000 + 30_000,
        io_ms: 5_000 + 4_500,
        queue_ms: 8_000 + 90_000,
        in_flight: 1,
    };
    let r = rates(&prev, &cur, 60_000).unwrap();
    assert_eq!(r.reads_per_second, 10.0);
    assert_eq!(r.writes_per_second, 20.0);
    assert_eq!(r.read_bytes_per_second, 1024.0 * 1024.0);
    assert_eq!(r.written_bytes_per_second, 2.0 * 1024.0 * 1024.0);
    assert_eq!(r.busy_ratio, 0.5);
    assert_eq!(r.wait_seconds, 0.0025);
    assert_eq!(r.queue_depth, 1.5);
    assert_eq!(rates(&prev, &cur, 0), None);
}

#[test]
fn idle_and_stuck_disks() {
    let idle = Counters {
        reads: 5,
        ..Counters::default()
    };
    let r = rates(&idle, &idle, 60_000).unwrap();
    assert_eq!(
        (r.busy_ratio, r.wait_seconds, r.reads_per_second),
        (0.0, 0.0, 0.0)
    );
    // I/Os in flight, none completed: they waited the whole minute.
    let stuck = Counters {
        busy_ms: 60_000,
        in_flight: 2,
        ..idle
    };
    let r = rates(&idle, &stuck, 60_000).unwrap();
    assert_eq!((r.busy_ratio, r.wait_seconds), (1.0, 60.0));
}

#[test]
fn millisecond_counters_wrap_but_a_replaced_device_does_not() {
    let base = Counters {
        reads: 1 << 33,
        busy_ms: u64::from(u32::MAX) - 999,
        io_ms: u64::from(u32::MAX) - 99,
        queue_ms: u64::from(u32::MAX),
        ..Counters::default()
    };
    let wrapped = Counters {
        reads: (1 << 33) + 10,
        busy_ms: 1_000,
        io_ms: 0,
        queue_ms: 1_999,
        ..base
    };
    let r = rates(&base, &wrapped, 10_000).unwrap();
    assert_eq!(r.busy_ratio, 0.2);
    assert_eq!(r.wait_seconds, 0.01);
    assert_eq!(r.queue_depth, 0.2);
    // Small counters starting over: the "wrap" would be weeks of busy time.
    let busy = Counters {
        reads: 1_000,
        busy_ms: 500_000,
        ..Counters::default()
    };
    let fresh = Counters {
        reads: 3,
        busy_ms: 20,
        ..Counters::default()
    };
    assert_eq!(rates(&busy, &fresh, 60_000), None);
    // 64-bit counters going back are a reset too.
    assert_eq!(rates(&base, &Counters { reads: 1, ..base }, 10_000), None);
}

#[test]
fn first_round_primes_then_rates_per_device() {
    let sensor = DiskIoSensor::default();
    let stats = || select(parse_diskstats(DEBIAN).unwrap(), None, |_| None);
    assert!(sensor.round(Duration::from_secs(10), stats()).is_empty());
    let mut later = stats();
    for d in &mut later {
        d.counters.reads += 120;
        d.counters.busy_ms += 6_000;
        d.counters.io_ms += 240;
    }
    let samples = sensor.round(Duration::from_secs(70), later);
    assert_eq!(samples.len(), 7 * 5);
    assert_eq!(instances(&samples), ["sda", "sdb", "md0", "dm-0", "dm-1"]);
    assert_eq!(value(&samples, "sda", "reads_per_second"), 2.0);
    assert_eq!(value(&samples, "md0", "busy_ratio"), 0.1);
    assert_eq!(value(&samples, "dm-0", "wait_seconds"), 0.002);
    assert_eq!(value(&samples, "sdb", "writes_per_second"), 0.0);
}

#[test]
fn hotplugged_devices_are_primed_and_forgotten() {
    let sensor = DiskIoSensor::default();
    let counters = |n: u64| Counters {
        reads: n,
        busy_ms: n,
        ..Counters::default()
    };
    sensor.round(
        Duration::from_secs(0),
        vec![device("sda", "8:0", counters(10))],
    );
    let added = sensor.round(
        Duration::from_secs(60),
        vec![
            device("sda", "8:0", counters(20)),
            device("sdb", "8:16", counters(1_000)),
        ],
    );
    assert_eq!(instances(&added), ["sda"], "sdb is only primed");
    let removed = sensor.round(
        Duration::from_secs(120),
        vec![device("sdb", "8:16", counters(1_060))],
    );
    assert_eq!(instances(&removed), ["sdb"]);
    // sda comes back: primed again rather than compared with old counters.
    let back = sensor.round(
        Duration::from_secs(180),
        vec![
            device("sda", "8:0", counters(30)),
            device("sdb", "8:16", counters(1_120)),
        ],
    );
    assert_eq!(instances(&back), ["sdb"]);
    // Same name, another device number: another disk, primed again.
    let swapped = sensor.round(
        Duration::from_secs(240),
        vec![
            device("sda", "8:0", counters(40)),
            device("sdb", "8:32", counters(2_000)),
        ],
    );
    assert_eq!(instances(&swapped), ["sda"]);
}

#[test]
fn devices_beyond_the_cap_are_left_out() {
    let stats: Vec<Stat> = (0..MAX_DEVICES + 5)
        .map(|i| Stat {
            major: 8,
            minor: i as u32,
            name: format!("nbd{i}"),
            counters: Counters {
                writes: 1,
                ..Counters::default()
            },
        })
        .collect();
    assert_eq!(select(stats, None, |_| None).len(), MAX_DEVICES);
}

#[cfg(unix)]
#[tokio::test]
async fn collects_on_this_host() {
    if !std::path::Path::new(DISKSTATS).exists() {
        return;
    }
    let sensor = DiskIoSensor::default();
    let deadline = tokio::time::Instant::now();
    assert!(sensor.collect(deadline).await.unwrap().is_empty());
    std::thread::sleep(Duration::from_millis(30));
    let samples = sensor.collect(deadline).await.unwrap();
    for s in &samples {
        assert!(s.value.is_finite() && s.value >= 0.0, "{s:?}");
        assert!(!s.instance.is_empty());
    }
    assert!(samples
        .iter()
        .filter(|s| s.sensor == "busy_ratio")
        .all(|s| s.value <= 1.0));
}
