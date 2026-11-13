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
                .contains("full path"),
            "{bad}"
        );
    }
    let windows = ("path", r"D:\Logs\IIS\u_ex261103.log");
    assert_eq!(check(&[windows, ("pattern", " 500 ")]), None);
    for bad in [
        r"C:",
        r"C:\",
        r"C:Logs\app.log",
        r"\\server\share\app.log",
        r"\\?\C:\app.log",
        r"\\.\pipe\x",
        r"C:\Logs\..\Windows\x",
        r"C:\Logs\.\x",
        r"C:\Logs\\x",
        r"C:\Logs\x\",
        r"C:\Logs\x.",
        r"C:\Logs/x",
        r"C:\Logs\app.log:secret",
        r"C:\Logs\*.log",
        r"C:\Logs\CON",
        r"C:\Logs\nul.txt",
        r"C:\Logs\com1.log",
    ] {
        assert!(
            check(&[("path", bad), ("pattern", "x")])
                .unwrap()
                .contains("full path"),
            "{bad}"
        );
    }
    assert_eq!(
        check(&[("path", r"C:\Logs\console.log"), ("pattern", "x")]),
        None
    );
    // Segments of 4 bytes whose 4th byte is inside a character are checked
    // for device names without panicking (security audit part 4).
    for multibyte in [
        r"C:\Logs\abé",
        r"C:\abé.log",
        r"C:\Logs\é1\x.log",
        r"C:\Logs\€x\x.log",
        r"C:\Logs\comé",
        r"C:\Logs\日志.log",
    ] {
        assert_eq!(
            check(&[("path", multibyte), ("pattern", "x")]),
            None,
            "{multibyte}"
        );
    }
    for device in [r"C:\Logs\LPT9", r"C:\Logs\Com3.txt"] {
        assert!(!windows_path(device), "{device}");
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
    let error = build(&allowed.path().join("x"), &[], &[outside.path()])
        .err()
        .unwrap();
    assert!(
        error.to_string().contains("not in the agent's log folders"),
        "{error}"
    );
    // A missing file under an allowed directory is fine; it may appear.
    assert!(build(&allowed.path().join("later.log"), &[], &[allowed.path()]).is_ok());
}

#[test]
fn paths_of_the_other_system_are_refused_by_the_agent() {
    let dir = tempfile::tempdir().unwrap();
    let other = if cfg!(windows) {
        "/var/log/syslog"
    } else {
        r"C:\Logs\app.log"
    };
    let error = build(Path::new(other), &[], &[dir.path()]).err().unwrap();
    assert!(
        matches!(error, BuildError::Refused { .. }) && error.to_string().contains("path"),
        "{error}"
    );
    let error = build(&dir.path().join("x.log"), &[], &[]).err().unwrap();
    assert!(error.to_string().contains("log_dirs is empty"), "{error}");
}

#[cfg(unix)]
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
            .contains("outside the agent's log folders"),
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
    // The writer finishes the old file late: read once more, then left.
    append(&path, "ERROR 5\n");
    append(&rotated, "ERROR 6 late\n");
    assert_eq!(matches(&s).await, 2.0);
    assert_eq!(matches(&s).await, 0.0);
    append(&rotated, "ERROR not followed any more\n");
    append(&path, "ERROR 7\n");
    assert_eq!(matches(&s).await, 1.0);

    // Renamed away and not recreated yet: the old file still counts.
    std::fs::rename(&path, dir.path().join("app.log.2")).unwrap();
    append(&dir.path().join("app.log.2"), "ERROR 8\n");
    assert_eq!(matches(&s).await, 1.0);
    append(&path, "ERROR 9\n");
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

#[cfg(unix)]
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

/// A sensor that saves its position in `positions`, as the agent's do.
fn saving(dir: &TempDir, positions: &Path, extra: &[(&str, &str)]) -> Arc<dyn Sensor> {
    let mut params: BTreeMap<String, String> = extra
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let path = dir.path().join("app.log");
    params.insert("path".into(), path.to_str().unwrap().into());
    params.entry("pattern".into()).or_insert("ERROR".into());
    let local = Local {
        log_positions: Some(positions.into()),
        ..local(&[dir.path()])
    };
    registry::build_local("log", &params, &local).unwrap()
}

