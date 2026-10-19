use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use tempfile::TempDir;

use super::*;
use crate::registry;

fn me() -> u32 {
    rustix::process::geteuid().as_raw()
}

/// A script directory only its owner can write, as an installer leaves it.
fn script_dir() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    dir
}

fn install(dir: &Path, name: &str, body: &str, mode: u32) -> PathBuf {
    let path = dir.join(name);
    crate::testing::write_executable(&path, body, mode);
    path
}

fn sensor(dir: &Path, name: &str, args: &[&str]) -> ScriptSensor {
    let args = args.iter().map(|a| a.to_string()).collect();
    ScriptSensor::new(name.into(), args, dir.into(), vec![me()])
}

fn deadline(secs: u64) -> Instant {
    Instant::now() + Duration::from_secs(secs)
}

fn value(samples: &[Sample], metric: &str) -> Option<f64> {
    samples.iter().find(|s| s.sensor == metric).map(|s| s.value)
}

#[test]
fn names_and_args_are_plain_words() {
    for ok in ["check_backup", "check-disk.sh", "a", "v1.2"] {
        assert_eq!(script_name(ok), Ok(()), "{ok}");
    }
    let long = "x".repeat(MAX_NAME_LEN + 1);
    for bad in [
        "",
        "../etc/passwd",
        "a/b",
        "/bin/sh",
        "..",
        ".hidden",
        "-rf",
        "a..b",
        "a b",
        "a;b",
        "$(id)",
        "é",
        &long,
    ] {
        assert!(script_name(bad).is_err(), "{bad}");
    }
    for ok in ["--warning=80", "/var/backups", "user@host", "50%", ""] {
        assert_eq!(script_arg(ok), Ok(()), "{ok}");
    }
    for bad in [
        "a b", "$HOME", "`id`", "a;b", "a|b", "~", "*", "a\nb", "\"x\"",
    ] {
        assert!(script_arg(bad).is_err(), "{bad}");
    }
}

#[test]
fn params_are_checked_and_strict() {
    let build = |pairs: &[(&str, &str)]| {
        let map: BTreeMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        registry::build("script", &map).err().map(|e| e.to_string())
    };
    assert_eq!(build(&[("script", "check"), ("args", "-w,80")]), None);
    assert!(build(&[]).unwrap().contains("is required"));
    assert!(build(&[("script", "../check")])
        .unwrap()
        .contains("not a script name"));
    assert!(build(&[("script", "check"), ("args", "a b")])
        .unwrap()
        .contains("plain argument"));
    let many = ["a"; MAX_ARGS + 1].join(",");
    assert!(build(&[("script", "check"), ("args", &many)])
        .unwrap()
        .contains("at most"));
    assert!(build(&[("script", "check"), ("command", "rm")])
        .unwrap()
        .contains("unknown parameter"));
}

#[test]
fn perfdata_is_parsed_and_bounded() {
    let output = "OK - backup is 2h old\nage_seconds=7200\nsize_bytes = 1.5e9\n\
                  status=0\nduration_seconds=1\nBad=1\nx-y=2\nnan_value=NaN\ninf=inf\n\
                  empty=\nage_seconds=1\n";
    assert_eq!(
        parse_perfdata(output),
        [
            ("age_seconds".to_owned(), 7200.0),
            ("size_bytes".to_owned(), 1.5e9)
        ]
    );
    let many: String = (0..MAX_PERFDATA + 5)
        .map(|i| format!("m{i}={i}\n"))
        .collect();
    let gauges = parse_perfdata(&many);
    assert_eq!(gauges.len(), MAX_PERFDATA);
    assert_eq!(gauges.last().unwrap().0, format!("m{}", MAX_PERFDATA - 1));
}

