use std::fs;
use std::io::Write;

use tempfile::TempDir;

use super::*;

const DAY: Duration = Duration::from_secs(24 * 60 * 60);
const NOW: i64 = 1_800_000_000_000;

fn record(i: u32) -> Vec<u8> {
    format!("record-{i:04}").into_bytes()
}

/// 12-byte payloads: 28 bytes a record on disk.
const RECORD_LEN: u64 = HEADER_LEN + 11;

fn limits(max_bytes: u64, segment_records: u64) -> Limits {
    Limits {
        max_bytes,
        max_age: DAY,
        segment_bytes: segment_records * RECORD_LEN,
    }
}

fn read_all(store: &mut Store, agent: &str) -> Vec<Vec<u8>> {
    store.read(agent, NOW, usize::MAX).unwrap()
}

fn segments(dir: &Path, agent: &str) -> Vec<PathBuf> {
    let mut files: Vec<_> = fs::read_dir(dir.join(format!("agent-{agent}")))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "seg"))
        .collect();
    files.sort();
    files
}

#[test]
fn limits_split_the_buffer_in_eighths() {
    let limits = Limits::new(1024 * 1024 * 1024, DAY);
    assert_eq!(limits.segment_bytes, MAX_SEGMENT);
    assert_eq!(Limits::new(8 * 1024 * 1024, DAY).segment_bytes, 1024 * 1024);
    assert_eq!(Limits::new(4, DAY).segment_bytes, 1);
}

#[test]
fn records_come_back_in_order_and_go_once_committed() {
    let dir = TempDir::new().unwrap();
    let mut store = Store::open(dir.path(), limits(1 << 20, 3)).unwrap();
    for i in 0..8 {
        assert!(store.append("web-01", NOW, &record(i)).unwrap());
    }
    store.append("web-02", NOW, &record(100)).unwrap();
    assert_eq!(segments(dir.path(), "web-01").len(), 3);
    assert_eq!(store.pending(), ["web-01", "web-02"]);

    let first = store.read("web-01", NOW, 4).unwrap();
    assert_eq!(first, (0..4).map(record).collect::<Vec<_>>());
    // The first segment was read to its end and goes; the second stays.
    store.commit("web-01").unwrap();
    assert_eq!(segments(dir.path(), "web-01").len(), 2);
    let rest = read_all(&mut store, "web-01");
    assert_eq!(rest, (4..8).map(record).collect::<Vec<_>>());
    store.commit("web-01").unwrap();
    assert!(segments(dir.path(), "web-01").is_empty());
    assert!(!store.has_pending("web-01"));
    assert_eq!(read_all(&mut store, "web-02"), [record(100)]);
    assert_eq!(store.stats().appended, 9);
}

#[test]
fn the_last_segment_keeps_growing_while_it_is_read() {
    let dir = TempDir::new().unwrap();
    let mut store = Store::open(dir.path(), limits(1 << 20, 10)).unwrap();
    store.append("web-01", NOW, &record(0)).unwrap();
    assert_eq!(read_all(&mut store, "web-01"), [record(0)]);
    // Appended after the read, before the commit: not lost.
    store.append("web-01", NOW, &record(1)).unwrap();
    store.commit("web-01").unwrap();
    assert!(store.has_pending("web-01"));
    assert_eq!(read_all(&mut store, "web-01"), [record(1)]);
    store.commit("web-01").unwrap();
    assert!(!store.has_pending("web-01"));
    // A new segment after the last was deleted.
    store.append("web-01", NOW, &record(2)).unwrap();
    assert_eq!(read_all(&mut store, "web-01"), [record(2)]);
    assert_eq!(store.stats().bytes, RECORD_LEN);
}

#[test]
fn rewind_reads_uncommitted_records_again() {
    let dir = TempDir::new().unwrap();
    let mut store = Store::open(dir.path(), limits(1 << 20, 2)).unwrap();
    for i in 0..5 {
        store.append("web-01", NOW, &record(i)).unwrap();
    }
    assert_eq!(store.read("web-01", NOW, 3).unwrap().len(), 3);
    store.commit("web-01").unwrap();
    store.read("web-01", NOW, 1).unwrap();
    // The stream failed: the third record's segment is read again.
    store.rewind("web-01");
    assert_eq!(
        read_all(&mut store, "web-01"),
        (2..5).map(record).collect::<Vec<_>>()
    );
}

