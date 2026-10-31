//! The mirror tools sidecar (ADR-0067). The production compose keeps
//! `aptly`, `reposync`, `createrepo_c` and `gpg` out of the server image:
//! they run in the `mirror` container, which shares `mirror_dir` with the
//! server and answers on a Unix socket in it.
//!
//! One connection is one command: a JSON [`Request`] line, then a JSON
//! [`Reply`] line when the command ends. A client that goes away, because
//! the job's process stopped, kills its command. [`ToolsRunner`] uses the
//! socket when it exists and the local programs otherwise, as on a host
//! with the tools installed.

use std::future::Future;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;

use monitorflow_common::config::MirrorConfig;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use super::layout::Layout;
use super::runner::{CommandRunner, Invocation, Output, RunError, Runner, Tool};

/// Longest request line: arguments are names and paths.
const MAX_REQUEST: u64 = 1024 * 1024;

/// The check that the tools are there, which downloads and changes nothing.
pub fn probe() -> Invocation {
    Invocation::new(Tool::Aptly, ["version"])
}

#[derive(Debug, Serialize, Deserialize)]
struct Request {
    invocation: Invocation,
    timeout_s: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Reply {
    Output(Output),
    Error(String),
}

/// Runs each tool in the sidecar, or here without one.
#[derive(Debug, Clone)]
pub struct ToolsRunner {
    socket: PathBuf,
    timeout_s: u64,
    local: CommandRunner,
}

impl ToolsRunner {
    pub fn new(config: &MirrorConfig, layout: &Layout) -> Self {
        Self {
            socket: layout.tools_socket(),
            timeout_s: config.sync_timeout,
            local: CommandRunner::new(config, layout.gnupg()),
        }
    }

    async fn remote(&self, invocation: &Invocation) -> Result<Output, RunError> {
        let line = serde_json::to_string(&Request {
            invocation: invocation.clone(),
            timeout_s: self.timeout_s,
        })
        .map_err(|e| RunError::Tools(e.to_string()))?;
        let stopped =
            |e: io::Error| RunError::Tools(format!("the mirror tools are not running: {e}"));
        let mut stream = UnixStream::connect(&self.socket).await.map_err(stopped)?;
        stream
            .write_all(format!("{line}\n").as_bytes())
            .await
            .map_err(stopped)?;
        let mut reply = String::new();
        BufReader::new(stream)
            .read_line(&mut reply)
            .await
            .map_err(stopped)?;
        if reply.is_empty() {
            return Err(RunError::Tools(
                "the mirror tools stopped during the command".into(),
            ));
        }
        match serde_json::from_str(&reply).map_err(|e| RunError::Tools(e.to_string()))? {
            Reply::Output(output) => Ok(output),
            Reply::Error(error) => Err(RunError::Tools(error)),
        }
    }
}

#[async_trait::async_trait]
impl Runner for ToolsRunner {
    async fn run(&self, invocation: &Invocation) -> Result<Output, RunError> {
        if tokio::fs::try_exists(&self.socket).await.unwrap_or(false) {
            self.remote(invocation).await
        } else {
            self.local.run(invocation).await
        }
    }
}

/// Serves `layout`'s socket with the programs of `config` until `stop`
/// ends. The directory and the socket are for their owner, the user the
/// server and the sidecar share.
pub async fn serve(
    layout: &Layout,
    config: MirrorConfig,
    stop: impl Future<Output = ()>,
) -> io::Result<()> {
    let socket = layout.tools_socket();
    let dir = socket.parent().expect("under mirror_dir").to_owned();
    tokio::fs::create_dir_all(&dir).await?;
    tokio::fs::set_permissions(&dir, PermissionsExt::from_mode(0o700)).await?;
    // Left by a container that was killed.
    let _ = tokio::fs::remove_file(&socket).await;
    let listener = UnixListener::bind(&socket)?;
    tracing::info!(socket = %socket.display(), "mirror tools ready");
    let runner = CommandRunner::new(&config, layout.gnupg());
    tokio::pin!(stop);
    loop {
        tokio::select! {
            _ = &mut stop => break,
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                tokio::spawn(answer(stream, runner.clone()));
            }
        }
    }
    let _ = tokio::fs::remove_file(&socket).await;
    Ok(())
}

