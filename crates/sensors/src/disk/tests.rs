use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::*;

const DEBIAN: &str = include_str!("../../tests/fixtures/debian12/mountinfo");
const RHEL: &str = include_str!("../../tests/fixtures/rhel9/mountinfo");

fn points(mounts: &[Mount]) -> Vec<&str> {
    mounts.iter().map(|m| m.mount_point.as_str()).collect()
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn parses_debian_and_rhel_mountinfo() {
    let debian = parse_mountinfo(DEBIAN).unwrap();
    assert_eq!(debian.len(), 15);
    let root = debian.iter().find(|m| m.mount_point == "/").unwrap();
    assert_eq!(
        *root,
        Mount {
            device_id: "8:1".into(),
            mount_point: "/".into(),
            fstype: "ext4".into(),
            source: "/dev/sda1".into(),
        }
    );
    assert!(debian.iter().any(|m| m.mount_point == "/mnt/backup copy"));
    let rhel = parse_mountinfo(RHEL).unwrap();
    assert_eq!(rhel[6].source, "/dev/mapper/rhel-root");
}

#[test]
fn refuses_odd_lines() {
    for bad in [
        "1 2 3",
        "1 2 0:1 / /x rw - ext4",
        "1 2 0:1 / /x rw shared:1 ext4 /dev/x rw",
    ] {
        assert!(parse_mountinfo(bad).is_err(), "{bad:?}");
    }
    assert_eq!(parse_mountinfo("\n").unwrap(), []);
}

#[test]
fn unescapes_octal() {
    assert_eq!(unescape(r"/a\040b\011c\134d"), "/a b\tc\\d");
    assert_eq!(unescape(r"/a\04"), r"/a\04");
    assert_eq!(unescape(r"/a\999"), r"/a\999");
    assert_eq!(unescape(r"/\303\251"), "/é");
}

#[test]
fn default_filter_keeps_real_filesystems_once() {
    let filter = Filter::default();
    let debian = filter.select(parse_mountinfo(DEBIAN).unwrap());
    // /mnt/backup copy is the same xfs as /srv/data.
    assert_eq!(points(&debian), ["/", "/srv/data", "/boot/efi", "/mnt/nfs"]);
    let rhel = filter.select(parse_mountinfo(RHEL).unwrap());
    assert_eq!(points(&rhel), ["/", "/boot", "/home"]);
}

#[test]
fn filter_lists_adjust_the_defaults() {
    let filter = Filter::new(
        &strings(&["tmpfs"]),
        &strings(&["nfs4", "vfat"]),
        &strings(&["/srv/", "/r"]),
    );
    let got = filter.select(parse_mountinfo(DEBIAN).unwrap());
    // /r does not exclude /run: only the path and what is below it.
    assert_eq!(points(&got), ["/run", "/", "/dev/shm", "/mnt/backup copy"]);
}

#[test]
fn params_are_checked() {
    let build = |pairs: &[(&str, &str)]| {
        let map = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        crate::registry::build("disk", &map).map(|_| ())
    };
    assert!(build(&[("fstypes_include", "tmpfs"), ("mounts_exclude", "/a,/b")]).is_ok());
    assert!(matches!(
        build(&[("mounts_exclude", "relative")]),
        Err(BuildError::BadParam {
            name: "mounts_exclude",
            ..
        })
    ));
    assert!(build(&[("fstypes_exclude", "")]).is_err());
    assert!(build(&[("fstype_include", "tmpfs")]).is_err());
}

fn mount(point: &str) -> Mount {
    Mount {
        device_id: point.into(),
        mount_point: point.into(),
        fstype: "ext4".into(),
        source: "/dev/x".into(),
    }
}

#[test]
fn usage_samples() {
    let usage = Usage {
        total: 1000,
        free: 400,
        available: 300,
        inodes: 50,
        inodes_free: 10,
    };
    let got: Vec<(String, f64)> = usage
        .samples(&mount("/data"))
        .into_iter()
        .inspect(|s| {
            assert_eq!(s.instance, "/data");
            assert_eq!(s.labels["fstype"], "ext4");
            assert_eq!(s.labels["device"], "/dev/x");
        })
        .map(|s| (s.sensor, s.value))
        .collect();
    let want = [
        ("total_bytes", 1000.0),
        ("free_bytes", 400.0),
        ("available_bytes", 300.0),
        // 600 used over 600 + 300.
        ("used_ratio", 600.0 / 900.0),
        ("inodes", 50.0),
        ("inodes_free", 10.0),
    ];
    assert_eq!(got, want.map(|(n, v)| (n.to_owned(), v)));

    let empty = Usage {
        total: 0,
        free: 0,
        available: 0,
        inodes: 0,
        inodes_free: 0,
    };
    let names: Vec<String> = empty
        .samples(&mount("/x"))
        .into_iter()
        .map(|s| s.sensor)
        .collect();
    assert_eq!(names, ["total_bytes", "free_bytes", "available_bytes"]);
}

/// Mount points that produced samples in one round.
async fn rounded<F>(
    sensor: &DiskSensor,
    mounts: Vec<Mount>,
    cutoff: Instant,
    stat: F,
) -> BTreeSet<String>
where
    F: Fn(&str) -> std::io::Result<Usage> + Clone + Send + 'static,
{
    let samples = sensor.stat_all(mounts, cutoff, stat).await;
    samples.into_iter().map(|s| s.instance).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hung_mount_is_skipped_until_it_answers() {
    let release = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(AtomicUsize::new(0));
    let stat = {
        let (release, calls) = (release.clone(), calls.clone());
        move |path: &str| {
            calls.fetch_add(1, Ordering::SeqCst);
            while path == "/hung" && !release.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(5));
            }
            if path == "/gone" {
                return Err(std::io::ErrorKind::NotFound.into());
            }
            Ok(Usage {
                total: 10,
                free: 5,
                available: 5,
                inodes: 0,
                inodes_free: 0,
            })
        }
    };
    let sensor = DiskSensor::default();
    let round = || {
        let cutoff = Instant::now() + Duration::from_millis(100);
        let mounts = vec![mount("/"), mount("/hung"), mount("/gone")];
        rounded(&sensor, mounts, cutoff, stat.clone())
    };

    assert_eq!(round().await, BTreeSet::from(["/".to_owned()]));
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert!(lock(&sensor.stuck).contains("/hung"));
    // The hung call still runs: no second call for it.
    assert_eq!(round().await, BTreeSet::from(["/".to_owned()]));
    assert_eq!(calls.load(Ordering::SeqCst), 5);

    release.store(true, Ordering::SeqCst);
    while lock(&sensor.in_flight).contains("/hung") {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(
        round().await,
        BTreeSet::from(["/".to_owned(), "/hung".to_owned()])
    );
    assert!(lock(&sensor.stuck).is_empty());
    assert!(lock(&sensor.in_flight).is_empty());
}

#[tokio::test]
async fn collects_on_this_host() {
    if !std::path::Path::new(MOUNTINFO).exists() {
        return;
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    let samples = DiskSensor::default().collect(deadline).await.unwrap();
    assert!(samples
        .iter()
        .all(|s| s.value.is_finite() && s.value >= 0.0));
}
