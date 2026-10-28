//! The `self_update` command (ADR-0053), as the agent's user: downloads
//! the release from the first URL that serves the version asked for and
//! checks it, then hands it to an [`Installer`]: the root helper through
//! sudo on Linux, the update service on Windows. Once the restart is
//! scheduled the agent is about to be stopped: the handler waits, and the
//! command's result comes from the status file (see
//! [`super::StatusResumer`]).

use std::str::FromStr;
use std::time::Duration;

use base64::Engine;
use monitorflow_common::proto::v1::SelfUpdate;
use monitorflow_common::release::{
    self, check_executable, check_upgrade, Platform, PublicKey, Version, SIGNATURE, SUMS,
    VERSION_FILE,
};
use prost::Message;
use reqwest::Url;

use super::{health_timeout, Header, Product, AGENT, RESTART_ALLOWANCE};
use crate::commands::{Handler, Output};

/// Most URLs in one command.
pub const MAX_URLS: usize = 8;
const MAX_URL_LEN: usize = 2048;
/// Largest small file (sums, signature, version) downloaded.
const MAX_SMALL: usize = 64 * 1024;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// A release downloaded and checked.
struct Downloaded {
    sums: Vec<u8>,
    signature: Vec<u8>,
    version_file: Vec<u8>,
    binary: Vec<u8>,
}

/// What the privileged side did with a release.
#[derive(Debug)]
pub enum Installed {
    /// Installed; the restart is scheduled. The text goes to the output.
    Restarting(String),
    /// Nothing to do, as the privileged side found.
    Current(String),
}

/// Hands a checked release to the side that may replace the binary.
#[async_trait::async_trait]
pub trait Installer: Send + Sync + 'static {
    /// `Err` is the command's failed output.
    async fn install(&self, header: &Header, binary: &[u8]) -> Result<Installed, Output>;
    /// Where to look when the restart never comes.
    fn log_hint(&self) -> &'static str;
}

pub struct SelfUpdateHandler {
    installer: Box<dyn Installer>,
    keys: Vec<PublicKey>,
    current: Version,
    platform: Option<Platform>,
    product: Product,
    client: reqwest::Client,
}

impl SelfUpdateHandler {
    /// With the keys and version of this build.
    pub fn new(installer: impl Installer) -> Self {
        let keys = release::trusted_keys()
            .map(<[_]>::to_vec)
            .unwrap_or_default();
        let current = Version::from_str(monitorflow_common::VERSION)
            .unwrap_or_else(|_| Version::from_str("0.0.0").expect("valid"));
        Self::with(installer, keys, current, Platform::current())
    }

