//! TOML configuration for the agent, relay and server.
//!
//! Precedence: environment beats file, file beats default. Every key can be
//! overridden by `MONITORFLOW_<KEY>`, where nested keys join with `_`:
//! `log.level` becomes `MONITORFLOW_LOG_LEVEL`. Empty variables are ignored.

use std::collections::HashMap;
use std::convert::Infallible;
use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::de::DeserializeOwned;
use serde::Deserialize;
use tonic::codegen::http::Uri;

use crate::grpc_url::GrpcUrl;
use crate::redact::{redact_url_password, Secret};

#[cfg(all(test, unix))]
mod tests;
#[cfg(all(test, windows))]
mod windows_tests;

/// `%ProgramData%\MonitorFlow`, where the Windows agent keeps its
/// configuration and data (ADR-0047). `C:\ProgramData` when the variable
/// is not set.
#[cfg(windows)]
pub fn windows_base_dir() -> PathBuf {
    std::env::var_os("ProgramData")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
        .join("MonitorFlow")
}

/// Prefix of every environment override.
pub const ENV_PREFIX: &str = "MONITORFLOW_";

/// Seconds between agent heartbeats unless configured otherwise.
pub const DEFAULT_HEARTBEAT_INTERVAL: u64 = 30;

/// Port of the server's enrollment listener unless configured otherwise.
pub const DEFAULT_ENROLL_PORT: u16 = 7445;
/// Default `agent_download_url`: the files of the latest GitHub release.
pub const DEFAULT_AGENT_DOWNLOAD_URL: &str =
    "https://github.com/Kidoly/monitorflow/releases/latest/download";

/// Port of the plain HTTP mirror listeners unless configured otherwise.
pub const DEFAULT_MIRROR_PORT: u16 = 7480;

/// Environment variables as a map, so tests can pass their own.
pub type Env = HashMap<String, String>;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read config file {}: {error}", path.display())]
    Read {
        path: PathBuf,
        error: std::io::Error,
    },
    /// The message comes from the TOML parser without the offending source
    /// line, so a secret on that line is never echoed.
    #[error("{}: {location}{message}", path.display())]
    Parse {
        path: PathBuf,
        location: String,
        message: String,
    },
    #[error("environment variable {var} (key `{key}`): {message}")]
    Env {
        var: String,
        key: String,
        message: String,
    },
    #[error("{}: key `{key}`: {message}", path.display())]
    Invalid {
        path: PathBuf,
        key: String,
        message: String,
    },
}

/// Implemented by each binary's top-level configuration.
pub trait LoadConfig: DeserializeOwned {
    fn apply_env(&mut self, env: &Overrides<'_>) -> Result<(), ConfigError>;
    fn validate(&self, check: &Check<'_>) -> Result<(), ConfigError>;
}

/// Loads `path`, applies `MONITORFLOW_*` variables from the process
/// environment and validates the result.
pub fn load<T: LoadConfig>(path: &Path) -> Result<T, ConfigError> {
    let env = std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
        .filter(|(k, _)| k.starts_with(ENV_PREFIX))
        .collect();
    load_with_env(path, &env)
}

/// Same as [`load`] with an explicit environment.
pub fn load_with_env<T: LoadConfig>(path: &Path, env: &Env) -> Result<T, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|error| ConfigError::Read {
        path: path.to_owned(),
        error,
    })?;
    parse_with_env(&text, path, env)
}

/// Parses `text` as if read from `path`, then applies `env` and validates.
pub fn parse_with_env<T: LoadConfig>(text: &str, path: &Path, env: &Env) -> Result<T, ConfigError> {
    let mut config: T = toml::from_str(text).map_err(|e| ConfigError::Parse {
        path: path.to_owned(),
        location: e.span().map(|s| locate(text, s.start)).unwrap_or_default(),
        message: e.message().trim_end().to_owned(),
    })?;
    config.apply_env(&Overrides { env })?;
    config.validate(&Check { path })?;
    Ok(config)
}

/// Describes where a parse error happened as `line N, key `a.b`: `.
fn locate(text: &str, offset: usize) -> String {
    let before = &text[..offset.min(text.len())];
    let line_no = before.matches('\n').count() + 1;
    let line = text.lines().nth(line_no - 1).unwrap_or_default();
    let table = before
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| l.starts_with('['))
        .map(|l| l.trim_matches(|c| c == '[' || c == ']').trim());
    match line.split_once('=') {
        Some((key, _)) => {
            let key = key.trim();
            match table {
                Some(t) => format!("line {line_no}, key `{t}.{key}`: "),
                None => format!("line {line_no}, key `{key}`: "),
            }
        }
        None => format!("line {line_no}: "),
    }
}

/// Environment variable name for a dotted config key.
pub fn env_var(key: &str) -> String {
    format!("{ENV_PREFIX}{}", key.replace('.', "_").to_uppercase())
}

/// Applies environment overrides to config fields.
pub struct Overrides<'a> {
    env: &'a Env,
}

impl Overrides<'_> {
    fn get(&self, key: &str) -> Option<(String, &str)> {
        let var = env_var(key);
        let value = self.env.get(&var)?.as_str();
        (!value.is_empty()).then_some((var, value))
    }

    fn parse<T: FromStr>(&self, key: &str) -> Result<Option<T>, ConfigError>
    where
        T::Err: fmt::Display,
    {
        let Some((var, value)) = self.get(key) else {
            return Ok(None);
        };
        value
            .parse()
            .map(Some)
            .map_err(|e: T::Err| ConfigError::Env {
                var,
                key: key.to_owned(),
                message: e.to_string(),
            })
    }

    /// A comma-separated list: items are trimmed and empty ones dropped, so
    /// a variable of only separators or spaces sets an empty list. An empty
    /// variable is ignored like the others.
    pub fn list(&self, key: &str) -> Option<Vec<String>> {
        let (_, value) = self.get(key)?;
        Some(
            value
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_owned)
                .collect(),
        )
    }

    pub fn set<T: FromStr>(&self, key: &str, field: &mut T) -> Result<(), ConfigError>
    where
        T::Err: fmt::Display,
    {
        if let Some(value) = self.parse(key)? {
            *field = value;
        }
        Ok(())
    }

    pub fn set_opt<T: FromStr>(&self, key: &str, field: &mut Option<T>) -> Result<(), ConfigError>
    where
        T::Err: fmt::Display,
    {
        if let Some(value) = self.parse(key)? {
            *field = Some(value);
        }
        Ok(())
    }
}

/// Validation helpers that build [`ConfigError::Invalid`] for one file.
pub struct Check<'a> {
    path: &'a Path,
}

