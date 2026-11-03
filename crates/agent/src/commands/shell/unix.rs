//! `shell` on Unix: `/bin/sh -c` in its own process group (ADR-0043).

use std::process::Stdio;
use std::time::Duration;

use super::{not_started, tail, timed_out};
use crate::commands::Output;

/// The shell ADR-0041's risk rules are written for.
const SHELL: &str = "/bin/sh";
/// `PATH` of the command; nothing else is inherited but `LC_ALL=C`.
const PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

pub(super) async fn run(line: &str, timeout: Duration) -> Output {
    let mut command = tokio::process::Command::new(SHELL);
    command
        .arg("-c")
        .arg(line)
        .env_clear()
        .env("PATH", PATH)
        .env("LC_ALL", "C")
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Its own process group, so a timeout also kills a pipeline's
        // other stages and anything they started.
        .process_group(0)
        .kill_on_drop(true);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return not_started(SHELL, error),
    };
    let pid = child.id();
    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");
    let finished = tokio::time::timeout(timeout, async {
        tokio::try_join!(tail(stdout), tail(stderr), child.wait())
    })
    .await;
    match finished {
        Ok(Ok((stdout, stderr, status))) => Output {
            // -1 when killed by a signal.
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
            if let Some(pid) = pid.and_then(|p| rustix::process::Pid::from_raw(p as i32)) {
                // The group may be gone already.
                let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
            }
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

    #[tokio::test]
    async fn runs_pipelines_with_a_clean_environment() {
        let handler = ShellHandler::new(true);
        let output = handler
            .run(&payload("echo $PATH $LC_ALL $HOME | tr a-z A-Z; exit 3", 0))
            .await;
        assert!(!output.rejected);
        assert_eq!(output.exit_code, 3);
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            format!("{} C\n", PATH.to_uppercase())
        );
        let output = handler.run(&payload("ls /nonexistent-dir", 0)).await;
        assert_ne!(output.exit_code, 0);
        assert!(!output.stderr.is_empty());
    }

    #[tokio::test]
    async fn timeouts_kill_the_whole_pipeline() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("late");
        let line = format!("sleep 3 | cat; touch {}", marker.display());
        let started = std::time::Instant::now();
        let output = ShellHandler::new(true).run(&payload(&line, 1)).await;
        assert!(started.elapsed() < Duration::from_secs(3));
        assert_eq!(output.exit_code, -1);
        assert_eq!(output.error.as_deref(), Some("timed out after 1 s"));
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(!marker.exists(), "the shell survived its timeout");
    }

    #[tokio::test]
    async fn keeps_the_end_of_long_output() {
        let output = ShellHandler::new(true)
            .run(&payload("head -c 300000 /dev/zero; echo end", 0))
            .await;
        assert_eq!(output.exit_code, 0);
        assert!(output.stdout.len() <= 2 * MAX_OUTPUT);
        assert!(output.stdout.ends_with(b"end\n"));
    }
}
