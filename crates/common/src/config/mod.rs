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

#[cfg(test)]
mod tests;

/// Prefix of every environment override.
pub const ENV_PREFIX: &str = "MONITORFLOW_";

/// Seconds between agent heartbeats unless configured otherwise.
pub const DEFAULT_HEARTBEAT_INTERVAL: u64 = 30;

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

impl TlsPaths {
    fn apply_env(&mut self, env: &Overrides<'_>) -> Result<(), ConfigError> {
        env.set("tls.ca_cert", &mut self.ca_cert)?;
        env.set("tls.cert", &mut self.cert)?;
        env.set("tls.key", &mut self.key)
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentConfig {
    /// gRPC endpoint of a relay or the server, `https://host:port`.
    pub upstream_url: String,
    pub data_dir: PathBuf,
    /// One-time token, only needed for the first run.
    pub enrollment_token: Option<Secret>,
    /// Seconds between heartbeats.
    pub heartbeat_interval: u64,
    pub tls: TlsPaths,
    pub log: LogConfig,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            upstream_url: String::new(),
            data_dir: "/var/lib/monitorflow/agent".into(),
            enrollment_token: None,
            heartbeat_interval: DEFAULT_HEARTBEAT_INTERVAL,
            tls: TlsPaths::default(),
            log: LogConfig::default(),
        }
    }
}

impl LoadConfig for AgentConfig {
    fn apply_env(&mut self, env: &Overrides<'_>) -> Result<(), ConfigError> {
        env.set("upstream_url", &mut self.upstream_url)?;
        env.set("data_dir", &mut self.data_dir)?;
        env.set_opt("enrollment_token", &mut self.enrollment_token)?;
        env.set("heartbeat_interval", &mut self.heartbeat_interval)?;
        self.tls.apply_env(env)?;
        self.log.apply_env(env)
    }

    fn validate(&self, check: &Check<'_>) -> Result<(), ConfigError> {
        check.grpc_url("upstream_url", &self.upstream_url)?;
        check.positive("heartbeat_interval", self.heartbeat_interval)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RelayConfig {
    /// gRPC listener for agents and downstream relays.
    pub listen_addr: SocketAddr,
    /// gRPC endpoint of the next relay or the server, `https://host:port`.
    pub upstream_url: String,
    pub data_dir: PathBuf,
    /// Maximum size of the store-and-forward buffer on disk.
    pub buffer_max_bytes: u64,
    pub tls: TlsPaths,
    pub log: LogConfig,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            listen_addr: SocketAddr::from(([0, 0, 0, 0], 7443)),
            upstream_url: String::new(),
            data_dir: "/var/lib/monitorflow/relay".into(),
            buffer_max_bytes: 1024 * 1024 * 1024,
            tls: TlsPaths::default(),
            log: LogConfig::default(),
        }
    }
}

impl LoadConfig for RelayConfig {
    fn apply_env(&mut self, env: &Overrides<'_>) -> Result<(), ConfigError> {
        env.set("listen_addr", &mut self.listen_addr)?;
        env.set("upstream_url", &mut self.upstream_url)?;
        env.set("data_dir", &mut self.data_dir)?;
        env.set("buffer_max_bytes", &mut self.buffer_max_bytes)?;
        self.tls.apply_env(env)?;
        self.log.apply_env(env)
    }

    fn validate(&self, check: &Check<'_>) -> Result<(), ConfigError> {
        check.grpc_url("upstream_url", &self.upstream_url)?;
        check.positive("buffer_max_bytes", self.buffer_max_bytes)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    /// gRPC listener for agents and relays.
    pub grpc_listen_addr: SocketAddr,
    /// Enrollment listener: TLS without client certificates, serving only
    /// `EnrollService` (ADR-0006).
    pub enroll_listen_addr: SocketAddr,
    /// HTTPS listener for the REST API and web interface.
    pub api_listen_addr: SocketAddr,
    pub database_url: DatabaseUrl,
    pub victoriametrics_url: String,
    pub victorialogs_url: String,
    pub data_dir: PathBuf,
    /// Seconds without any message after which a peer's stream is closed.
    pub heartbeat_timeout: u64,
    /// DNS names and IP addresses peers use to reach this server, written
    /// in the certificate the built-in CA issues it (ADR-0006). The
    /// environment override is a comma-separated list.
    pub server_names: Vec<String>,
    pub tls: TlsPaths,
    pub log: LogConfig,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            grpc_listen_addr: SocketAddr::from(([0, 0, 0, 0], 7443)),
            enroll_listen_addr: SocketAddr::from(([0, 0, 0, 0], 7445)),
            api_listen_addr: SocketAddr::from(([0, 0, 0, 0], 7444)),
            database_url: DatabaseUrl::default(),
            victoriametrics_url: "http://localhost:8428".into(),
            victorialogs_url: "http://localhost:9428".into(),
            data_dir: "/var/lib/monitorflow/server".into(),
            // Three missed heartbeats at the agent's default interval.
            heartbeat_timeout: 3 * DEFAULT_HEARTBEAT_INTERVAL,
            server_names: vec!["localhost".into(), "127.0.0.1".into()],
            tls: TlsPaths::default(),
            log: LogConfig::default(),
        }
    }
}

impl LoadConfig for ServerConfig {
    fn apply_env(&mut self, env: &Overrides<'_>) -> Result<(), ConfigError> {
        env.set("grpc_listen_addr", &mut self.grpc_listen_addr)?;
        env.set("enroll_listen_addr", &mut self.enroll_listen_addr)?;
        env.set("api_listen_addr", &mut self.api_listen_addr)?;
        env.set("database_url", &mut self.database_url)?;
        env.set("victoriametrics_url", &mut self.victoriametrics_url)?;
        env.set("victorialogs_url", &mut self.victorialogs_url)?;
        env.set("data_dir", &mut self.data_dir)?;
        env.set("heartbeat_timeout", &mut self.heartbeat_timeout)?;
        if let Some((_, names)) = env.get("server_names") {
            self.server_names = names.split(',').map(|n| n.trim().to_owned()).collect();
        }
        self.tls.apply_env(env)?;
        self.log.apply_env(env)
    }

    fn validate(&self, check: &Check<'_>) -> Result<(), ConfigError> {
        check.positive("heartbeat_timeout", self.heartbeat_timeout)?;
        if self.server_names.is_empty() || self.server_names.iter().any(String::is_empty) {
            return Err(check.fail(
                "server_names",
                "must list at least one DNS name or IP address, none empty",
            ));
        }
        let db = self.database_url.expose();
        check.required("database_url", db)?;
        // The value is never echoed: it may hold a password.
        if !(db.starts_with("postgres://") || db.starts_with("postgresql://")) {
            return Err(check.fail("database_url", "must start with postgres://"));
        }
        check.http_url("victoriametrics_url", &self.victoriametrics_url)?;
        check.http_url("victorialogs_url", &self.victorialogs_url)
    }
}