impl Check<'_> {
    fn fail(&self, key: &str, message: impl Into<String>) -> ConfigError {
        ConfigError::Invalid {
            path: self.path.to_owned(),
            key: key.to_owned(),
            message: message.into(),
        }
    }

    pub fn required(&self, key: &str, value: &str) -> Result<(), ConfigError> {
        if value.is_empty() {
            return Err(self.fail(
                key,
                format!("is required; set it in the file or with {}", env_var(key)),
            ));
        }
        Ok(())
    }

    /// gRPC upstreams, see [`GrpcUrl::parse`].
    pub fn grpc_url(&self, key: &str, value: &str) -> Result<(), ConfigError> {
        self.required(key, value)?;
        GrpcUrl::parse(value).map(drop).map_err(|reason| {
            self.fail(
                key,
                format!("must be an https URL like https://host:7443 ({reason}), got `{value}`"),
            )
        })
    }

    /// HTTP endpoints of storage backends; `http` is allowed for development.
    pub fn http_url(&self, key: &str, value: &str) -> Result<(), ConfigError> {
        self.required(key, value)?;
        self.url(key, value, &["http", "https"])
            .map_err(|_| self.fail(key, format!("must be an http or https URL, got `{value}`")))
    }

    fn url(&self, key: &str, value: &str, schemes: &[&str]) -> Result<(), ConfigError> {
        let uri: Uri = value.parse().map_err(|_| self.fail(key, ""))?;
        match (uri.scheme_str(), uri.host()) {
            (Some(s), Some(h)) if schemes.contains(&s) && !h.is_empty() => Ok(()),
            _ => Err(self.fail(key, "")),
        }
    }

    pub fn positive(&self, key: &str, value: u64) -> Result<(), ConfigError> {
        if value == 0 {
            return Err(self.fail(key, "must be greater than zero"));
        }
        Ok(())
    }
}

/// PostgreSQL connection URL. `Debug` hides the password.
#[derive(Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct DatabaseUrl(String);

impl DatabaseUrl {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for DatabaseUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", redact_url_password(&self.0))
    }
}

impl FromStr for DatabaseUrl {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.to_owned()))
    }
}

/// Paths to the PEM files of the mTLS identity. Loading is done elsewhere.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TlsPaths {
    pub ca_cert: PathBuf,
    pub cert: PathBuf,
    pub key: PathBuf,
}

impl Default for TlsPaths {
    fn default() -> Self {
        Self {
            ca_cert: "/etc/monitorflow/tls/ca.pem".into(),
            cert: "/etc/monitorflow/tls/cert.pem".into(),
            key: "/etc/monitorflow/tls/key.pem".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Trace,
    Debug,
    #[default]
    Info,
    Warn,
    Error,
}

impl FromStr for LogLevel {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "trace" => Ok(Self::Trace),
            "debug" => Ok(Self::Debug),
            "info" => Ok(Self::Info),
            "warn" => Ok(Self::Warn),
            "error" => Ok(Self::Error),
            _ => Err(format!(
                "unknown log level `{s}`, expected trace, debug, info, warn or error"
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    #[default]
    Text,
    Json,
}

impl FromStr for LogFormat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "text" => Ok(Self::Text),
            "json" => Ok(Self::Json),
            _ => Err(format!("unknown log format `{s}`, expected text or json")),
        }
    }
}

/// Batching of the server's writes to VictoriaMetrics and VictoriaLogs
/// (ADR-0013). Each writer has its own queue with these settings.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IngestConfig {
    /// Most items (samples or log lines) sent in one HTTP request.
    pub batch_size: usize,
    /// Milliseconds between flushes of a batch that is not full.
    pub flush_interval_ms: u64,
    /// Items queued in memory; new items are dropped while it is full.
    pub queue_capacity: usize,
    /// Seconds allowed at shutdown to flush what is queued.
    pub shutdown_timeout: u64,
}

impl Default for IngestConfig {
    fn default() -> Self {
        Self {
            batch_size: 5_000,
            flush_interval_ms: 1_000,
            queue_capacity: 100_000,
            shutdown_timeout: 10,
        }
    }
}

/// Upper bounds that keep one request and the queue's memory reasonable.
const MAX_INGEST_BATCH: usize = 100_000;
const MAX_INGEST_QUEUE: usize = 10_000_000;
const MAX_FLUSH_INTERVAL_MS: u64 = 60_000;

impl IngestConfig {
    fn apply_env(&mut self, env: &Overrides<'_>) -> Result<(), ConfigError> {
        env.set("ingest.batch_size", &mut self.batch_size)?;
        env.set("ingest.flush_interval_ms", &mut self.flush_interval_ms)?;
        env.set("ingest.queue_capacity", &mut self.queue_capacity)?;
        env.set("ingest.shutdown_timeout", &mut self.shutdown_timeout)
    }

    fn validate(&self, check: &Check<'_>) -> Result<(), ConfigError> {
        if !(1..=MAX_INGEST_BATCH).contains(&self.batch_size) {
            return Err(check.fail(
                "ingest.batch_size",
                format!("must be 1 to {MAX_INGEST_BATCH}"),
            ));
        }
        if !(1..=MAX_FLUSH_INTERVAL_MS).contains(&self.flush_interval_ms) {
            return Err(check.fail(
                "ingest.flush_interval_ms",
                format!("must be 1 to {MAX_FLUSH_INTERVAL_MS}"),
            ));
        }
        if !(self.batch_size..=MAX_INGEST_QUEUE).contains(&self.queue_capacity) {
            return Err(check.fail(
                "ingest.queue_capacity",
                format!("must be at least ingest.batch_size and at most {MAX_INGEST_QUEUE}"),
            ));
        }
        check.positive("ingest.shutdown_timeout", self.shutdown_timeout)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LogConfig {
    pub level: LogLevel,
    pub format: LogFormat,
}

impl LogConfig {
    fn apply_env(&mut self, env: &Overrides<'_>) -> Result<(), ConfigError> {
        env.set("log.level", &mut self.level)?;
        env.set("log.format", &mut self.format)
    }
}

/// Identity files of a peer that owns them. Unset paths default to files
/// under `<data_dir>/tls/`, where enrollment (or the server's built-in CA,
/// ADR-0007) writes them; see [`Self::resolve`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DataDirTlsPaths {
    pub ca_cert: Option<PathBuf>,
    pub cert: Option<PathBuf>,
    pub key: Option<PathBuf>,
}

pub type AgentTlsPaths = DataDirTlsPaths;
pub type RelayTlsPaths = DataDirTlsPaths;
pub type ServerTlsPaths = DataDirTlsPaths;

impl DataDirTlsPaths {
    fn apply_env(&mut self, env: &Overrides<'_>) -> Result<(), ConfigError> {
        env.set_opt("tls.ca_cert", &mut self.ca_cert)?;
        env.set_opt("tls.cert", &mut self.cert)?;
        env.set_opt("tls.key", &mut self.key)
    }

