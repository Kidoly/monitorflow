//! The `sources` subcommand the agent runs through sudo (ADR-0032). It
//! always works on `/`, so only inputs refused before any change are run:
//! the helper acts on a command the central server signed and on nothing
//! else (ADR-0108). What it checks of a signed command is tested in
//! `helper_trust`.

// The real server and the Linux helpers only build on Unix (ADR-0047).
#![cfg(unix)]

use std::io::Write;
use std::process::{Command, Stdio};

use monitorflow_agent::helper_trust::HelperRequest;
use monitorflow_common::command::CONFIGURE_SOURCES;
use monitorflow_common::proto::v1::{Command as Signed, ConfigureSources};
use prost::Message;

fn sources(verb: &str, stdin: &[u8]) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_monitorflow-agent"))
        .args(["sources", verb])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn apply_refuses_a_spec_without_its_signed_command() {
    // What the agent's user could write on stdin before ADR-0108: the
    // bare spec, with any repository and key it liked.
    let spec = ConfigureSources {
        mirror_base_url: "http://attacker.example".into(),
        ..ConfigureSources::default()
    };
    for stdin in [spec.encode_to_vec(), Vec::new(), b"\xff\xff\xff".to_vec()] {
        let output = sources("apply", &stdin);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty(), "something changed");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("invalid input"), "{stderr}");
    }
}

#[test]
fn revert_needs_a_signed_command_too() {
    let output = sources("revert", b"");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no signed command"), "{stderr}");
}

#[test]
fn apply_refuses_a_command_nobody_signed() {
    let command = Signed {
        command_id: "c-1".into(),
        kind: CONFIGURE_SOURCES.into(),
        payload: ConfigureSources::default().encode_to_vec(),
        expires_at_unix_ms: i64::MAX,
        agent_id: "web-01".into(),
        ..Signed::default()
    };
    // Unsigned, and run here without root's trust anchor: refused either
    // way before any source is touched.
    let output = sources("apply", &HelperRequest::of(&command).framed());
    assert!(!output.status.success());
    assert!(output.stdout.is_empty(), "something changed");
}
