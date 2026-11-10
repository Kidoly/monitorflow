//! The `packages update` subcommand the agent runs through sudo
//! (ADR-0036). It runs the host's package manager, so only inputs refused
//! before anything runs are tested here: the helper acts on a command the
//! central server signed and on nothing else (ADR-0108). What it checks of
//! a signed command is tested in `helper_trust`, and what it runs in the
//! library against fake tools.

// The real server and the Linux helpers only build on Unix (ADR-0047).
#![cfg(unix)]

use std::io::Write;
use std::process::{Command, Stdio};

use monitorflow_agent::helper_trust::HelperRequest;
use monitorflow_common::command::APPLY_UPDATES;
use monitorflow_common::proto::v1::{ApplyUpdates, Command as Signed};
use prost::Message;

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
fn update_refuses_the_specs_it_read_before_commands_were_checked() {
    // What the agent's user could write on stdin before ADR-0108.
    refused(br#"{"kind":"all"}"#, "invalid input");
    refused(
        br#"{"kind":"named","packages":["bash","-oDpkg::Pre-Invoke::=id"]}"#,
        "invalid input",
    );
    refused(b"", "no signed command");
    refused(b"\xff\xff", "no signed command");
    refused(&vec![b' '; 70 * 1024], "invalid input");
}

#[test]
fn update_refuses_a_command_nobody_signed() {
    let payload = ApplyUpdates::default().encode_to_vec();
    let command = Signed {
        command_id: "c-1".into(),
        kind: APPLY_UPDATES.into(),
        payload,
        expires_at_unix_ms: i64::MAX,
        agent_id: "web-01".into(),
        ..Signed::default()
    };
    // Unsigned, and run here without root's trust anchor: refused either
    // way before the package manager is reached.
    let output = update(&HelperRequest::of(&command).framed());
    assert!(!output.status.success());
    assert!(output.stdout.is_empty(), "something ran");
}
