//! Runs package tools: privileged ones through `sudo -n` with exactly the
//! arguments listed in ADR-0031, so sudoers rules can name them, and reads
//! as the agent's own user.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use super::PackageError;

/// Time allowed to an unprivileged read like `dpkg-query`.
pub const READ_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Where the tools are. [`Default`] is the real system; tests point them at
/// fakes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tools {
    pub sudo: PathBuf,
    pub apt_get: PathBuf,
    pub dpkg_query: PathBuf,
    pub dnf: PathBuf,
    pub rpm: PathBuf,
    pub shutdown: PathBuf,
    /// Exists when a Debian-family host needs a reboot.
    pub reboot_required_file: PathBuf,
}

impl Default for Tools {
    fn default() -> Self {
        Self {
            sudo: "/usr/bin/sudo".into(),
            apt_get: "/usr/bin/apt-get".into(),
            dpkg_query: "/usr/bin/dpkg-query".into(),
            dnf: "/usr/bin/dnf".into(),
            rpm: "/usr/bin/rpm".into(),
            shutdown: "/usr/sbin/shutdown".into(),
            reboot_required_file: "/var/run/reboot-required".into(),
        }
    }
}

/// What a finished process left.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Run {
    /// The command line, for the command's output.
    pub line: String,
    /// -1 when killed by a signal.
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Run {
    pub fn success(&self) -> bool {
        self.code == 0
    }
}

impl Tools {
    /// Runs `program args` as root: `sudo -n [env…] program args`. Sudo
    /// passes `env` only where sudoers keeps those variables. No timeout:
    /// the command dispatcher bounds the whole command, and dropping the
    /// future kills the process.
    pub async fn privileged(
        &self,
        env: &[(&str, &str)],
        program: &Path,
        args: &[&str],
    ) -> Result<Run, PackageError> {
        let mut argv: Vec<String> = vec!["-n".into()];
        argv.extend(env.iter().map(|(k, v)| format!("{k}={v}")));
        argv.push(program.display().to_string());
        argv.extend(args.iter().map(|a| a.to_string()));
        let line = format!("sudo {}", argv.join(" "));
        run(&self.sudo, &argv, &[], line, None).await
    }

    /// Runs `program args` as the agent's user, in the C locale, within
    /// [`READ_TIMEOUT`].
    pub async fn read(&self, program: &Path, args: &[&str]) -> Result<Run, PackageError> {
        let line = format!("{} {}", program.display(), args.join(" "));
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        run(program, &args, &[("LC_ALL", "C")], line, Some(READ_TIMEOUT)).await
    }
}

async fn run(
    program: &Path,
    args: &[String],
    env: &[(&str, &str)],
    line: String,
    limit: Option<Duration>,
) -> Result<Run, PackageError> {
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .envs(env.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let child = command
        .spawn()
        .map_err(|e| PackageError::Spawn(program.display().to_string(), e.to_string()))?;
    let output = child.wait_with_output();
    let output = match limit {
        Some(limit) => tokio::time::timeout(limit, output)
            .await
            .map_err(|_| PackageError::Timeout(line.clone()))?,
        None => output.await,
    }
    .map_err(|e| PackageError::Spawn(program.display().to_string(), e.to_string()))?;
    Ok(Run {
        line,
        code: output.status.code().unwrap_or(-1),
        stdout: output.stdout,
        stderr: output.stderr,
    })
}
