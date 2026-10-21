//! The `sources` subcommand the agent runs through sudo (ADR-0032). It
//! always works on `/`, so only specs refused before any change are run.

use std::io::Write;
use std::process::{Command, Stdio};

fn apply(stdin: &[u8]) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_monitorflow-agent"))
        .args(["sources", "apply"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn apply_refuses_input_that_is_not_a_message() {
    let output = apply(b"\xff\xff\xff");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not a ConfigureSources message"),
        "{stderr}"
    );
}

#[test]
fn apply_checks_the_spec_before_touching_the_host() {
    // An empty message decodes, and has no family.
    let output = apply(b"");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("family is required"), "{stderr}");
}
