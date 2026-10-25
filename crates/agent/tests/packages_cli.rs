//! The `packages update` subcommand the agent runs through sudo
//! (ADR-0036). It runs the host's package manager, so only specs refused
//! before anything runs are tested here; what it runs is tested in the
//! library against fake tools.

// The real server and the Linux helpers only build on Unix (ADR-0047).
#![cfg(unix)]

use std::io::Write;
use std::process::{Command, Stdio};

fn update(stdin: &[u8]) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_monitorflow-agent"))
        .args(["packages", "update"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // The helper may exit before reading a large input.
    let _ = child.stdin.take().unwrap().write_all(stdin);
    child.wait_with_output().unwrap()
}

fn refused(stdin: &[u8], expected: &str) {
    let output = update(stdin);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty(), "something ran");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(expected), "{stderr}");
}

#[test]
fn update_refuses_input_that_is_not_a_spec() {
    refused(b"\xff\xff", "not an update spec");
    refused(
        br#"{"kind":"all","args":["-o","APT::x=y"]}"#,
        "unknown field",
    );
    refused(&vec![b' '; 70 * 1024], "larger than");
}

#[test]
fn update_refuses_options_and_paths_as_package_names() {
    refused(
        br#"{"kind":"named","packages":["bash","-oDpkg::Pre-Invoke::=id"]}"#,
        "invalid package name",
    );
    refused(
        br#"{"kind":"security","packages":["/tmp/evil.deb"]}"#,
        "invalid package name",
    );
    refused(br#"{"kind":"named","packages":[]}"#, "at least one package");
}