#[tokio::test]
async fn a_restart_resumes_where_the_sensor_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let positions = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.log");
    append(&path, "ERROR history\n");
    let s = saving(&dir, positions.path(), &[]);
    assert_eq!(matches(&s).await, 0.0);
    append(&path, "ERROR 1\nERR");
    assert_eq!(matches(&s).await, 1.0);
    drop(s);

    // Written while the agent was stopped, the split line included.
    append(&path, "OR 2\nERROR 3\n");
    let s = saving(&dir, positions.path(), &[]);
    assert_eq!(matches(&s).await, 2.0);
    // Another sensor on the same file has a position of its own.
    let other = saving(&dir, positions.path(), &[("exclude", "3")]);
    assert_eq!(matches(&other).await, 0.0);
    drop((s, other));

    // Rotated while stopped: the new file is read from its start, as all
    // of it was written since.
    std::fs::rename(&path, dir.path().join("app.log.1")).unwrap();
    append(&path, "ERROR 4\n");
    let s = saving(&dir, positions.path(), &[]);
    assert_eq!(matches(&s).await, 1.0);
    drop(s);

    // Truncated and written again while stopped: from its start too.
    std::fs::write(&path, "ERROR 5 rewritten, longer than before\n").unwrap();
    let s = saving(&dir, positions.path(), &[]);
    assert_eq!(matches(&s).await, 1.0);
    drop(s);

    // Without a saved position, history is skipped.
    let elsewhere = tempfile::tempdir().unwrap();
    let s = saving(&dir, elsewhere.path(), &[]);
    assert_eq!(matches(&s).await, 0.0);
}

#[tokio::test]
async fn a_truncated_file_written_past_the_old_end_is_read_again() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.log");
    append(&path, "first line\n");
    let s = sensor(&dir, "app.log");
    matches(&s).await;
    // copytruncate, then more written than there was before.
    std::fs::write(&path, "ERROR a\nERROR b\nERROR c\n").unwrap();
    assert_eq!(matches(&s).await, 3.0);
    append(&path, "ERROR d\n");
    assert_eq!(matches(&s).await, 1.0);
}

#[tokio::test]
async fn crlf_and_utf8_byte_order_marks_are_handled() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.log");
    let s = build(&path, &[("pattern", "^ERROR [a-z]+$")], &[dir.path()]).unwrap();
    assert!(collect(&s).await.is_err());
    append(&path, "\u{feff}ERROR first\r\nERROR second\r\nERROR 3\r\n");
    assert_eq!(matches(&s).await, 2.0);
}

/// `text` in UTF-16, little or big endian, with its byte order mark if
/// `mark`.
fn utf16(text: &str, little: bool, mark: bool) -> Vec<u8> {
    let units = mark
        .then_some(0xfeff)
        .into_iter()
        .chain(text.encode_utf16());
    units
        .flat_map(|u| {
            if little {
                u.to_le_bytes()
            } else {
                u.to_be_bytes()
            }
        })
        .collect()
}

fn append_bytes(path: &Path, bytes: &[u8]) {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
}

#[tokio::test]
async fn utf16_files_are_decoded() {
    for little in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.log");
        let s = build(&path, &[("pattern", "^Erreur é[^\n]*$")], &[dir.path()]).unwrap();
        assert!(collect(&s).await.is_err());
        // The mark alone first, as some writers flush it.
        append_bytes(&path, &utf16("", little, true));
        assert_eq!(matches(&s).await, 0.0);
        append_bytes(
            &path,
            &utf16("Erreur é 1\r\nInfo\r\nErreur é 😀\r\n", little, false),
        );
        assert_eq!(matches(&s).await, 2.0, "little endian: {little}");
        // A line written in pieces, one code unit cut in half.
        let line = utf16("Erreur é 3\r\n", little, false);
        append_bytes(&path, &line[..7]);
        assert_eq!(matches(&s).await, 0.0);
        append_bytes(&path, &line[7..]);
        assert_eq!(matches(&s).await, 1.0);
        let (_, lag) = collect(&s).await.unwrap();
        assert_eq!(lag, 0.0);

        // The first collection starts at the end, after the mark.
        let s = build(&path, &[("pattern", "^Erreur")], &[dir.path()]).unwrap();
        assert_eq!(matches(&s).await, 0.0);
        append_bytes(&path, &utf16("Erreur 4\r\n", little, false));
        assert_eq!(matches(&s).await, 1.0);
    }
}