    /// The configured paths, with defaults under `<data_dir>/tls/`.
    pub fn resolve(&self, data_dir: &Path) -> TlsPaths {
        let dir = data_dir.join("tls");
        let or =
            |path: &Option<PathBuf>, name: &str| path.clone().unwrap_or_else(|| dir.join(name));
        TlsPaths {
            ca_cert: or(&self.ca_cert, "ca.pem"),
            cert: or(&self.cert, "cert.pem"),
            key: or(&self.key, "key.pem"),
        }
    }
}

/// `https://<host of upstream_url>:7445`, or empty for an invalid URL.
fn default_enroll_url(upstream_url: &str) -> String {
    match GrpcUrl::parse(upstream_url) {
        Ok(mut url) => {
            url.port = DEFAULT_ENROLL_PORT;
            url.to_string()
        }
        Err(_) => String::new(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentConfig {
    /// gRPC endpoint of a relay or the server, `https://host:port`.
    pub upstream_url: String,
    /// Enrollment endpoint of the server (ADR-0007). Defaults to the host of
    /// `upstream_url` on port [`DEFAULT_ENROLL_PORT`]; see
    /// [`Self::enroll_url`].
    pub enroll_url: Option<String>,
    pub data_dir: PathBuf,
    /// One-time token, only needed for the first run.
    pub enrollment_token: Option<Secret>,
    /// Seconds between heartbeats.
    pub heartbeat_interval: u64,
    pub tls: AgentTlsPaths,
    pub log: LogConfig,
    pub sensors: AgentSensorsConfig,
    pub packages: AgentPackagesConfig,
    pub commands: AgentCommandsConfig,
}

/// Which optional command kinds the server may send this host. Only the
/// host's administrator sets these.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentCommandsConfig {
    /// Runs `shell` commands of the AI remediation loop (ADR-0043), with
    /// `/bin/sh -c` as the agent's user. Off unless enabled here.
    pub shell: bool,
}

/// Package reports and updates (ADR-0031).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentPackagesConfig {
    /// Seconds between two package reports; one is also sent at start and
    /// after every update.
    pub report_interval: u64,
}

impl Default for AgentPackagesConfig {
    fn default() -> Self {
        Self {
            report_interval: 6 * 60 * 60,
        }
    }
}

/// What sensors configured by the server may touch on this host
/// (ADR-0024). Only the host's administrator sets these.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentSensorsConfig {
    /// Directory of the scripts the `script` sensor may run.
    pub script_dir: PathBuf,
    /// Directories the `log` sensor may read files under; empty disables
    /// it.
    pub log_dirs: Vec<PathBuf>,
    /// Event Log channels the `eventlog` sensor may read on Windows
    /// (ADR-0048); empty disables it.
    pub event_channels: Vec<String>,
}

#[cfg(unix)]
impl Default for AgentSensorsConfig {
    fn default() -> Self {
        Self {
            script_dir: "/etc/monitorflow/scripts.d".into(),
            log_dirs: vec!["/var/log".into()],
            event_channels: vec!["Application".into(), "System".into()],
        }
    }
}

/// The `log` sensor does not run on Windows (ADR-0048), so it gets no
/// directories; the Event Log channels are the same as on Unix.
#[cfg(windows)]
impl Default for AgentSensorsConfig {
    fn default() -> Self {
        Self {
            script_dir: windows_base_dir().join("scripts.d"),
            log_dirs: Vec::new(),
            event_channels: vec!["Application".into(), "System".into()],
        }
    }
}

impl AgentSensorsConfig {
    fn validate(&self, check: &Check<'_>) -> Result<(), ConfigError> {
        if !self.script_dir.is_absolute() {
            return Err(check.fail("sensors.script_dir", "must be an absolute path"));
        }
        let plain = |p: &Path| p.is_absolute() && !p.components().any(|c| c.as_os_str() == "..");
        if let Some(dir) = self.log_dirs.iter().find(|d| !plain(d)) {
            return Err(check.fail(
                "sensors.log_dirs",
                format!("`{}` must be an absolute path without `..`", dir.display()),
            ));
        }
        Ok(())
    }
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            upstream_url: String::new(),
            enroll_url: None,
            data_dir: default_agent_data_dir(),
            enrollment_token: None,
            heartbeat_interval: DEFAULT_HEARTBEAT_INTERVAL,
            tls: AgentTlsPaths::default(),
            log: LogConfig::default(),
            sensors: AgentSensorsConfig::default(),
            packages: AgentPackagesConfig::default(),
            commands: AgentCommandsConfig::default(),
        }
    }
}

#[cfg(unix)]
fn default_agent_data_dir() -> PathBuf {
    "/var/lib/monitorflow/agent".into()
}

#[cfg(windows)]
fn default_agent_data_dir() -> PathBuf {
    windows_base_dir().join("agent")
}

impl AgentConfig {
    /// The configured `enroll_url`, or `https://<upstream host>:7445`.
    /// Call on a validated config.
    pub fn enroll_url(&self) -> String {
        self.enroll_url
            .clone()
            .unwrap_or_else(|| default_enroll_url(&self.upstream_url))
    }

    /// Identity file paths, with defaults under `<data_dir>/tls/`.
    pub fn tls_paths(&self) -> TlsPaths {
        self.tls.resolve(&self.data_dir)
    }
}

impl LoadConfig for AgentConfig {
    fn apply_env(&mut self, env: &Overrides<'_>) -> Result<(), ConfigError> {
        env.set("upstream_url", &mut self.upstream_url)?;
        env.set_opt("enroll_url", &mut self.enroll_url)?;
        env.set("data_dir", &mut self.data_dir)?;
        env.set_opt("enrollment_token", &mut self.enrollment_token)?;
        env.set("heartbeat_interval", &mut self.heartbeat_interval)?;
        env.set("sensors.script_dir", &mut self.sensors.script_dir)?;
        env.set(
            "packages.report_interval",
            &mut self.packages.report_interval,
        )?;
        env.set("commands.shell", &mut self.commands.shell)?;
        if let Some(dirs) = env.list("sensors.log_dirs") {
            self.sensors.log_dirs = dirs.into_iter().map(PathBuf::from).collect();
        }
        if let Some(channels) = env.list("sensors.event_channels") {
            self.sensors.event_channels = channels;
        }
        self.tls.apply_env(env)?;
        self.log.apply_env(env)
    }

