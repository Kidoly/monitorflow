//! The post-check on a real host: refresh the package manager's metadata
//! from the MonitorFlow sources only, so another source being down does
//! not revert a good change in `add` mode.

use std::io::Read;
use std::process::{Command, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::{apt, Family, PostCheck};

/// Characters of output kept in an error.
const ERROR_TAIL: usize = 2000;

/// Runs `apt-get update` or `dnf makecache` with the new sources. Root
/// only, and only meaningful with `/` as the root.
#[derive(Debug, Clone)]
pub struct SystemCheck {
    pub timeout: Duration,
}

impl Default for SystemCheck {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(300),
        }
    }
}

impl PostCheck for SystemCheck {
    fn check(&self, family: Family) -> Result<(), String> {
        run(command(family), family, self.timeout)
    }
}

fn command(family: Family) -> Command {
    let mut command = match family {
        Family::Apt => {
            let mut command = Command::new("apt-get");
            command.args(["update", "-q"]);
            for option in [
                &format!("Dir::Etc::SourceList={}", apt::SOURCES),
                "Dir::Etc::SourceParts=-",
                // Keeps the lists of the other sources in `add` mode.
                "APT::Get::List-Cleanup=0",
                // apt 2.1+; older versions ignore it, hence the output check.
                "APT::Update::Error-Mode=any",
            ] {
                command.args(["-o", option]);
            }
            command
        }
        Family::Dnf => {
            let mut command = Command::new("dnf");
            command.args([
                "makecache",
                "--refresh",
                "--disablerepo=*",
                "--enablerepo=monitorflow-*",
                "--setopt=skip_if_unavailable=False",
            ]);
            command
        }
    };
    command.env("LC_ALL", "C").stdin(Stdio::null());
    command
}

fn run(mut command: Command, family: Family, timeout: Duration) -> Result<(), String> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run {:?}: {e}", command.get_program()))?;
    let stdout = child.stdout.take().map(drain);
    let stderr = child.stderr.take().map(drain);
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("timed out after {} s", timeout.as_secs()));
            }
            Ok(None) => thread::sleep(Duration::from_millis(100)),
            Err(error) => return Err(error.to_string()),
        }
    };
    let mut output = String::new();
    for reader in [stdout, stderr].into_iter().flatten() {
        output.push_str(&reader.join().unwrap_or_default());
    }
    // apt before 2.1 exits 0 when an index fails to download.
    let warned = family == Family::Apt
        && output
            .lines()
            .any(|line| ["W:", "E:", "Err:"].iter().any(|p| line.starts_with(p)));
    if status.success() && !warned {
        return Ok(());
    }
    let tail: String = {
        let chars: Vec<char> = output.trim_end().chars().collect();
        chars[chars.len().saturating_sub(ERROR_TAIL)..]
            .iter()
            .collect()
    };
    Err(format!("{status}: {tail}"))
}

fn drain(mut pipe: impl Read + Send + 'static) -> JoinHandle<String> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = pipe.read_to_end(&mut bytes);
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> Command {
        let mut command = Command::new("sh");
        command.args(["-c", script]);
        command
    }

    #[test]
    fn succeeds_on_a_clean_exit() {
        assert_eq!(
            run(sh("echo Hit: ok"), Family::Apt, Duration::from_secs(5)),
            Ok(())
        );
    }

    #[test]
    fn fails_on_an_exit_code_with_the_output() {
        let error = run(
            sh("echo boom >&2; exit 1"),
            Family::Dnf,
            Duration::from_secs(5),
        );
        assert!(error.unwrap_err().contains("boom"));
    }

    #[test]
    fn apt_warnings_fail_even_with_exit_zero() {
        let script = "echo 'W: Failed to fetch https://mirror/apt/x/current/InRelease'";
        assert!(run(sh(script), Family::Apt, Duration::from_secs(5)).is_err());
        assert!(run(sh(script), Family::Dnf, Duration::from_secs(5)).is_ok());
    }

    #[test]
    fn a_hung_command_times_out() {
        let error = run(sh("exec sleep 10"), Family::Apt, Duration::from_millis(200));
        assert!(error.unwrap_err().contains("timed out"));
    }

    #[test]
    fn commands_select_the_monitorflow_sources_only() {
        let args = |family| {
            command(family)
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        assert!(args(Family::Apt).contains(&format!("Dir::Etc::SourceList={}", apt::SOURCES)));
        assert!(args(Family::Dnf).contains(&"--enablerepo=monitorflow-*".to_owned()));
    }
}
