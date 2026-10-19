use std::collections::BTreeMap;
use std::io::Write;

use tempfile::TempDir;

use super::*;
use crate::registry::{self, BuildError};

fn local(dirs: &[&Path]) -> Local {
    Local {
        log_dirs: dirs.iter().map(|d| d.to_path_buf()).collect(),
        ..Local::default()
    }
}

fn build(
    path: &Path,
    extra: &[(&str, &str)],
    dirs: &[&Path],
) -> Result<Arc<dyn Sensor>, BuildError> {
    let mut params: BTreeMap<String, String> = extra
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    params.insert("path".into(), path.to_str().unwrap().into());
    params.entry("pattern".into()).or_insert("ERROR".into());
    registry::build_local("log", &params, &local(dirs))
}

fn sensor(dir: &TempDir, name: &str) -> Arc<dyn Sensor> {
    build(&dir.path().join(name), &[], &[dir.path()]).unwrap()
}

fn append(path: &Path, text: &str) {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    file.write_all(text.as_bytes()).unwrap();
}

/// `(matches, lag_bytes)` of one collection.
async fn collect(sensor: &Arc<dyn Sensor>) -> Result<(f64, f64), SensorError> {
    let samples = sensor
        .collect(Instant::now() + Duration::from_secs(10))
        .await?;
    let value = |m: &str| samples.iter().find(|s| s.sensor == m).unwrap().value;
    assert!(samples.iter().all(|s| s.labels.contains_key("path")));
    Ok((value("matches"), value("lag_bytes")))
}

async fn matches(sensor: &Arc<dyn Sensor>) -> f64 {
    collect(sensor).await.unwrap().0
}

#[test]
fn params_are_checked() {
    let check = |pairs: &[(&str, &str)]| {
        let map: BTreeMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        registry::build("log", &map).err().map(|e| e.to_string())
    };
    let path = ("path", "/var/log/app.log");
    assert_eq!(check(&[path, ("pattern", "ERROR|FATAL")]), None);
    assert_eq!(check(&[path, ("pattern", "x"), ("exclude", "debug")]), None);
    assert!(check(&[path]).unwrap().contains("`pattern` is required"));
    assert!(check(&[("pattern", "x")])
        .unwrap()
        .contains("`path` is required"));
    for bad in [
        "var/log/x",
        "/var/log/../../etc/shadow",
        "/var/log/./x",
        "/var//log",
        "/var/log/",
        "/a\nb",
    ] {
        assert!(
            check(&[("path", bad), ("pattern", "x")])
                .unwrap()
                .contains("absolute path"),
            "{bad}"
        );
    }
    let long = format!("/var/log/{}", "x".repeat(validate::MAX_VALUE_LEN));
    assert!(check(&[("path", &long), ("pattern", "x")]).is_some());
    assert!(check(&[path, ("pattern", "(")])
        .unwrap()
        .contains("not a valid pattern"));
    assert!(check(&[path, ("pattern", &"a".repeat(MAX_PATTERN + 1))])
        .unwrap()
        .contains("1 to"));
    assert!(check(&[path, ("pattern", r"(\w{1000}){1000}")])
        .unwrap()
        .contains("not a valid pattern"));
    assert!(check(&[path, ("pattern", "x"), ("exclude", "[")])
        .unwrap()
        .contains("`exclude`"));
}

#[test]
fn paths_outside_the_log_directories_are_refused() {
    let allowed = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret"), "ERROR\n").unwrap();
    let error = build(&outside.path().join("secret"), &[], &[allowed.path()])
        .err()
        .unwrap();
    assert!(
        matches!(error, BuildError::Refused { kind: "log", .. }),
        "{error}"
    );
    let error = build(&allowed.path().join("x"), &[], &[]).err().unwrap();
    assert!(error.to_string().contains("not under"), "{error}");
    // A missing file under an allowed directory is fine; it may appear.
    assert!(build(&allowed.path().join("later.log"), &[], &[allowed.path()]).is_ok());
}

#[tokio::test]
async fn a_symlink_escaping_the_log_directories_is_refused() {
    let allowed = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("shadow"), "ERROR\n").unwrap();
    let link = allowed.path().join("app.log");
    std::os::unix::fs::symlink(outside.path().join("shadow"), &link).unwrap();
    let error = build(&link, &[], &[allowed.path()]).err().unwrap();
    assert!(
        error
            .to_string()
            .contains("outside the agent's log directories"),
        "{error}"
    );

    // Swapped in after the sensor was accepted: refused at collection.
    std::fs::remove_file(&link).unwrap();
    std::fs::write(&link, "").unwrap();
    let s = build(&link, &[], &[allowed.path()]).unwrap();
    assert_eq!(matches(&s).await, 0.0);
    std::fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(outside.path().join("shadow"), &link).unwrap();
    let error = collect(&s).await.unwrap_err();
    assert!(error.is_permanent(), "{error}");

    // A symlink that stays inside is followed.
    let inside = allowed.path().join("real.log");
    std::fs::write(&inside, "").unwrap();
    std::fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(&inside, &link).unwrap();
    let s = build(&link, &[], &[allowed.path()]).unwrap();
    assert_eq!(matches(&s).await, 0.0);
    append(&inside, "ERROR one\n");
    assert_eq!(matches(&s).await, 1.0);
}