    fn validate(&self, check: &Check<'_>) -> Result<(), ConfigError> {
        check.grpc_url("upstream_url", &self.upstream_url)?;
        if let Some(url) = &self.enroll_url {
            check.grpc_url("enroll_url", url)?;
        }
        check.positive("heartbeat_interval", self.heartbeat_interval)?;
        check.positive("packages.report_interval", self.packages.report_interval)?;
        self.sensors.validate(check)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RelayConfig {
    /// gRPC listener for agents (ADR-0025).
    pub listen_addr: SocketAddr,
    /// gRPC endpoint of the server (or, later, the next relay),
    /// `https://host:port`.
    pub upstream_url: String,
    /// Enrollment endpoint of the server (ADR-0007). Defaults to the host of
    /// `upstream_url` on port [`DEFAULT_ENROLL_PORT`].
    pub enroll_url: Option<String>,
    pub data_dir: PathBuf,
    /// One-time relay token, only needed for the first run.
    pub enrollment_token: Option<Secret>,
    /// Seconds between heartbeats on the relay's own stream.
    pub heartbeat_interval: u64,
    pub buffer: BufferConfig,
    pub tls: RelayTlsPaths,
    pub log: LogConfig,
    /// Plain HTTP listener serving the relay's copy of the mirrors
    /// (ADR-0034), with the server's layout.
    pub mirror_listen_addr: SocketAddr,
    /// Root of the mirror copies; defaults to `<data_dir>/mirrors`, see
    /// [`Self::mirror_dir`].
    pub mirror_dir: Option<PathBuf>,
    pub mirror: RelayMirrorConfig,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            listen_addr: SocketAddr::from(([0, 0, 0, 0], 7443)),
            upstream_url: String::new(),
            enroll_url: None,
            data_dir: "/var/lib/monitorflow/relay".into(),
            enrollment_token: None,
            heartbeat_interval: DEFAULT_HEARTBEAT_INTERVAL,
            buffer: BufferConfig::default(),
            tls: RelayTlsPaths::default(),
            log: LogConfig::default(),
            mirror_listen_addr: SocketAddr::from(([0, 0, 0, 0], DEFAULT_MIRROR_PORT)),
            mirror_dir: None,
            mirror: RelayMirrorConfig::default(),
        }
    }
}

impl RelayConfig {
    /// The configured `enroll_url`, or `https://<upstream host>:7445`.
    pub fn enroll_url(&self) -> String {
        self.enroll_url
            .clone()
            .unwrap_or_else(|| default_enroll_url(&self.upstream_url))
    }

    /// Identity file paths, with defaults under `<data_dir>/tls/`.
    pub fn tls_paths(&self) -> TlsPaths {
        self.tls.resolve(&self.data_dir)
    }

    /// Directory of the store-and-forward buffer.
    pub fn buffer_dir(&self) -> PathBuf {
        self.data_dir.join("buffer")
    }

    /// `mirror_dir`, or `<data_dir>/mirrors`.
    pub fn mirror_dir(&self) -> PathBuf {
        self.mirror_dir
            .clone()
            .unwrap_or_else(|| self.data_dir.join("mirrors"))
    }

    /// The configured `mirror.upstream_url`, or `http://<upstream host>:7480`:
    /// the mirror listener of the server or of the parent relay.
    pub fn mirror_upstream_url(&self) -> String {
        if let Some(url) = &self.mirror.upstream_url {
            return url.trim_end_matches('/').to_owned();
        }
        match GrpcUrl::parse(&self.upstream_url) {
            Ok(mut url) => {
                url.port = DEFAULT_MIRROR_PORT;
                format!("http://{}", url.authority())
            }
            Err(_) => String::new(),
        }
    }
}

/// The relay's copy of the upstream's mirrors (ADR-0034).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RelayMirrorConfig {
    /// Sync and serve mirrors at all. Off unless configured: a relay copies
    /// tens of gigabytes only on request (ADR-0034, amendment).
    pub enabled: bool,
    /// Mirror listener of the upstream, `http(s)://host:port`. Defaults to
    /// the host of `upstream_url` on port 7480.
    pub upstream_url: Option<String>,
    /// Base URL this relay's agents reach `mirror_listen_addr` at, like
    /// `http://relay.example.net:7480`, reported to the server so the
    /// package sources it sends them point here (ADR-0037). Unset, they
    /// keep the server's `mirror_public_url`.
    pub public_url: Option<String>,
    /// Seconds between syncs.
    pub sync_interval: u64,
    /// Mirror names to copy; empty copies every mirror the upstream lists.
    /// The environment override is a comma-separated list.
    pub mirrors: Vec<String>,
    /// Snapshots kept per mirror, including the current one.
    pub keep_snapshots: usize,
    /// Files downloaded at once.
    pub max_downloads: usize,
    /// Seconds a request may go without progress.
    pub request_timeout: u64,
    /// Largest total size of one snapshot, as its metadata lists it; a
    /// larger one fails the sync before anything past the limit is fetched.
    pub max_snapshot_bytes: u64,
}

const MIN_MIRROR_SYNC_INTERVAL: u64 = 60;
/// Default of `mirror.max_snapshot_bytes`, 256 GiB.
pub const DEFAULT_MIRROR_MAX_SNAPSHOT_BYTES: u64 = 256 << 30;
const MAX_MIRROR_DOWNLOADS: usize = 64;

impl Default for RelayMirrorConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            upstream_url: None,
            public_url: None,
            sync_interval: 15 * 60,
            mirrors: Vec::new(),
            keep_snapshots: 3,
            max_downloads: 4,
            request_timeout: 600,
            max_snapshot_bytes: DEFAULT_MIRROR_MAX_SNAPSHOT_BYTES,
        }
    }
}

impl RelayMirrorConfig {
    fn apply_env(&mut self, env: &Overrides<'_>) -> Result<(), ConfigError> {
        env.set("mirror.enabled", &mut self.enabled)?;
        env.set_opt("mirror.upstream_url", &mut self.upstream_url)?;
        env.set_opt("mirror.public_url", &mut self.public_url)?;
        env.set("mirror.sync_interval", &mut self.sync_interval)?;
        if let Some(names) = env.list("mirror.mirrors") {
            self.mirrors = names;
        }
        env.set("mirror.keep_snapshots", &mut self.keep_snapshots)?;
        env.set("mirror.max_downloads", &mut self.max_downloads)?;
        env.set("mirror.request_timeout", &mut self.request_timeout)?;
        env.set("mirror.max_snapshot_bytes", &mut self.max_snapshot_bytes)
    }

    fn validate(&self, check: &Check<'_>) -> Result<(), ConfigError> {
        if let Some(url) = &self.upstream_url {
            check.http_url("mirror.upstream_url", url)?;
        }
        if let Some(url) = &self.public_url {
            if !valid_mirror_url(url) {
                return Err(check.fail(
                    "mirror.public_url",
                    "must be an http or https URL without user, query or fragment",
                ));
            }
        }
        if self.sync_interval < MIN_MIRROR_SYNC_INTERVAL {
            return Err(check.fail(
                "mirror.sync_interval",
                format!("must be at least {MIN_MIRROR_SYNC_INTERVAL} seconds"),
            ));
        }
        if let Some(bad) = self
            .mirrors
            .iter()
            .find(|m| !crate::mirror::is_mirror_name(m))
        {
            return Err(check.fail(
                "mirror.mirrors",
                format!("`{bad}` is not a mirror name (a-z, 0-9, `.`, `_`, `-`)"),
            ));
        }
        check.positive("mirror.keep_snapshots", self.keep_snapshots as u64)?;
        if !(1..=MAX_MIRROR_DOWNLOADS).contains(&self.max_downloads) {
            return Err(check.fail(
                "mirror.max_downloads",
                format!("must be 1 to {MAX_MIRROR_DOWNLOADS}"),
            ));
        }
        check.positive("mirror.request_timeout", self.request_timeout)?;
        check.positive("mirror.max_snapshot_bytes", self.max_snapshot_bytes)
    }
}

