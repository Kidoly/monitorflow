//! `shell` on Windows: Windows PowerShell 5.1 in a job object
//! (ADR-0084).
//!
//! The line goes to `-EncodedCommand`, UTF-16LE in base64, so no layer of
//! Windows command-line quoting can change it: PowerShell parses exactly
//! the text the server classified. A short prelude switches output to
//! UTF-8 and silences progress bars; an epilogue turns the line's outcome
//! into the exit code.

use std::process::Stdio;
use std::time::Duration;

use base64::Engine;
use monitorflow_sensors::powershell::{self, Job};

use super::{not_started, tail, timed_out};
use crate::commands::Output;

/// Before the line: UTF-8 output without a byte order mark, for
/// PowerShell's own output and for what it decodes from programs, and no
/// progress records, which would go to stderr.
const PRELUDE: &str = "$ProgressPreference = 'SilentlyContinue'\n\
     try { [Console]::OutputEncoding = [Text.UTF8Encoding]::new($false) } catch {}\n\
     $OutputEncoding = [Text.UTF8Encoding]::new($false)\n";
/// After the line: a program's own exit code, else 1 when the line
/// failed, else 0.
const EPILOGUE: &str = "\n$ok = $?\n\
     if ($LASTEXITCODE) { exit $LASTEXITCODE }\n\
     if (-not $ok) { exit 1 }\n\
     exit 0\n";

/// The script PowerShell runs for `line`, as `-EncodedCommand` takes it.
fn encoded(line: &str) -> String {
    let script = format!("{PRELUDE}{line}{EPILOGUE}");
    let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64::engine::general_purpose::STANDARD.encode(utf16)
}

pub(super) async fn run(line: &str, timeout: Duration) -> Output {
    let exe = match powershell::system_dir() {
        Ok(system) => powershell::exe(&system),
        Err(error) => return not_started("powershell.exe", error),
    };
    let job = match Job::new() {
        Ok(job) => job,
        Err(error) => return not_started("powershell.exe (job object)", error),
    };
    let spawned = powershell::command(&exe).and_then(|mut command| {
        command
            .args(powershell::OPTIONS)
            .arg("-EncodedCommand")
            .arg(encoded(line))
            .stderr(Stdio::piped())
            .spawn()
    });
    let mut child = match spawned {
        Ok(child) => child,
        Err(error) => return not_started("powershell.exe", error),
    };
    if let Err(error) = job.assign(&mut child) {
        return not_started("powershell.exe (job object)", error);
    }
    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");
    let finished = tokio::time::timeout(timeout, async {
        tokio::try_join!(tail(stdout), tail(stderr), child.wait())
    })
    .await;
    // Ends whatever the line started, now and not only at a timeout:
    // nothing it ran outlives the command.
    drop(job);
    match finished {
        Ok(Ok((stdout, stderr, status))) => Output {
            exit_code: status.code().unwrap_or(-1),
            stdout,
            stderr,
            error: None,
            rejected: false,
        },
        Ok(Err(error)) => Output {
            exit_code: -1,
            error: Some(format!("reading the command's output: {error}")),
            ..Output::default()
        },
        Err(_) => {
            let _ = child.kill().await;
            timed_out(timeout)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::payload;
    use super::super::ShellHandler;
    use super::*;
    use crate::commands::{Handler, MAX_OUTPUT};

    fn text(bytes: &[u8]) -> String {
        String::from_utf8(bytes.to_vec()).expect("UTF-8 output")
    }

    #[tokio::test]
    async fn runs_powershell_with_a_clean_environment() {
        let handler = ShellHandler::new(true);
        let output = handler
            .run(&payload(
                "Write-Output \"$env:PATH|$env:USERNAME|$((Get-Location).Path)|café\"",
                0,
            ))
            .await;
        assert!(!output.rejected, "{output:?}");
        assert_eq!(output.exit_code, 0, "{output:?}");
        let system = powershell::system_dir().unwrap();
        let expected = format!(
            "{}||{}|café",
            powershell::path(&system).to_string_lossy(),
            system.ancestors().last().unwrap().display()
        );
        assert_eq!(text(&output.stdout).trim_end(), expected);
    }

    #[tokio::test]
    async fn exit_codes_follow_the_line() {
        let handler = ShellHandler::new(true);
        let output = handler.run(&payload("cmd.exe /c exit 3", 0)).await;
        assert_eq!(output.exit_code, 3, "{output:?}");
        let output = handler
            .run(&payload("Get-Service -Name MonitorFlowNoSuchService", 0))
            .await;
        assert_eq!(output.exit_code, 1, "{output:?}");
        assert!(text(&output.stderr).contains("MonitorFlowNoSuchService"));
        let output = handler
            .run(&payload("Get-Service | Select-Object -First 1", 0))
            .await;
        assert_eq!(output.exit_code, 0, "{output:?}");
        assert!(!output.stdout.is_empty());
        // A parse error runs nothing.
        let output = handler.run(&payload("Write-Output 'x' }", 0)).await;
        assert_eq!(output.exit_code, 1, "{output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
    }

    #[tokio::test]
    async fn timeouts_kill_the_whole_process_tree() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("late");
        // A grandchild that would write the marker after the timeout.
        let line = format!(
            "powershell.exe -NoProfile -Command \"Start-Sleep -Seconds 4; Set-Content -Path '{}' -Value late\"",
            marker.display()
        );
        let started = std::time::Instant::now();
        let output = ShellHandler::new(true).run(&payload(&line, 2)).await;
        assert!(started.elapsed() < Duration::from_secs(4), "{output:?}");
        assert_eq!(output.exit_code, -1);
        assert_eq!(output.error.as_deref(), Some("timed out after 2 s"));
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert!(!marker.exists(), "a process survived the timeout");
    }

    #[tokio::test]
    async fn keeps_the_end_of_long_output() {
        let output = ShellHandler::new(true)
            .run(&payload("Write-Output ('x' * 300000); Write-Output end", 0))
            .await;
        assert_eq!(output.exit_code, 0, "{output:?}");
        assert!(output.stdout.len() <= 2 * MAX_OUTPUT);
        assert!(text(&output.stdout).trim_end().ends_with("end"));
    }

    #[test]
    fn the_line_is_encoded_whole() {
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded("Get-Service 'a\"b'"))
            .unwrap();
        let units: Vec<u16> = decoded
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let script = String::from_utf16(&units).unwrap();
        assert!(script.contains("\nGet-Service 'a\"b'\n"));
        assert!(script.starts_with(PRELUDE) && script.ends_with(EPILOGUE));
    }
}