#[test]
fn records_and_hello_survive_a_restart() {
    let dir = TempDir::new().unwrap();
    {
        let mut store = Store::open(dir.path(), limits(1 << 20, 2)).unwrap();
        store.set_hello("web-01", b"hello v1").unwrap();
        store.set_hello("web-01", b"hello v2").unwrap();
        for i in 0..3 {
            store.append("web-01", NOW, &record(i)).unwrap();
        }
        store.sync().unwrap();
    }
    let mut store = Store::open(dir.path(), limits(1 << 20, 2)).unwrap();
    assert_eq!(store.hello("web-01"), Some(&b"hello v2"[..]));
    assert_eq!(store.stats().bytes, 3 * RECORD_LEN);
    // New records go after the old ones, in a new segment.
    store.append("web-01", NOW, &record(3)).unwrap();
    assert_eq!(segments(dir.path(), "web-01").len(), 3);
    assert_eq!(
        read_all(&mut store, "web-01"),
        (0..4).map(record).collect::<Vec<_>>()
    );
}

#[test]
fn a_torn_last_record_is_truncated_at_start() {
    let dir = TempDir::new().unwrap();
    {
        let mut store = Store::open(dir.path(), limits(1 << 20, 10)).unwrap();
        for i in 0..3 {
            store.append("web-01", NOW, &record(i)).unwrap();
        }
    }
    let path = segments(dir.path(), "web-01").remove(0);
    // A crash in the middle of the fourth record: a header and half a
    // payload.
    let mut torn = Vec::new();
    torn.extend_from_slice(&11u32.to_le_bytes());
    torn.extend_from_slice(&checksum(NOW, &record(3)).to_le_bytes());
    torn.extend_from_slice(&NOW.to_le_bytes());
    torn.extend_from_slice(&record(3)[..5]);
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(&torn)
        .unwrap();

    let mut store = Store::open(dir.path(), limits(1 << 20, 10)).unwrap();
    assert_eq!(fs::metadata(&path).unwrap().len(), 3 * RECORD_LEN);
    assert_eq!(store.stats().bytes, 3 * RECORD_LEN);
    store.append("web-01", NOW, &record(4)).unwrap();
    let expected: Vec<_> = [0, 1, 2, 4].into_iter().map(record).collect();
    assert_eq!(read_all(&mut store, "web-01"), expected);
}

#[test]
fn a_corrupt_record_ends_its_segment_and_empty_segments_go() {
    let dir = TempDir::new().unwrap();
    {
        let mut store = Store::open(dir.path(), limits(1 << 20, 3)).unwrap();
        for i in 0..6 {
            store.append("web-01", NOW, &record(i)).unwrap();
        }
    }
    let files = segments(dir.path(), "web-01");
    // Flip a payload byte of the second record of the first segment, and
    // a length byte of the first record of the second.
    let mut first = fs::read(&files[0]).unwrap();
    first[(RECORD_LEN + HEADER_LEN) as usize] ^= 0xff;
    fs::write(&files[0], first).unwrap();
    let mut second = fs::read(&files[1]).unwrap();
    second[3] = 0xff;
    fs::write(&files[1], second).unwrap();

    let mut store = Store::open(dir.path(), limits(1 << 20, 3)).unwrap();
    assert_eq!(read_all(&mut store, "web-01"), [record(0)]);
    assert_eq!(segments(dir.path(), "web-01").len(), 1);
}