    pub fn with(
        installer: impl Installer,
        keys: Vec<PublicKey>,
        current: Version,
        platform: Option<Platform>,
    ) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(DOWNLOAD_TIMEOUT)
            .user_agent(concat!("monitorflow-agent/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("a client with default TLS settings builds");
        Self {
            installer: Box::new(installer),
            keys,
            current,
            platform,
            product: AGENT,
            client,
        }
    }

    /// Updates `product` instead of the agent: the relay (ADR-0055).
    pub fn for_product(mut self, product: Product) -> Self {
        self.product = product;
        self
    }

    async fn get(&self, url: &Url, limit: usize) -> Result<Vec<u8>, String> {
        let mut response = self
            .client
            .get(url.clone())
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| format!("{url}: {}", chain(&e)))?;
        if response.content_length().is_some_and(|n| n > limit as u64) {
            return Err(format!("{url}: larger than {limit} bytes"));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| format!("{url}: {}", chain(&e)))?
        {
            if body.len() + chunk.len() > limit {
                return Err(format!("{url}: larger than {limit} bytes"));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    /// The release at `base`, checked: signed, of version `target`, with a
    /// binary for this platform.
    async fn download(
        &self,
        base: &Url,
        target: &Version,
        platform: Platform,
    ) -> Result<Downloaded, String> {
        let file = |name: &str| base.join(name).map_err(|e| format!("{base}: {e}"));
        let sums = self.get(&file(SUMS)?, release::MAX_SUMS).await?;
        let signature = self.get(&file(SIGNATURE)?, MAX_SMALL).await?;
        let version_file = self.get(&file(VERSION_FILE)?, MAX_SMALL).await?;
        let verified = release::verify(&sums, &signature, &version_file, &self.keys)
            .map_err(|e| format!("{base}: {e}"))?;
        if &verified.version != target {
            return Err(format!("{base}: serves {}, not {target}", verified.version));
        }
        let asset = self
            .product
            .asset(platform)
            .ok_or_else(|| format!("no {} is released for this platform", self.product.name))?;
        // The size is bounded by the helper's limit; checked once whole.
        let binary = self
            .get(&file(asset)?, super::swap::MAX_BINARY as usize)
            .await?;
        verified
            .check(asset, &binary)
            .and_then(|()| check_executable(platform, &binary))
            .map_err(|e| format!("{base}: {e}"))?;
        Ok(Downloaded {
            sums,
            signature,
            version_file,
            binary,
        })
    }
}

/// `error` and its causes, so a TLS or DNS failure says which: reqwest's
/// own text is only "error sending request".
fn chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

/// The base URLs of `spec`, checked: http or https, without credentials,
/// each ending with `/` so file names join under it.
pub fn base_urls(spec: &SelfUpdate) -> Result<Vec<Url>, String> {
    if spec.base_urls.is_empty() || spec.base_urls.len() > MAX_URLS {
        return Err(format!("between 1 and {MAX_URLS} base URLs are needed"));
    }
    spec.base_urls
        .iter()
        .map(|text| {
            let bad = || format!("invalid base URL `{}`", text.escape_debug());
            if text.len() > MAX_URL_LEN {
                return Err(bad());
            }
            let mut url = Url::parse(text).map_err(|_| bad())?;
            let plain = matches!(url.scheme(), "http" | "https")
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && url.host().is_some();
            if !plain {
                return Err(bad());
            }
            if !url.path().ends_with('/') {
                url.set_path(&format!("{}/", url.path()));
            }
            Ok(url)
        })
        .collect()
}

fn failed(error: String) -> Output {
    Output {
        exit_code: 1,
        error: Some(error),
        ..Output::default()
    }
}

/// Printed by the helper once the watchdog is started.
pub const RESTARTING: &str = "restarting into";

#[async_trait::async_trait]
impl Handler for SelfUpdateHandler {
    async fn run(&self, _payload: &[u8]) -> Output {
        Output::rejected("self_update needs its command ID")
    }

    async fn run_command(&self, command_id: &str, payload: &[u8]) -> Output {
        let Ok(spec) = SelfUpdate::decode(payload) else {
            return Output::rejected("the payload is not a SelfUpdate message");
        };
        let checked = Version::from_str(&spec.version)
            .map_err(|e| e.to_string())
            .and_then(|target| Ok((target, base_urls(&spec)?)))
            .and_then(|(target, urls)| {
                Ok((target, urls, health_timeout(spec.health_timeout_seconds)?))
            });
        let (target, urls, health) = match checked {
            Ok(checked) => checked,
            Err(error) => return Output::rejected(error),
        };
        let Some(platform) = self.platform else {
            return Output::rejected("releases are not built for this platform");
        };
        match check_upgrade(&self.current, &target, spec.allow_downgrade) {
            Ok(true) => {}
            Ok(false) => {
                return Output {
                    stdout: format!("already running {target}\n").into_bytes(),
                    ..Output::default()
                }
            }
            Err(error) => return Output::rejected(error.to_string()),
        }

        let mut log = String::new();
        let mut found = None;
        for url in &urls {
            match self.download(url, &target, platform).await {
                Ok(files) => {
                    log.push_str(&format!(
                        "downloaded {target} from {url} and verified its signature\n"
                    ));
                    found = Some(files);
                    break;
                }
                Err(error) => {
                    tracing::warn!(%error, "cannot use a release source");
                    log.push_str(&format!("skipped: {error}\n"));
                }
            }
        }
        let Some(files) = found else {
            return Output {
                stdout: log.into_bytes(),
                ..failed(format!("no source served a valid {target} release"))
            };
        };
        let header = Header {
            command_id: command_id.to_owned(),
            sums: String::from_utf8_lossy(&files.sums).into_owned(),
            signature: base64::engine::general_purpose::STANDARD.encode(&files.signature),
            version_file: String::from_utf8_lossy(&files.version_file).into_owned(),
            allow_downgrade: spec.allow_downgrade,
            health_timeout_ms: health.as_millis() as u64,
        };
        let binary = files.binary;
        match self.installer.install(&header, &binary).await {
            Err(output) => Output {
                stdout: [log.into_bytes(), output.stdout].concat(),
                ..output
            },
            Ok(Installed::Current(text)) => {
                log.push_str(&text);
                Output {
                    stdout: log.into_bytes(),
                    ..Output::default()
                }
            }
            Ok(Installed::Restarting(text)) => {
                log.push_str(&text);
                drop(binary);
                // The restart stops this process; the result then comes
                // from the status file. Still here much later, it failed.
                tracing::info!(version = %target, "installed; waiting for the restart");
                tokio::time::sleep(health + 2 * RESTART_ALLOWANCE).await;
                Output {
                    stdout: log.into_bytes(),
                    ..failed(format!(
                        "installed, but the {} was not restarted; see {}",
                        self.product.name,
                        self.installer.log_hint()
                    ))
                }
            }
        }
    }
}

/// The update service on its pipe (Windows, ADR-0049): the release is
/// staged in `<data_dir>\self-update\staged\`, which the service reads,
/// checks again and installs.
pub struct UpdateServiceInstaller {
    pub service: std::sync::Arc<dyn crate::packages::windows_update::client::UpdateService>,
    pub data_dir: std::path::PathBuf,
}

#[async_trait::async_trait]
impl Installer for UpdateServiceInstaller {
    async fn install(&self, header: &Header, binary: &[u8]) -> Result<Installed, Output> {
        use crate::packages::windows_update::{Reply, Request};
        use base64::Engine;

        let staged = super::swap::staged_dir(&self.data_dir);
        let signature = base64::engine::general_purpose::STANDARD
            .decode(header.signature.as_bytes())
            .map_err(|_| failed("the signature is not base64".into()))?;
        let files: [(&str, &[u8]); 4] = [
            (SUMS, header.sums.as_bytes()),
            (SIGNATURE, &signature),
            (VERSION_FILE, header.version_file.as_bytes()),
            (Platform::WindowsX86_64.agent_asset(), binary),
        ];
        let stage = async {
            let _ = tokio::fs::remove_dir_all(&staged).await;
            tokio::fs::create_dir_all(&staged).await?;
            for (name, bytes) in files {
                tokio::fs::write(staged.join(name), bytes).await?;
            }
            std::io::Result::Ok(())
        };
        stage.await.map_err(|e| {
            failed(format!(
                "cannot stage the release in {}: {e}",
                staged.display()
            ))
        })?;
        let request = Request::SelfUpdate(super::swap::Request {
            command_id: header.command_id.clone(),
            data_dir: self.data_dir.clone(),
            allow_downgrade: header.allow_downgrade,
            health_timeout_ms: header.health_timeout_ms,
        });
        match self.service.call(&request).await {
            Ok(Reply::SelfUpdate(done)) if done.restarting => {
                Ok(Installed::Restarting(done.message + "\n"))
            }
            Ok(Reply::SelfUpdate(done)) => Ok(Installed::Current(done.message + "\n")),
            Ok(Reply::Error(error)) => Err(failed(format!("update service: {error}"))),
            Ok(other) => Err(failed(format!(
                "update service: unexpected reply {other:?}"
            ))),
            Err(error) => Err(failed(error.to_string())),
        }
    }

    fn log_hint(&self) -> &'static str {
        r"%ProgramData%\MonitorFlow\update\update.log"
    }
}

/// The root helper, `sudo -n /usr/bin/monitorflow-agent self-update`
/// (or the relay's), with the header line and the binary on its stdin.
#[cfg(unix)]
pub struct SudoHelper {
    /// `tools.agent` is the binary whose helper runs.
    pub tools: crate::packages::Tools,
    pub product: Product,
}

#[cfg(unix)]
impl SudoHelper {
    /// The helper of `product` on the real system.
    pub fn of(product: Product) -> Self {
        Self {
            tools: crate::packages::Tools {
                agent: product.bin.into(),
                ..crate::packages::Tools::default()
            },
            product,
        }
    }
}

#[cfg(unix)]
#[async_trait::async_trait]
impl Installer for SudoHelper {
    async fn install(&self, header: &Header, binary: &[u8]) -> Result<Installed, Output> {
        let mut input = serde_json::to_vec(header).expect("a header serializes");
        input.push(b'\n');
        input.extend_from_slice(binary);
        let tools = &self.tools;
        let run = tools
            .privileged_with_input(&[], &tools.agent, &["self-update"], Some(&input))
            .await
            .map_err(|error| failed(error.to_string()))?;
        let stdout = String::from_utf8_lossy(&run.stdout).into_owned();
        if !run.success() {
            let stderr = String::from_utf8_lossy(&run.stderr);
            let error = stderr
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .map_or_else(
                    || format!("the helper exited with {}", run.code),
                    str::to_owned,
                );
            return Err(Output {
                exit_code: run.code,
                stdout: stdout.into_bytes(),
                stderr: run.stderr,
                error: Some(error),
                rejected: false,
            });
        }
        Ok(if stdout.contains(RESTARTING) {
            Installed::Restarting(stdout)
        } else {
            Installed::Current(stdout)
        })
    }

    fn log_hint(&self) -> &'static str {
        self.product.watch_log
    }
}

// Fakes sudo with a shell script.
#[cfg(all(test, unix))]
mod tests;
