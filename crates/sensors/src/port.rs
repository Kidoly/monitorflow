//! Whether a TCP port accepts connections. See ADR-0021.
//!
//! Metrics (prefixed with the configured name, `port` by default), with
//! label `target` (`host:port`):
//! - `up`: 1 when a connection was established before the deadline;
//! - `connect_seconds`: how long the successful connection took, omitted
//!   when down.
//!
//! Parameters: `port` (required), `host` (default `127.0.0.1`, an IP
//! address or a host name) and `protocol` (only `tcp`).
//!
//! A host name is resolved once per collection, on the blocking pool and
//! bounded by the deadline; while a lookup is still running, later
//! collections report down without starting another. The connection is
//! closed as soon as it is established; nothing is sent.

use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpStream;
use tokio::time::{timeout_at, Instant};

use crate::registry::{BuildError, Params};
use crate::{Sample, Sensor, SensorError};

/// Time kept before the deadline to return `up 0` rather than be
/// cancelled with nothing, which could not alert.
const MARGIN: Duration = Duration::from_millis(200);
/// Longest DNS name.
const HOST_MAX: usize = 253;

/// Where to connect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Host {
    Ip(IpAddr),
    Name(String),
}

/// Checks `host`: an IP address, with or without brackets for IPv6, or a
/// DNS name of letters, digits, `-`, `_` and dots.
pub fn parse_host(host: &str) -> Result<Host, String> {
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    if let Ok(ip) = bare.parse() {
        return Ok(Host::Ip(ip));
    }
    let labels_ok = host.split('.').all(|label| {
        !label.is_empty()
            && !label.starts_with('-')
            && label
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    });
    if host.len() > HOST_MAX || !labels_ok {
        return Err(format!(
            "`{}` is neither an IP address nor a host name",
            host.chars().take(64).collect::<String>().escape_debug()
        ));
    }
    Ok(Host::Name(host.to_owned()))
}

/// `host:port`, with brackets around an IPv6 address.
pub fn target(host: &Host, port: u16) -> String {
    match host {
        Host::Ip(IpAddr::V6(ip)) => format!("[{ip}]:{port}"),
        Host::Ip(ip) => format!("{ip}:{port}"),
        Host::Name(name) => format!("{name}:{port}"),
    }
}

#[derive(Debug)]
pub struct PortSensor {
    host: Host,
    port: u16,
    target: String,
    /// A lookup is still running on the blocking pool.
    resolving: Arc<AtomicBool>,
}

/// Clears the resolving flag when the blocking lookup returns.
struct Resolving(Arc<AtomicBool>);

impl Drop for Resolving {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl PortSensor {
    pub fn new(host: Host, port: u16) -> Self {
        Self {
            target: target(&host, port),
            host,
            port,
            resolving: Arc::default(),
        }
    }

    pub(crate) fn from_params(params: &mut Params<'_>) -> Result<Self, BuildError> {
        match params.get("protocol") {
            None | Some("tcp") => {}
            Some("udp") => {
                return Err(params.bad("protocol", "`udp` is not supported, see ADR-0021"))
            }
            Some(_) => return Err(params.bad("protocol", "must be `tcp`")),
        }
        let host = match params.get("host") {
            None => Host::Ip(IpAddr::from([127, 0, 0, 1])),
            Some(host) => parse_host(host).map_err(|reason| params.bad("host", reason))?,
        };
        let port = match params.get("port") {
            None => return Err(params.bad("port", "is required")),
            Some(port) => port.parse::<u16>().ok().filter(|p| *p > 0),
        };
        let port = port.ok_or_else(|| params.bad("port", "must be between 1 and 65535"))?;
        Ok(Self::new(host, port))
    }

    /// The addresses to try, in resolver order; empty when the lookup
    /// failed, is still running or did not answer before `cutoff`.
    async fn resolve(&self, cutoff: Instant) -> Vec<SocketAddr> {
        let name = match &self.host {
            Host::Ip(ip) => return vec![SocketAddr::new(*ip, self.port)],
            Host::Name(name) => name.clone(),
        };
        if self.resolving.swap(true, Ordering::SeqCst) {
            tracing::debug!(endpoint = %self.target, "previous lookup still running");
            return Vec::new();
        }
        let guard = Resolving(self.resolving.clone());
        let port = self.port;
        let lookup = tokio::task::spawn_blocking(move || {
            let _guard = guard;
            (name.as_str(), port).to_socket_addrs()
        });
        match timeout_at(cutoff, lookup).await {
            Ok(Ok(Ok(addrs))) => addrs.collect(),
            Ok(Ok(Err(error))) => {
                tracing::debug!(endpoint = %self.target, %error, "lookup failed");
                Vec::new()
            }
            // A panicked task, or a lookup that outlives the deadline; the
            // guard clears the flag when it returns.
            Ok(Err(_)) | Err(_) => Vec::new(),
        }
    }
}

#[async_trait::async_trait]
impl Sensor for PortSensor {
    async fn collect(&self, deadline: Instant) -> Result<Vec<Sample>, SensorError> {
        let now = Instant::now();
        let cutoff = deadline - MARGIN.min(deadline.saturating_duration_since(now) / 2);
        let sample = |metric: &str, value: f64| {
            Sample::metric(metric, value).with_label("target", &self.target)
        };
        for addr in self.resolve(cutoff).await {
            let started = Instant::now();
            match timeout_at(cutoff, TcpStream::connect(addr)).await {
                Ok(Ok(_stream)) => {
                    let took = started.elapsed().as_secs_f64();
                    return Ok(vec![sample("up", 1.0), sample("connect_seconds", took)]);
                }
                Ok(Err(error)) => tracing::debug!(%addr, %error, "connect failed"),
                Err(_) => break,
            }
        }
        Ok(vec![sample("up", 0.0)])
    }
}

#[cfg(test)]
mod tests;