async fn answer(stream: UnixStream, runner: CommandRunner) {
    let (read, mut write) = stream.into_split();
    let mut read = BufReader::new(read.take(MAX_REQUEST));
    let mut line = String::new();
    if let Err(error) = read.read_line(&mut line).await {
        tracing::warn!(%error, "cannot read a mirror tools request");
        return;
    }
    let reply = match serde_json::from_str::<Request>(&line) {
        Err(error) => Reply::Error(format!("bad request: {error}")),
        Ok(Request {
            invocation,
            timeout_s,
        }) => {
            let runner = runner.with_timeout(Duration::from_secs(timeout_s));
            tokio::select! {
                result = runner.run(&invocation) => match result {
                    Ok(output) => Reply::Output(output),
                    Err(error) => Reply::Error(error.to_string()),
                },
                // Dropping the command kills it.
                () = closed(read) => {
                    tracing::info!(tool = invocation.tool.name(), "client left, command killed");
                    return;
                }
            }
        }
    };
    let bytes = serde_json::to_vec(&reply).expect("replies serialize");
    if write.write_all(&bytes).await.is_ok() {
        let _ = write.write_all(b"\n").await;
    }
}

/// Ends when the client closes the connection: it sends nothing after its
/// request.
async fn closed(mut read: impl AsyncRead + Unpin) {
    let mut byte = [0];
    while matches!(read.read(&mut byte).await, Ok(n) if n > 0) {}
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    /// A sidecar on a temporary `mirror_dir` whose tools are scripts.
    async fn sidecar(dir: &Path, aptly: &str) -> ToolsRunner {
        let script = dir.join("fake-aptly");
        std::fs::write(&script, format!("#!/bin/sh\n{aptly}\n")).unwrap();
        std::fs::set_permissions(&script, PermissionsExt::from_mode(0o755)).unwrap();
        let config = MirrorConfig {
            aptly: script,
            ..MirrorConfig::default()
        };
        let layout = Layout::new(dir);
        let server = config.clone();
        let served = layout.clone();
        tokio::spawn(async move {
            serve(&served, server, std::future::pending())
                .await
                .unwrap();
        });
        for _ in 0..500 {
            if layout.tools_socket().exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // The client's own programs must not be what runs.
        let client = MirrorConfig {
            aptly: "/nonexistent".into(),
            ..MirrorConfig::default()
        };
        ToolsRunner::new(&client, &layout)
    }

    #[tokio::test]
    async fn commands_run_in_the_sidecar_with_their_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let runner = sidecar(dir.path(), r#"echo "args: $*"; echo oops >&2; exit 3"#).await;
        let output = runner
            .run(&Invocation::new(Tool::Aptly, ["mirror", "a b"]))
            .await
            .unwrap();
        assert!(!output.success);
        assert_eq!(output.stdout, "args: mirror a b\n");
        assert_eq!(output.stderr, "oops\n");
    }

    #[tokio::test]
    async fn a_client_that_leaves_kills_its_command() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("finished");
        let runner = sidecar(dir.path(), &format!("sleep 2; touch {}", marker.display())).await;
        let invocation = Invocation::new(Tool::Aptly, ["slow"]);
        let cut = tokio::time::timeout(Duration::from_millis(300), runner.run(&invocation));
        assert!(cut.await.is_err());
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert!(!marker.exists());
    }

    #[tokio::test]
    async fn without_a_sidecar_the_local_programs_run() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("local-aptly");
        std::fs::write(&script, "#!/bin/sh\necho local\n").unwrap();
        std::fs::set_permissions(&script, PermissionsExt::from_mode(0o755)).unwrap();
        let config = MirrorConfig {
            aptly: script,
            ..MirrorConfig::default()
        };
        let runner = ToolsRunner::new(&config, &Layout::new(dir.path()));
        assert_eq!(runner.run(&probe()).await.unwrap().stdout, "local\n");
    }

    #[tokio::test]
    async fn a_sidecar_that_is_gone_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        let socket = layout.tools_socket();
        std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
        drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());
        let runner = ToolsRunner::new(&MirrorConfig::default(), &layout);
        let error = runner.run(&probe()).await.unwrap_err();
        assert!(error.to_string().contains("not running"), "{error}");
    }
}
