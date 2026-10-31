//! What the updater does to the Docker host, behind [`Host`] so the update
//! logic is tested without Docker. [`Compose`] runs the Docker CLI with
//! fixed arguments: image references are checked digests and paths are
//! the updater's own.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use monitorflow_common::release::Version;
use monitorflow_common::server_update::ImageRef;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::project::Project;

/// The PostgreSQL role and database of the compose file (ADR-0051).
const DB: &str = "monitorflow";
/// Services an update recreates.
pub const SERVICES: [&str; 2] = ["server", "web"];
/// Recreated with them when the compose file has it: one from before
/// ADR-0067 does not.
pub const MIRROR_SERVICE: &str = "mirror";
/// Where the update volume is mounted in the server container too.
pub const SERVER_UPDATE_DIR: &str = "/var/lib/monitorflow/update";

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct HostError(pub String);

#[async_trait]
pub trait Host: Send + Sync {
    /// What `monitorflow-server --version` prints in the running server.
    async fn server_version(&self) -> Result<Version, HostError>;
    /// Pulls `image` unless it is already there, as after `docker load`.
    async fn pull(&self, image: &ImageRef) -> Result<(), HostError>;
    /// The latest migration applied to the database, 0 for none.
    async fn latest_migration(&self) -> Result<i64, HostError>;
    /// Writes a `pg_dump -Fc` of the database to `dest`.
    async fn dump_database(&self, dest: &Path) -> Result<(), HostError>;
    /// Recreates `server`, `web` and `mirror` with the images of `.env` and waits
    /// until they are healthy.
    async fn recreate(&self) -> Result<(), HostError>;
    /// `monitorflow-server agent-release import` of `dir`, a path of the
    /// update volume as the server container sees it.
    async fn import_agent_release(&self, dir: &Path) -> Result<(), HostError>;
    /// Recreates the `updater` service from a one-off container of its new
    /// image, which outlives this one.
    async fn replace_updater(&self) -> Result<(), HostError>;
    /// What `monitorflow-server --version` prints in the server image of
    /// `.env`, whether the server runs or not.
    async fn image_version(&self) -> Result<Version, HostError>;
    /// Stops `server` and `web`.
    async fn stop_server(&self) -> Result<(), HostError>;
    /// Starts PostgreSQL if needed, creates the database again and loads
    /// the `pg_dump -Fc` at `dump` into it.
    async fn restore_database(&self, dump: &Path) -> Result<(), HostError>;
    /// Starts every service and waits until they are healthy.
    async fn start_all(&self) -> Result<(), HostError>;
}

/// The Docker CLI and its compose plugin, on the project of [`Project`].
#[derive(Debug, Clone)]
pub struct Compose {
    pub docker: PathBuf,
    pub project: Project,
    /// `--wait-timeout` of the recreation.
    pub timeout: Duration,
}