/// The relay's store-and-forward buffer on disk (ADR-0027).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BufferConfig {
    /// Total size of the buffer; the oldest records go first when full.
    pub max_bytes: u64,
    /// Seconds a record is kept; at most the server's 7-day sample age
    /// limit (ADR-0013).
    pub max_age: u64,
    /// Milliseconds between syncs of new records to disk; 0 syncs after
    /// every record.
    pub fsync_interval_ms: u64,
}

/// The server drops samples older than this (ADR-0013).
pub const MAX_BUFFER_AGE: u64 = 7 * 24 * 60 * 60;
const MIN_BUFFER_BYTES: u64 = 1024 * 1024;
const MAX_FSYNC_INTERVAL_MS: u64 = 60_000;

impl Default for BufferConfig {
    fn default() -> Self {
        Self {
            max_bytes: 1024 * 1024 * 1024,
            max_age: MAX_BUFFER_AGE,
            fsync_interval_ms: 1_000,
        }
    }
}

impl BufferConfig {
    fn apply_env(&mut self, env: &Overrides<'_>) -> Result<(), ConfigError> {
        env.set("buffer.max_bytes", &mut self.max_bytes)?;
        env.set("buffer.max_age", &mut self.max_age)?;
        env.set("buffer.fsync_interval_ms", &mut self.fsync_interval_ms)
    }

    fn validate(&self, check: &Check<'_>) -> Result<(), ConfigError> {
        if self.max_bytes < MIN_BUFFER_BYTES {
            return Err(check.fail(
                "buffer.max_bytes",
                format!("must be at least {MIN_BUFFER_BYTES} (1 MiB)"),
            ));
        }
        if !(1..=MAX_BUFFER_AGE).contains(&self.max_age) {
            return Err(check.fail(
                "buffer.max_age",
                format!(
                    "must be 1 to {MAX_BUFFER_AGE} seconds (7 days, the server's sample age limit)"
                ),
            ));
        }
        if self.fsync_interval_ms > MAX_FSYNC_INTERVAL_MS {
            return Err(check.fail(
                "buffer.fsync_interval_ms",
                format!("must be at most {MAX_FSYNC_INTERVAL_MS}"),
            ));
        }
        Ok(())
    }
}

impl LoadConfig for RelayConfig {
    fn apply_env(&mut self, env: &Overrides<'_>) -> Result<(), ConfigError> {
        env.set("listen_addr", &mut self.listen_addr)?;
        env.set("upstream_url", &mut self.upstream_url)?;
        env.set_opt("enroll_url", &mut self.enroll_url)?;
        env.set("data_dir", &mut self.data_dir)?;
        env.set_opt("enrollment_token", &mut self.enrollment_token)?;
        env.set("heartbeat_interval", &mut self.heartbeat_interval)?;
        self.buffer.apply_env(env)?;
        self.tls.apply_env(env)?;
        env.set("mirror_listen_addr", &mut self.mirror_listen_addr)?;
        env.set_opt("mirror_dir", &mut self.mirror_dir)?;
        self.mirror.apply_env(env)?;
        self.log.apply_env(env)
    }

    fn validate(&self, check: &Check<'_>) -> Result<(), ConfigError> {
        check.grpc_url("upstream_url", &self.upstream_url)?;
        if let Some(url) = &self.enroll_url {
            check.grpc_url("enroll_url", url)?;
        }
        check.positive("heartbeat_interval", self.heartbeat_interval)?;
        self.buffer.validate(check)?;
        self.mirror.validate(check)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    /// gRPC listener for agents and relays.
    pub grpc_listen_addr: SocketAddr,
    /// Enrollment listener: TLS without client certificates, serving only
    /// `EnrollService` (ADR-0007).
    pub enroll_listen_addr: SocketAddr,
    /// Ports peers reach the two listeners at, when they differ from the
    /// listeners' own, as behind a published container port; written in
    /// the install commands of new tokens.
    pub grpc_public_port: Option<u16>,
    pub enroll_public_port: Option<u16>,
    /// Plain HTTP listener for the REST API, meant to sit behind a TLS
    /// reverse proxy on the same host (ADR-0006). Defaults to loopback.
    pub api_listen_addr: SocketAddr,
    /// Mark the session cookie `Secure`. Only turn off for local development
    /// over plain HTTP without a proxy.
    pub api_cookie_secure: bool,
    /// Seconds a session lasts after login, whatever its activity.
    pub api_session_lifetime: u64,
    /// Seconds without a request after which a session expires.
    pub api_session_idle_timeout: u64,
    pub database_url: DatabaseUrl,
    pub victoriametrics_url: String,
    pub victorialogs_url: String,
    pub data_dir: PathBuf,
    /// Seconds without any message after which a peer's stream is closed.
    pub heartbeat_timeout: u64,
    /// DNS names and IP addresses peers use to reach this server, written
    /// in the certificate the built-in CA issues it (ADR-0007). The
    /// environment override is a comma-separated list.
    pub server_names: Vec<String>,
    /// https URL browsers open the web interface at, like
    /// `https://monitor.example.net`, for the setup link (ADR-0057).
    /// Unset, `https://` and the first of `server_names`.
    pub web_url: Option<String>,
    pub ingest: IngestConfig,
    pub tls: ServerTlsPaths,
    pub log: LogConfig,
    /// Plain HTTP listener serving mirror content (ADR-0030). Clients
    /// check it by its signatures, not by the transport.
    pub mirror_listen_addr: SocketAddr,
    /// Root of the mirrors, snapshots and signing key; defaults to
    /// `<data_dir>/mirrors`, see [`Self::mirror_dir`].
    pub mirror_dir: Option<PathBuf>,
    /// Base URL agents reach `mirror_listen_addr` at, like
    /// `http://mirror.example.net:7480`, written into the package sources
    /// the server sends them (ADR-0035). Unset refuses source
    /// configuration commands.
    pub mirror_public_url: Option<String>,
    /// https URL of the directory holding the agent release files, put in
    /// the install commands the web interface shows with a new token
    /// (ADR-0052). The latest GitHub release by default.
    pub agent_download_url: String,
    /// http or https URL of the directory holding server releases, for
    /// server updates (ADR-0056); `agent_download_url` when unset. Plain
    /// HTTP is allowed: the release signature protects the files.
    pub release_url: Option<String>,
    /// The volume shared with the updater container (ADR-0056); unset, the
    /// server cannot be updated from the web interface.
    pub server_update_dir: Option<PathBuf>,
    /// The updater's backups volume, mounted read-only (ADR-0058); unset,
    /// backups cannot be downloaded from the web interface.
    pub backup_dir: Option<PathBuf>,
    /// Default source of those install commands (ADR-0054). Unset leaves
    /// it to the web interface's setting; `custom` uses
    /// `agent_download_url`, which must then not be GitHub's.
    pub agent_install_source: Option<AgentInstallSource>,
    pub mirror: MirrorConfig,
    /// AI provider of the remediation loop (ADR-0039). Off by default; when
    /// off, the web interface may turn it on (ADR-0064).
    pub ai: AiConfig,
    /// Proxmox VE clusters snapshotted before change-tier commands
    /// (ADR-0044). Off by default.
    pub proxmox: ProxmoxConfig,
    /// Limits of the remediation loop (ADR-0043), which runs while AI is
    /// on.
    pub remediation: RemediationConfig,
    /// Automatic agent and relay updates (ADR-0055).
    pub auto_update: AutoUpdateConfig,
}

/// Automatic updates (ADR-0055). The setting lives in the database and the
/// web interface; a value here wins over it and locks it there.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AutoUpdateConfig {
    /// The server-wide default of sites that inherit it. Unset, the
    /// administrators' choice in the web interface applies (on by default).
    pub enabled: Option<bool>,
    /// Seconds between two looks at what to update.
    pub interval: u64,
}