#[test]
fn a_full_buffer_drops_the_oldest_segments_of_any_agent() {
    let dir = TempDir::new().unwrap();
    // Room for 6 records, in segments of 2.
    let mut store = Store::open(dir.path(), limits(6 * RECORD_LEN, 2)).unwrap();
    for i in 0..4 {
        store.append("web-01", NOW, &record(i)).unwrap();
    }
    store.append("web-02", NOW, &record(100)).unwrap();
    store.append("web-02", NOW, &record(101)).unwrap();
    // Full: the next record drops web-01's first segment, then the next
    // drops web-01's second, older than web-02's.
    store.append("web-02", NOW, &record(102)).unwrap();
    assert_eq!(store.stats().dropped_full, 2);
    store.append("web-02", NOW, &record(103)).unwrap();
    store.append("web-02", NOW, &record(104)).unwrap();
    assert_eq!(store.stats().dropped_full, 4);
    assert!(store.stats().bytes <= 6 * RECORD_LEN);
    assert!(!store.has_pending("web-01"));
    assert_eq!(
        read_all(&mut store, "web-02"),
        (100..105).map(record).collect::<Vec<_>>()
    );
    // A record larger than the whole buffer is dropped alone.
    assert!(!store.append("web-02", NOW, &[0; 200]).unwrap());
    assert_eq!(store.stats().dropped_full, 5);
}

#[test]
fn dropping_a_segment_being_read_counts_only_unread_records() {
    let dir = TempDir::new().unwrap();
    let mut store = Store::open(dir.path(), limits(4 * RECORD_LEN, 2)).unwrap();
    for i in 0..4 {
        store.append("web-01", NOW, &record(i)).unwrap();
    }
    assert_eq!(store.read("web-01", NOW, 1).unwrap(), [record(0)]);
    store.append("web-01", NOW, &record(4)).unwrap();
    assert_eq!(store.stats().dropped_full, 1);
    // The cursor restarts at the oldest remaining record.
    assert_eq!(
        read_all(&mut store, "web-01"),
        (2..5).map(record).collect::<Vec<_>>()
    );
}

#[test]
fn old_records_are_skipped_and_old_segments_expire() {
    let dir = TempDir::new().unwrap();
    let mut store = Store::open(dir.path(), limits(1 << 20, 2)).unwrap();
    let old = NOW - DAY.as_millis() as i64 - 1;
    store.append("web-01", old, &record(0)).unwrap();
    store.append("web-01", old, &record(1)).unwrap();
    store.append("web-01", old, &record(2)).unwrap();
    store.append("web-01", NOW, &record(3)).unwrap();
    store.append("web-02", old, &record(9)).unwrap();
    // Expiry goes by segment: web-01's second one has a recent record.
    assert_eq!(store.expire(NOW).unwrap(), 3);
    assert_eq!(segments(dir.path(), "web-01").len(), 1);
    assert!(!store.has_pending("web-02"));
    // Reading skips the old record left in it.
    assert_eq!(read_all(&mut store, "web-01"), [record(3)]);
    assert_eq!(store.stats().dropped_expired, 4);
}

#[test]
fn discard_deletes_an_agents_records() {
    let dir = TempDir::new().unwrap();
    let mut store = Store::open(dir.path(), limits(1 << 20, 2)).unwrap();
    for i in 0..3 {
        store.append("web-01", NOW, &record(i)).unwrap();
    }
    assert_eq!(store.discard("web-01").unwrap(), 3);
    assert_eq!(store.stats().dropped_refused, 3);
    assert_eq!(store.stats().bytes, 0);
    assert!(segments(dir.path(), "web-01").is_empty());
    store.append("web-01", NOW, &record(3)).unwrap();
    assert_eq!(read_all(&mut store, "web-01"), [record(3)]);
}

#[test]
fn unsafe_agent_ids_and_stray_files_are_refused_or_ignored() {
    let dir = TempDir::new().unwrap();
    let mut store = Store::open(dir.path(), limits(1 << 20, 2)).unwrap();
    assert!(store.append("../etc", NOW, &record(0)).is_err());
    // `..` is a valid ID, and stays inside the buffer.
    store.append("..", NOW, &record(0)).unwrap();
    assert!(dir.path().join("agent-..").is_dir());
    fs::create_dir(dir.path().join("lost+found")).unwrap();
    fs::write(dir.path().join("agent-..").join("hello.pb.tmp"), b"x").unwrap();
    drop(store);
    let store = Store::open(dir.path(), limits(1 << 20, 2)).unwrap();
    assert_eq!(store.pending(), [".."]);
    assert!(!dir.path().join("agent-..").join("hello.pb.tmp").exists());
}