impl Compose {
    fn command(&self, compose: bool, args: &[&str]) -> Command {
        let mut command = Command::new(&self.docker);
        // Compose interpolates the shell's variables before `.env`: only
        // `.env` must decide the images.
        command.env_clear();
        for key in ["PATH", "HOME", "DOCKER_HOST", "DOCKER_CONFIG"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        if compose {
            command.args(self.project.compose_args());
        }
        command.args(args).stdin(Stdio::null()).kill_on_drop(true);
        command
    }

    async fn output(&self, compose: bool, args: &[&str]) -> Result<String, HostError> {
        let output = self
            .command(compose, args)
            .output()
            .await
            .map_err(|e| HostError(format!("cannot run docker: {e}")))?;
        if !output.status.success() {
            return Err(HostError(failure(args, &output.stderr)));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// `compose exec -T` in `service`.
    async fn exec(&self, service: &str, args: &[&str]) -> Result<String, HostError> {
        let mut all = vec!["exec", "-T", service];
        all.extend_from_slice(args);
        self.output(true, &all).await
    }

    /// `compose exec -T postgres` of `args`, with `stdin` from a file.
    async fn exec_with_input(&self, args: &[&str], stdin: &Path) -> Result<(), HostError> {
        let file = std::fs::File::open(stdin)
            .map_err(|e| HostError(format!("{}: {e}", stdin.display())))?;
        let mut all = vec!["exec", "-T", "postgres"];
        all.extend_from_slice(args);
        let output = self
            .command(true, &all)
            .stdin(file)
            .output()
            .await
            .map_err(|e| HostError(format!("cannot run docker: {e}")))?;
        if !output.status.success() {
            return Err(HostError(failure(&all, &output.stderr)));
        }
        Ok(())
    }

    fn up_args(&self, mirror: bool) -> Vec<String> {
        let mut args: Vec<String> = [
            "up",
            "--detach",
            "--no-deps",
            "--no-build",
            "--pull",
            "never",
            "--wait",
            "--wait-timeout",
        ]
        .map(String::from)
        .into();
        args.push(self.timeout.as_secs().to_string());
        args.extend(SERVICES.map(String::from));
        if mirror {
            args.push(MIRROR_SERVICE.into());
        }
        args
    }
}

/// The end of a failed command's error output, for the status.
fn failure(args: &[&str], stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let tail = lines[lines.len().saturating_sub(5)..].join(" / ");
    let what = args.iter().take(3).copied().collect::<Vec<_>>().join(" ");
    format!("docker {what} failed: {tail}")
}

/// The version in `monitorflow-server 0.2.0`.
pub fn parse_version(output: &str) -> Result<Version, HostError> {
    output
        .trim()
        .strip_prefix("monitorflow-server ")
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| HostError(format!("unexpected version output `{}`", output.trim())))
}

#[async_trait]
impl Host for Compose {
    async fn server_version(&self) -> Result<Version, HostError> {
        parse_version(
            &self
                .exec("server", &["monitorflow-server", "--version"])
                .await?,
        )
    }

    async fn pull(&self, image: &ImageRef) -> Result<(), HostError> {
        let present = self
            .output(
                false,
                &["image", "inspect", "--format", "{{.Id}}", image.as_str()],
            )
            .await
            .is_ok();
        if !present {
            self.output(false, &["pull", "--quiet", image.as_str()])
                .await?;
        }
        Ok(())
    }

    async fn latest_migration(&self) -> Result<i64, HostError> {
        let out = self
            .exec(
                "postgres",
                &[
                    "psql",
                    "-U",
                    DB,
                    "-d",
                    DB,
                    "-tAc",
                    "SELECT coalesce(max(version), 0) FROM _sqlx_migrations",
                ],
            )
            .await?;
        out.trim()
            .parse()
            .map_err(|_| HostError(format!("unexpected migration `{}`", out.trim())))
    }

    async fn dump_database(&self, dest: &Path) -> Result<(), HostError> {
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dest)
            .await
            .map_err(|e| HostError(format!("{}: {e}", dest.display())))?;
        let args = [
            "exec", "-T", "postgres", "pg_dump", "-U", DB, "-d", DB, "-Fc",
        ];
        let mut child = self
            .command(true, &args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| HostError(format!("cannot run docker: {e}")))?;
        let mut stdout = child.stdout.take().expect("piped");
        let copied = tokio::io::copy(&mut stdout, &mut file).await;
        let output = child
            .wait_with_output()
            .await
            .map_err(|e| HostError(e.to_string()))?;
        let copied = copied.map_err(|e| HostError(format!("{}: {e}", dest.display())))?;
        if !output.status.success() {
            return Err(HostError(failure(&args, &output.stderr)));
        }
        if copied == 0 {
            return Err(HostError("pg_dump wrote nothing".into()));
        }
        file.flush().await.map_err(|e| HostError(e.to_string()))?;
        file.sync_all().await.map_err(|e| HostError(e.to_string()))
    }

    async fn recreate(&self) -> Result<(), HostError> {
        let services = self.output(true, &["config", "--services"]).await?;
        let args = self.up_args(services.lines().any(|s| s == MIRROR_SERVICE));
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        self.output(true, &args).await.map(drop)
    }

    async fn import_agent_release(&self, dir: &Path) -> Result<(), HostError> {
        let dir = dir.display().to_string();
        self.exec(
            "server",
            &[
                "monitorflow-server",
                "agent-release",
                "import",
                "--dir",
                &dir,
            ],
        )
        .await
        .map(drop)
    }

    async fn replace_updater(&self) -> Result<(), HostError> {
        self.output(
            true,
            &[
                "run",
                "--detach",
                "--rm",
                "--no-deps",
                "updater",
                "replace-self",
            ],
        )
        .await
        .map(drop)
    }

    async fn image_version(&self) -> Result<Version, HostError> {
        parse_version(
            &self
                .output(
                    true,
                    &[
                        "run",
                        "--rm",
                        "--no-deps",
                        "-T",
                        "--pull",
                        "never",
                        "--entrypoint",
                        "monitorflow-server",
                        "server",
                        "--version",
                    ],
                )
                .await?,
        )
    }

    async fn stop_server(&self) -> Result<(), HostError> {
        let mut args = vec!["stop"];
        args.extend(SERVICES);
        self.output(true, &args).await.map(drop)
    }

    async fn restore_database(&self, dump: &Path) -> Result<(), HostError> {
        self.output(
            true,
            &[
                "up",
                "--detach",
                "--no-build",
                "--pull",
                "never",
                "--wait",
                "postgres",
            ],
        )
        .await?;
        let recreate = [
            "psql",
            "-U",
            DB,
            "-d",
            "postgres",
            "-v",
            "ON_ERROR_STOP=1",
            "-c",
            "DROP DATABASE IF EXISTS monitorflow WITH (FORCE)",
            "-c",
            "CREATE DATABASE monitorflow OWNER monitorflow",
        ];
        self.exec("postgres", &recreate).await?;
        self.exec_with_input(
            &[
                "pg_restore",
                "-U",
                DB,
                "-d",
                DB,
                "--no-owner",
                "--exit-on-error",
                "--single-transaction",
            ],
            dump,
        )
        .await
    }

    async fn start_all(&self) -> Result<(), HostError> {
        let timeout = self.timeout.as_secs().to_string();
        self.output(
            true,
            &[
                "up",
                "--detach",
                "--no-build",
                "--pull",
                "never",
                "--wait",
                "--wait-timeout",
                &timeout,
            ],
        )
        .await
        .map(drop)
    }
}

/// What `replace-self` runs, from the one-off container.
pub async fn recreate_updater(compose: &Compose) -> Result<(), HostError> {
    compose
        .output(
            true,
            &[
                "up",
                "--detach",
                "--no-deps",
                "--no-build",
                "--pull",
                "never",
                "updater",
            ],
        )
        .await
        .map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_and_failures_are_read() {
        assert_eq!(
            parse_version("monitorflow-server 0.2.0-rc.1\n")
                .unwrap()
                .to_string(),
            "0.2.0-rc.1"
        );
        assert!(parse_version("bash: not found").is_err());
        let message = failure(&["up", "--detach", "--no-deps", "x"], b"a\n\nb\nc\n");
        assert_eq!(message, "docker up --detach --no-deps failed: a / b / c");
    }

    #[test]
    fn recreation_waits_for_server_and_web_without_building_or_pulling() {
        let compose = Compose {
            docker: "docker".into(),
            project: crate::project::Project {
                name: "p".into(),
                working_dir: "/d".into(),
                config_files: vec!["/d/c.yml".into()],
                env_file: "/d/.env".into(),
                mount: "/deploy".into(),
            },
            timeout: Duration::from_secs(300),
        };
        assert_eq!(
            compose.up_args(false).join(" "),
            "up --detach --no-deps --no-build --pull never --wait --wait-timeout 300 server web"
        );
        assert!(compose
            .up_args(true)
            .join(" ")
            .ends_with(" server web mirror"));
    }
}