const MIN_AUTO_UPDATE_INTERVAL: u64 = 10;

impl Default for AutoUpdateConfig {
    fn default() -> Self {
        Self {
            enabled: None,
            interval: 5 * 60,
        }
    }
}

impl AutoUpdateConfig {
    fn apply_env(&mut self, env: &Overrides<'_>) -> Result<(), ConfigError> {
        env.set_opt("auto_update.enabled", &mut self.enabled)?;
        env.set("auto_update.interval", &mut self.interval)
    }

    fn validate(&self, check: &Check<'_>) -> Result<(), ConfigError> {
        if self.interval < MIN_AUTO_UPDATE_INTERVAL {
            return Err(check.fail(
                "auto_update.interval",
                format!("must be at least {MIN_AUTO_UPDATE_INTERVAL} seconds"),
            ));
        }
        Ok(())
    }
}

/// Whether `url` can be written into agents' package sources: http or
/// https, a host, no userinfo, query or fragment, no whitespace.
pub fn valid_mirror_url(url: &str) -> bool {
    let Some(rest) = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
    else {
        return false;
    };
    let authority = rest.split('/').next().unwrap_or("");
    !authority.is_empty()
        && !authority.contains('@')
        && !url.contains(['?', '#'])
        && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            grpc_listen_addr: SocketAddr::from(([0, 0, 0, 0], 7443)),
            enroll_listen_addr: SocketAddr::from(([0, 0, 0, 0], DEFAULT_ENROLL_PORT)),
            grpc_public_port: None,
            enroll_public_port: None,
            api_listen_addr: SocketAddr::from(([127, 0, 0, 1], 7444)),
            api_cookie_secure: true,
            api_session_lifetime: 12 * 60 * 60,
            api_session_idle_timeout: 30 * 60,
            database_url: DatabaseUrl::default(),
            victoriametrics_url: "http://localhost:8428".into(),
            victorialogs_url: "http://localhost:9428".into(),
            data_dir: "/var/lib/monitorflow/server".into(),
            // Three missed heartbeats at the agent's default interval.
            heartbeat_timeout: 3 * DEFAULT_HEARTBEAT_INTERVAL,
            server_names: vec!["localhost".into(), "127.0.0.1".into()],
            web_url: None,
            ingest: IngestConfig::default(),
            tls: ServerTlsPaths::default(),
            log: LogConfig::default(),
            mirror_listen_addr: SocketAddr::from(([0, 0, 0, 0], 7480)),
            mirror_dir: None,
            mirror_public_url: None,
            agent_download_url: DEFAULT_AGENT_DOWNLOAD_URL.into(),
            release_url: None,
            server_update_dir: None,
            backup_dir: None,
            agent_install_source: None,
            mirror: MirrorConfig::default(),
            ai: AiConfig::default(),
            proxmox: ProxmoxConfig::default(),
            remediation: RemediationConfig::default(),
            auto_update: AutoUpdateConfig::default(),
        }
    }
}

impl ServerConfig {
    /// Base URL of the web interface without a trailing slash: `web_url`,
    /// or `https://` and the first server name. An explicit `:443` is
    /// dropped, as compose writes it from the published port.
    pub fn web_base_url(&self) -> String {
        let url = match &self.web_url {
            Some(url) => url.trim_end_matches('/').to_owned(),
            None => {
                let name = self
                    .server_names
                    .first()
                    .map_or("localhost", String::as_str);
                // An IPv6 address needs brackets in a URL.
                if name.contains(':') {
                    format!("https://[{name}]")
                } else {
                    format!("https://{name}")
                }
            }
        };
        match url.strip_suffix(":443") {
            Some(base) if !base["https://".len()..].contains('/') => base.to_owned(),
            _ => url,
        }
    }

    /// `agent_download_url` without a trailing slash, unless it is the
    /// default: a custom install source (ADR-0054).
    pub fn custom_agent_download_url(&self) -> Option<String> {
        let url = self.agent_download_url.trim_end_matches('/');
        (url != DEFAULT_AGENT_DOWNLOAD_URL).then(|| url.to_owned())
    }

    /// Identity file paths, with defaults under `<data_dir>/tls/`.
    pub fn tls_paths(&self) -> TlsPaths {
        self.tls.resolve(&self.data_dir)
    }

    /// `mirror_dir`, or `<data_dir>/mirrors`.
    pub fn mirror_dir(&self) -> PathBuf {
        self.mirror_dir
            .clone()
            .unwrap_or_else(|| self.data_dir.join("mirrors"))
    }
}

/// Tools and limits of the server's mirror jobs (ADR-0030).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MirrorConfig {
    /// Programs, looked up on `PATH` unless absolute.
    pub aptly: PathBuf,
    pub reposync: PathBuf,
    pub createrepo_c: PathBuf,
    pub gpg: PathBuf,
    /// Seconds one command of a sync or snapshot may run before it is
    /// killed and the run fails.
    pub sync_timeout: u64,
    /// Scheduled syncs running at once; manual ones are not counted.
    pub max_concurrent_syncs: usize,
}

impl Default for MirrorConfig {
    fn default() -> Self {
        Self {
            aptly: "aptly".into(),
            reposync: "reposync".into(),
            createrepo_c: "createrepo_c".into(),
            gpg: "gpg".into(),
            // A first sync of a full distribution takes hours.
            sync_timeout: 12 * 60 * 60,
            max_concurrent_syncs: 2,
        }
    }
}

impl MirrorConfig {
    fn apply_env(&mut self, env: &Overrides<'_>) -> Result<(), ConfigError> {
        env.set("mirror.aptly", &mut self.aptly)?;
        env.set("mirror.reposync", &mut self.reposync)?;
        env.set("mirror.createrepo_c", &mut self.createrepo_c)?;
        env.set("mirror.gpg", &mut self.gpg)?;
        env.set("mirror.sync_timeout", &mut self.sync_timeout)?;
        env.set(
            "mirror.max_concurrent_syncs",
            &mut self.max_concurrent_syncs,
        )
    }