#[tokio::test]
async fn a_deleted_file_is_let_go() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.log");
    append(&path, "");
    let s = sensor(&dir, "app.log");
    matches(&s).await;
    append(&path, "ERROR last words");
    std::fs::remove_file(&path).unwrap();
    // The last line had no newline; the file is gone for good.
    assert_eq!(matches(&s).await, 1.0);
    assert!(collect(&s).await.is_err());
    // Created again by its writer, which the sensor must not prevent.
    append(&path, "ERROR new\n");
    assert_eq!(matches(&s).await, 1.0);
}

#[tokio::test]
async fn access_denied_is_reported_plainly() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.log");
    append(&path, "");
    let s = sensor(&dir, "app.log");
    if !deny_read(&path) {
        return;
    }
    let error = collect(&s).await.unwrap_err();
    allow_read(&path);
    assert!(
        error.is_permanent() && error.to_string().contains("access denied"),
        "{error}"
    );
    assert!(error.to_string().contains(sys::ACCOUNT), "{error}");
}

/// Takes the read right on `path` from this process; `false` when it
/// cannot (root reads everything).
#[cfg(unix)]
fn deny_read(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).unwrap();
    std::fs::File::open(path).is_err()
}

#[cfg(unix)]
fn allow_read(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
}

/// A deny entry for the current user; the owner can still change the ACL
/// back, so no administrator right is needed.
#[cfg(windows)]
fn icacls(path: &Path, args: &[&str]) {
    let user = std::env::var("USERNAME").unwrap();
    let args: Vec<String> = args.iter().map(|a| a.replace("{user}", &user)).collect();
    let status = std::process::Command::new("icacls")
        .arg(path)
        .args(&args)
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "icacls {args:?}");
}

#[cfg(windows)]
fn deny_read(path: &Path) -> bool {
    icacls(path, &["/deny", "{user}:(R)"]);
    true
}

#[cfg(windows)]
fn allow_read(path: &Path) {
    icacls(path, &["/remove:d", "{user}"]);
}

/// The writer opens, writes, renames, truncates and deletes its log while
/// the sensor holds it, the old file after a rotation included.
#[cfg(windows)]
#[tokio::test]
async fn the_writer_is_never_locked_out() {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_DELETE, FILE_SHARE_READ};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.log");
    append(&path, "");
    let s = sensor(&dir, "app.log");
    matches(&s).await;
    // A writer that lets others read only, like .NET's FileShare.Read.
    let mut writer = std::fs::OpenOptions::new()
        .append(true)
        .share_mode(FILE_SHARE_READ)
        .open(&path)
        .unwrap();
    writer.write_all(b"ERROR 1\r\n").unwrap();
    assert_eq!(matches(&s).await, 1.0);
    drop(writer);

    // Rename and create, keeping the old one open with delete sharing.
    let rotated = dir.path().join("app.1.log");
    let mut old = std::fs::OpenOptions::new()
        .append(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
        .open(&path)
        .unwrap();
    std::fs::rename(&path, &rotated).unwrap();
    append(&path, "ERROR 2\r\n");
    assert_eq!(matches(&s).await, 1.0);
    old.write_all(b"ERROR 3 late\r\n").unwrap();
    drop(old);
    assert_eq!(matches(&s).await, 1.0);

    // Truncate in place, delete the old file, then the current one.
    std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&path)
        .unwrap();
    std::fs::remove_file(&rotated).unwrap();
    append(&path, "ERROR 4\r\n");
    assert_eq!(matches(&s).await, 1.0);
    std::fs::remove_file(&path).unwrap();
    matches(&s).await;
    append(&path, "ERROR 5\r\n");
    assert_eq!(matches(&s).await, 1.0);
}

/// Windows compares names without case; the configured folder and path may
/// differ from the disk's.
#[cfg(windows)]
#[tokio::test]
async fn windows_paths_ignore_case() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("App.log");
    append(&path, "");
    let upper = PathBuf::from(dir.path().to_str().unwrap().to_uppercase());
    let s = build(&upper.join("APP.LOG"), &[], &[dir.path()]).unwrap();
    matches(&s).await;
    append(&path, "ERROR 1\r\n");
    assert_eq!(matches(&s).await, 1.0);
}