#[tokio::test]
async fn exit_codes_become_status() {
    let dir = script_dir();
    for (code, expected) in [(0, 0.0), (1, 1.0), (2, 2.0), (3, 3.0), (4, 3.0), (255, 3.0)] {
        install(
            dir.path(),
            "check",
            &format!("#!/bin/sh\necho 'free=42'\nexit {code}\n"),
            0o755,
        );
        let samples = sensor(dir.path(), "check", &[])
            .collect(deadline(10))
            .await
            .unwrap();
        assert_eq!(value(&samples, "status"), Some(expected), "exit {code}");
        assert!(value(&samples, "duration_seconds").unwrap() >= 0.0);
        assert_eq!(value(&samples, "free"), Some(42.0));
        assert!(samples.iter().all(|s| s.labels["script"] == "check"));
    }
    // Killed by a signal.
    install(dir.path(), "check", "#!/bin/sh\nkill -9 $$\n", 0o755);
    let samples = sensor(dir.path(), "check", &[])
        .collect(deadline(10))
        .await
        .unwrap();
    assert_eq!(value(&samples, "status"), Some(UNKNOWN));
}

#[tokio::test]
async fn the_timeout_kills_the_whole_process_group() {
    let dir = script_dir();
    let pids = tempfile::tempdir().unwrap();
    let pid_file = pids.path().join("child.pid");
    let body = format!(
        "#!/bin/sh\nsleep 300 &\necho $! > {}\necho 'early=1'\nsleep 300\n",
        pid_file.display()
    );
    install(dir.path(), "hang", &body, 0o755);
    let started = Instant::now();
    let samples = sensor(dir.path(), "hang", &[])
        .collect(Instant::now() + Duration::from_secs(2))
        .await
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "returned before the deadline"
    );
    assert_eq!(value(&samples, "status"), Some(UNKNOWN));
    // No perfdata from a run that did not finish.
    assert_eq!(value(&samples, "early"), None);

    let pid: i32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // SIGKILL is asynchronous: wait for the background child to be gone
    // (or a zombie waiting for init), with a deadline.
    let gone = Instant::now() + Duration::from_secs(10);
    loop {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        let state = stat.rsplit_once(") ").map(|(_, rest)| &rest[..1]);
        if matches!(state, None | Some("Z") | Some("X")) {
            break;
        }
        assert!(
            Instant::now() < gone,
            "background child {pid} survived: {stat}"
        );
        tokio::task::yield_now().await;
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[tokio::test]
async fn too_much_output_is_unknown() {
    let dir = script_dir();
    install(
        dir.path(),
        "chatty",
        "#!/bin/sh\necho 'n=1'\nexec yes\n",
        0o755,
    );
    let samples = sensor(dir.path(), "chatty", &[])
        .collect(deadline(10))
        .await
        .unwrap();
    assert_eq!(value(&samples, "status"), Some(UNKNOWN));
    assert_eq!(value(&samples, "n"), None);
}

#[tokio::test]
async fn the_environment_is_not_inherited_and_there_is_no_shell() {
    let dir = script_dir();
    // HOME is always set for the test runner; the script must not see it.
    assert!(std::env::var_os("HOME").is_some() || std::env::var_os("USER").is_some());
    let body = "#!/bin/sh\n\
        [ -z \"$HOME\" ] && [ -z \"$USER\" ] && echo clean_env=1\n\
        [ \"$PATH\" = '/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin' ] && echo fixed_path=1\n\
        [ \"$(pwd)\" = / ] && echo in_root=1\n\
        echo vars=$(env | grep -cv '^PWD=\\|^SHLVL=\\|^_=')\n\
        echo args=$#\n\
        [ \"$1\" = 'a=b' ] && [ \"$2\" = '--x=$y' ] && echo literal_args=1\n";
    install(dir.path(), "env", body, 0o755);
    // `--x=$y` cannot come from params (see `script_arg`); `new` takes it
    // to show that nothing expands it either.
    let samples = sensor(dir.path(), "env", &["a=b", "--x=$y"])
        .collect(deadline(10))
        .await
        .unwrap();
    for metric in ["clean_env", "fixed_path", "in_root", "literal_args"] {
        assert_eq!(value(&samples, metric), Some(1.0), "{metric}: {samples:?}");
    }
    assert_eq!(value(&samples, "vars"), Some(2.0), "PATH and LC_ALL only");
    assert_eq!(value(&samples, "args"), Some(2.0));

    // A file of shell commands without `#!` is not handed to a shell.
    let marker = dir.path().join("ran");
    install(
        dir.path(),
        "plain",
        &format!("touch {}\n", marker.display()),
        0o755,
    );
    let error = sensor(dir.path(), "plain", &[])
        .collect(deadline(10))
        .await
        .unwrap_err();
    assert!(
        error.is_permanent() && error.to_string().contains("#!"),
        "{error}"
    );
    assert!(!marker.exists());
}

#[tokio::test]
async fn unsafe_installs_are_refused_before_each_run() {
    let dir = script_dir();
    let path = install(dir.path(), "check", "#!/bin/sh\nexit 0\n", 0o755);
    let s = sensor(dir.path(), "check", &[]);
    assert!(s.collect(deadline(10)).await.is_ok());

    let refused = |error: SensorError, why: &str| {
        assert!(error.is_permanent(), "{error}");
        assert!(error.to_string().contains(why), "{error} should say {why}");
    };
    for (mode, why) in [
        (0o775, "writable by group"),
        (0o757, "writable by group or other"),
        (0o644, "not executable"),
    ] {
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        refused(s.collect(deadline(10)).await.unwrap_err(), why);
    }
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();

    // The directory too.
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
    refused(s.collect(deadline(10)).await.unwrap_err(), "directory");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();

    // Owned by someone else: only root can chown, so tests trust another uid.
    let other = ScriptSensor::new("check".into(), vec![], dir.path().into(), vec![0]);
    if me() != 0 {
        refused(
            other.collect(deadline(10)).await.unwrap_err(),
            "owned by uid",
        );
    }

    // Missing, a directory, a FIFO.
    refused(
        sensor(dir.path(), "nope", &[])
            .collect(deadline(10))
            .await
            .unwrap_err(),
        "not installed",
    );
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    refused(
        sensor(dir.path(), "sub", &[])
            .collect(deadline(10))
            .await
            .unwrap_err(),
        "regular file",
    );
    let fifo = dir.path().join("fifo");
    rustix::fs::mknodat(
        rustix::fs::CWD,
        &fifo,
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::from_raw_mode(0o755),
        0,
    )
    .unwrap();
    refused(
        sensor(dir.path(), "fifo", &[])
            .collect(deadline(10))
            .await
            .unwrap_err(),
        "regular file",
    );
}

#[tokio::test]
async fn names_cannot_leave_the_directory() {
    let dir = script_dir();
    let outside = script_dir();
    install(outside.path(), "evil", "#!/bin/sh\necho pwned=1\n", 0o755);
    let escape = format!(
        "../{}/evil",
        outside.path().file_name().unwrap().to_str().unwrap()
    );
    for name in [escape.as_str(), "/bin/true", "..", "sub/../evil"] {
        let error = sensor(dir.path(), name, &[])
            .collect(deadline(10))
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("not a script name"),
            "{name}: {error}"
        );
    }

    // A symlink is followed only to a file that passes the same checks,
    // in a directory that does too.
    std::os::unix::fs::symlink(outside.path().join("evil"), dir.path().join("link")).unwrap();
    let samples = sensor(dir.path(), "link", &[])
        .collect(deadline(10))
        .await
        .unwrap();
    assert_eq!(value(&samples, "pwned"), Some(1.0));
    std::fs::set_permissions(outside.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
    let error = sensor(dir.path(), "link", &[])
        .collect(deadline(10))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("directory"), "{error}");
    std::fs::set_permissions(outside.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[tokio::test]
async fn a_spent_deadline_reports_unknown_without_waiting() {
    let dir = script_dir();
    install(dir.path(), "slow", "#!/bin/sh\nsleep 300\n", 0o755);
    let samples = sensor(dir.path(), "slow", &[])
        .collect(Instant::now())
        .await
        .unwrap();
    assert_eq!(value(&samples, "status"), Some(UNKNOWN));
}