    fn validate(&self, check: &Check<'_>) -> Result<(), ConfigError> {
        for (key, path) in [
            ("mirror.aptly", &self.aptly),
            ("mirror.reposync", &self.reposync),
            ("mirror.createrepo_c", &self.createrepo_c),
            ("mirror.gpg", &self.gpg),
        ] {
            if path.as_os_str().is_empty() {
                return Err(check.fail(key, "must name a program"));
            }
        }
        check.positive("mirror.sync_timeout", self.sync_timeout)?;
        check.positive(
            "mirror.max_concurrent_syncs",
            self.max_concurrent_syncs as u64,
        )
    }
}

/// Model the Claude adapter uses unless configured otherwise.
pub const DEFAULT_AI_MODEL: &str = "claude-opus-5-5";

/// External AI API used to diagnose incidents and propose commands
/// (ADR-0039). Its answers are untrusted text whatever the provider.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AiConfig {
    pub enabled: bool,
    pub provider: AiProviderKind,
    /// File holding the API key, read at start. The key itself never goes
    /// in this file, the environment or logs.
    pub api_key_file: Option<PathBuf>,
    pub model: String,
    /// Most tokens one answer may use, thinking included.
    pub max_tokens: u32,
    /// Seconds one HTTP request may take, connection included.
    pub request_timeout: u64,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: AiProviderKind::Claude,
            api_key_file: None,
            model: DEFAULT_AI_MODEL.into(),
            max_tokens: 16_000,
            // Answers with thinking may take minutes.
            request_timeout: 300,
        }
    }
}

impl AiConfig {
    fn apply_env(&mut self, env: &Overrides<'_>) -> Result<(), ConfigError> {
        env.set("ai.enabled", &mut self.enabled)?;
        env.set("ai.provider", &mut self.provider)?;
        env.set_opt("ai.api_key_file", &mut self.api_key_file)?;
        env.set("ai.model", &mut self.model)?;
        env.set("ai.max_tokens", &mut self.max_tokens)?;
        env.set("ai.request_timeout", &mut self.request_timeout)
    }

    fn validate(&self, check: &Check<'_>) -> Result<(), ConfigError> {
        if !self.enabled {
            return Ok(());
        }
        let key_file = self.api_key_file.as_deref().unwrap_or(Path::new(""));
        check.required("ai.api_key_file", &key_file.to_string_lossy())?;
        check.required("ai.model", &self.model)?;
        check.positive("ai.max_tokens", self.max_tokens.into())?;
        check.positive("ai.request_timeout", self.request_timeout)
    }
}

/// Longest `remediation.command_timeout`, the agent's own limit.
pub const MAX_REMEDIATION_COMMAND_TIMEOUT: u64 = 60 * 60;

/// Limits of each remediation incident (ADR-0043). Hitting one hands the
/// incident to a human. They are copied into the incident when it opens.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RemediationConfig {
    /// Most commands run for one incident.
    pub max_commands: u32,
    /// Seconds from opening to escalation, waiting for approval included.
    pub max_duration: u64,
    /// Seconds one command may take from queueing to its result.
    pub command_timeout: u64,
}

impl Default for RemediationConfig {
    fn default() -> Self {
        Self {
            max_commands: 10,
            max_duration: 30 * 60,
            command_timeout: 120,
        }
    }
}

impl RemediationConfig {
    fn apply_env(&mut self, env: &Overrides<'_>) -> Result<(), ConfigError> {
        env.set("remediation.max_commands", &mut self.max_commands)?;
        env.set("remediation.max_duration", &mut self.max_duration)?;
        env.set("remediation.command_timeout", &mut self.command_timeout)
    }

    fn validate(&self, check: &Check<'_>) -> Result<(), ConfigError> {
        check.positive("remediation.max_commands", self.max_commands.into())?;
        check.positive("remediation.max_duration", self.max_duration)?;
        if self.max_duration > i32::MAX as u64 {
            return Err(check.fail("remediation.max_duration", "is too large"));
        }
        check.positive("remediation.command_timeout", self.command_timeout)?;
        if self.command_timeout > MAX_REMEDIATION_COMMAND_TIMEOUT {
            return Err(check.fail(
                "remediation.command_timeout",
                format!("must be at most {MAX_REMEDIATION_COMMAND_TIMEOUT} seconds"),
            ));
        }
        Ok(())
    }
}

/// Where install commands download the agent from (ADR-0054).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentInstallSource {
    /// The latest GitHub release.
    Github,
    /// The central server's release cache, behind the web interface.
    Central,
    /// An https directory the operator gives.
    Custom,
}

impl AgentInstallSource {
    pub const ALL: [Self; 3] = [Self::Github, Self::Central, Self::Custom];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Github => "github",
            Self::Central => "central",
            Self::Custom => "custom",
        }
    }
}

impl FromStr for AgentInstallSource {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|source| source.as_str() == s.to_ascii_lowercase())
            .ok_or_else(|| {
                format!("unknown install source `{s}`, expected github, central or custom")
            })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AiProviderKind {
    #[default]
    Claude,
}

impl FromStr for AiProviderKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "claude" => Ok(Self::Claude),
            _ => Err(format!("unknown AI provider `{s}`, expected claude")),
        }
    }
}

/// Proxmox VE integration (ADR-0044): VM snapshots before change-tier
/// commands of the remediation loop. Off by default; servers are mapped to
/// VMs through the API.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProxmoxConfig {
    pub enabled: bool,
    /// Snapshots MonitorFlow keeps per VM; older ones it took are deleted.
    pub keep_snapshots: u32,
    /// Seconds one HTTP request may take, connection included.
    pub request_timeout: u64,
    /// Seconds a snapshot, rollback or deletion task may run.
    pub task_timeout: u64,
    /// Only from the file; the environment overrides the keys above.
    pub clusters: Vec<ProxmoxClusterConfig>,
}

impl Default for ProxmoxConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            keep_snapshots: 3,
            request_timeout: 30,
            task_timeout: 600,
            clusters: Vec::new(),
        }
    }
}

/// One Proxmox VE cluster, reached through any of its nodes.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxmoxClusterConfig {
    /// Name used in server mappings, 1 to 64 of `[A-Za-z0-9._-]`.
    pub name: String,
    /// Base URL of the API, like `https://pve1.example.net:8006`.
    pub url: String,
    /// API token ID, `user@realm!tokenid`.
    pub token_id: String,
    /// File holding the token secret alone. The secret never goes in this
    /// file, the environment or logs.
    pub token_secret_file: PathBuf,
    /// PEM CA certificates trusted for this cluster instead of the
    /// built-in web roots, for self-signed clusters. Certificates are
    /// always verified.
    pub ca_file: Option<PathBuf>,
}

impl ProxmoxConfig {
    fn apply_env(&mut self, env: &Overrides<'_>) -> Result<(), ConfigError> {
        env.set("proxmox.enabled", &mut self.enabled)?;
        env.set("proxmox.keep_snapshots", &mut self.keep_snapshots)?;
        env.set("proxmox.request_timeout", &mut self.request_timeout)?;
        env.set("proxmox.task_timeout", &mut self.task_timeout)
    }

