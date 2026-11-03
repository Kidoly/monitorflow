//! The real server and relays, run as child processes the way an operator
//! runs them: a configuration file, `user add` for the administrator, then
//! the REST API for sites and tokens.

use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{bail, Context};
use rustix::process::{kill_process, Pid, Signal};
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};

/// Where the server stores data and how it is reached.
#[derive(Debug, Clone)]
pub struct ServerPorts {
    pub grpc: SocketAddr,
    pub enroll: SocketAddr,
    pub api: SocketAddr,
    pub mirror: SocketAddr,
}

impl ServerPorts {
    pub fn free() -> anyhow::Result<Self> {
        Ok(Self {
            grpc: free_addr()?,
            enroll: free_addr()?,
            api: free_addr()?,
            mirror: free_addr()?,
        })
    }

    pub fn grpc_url(&self) -> String {
        format!("https://{}", self.grpc)
    }

    pub fn enroll_url(&self) -> String {
        format!("https://{}", self.enroll)
    }

    pub fn api_url(&self) -> String {
        format!("http://{}", self.api)
    }
}

/// A free loopback port. Another process could take it before we bind it,
/// which nothing else on a test host does in practice.
pub fn free_addr() -> anyhow::Result<SocketAddr> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?)
}

/// A child process with its output in a log file.
pub struct Process {
    name: String,
    child: Child,
}

impl Process {
    fn spawn(name: &str, mut command: Command, log: &Path) -> anyhow::Result<Self> {
        let out = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
            .with_context(|| format!("opening {}", log.display()))?;
        let child = command
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .stdout(out.try_clone()?)
            .stderr(out)
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("starting the {name}"))?;
        Ok(Self {
            name: name.to_owned(),
            child,
        })
    }

    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    /// SIGTERM, then SIGKILL after `grace`.
    pub async fn stop(&mut self, grace: Duration) -> anyhow::Result<()> {
        if let Some(pid) = self.pid().and_then(|p| Pid::from_raw(p as i32)) {
            let _ = kill_process(pid, Signal::TERM);
        }
        match tokio::time::timeout(grace, self.child.wait()).await {
            Ok(status) => {
                let status = status?;
                if !status.success() {
                    tracing::warn!(name = %self.name, %status, "stopped with an error status");
                }
            }
            Err(_) => {
                tracing::warn!(name = %self.name, "did not stop in time; killing it");
                self.child.kill().await?;
            }
        }
        Ok(())
    }

    pub fn check_running(&mut self) -> anyhow::Result<()> {
        if let Some(status) = self.child.try_wait()? {
            bail!("the {} exited: {status}", self.name);
        }
        Ok(())
    }
}

/// The central server under test.
pub struct Server {
    pub binary: PathBuf,
    pub config: PathBuf,
    pub ports: ServerPorts,
    pub log: PathBuf,
    pub process: Option<Process>,
}

impl Server {
    /// Writes the configuration in `dir`.
    pub fn prepare(
        binary: &Path,
        dir: &Path,
        database_url: &str,
        victoriametrics_url: &str,
        victorialogs_url: &str,
        storage_tls_dir: Option<&Path>,
    ) -> anyhow::Result<Self> {
        let ports = ServerPorts::free()?;
        let storage_tls = storage_tls_dir
            .map(|dir| format!("storage_tls_dir = \"{}\"\n", dir.display()))
            .unwrap_or_default();
        let data = dir.join("server");
        std::fs::create_dir_all(&data)?;
        let config = dir.join("server.toml");
        std::fs::write(
            &config,
            format!(
                r#"grpc_listen_addr = "{grpc}"
enroll_listen_addr = "{enroll}"
api_listen_addr = "{api}"
mirror_listen_addr = "{mirror}"
api_cookie_secure = false
database_url = "{database_url}"
victoriametrics_url = "{victoriametrics_url}"
victorialogs_url = "{victorialogs_url}"
{storage_tls}data_dir = "{data}"
server_names = ["127.0.0.1"]

[tls]
ca_cert = "{data}/tls/ca.pem"
cert = "{data}/tls/cert.pem"
key = "{data}/tls/key.pem"

[auto_update]
enabled = false
"#,
                grpc = ports.grpc,
                enroll = ports.enroll,
                api = ports.api,
                mirror = ports.mirror,
                data = data.display(),
            ),
        )?;
        Ok(Self {
            binary: binary.to_owned(),
            config,
            ports,
            log: dir.join("server.log"),
            process: None,
        })
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.binary);
        command.arg("--config").arg(&self.config);
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("MONITORFLOW_") {
                command.env_remove(key);
            }
        }
        command
    }

    /// Starts the server and waits until its API says it is ready.
    pub async fn start(&mut self) -> anyhow::Result<()> {
        let process = Process::spawn("server", self.command(), &self.log)?;
        self.process = Some(process);
        let ready = format!("{}/api/health/ready", self.ports.api_url());
        let client = reqwest::Client::new();
        for _ in 0..600 {
            if let Some(process) = &mut self.process {
                process.check_running()?;
            }
            if let Ok(response) = client.get(&ready).send().await {
                if response.status().is_success() {
                    return Ok(());
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        bail!(
            "the server was not ready within a minute; see {}",
            self.log.display()
        )
    }

    pub async fn stop(&mut self) -> anyhow::Result<()> {
        if let Some(mut process) = self.process.take() {
            process.stop(Duration::from_secs(30)).await?;
        }
        Ok(())
    }

    pub fn pid(&self) -> Option<u32> {
        self.process.as_ref().and_then(Process::pid)
    }

    /// `user add`: an administrator of everything, password on stdin.
    pub async fn add_admin(&self, username: &str, password: &str) -> anyhow::Result<()> {
        let mut child = self
            .command()
            .args(["user", "add", "--username", username, "--role", "admin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("starting user add")?;
        let mut stdin = child.stdin.take().context("user add stdin")?;
        stdin.write_all(format!("{password}\n").as_bytes()).await?;
        drop(stdin);
        let output = child.wait_with_output().await?;
        if !output.status.success() {
            bail!(
                "user add failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(())
    }
}

/// A relay, enrolled with a single-use token.
pub struct Relay {
    pub listen: SocketAddr,
    pub process: Process,
}

impl Relay {
    pub async fn start(
        binary: &Path,
        dir: &Path,
        id: &str,
        token: &str,
        server: &ServerPorts,
    ) -> anyhow::Result<Self> {
        let listen = free_addr()?;
        let data = dir.join(id);
        std::fs::create_dir_all(&data)?;
        let config = dir.join(format!("{id}.toml"));
        std::fs::write(
            &config,
            format!(
                r#"listen_addr = "{listen}"
upstream_url = "{upstream}"
enroll_url = "{enroll}"
data_dir = "{data}"
enrollment_token = "{token}"
mirror_listen_addr = "{mirror}"
"#,
                upstream = server.grpc_url(),
                enroll = server.enroll_url(),
                data = data.display(),
                mirror = free_addr()?,
            ),
        )?;
        let mut command = Command::new(binary);
        command.arg("--config").arg(&config);
        let process = Process::spawn(id, command, &dir.join(format!("{id}.log")))?;
        Ok(Self { listen, process })
    }

    pub fn url(&self) -> String {
        format!("https://{}", self.listen)
    }
}