#[tokio::test]
async fn history_is_skipped_and_appended_matches_counted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.log");
    append(&path, "ERROR old\nERROR old\n");
    let s = sensor(&dir, "app.log");
    assert_eq!(collect(&s).await.unwrap(), (0.0, 0.0));
    append(&path, "ERROR new\ninfo\nan ERROR\r\n");
    assert_eq!(matches(&s).await, 2.0);
    assert_eq!(matches(&s).await, 0.0);

    // Excluded lines are not counted.
    let s = build(&path, &[("exclude", "ignore me")], &[dir.path()]).unwrap();
    assert_eq!(matches(&s).await, 0.0);
    append(&path, "ERROR ignore me\nERROR real\n");
    assert_eq!(matches(&s).await, 1.0);
}

#[tokio::test]
async fn partial_lines_wait_for_their_end() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.log");
    append(&path, "");
    let s = sensor(&dir, "app.log");
    matches(&s).await;
    append(&path, "ERR");
    assert_eq!(matches(&s).await, 0.0);
    append(&path, "OR split\nERROR");
    assert_eq!(matches(&s).await, 1.0);
    append(&path, "\n");
    assert_eq!(matches(&s).await, 1.0);
}

#[tokio::test]
async fn rotation_by_rename_reads_the_old_file_to_its_end() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.log");
    append(&path, "ERROR before start\n");
    let s = sensor(&dir, "app.log");
    matches(&s).await;
    append(&path, "ERROR 1\n");
    let rotated = dir.path().join("app.log.1");
    std::fs::rename(&path, &rotated).unwrap();
    // A writer that kept the old file open, then the new file.
    append(&rotated, "ERROR 2\n");
    append(&path, "ERROR 3\nERROR 4\n");
    assert_eq!(matches(&s).await, 4.0);
    append(&path, "ERROR 5\n");
    append(&rotated, "ERROR not followed any more\n");
    assert_eq!(matches(&s).await, 1.0);

    // Renamed away and not recreated yet: the old file still counts.
    std::fs::rename(&path, dir.path().join("app.log.2")).unwrap();
    append(&dir.path().join("app.log.2"), "ERROR 6\n");
    assert_eq!(matches(&s).await, 1.0);
    append(&path, "ERROR 7\n");
    assert_eq!(matches(&s).await, 1.0);
}

#[tokio::test]
async fn truncation_restarts_from_the_beginning() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.log");
    append(&path, &"ERROR padding\n".repeat(10));
    let s = sensor(&dir, "app.log");
    matches(&s).await;
    std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&path)
        .unwrap();
    append(&path, "ERROR after\n");
    assert_eq!(matches(&s).await, 1.0);
}

#[tokio::test]
async fn a_missing_file_is_read_from_its_start_once_created() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("late.log");
    let s = sensor(&dir, "late.log");
    let error = collect(&s).await.unwrap_err();
    assert!(
        !error.is_permanent() && error.to_string().contains("does not exist"),
        "{error}"
    );
    append(&path, "ERROR first\nERROR second\n");
    assert_eq!(matches(&s).await, 2.0);
}

#[tokio::test]
async fn long_lines_are_matched_on_their_start_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.log");
    append(&path, "");
    let s = sensor(&dir, "app.log");
    matches(&s).await;
    let late = format!("{}ERROR\n", "x".repeat(MAX_LINE));
    let early = format!("ERROR{}\n", "x".repeat(3 * MAX_LINE));
    append(&path, &format!("{late}{early}ERROR short\n"));
    assert_eq!(matches(&s).await, 2.0);
}

#[tokio::test]
async fn reads_are_capped_per_collection() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.log");
    append(&path, "");
    let s = sensor(&dir, "app.log");
    matches(&s).await;
    let line = "ERROR 0123456789abcdef0123456789abcdef0123456789abcdef0123456789\n";
    let lines = 3 * MAX_READ as usize / line.len();
    append(&path, &line.repeat(lines));
    let total = (lines * line.len()) as f64;

    let (first, lag) = collect(&s).await.unwrap();
    assert!(first <= (MAX_READ as usize / line.len()) as f64, "{first}");
    assert_eq!(lag, total - MAX_READ as f64);
    let mut counted = first;
    for _ in 0..3 {
        counted += matches(&s).await;
    }
    assert_eq!(counted, lines as f64);
    assert_eq!(collect(&s).await.unwrap(), (0.0, 0.0));
}

#[tokio::test]
async fn special_files_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let fifo = dir.path().join("fifo");
    rustix::fs::mknodat(
        rustix::fs::CWD,
        &fifo,
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::from_raw_mode(0o644),
        0,
    )
    .unwrap();
    let s = sensor(&dir, "fifo");
    let error = collect(&s).await.unwrap_err();
    assert!(
        error.is_permanent() && error.to_string().contains("regular file"),
        "{error}"
    );
}