    fn validate(&self, check: &Check<'_>) -> Result<(), ConfigError> {
        let mut names = std::collections::HashSet::new();
        for cluster in &self.clusters {
            if !valid_proxmox_cluster_name(&cluster.name) {
                return Err(check.fail(
                    "proxmox.clusters.name",
                    "must be 1 to 64 letters, digits, `.`, `_` or `-`",
                ));
            }
            if !names.insert(cluster.name.as_str()) {
                return Err(check.fail(
                    "proxmox.clusters.name",
                    format!("`{}` is listed twice", cluster.name),
                ));
            }
            let key = |field: &str| format!("proxmox.clusters.{}.{field}", cluster.name);
            check
                .url(&key("url"), &cluster.url, &["https"])
                .map_err(|_| check.fail(&key("url"), "must be an https URL"))?;
            if !valid_proxmox_token_id(&cluster.token_id) {
                return Err(check.fail(&key("token_id"), "must look like user@realm!tokenid"));
            }
            if cluster.token_secret_file.as_os_str().is_empty() {
                return Err(check.fail(&key("token_secret_file"), "is required"));
            }
        }
        if !self.enabled {
            return Ok(());
        }
        if self.clusters.is_empty() {
            return Err(check.fail("proxmox.clusters", "must list at least one cluster"));
        }
        check.positive("proxmox.keep_snapshots", self.keep_snapshots.into())?;
        check.positive("proxmox.request_timeout", self.request_timeout)?;
        check.positive("proxmox.task_timeout", self.task_timeout)
    }
}

/// Whether `name` can name a Proxmox cluster in mappings.
pub fn valid_proxmox_cluster_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-_".contains(&b))
}

/// Whether `id` is a Proxmox API token ID: `user@realm!tokenid`, realm and
/// token ID a letter then letters, digits, `.`, `_` or `-`.
pub fn valid_proxmox_token_id(id: &str) -> bool {
    let ident = |s: &str| {
        s.starts_with(|c: char| c.is_ascii_alphabetic())
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".-_".contains(&b))
    };
    let Some((user_realm, token)) = id.rsplit_once('!') else {
        return false;
    };
    let Some((user, realm)) = user_realm.rsplit_once('@') else {
        return false;
    };
    !user.is_empty()
        && user
            .chars()
            .all(|c| c.is_ascii_graphic() && !"@!=".contains(c))
        && ident(realm)
        && ident(token)
}

impl LoadConfig for ServerConfig {
    fn apply_env(&mut self, env: &Overrides<'_>) -> Result<(), ConfigError> {
        env.set("grpc_listen_addr", &mut self.grpc_listen_addr)?;
        env.set("enroll_listen_addr", &mut self.enroll_listen_addr)?;
        env.set_opt("grpc_public_port", &mut self.grpc_public_port)?;
        env.set_opt("enroll_public_port", &mut self.enroll_public_port)?;
        env.set("api_listen_addr", &mut self.api_listen_addr)?;
        env.set("api_cookie_secure", &mut self.api_cookie_secure)?;
        env.set("api_session_lifetime", &mut self.api_session_lifetime)?;
        env.set(
            "api_session_idle_timeout",
            &mut self.api_session_idle_timeout,
        )?;
        env.set("database_url", &mut self.database_url)?;
        env.set("victoriametrics_url", &mut self.victoriametrics_url)?;
        env.set("victorialogs_url", &mut self.victorialogs_url)?;
        env.set("data_dir", &mut self.data_dir)?;
        env.set("heartbeat_timeout", &mut self.heartbeat_timeout)?;
        if let Some(names) = env.list("server_names") {
            self.server_names = names;
        }
        env.set_opt("web_url", &mut self.web_url)?;
        self.ingest.apply_env(env)?;
        self.tls.apply_env(env)?;
        env.set("mirror_listen_addr", &mut self.mirror_listen_addr)?;
        env.set_opt("mirror_dir", &mut self.mirror_dir)?;
        env.set_opt("mirror_public_url", &mut self.mirror_public_url)?;
        env.set("agent_download_url", &mut self.agent_download_url)?;
        env.set_opt("release_url", &mut self.release_url)?;
        env.set_opt("server_update_dir", &mut self.server_update_dir)?;
        env.set_opt("backup_dir", &mut self.backup_dir)?;
        env.set_opt("agent_install_source", &mut self.agent_install_source)?;
        self.mirror.apply_env(env)?;
        self.ai.apply_env(env)?;
        self.proxmox.apply_env(env)?;
        self.proxmox.apply_env(env)?;
        self.remediation.apply_env(env)?;
        self.auto_update.apply_env(env)?;
        self.log.apply_env(env)
    }

    fn validate(&self, check: &Check<'_>) -> Result<(), ConfigError> {
        check.positive("heartbeat_timeout", self.heartbeat_timeout)?;
        self.mirror.validate(check)?;
        self.ai.validate(check)?;
        self.proxmox.validate(check)?;
        self.remediation.validate(check)?;
        self.auto_update.validate(check)?;
        if let Some(url) = &self.mirror_public_url {
            if !valid_mirror_url(url) {
                return Err(check.fail(
                    "mirror_public_url",
                    "must be an http or https URL without user, query or fragment",
                ));
            }
        }
        if !(self.agent_download_url.starts_with("https://")
            && valid_mirror_url(&self.agent_download_url))
        {
            return Err(check.fail(
                "agent_download_url",
                "must be an https URL without user, query or fragment",
            ));
        }
        if let Some(url) = &self.web_url {
            if !(url.starts_with("https://") && valid_mirror_url(url)) {
                return Err(check.fail(
                    "web_url",
                    "must be an https URL without user, query or fragment",
                ));
            }
        }
        if let Some(url) = &self.release_url {
            if !valid_mirror_url(url) {
                return Err(check.fail(
                    "release_url",
                    "must be an http or https URL without user, query or fragment",
                ));
            }
        }
        if self.agent_install_source == Some(AgentInstallSource::Custom)
            && self.custom_agent_download_url().is_none()
        {
            return Err(check.fail(
                "agent_install_source",
                "`custom` needs agent_download_url set to the files' https URL",
            ));
        }
        if self.server_names.is_empty() || self.server_names.iter().any(String::is_empty) {
            return Err(check.fail(
                "server_names",
                "must list at least one DNS name or IP address, none empty",
            ));
        }
        check.positive("api_session_lifetime", self.api_session_lifetime)?;
        check.positive("api_session_idle_timeout", self.api_session_idle_timeout)?;
        let db = self.database_url.expose();
        check.required("database_url", db)?;
        // The value is never echoed: it may hold a password.
        if !(db.starts_with("postgres://") || db.starts_with("postgresql://")) {
            return Err(check.fail("database_url", "must start with postgres://"));
        }
        check.http_url("victoriametrics_url", &self.victoriametrics_url)?;
        check.http_url("victorialogs_url", &self.victorialogs_url)?;
        self.ingest.validate(check)
    }
}
