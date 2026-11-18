//! Whether a TCP port accepts connections (ADR-0021), or a UDP port
//! answers or at least does not refuse (ADR-0148).
//!
//! Metrics (prefixed with the configured name, `port` by default), with
//! label `target` (`host:port`):
//! - `up`: TCP: 1 when a connection was established before the deadline;
//!   UDP: 1 when the query was answered, or for `query=none` when no ICMP
//!   "port unreachable" came back;
//! - `connect_seconds` (TCP): how long the successful connection took,
//!   omitted when down;
//! - `answered` (UDP): 1 when something answered, 0 otherwise;
//! - `response_seconds` (UDP): how long the answer took, omitted without.
//!
//! Parameters: `port` (required), `host` (default `127.0.0.1`, an IP
//! address or a host name), `protocol` (`tcp`, the default, or `udp`) and,
//! for UDP, `query`: `dns` (a query for the root name servers), `ntp` (a
//! client request), or `none` (one empty datagram; nothing tells an open
//! port that does not answer from a filtered one, only a refusal is
//! certain). By default `dns` on port 53, `ntp` on 123, `none` otherwise.
//! UDP runs from a probe only: agents refuse it (ADR-0148).
//!
//! A host name is resolved once per collection, on the blocking pool and
//! bounded by the deadline; while a lookup is still running, later
//! collections report down without starting another. The connection is
//! closed as soon as it is established; nothing is sent.
//!
//! On the agent, `sensors.port_targets` may limit where it connects
//! ([`PortTargets`], ADR-0111): a target outside it is refused when the
//! configuration is applied, and the addresses a host name resolves to
//! outside it are skipped at every collection.

use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::net::{TcpStream, UdpSocket};
use tokio::time::{timeout_at, Instant};

use crate::registry::{BuildError, Local, Params};
use crate::{Sample, Sensor, SensorError};

mod targets;

pub use targets::{PortTargets, PortTargetsError};

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

/// What a UDP check sends, and what counts as an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UdpQuery {
    Dns,
    Ntp,
    /// One empty datagram: up unless refused.
    None,
}

impl UdpQuery {
    /// The query used on `port` when none is given.
    pub fn default_for(port: u16) -> Self {
        match port {
            53 => Self::Dns,
            123 => Self::Ntp,
            _ => Self::None,
        }
    }

    /// The datagram, with `id` in a DNS query.
    fn datagram(self, id: u16) -> Vec<u8> {
        match self {
            // Header: ID, recursion desired, one question; then the root
            // name, type NS, class IN.
            Self::Dns => {
                let [hi, lo] = id.to_be_bytes();
                vec![hi, lo, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 1]
            }
            // Version 4, client mode, the rest zero.
            Self::Ntp => {
                let mut packet = vec![0u8; 48];
                packet[0] = 0x23;
                packet
            }
            Self::None => Vec::new(),
        }
    }

    /// Whether `reply` answers the datagram sent with `id`.
    fn answers(self, id: u16, reply: &[u8]) -> bool {
        match self {
            Self::Dns => {
                reply.len() >= 12 && reply[..2] == id.to_be_bytes() && reply[2] & 0x80 != 0
            }
            // Server mode.
            Self::Ntp => reply.len() >= 48 && reply[0] & 0x07 == 4,
            Self::None => true,
        }
    }
}

/// Time between two sends of a UDP query that was not answered yet.
const UDP_RESEND: Duration = Duration::from_secs(1);

#[derive(Debug)]
pub struct PortSensor {
    host: Host,
    port: u16,
    /// `None` for TCP.
    udp: Option<UdpQuery>,
    target: String,
    /// The addresses it may connect to, when the host is a name the
    /// agent's `sensors.port_targets` does not list itself.
    allowed: Option<PortTargets>,
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
            udp: None,
            allowed: None,
            resolving: Arc::default(),
        }
    }

    /// Checks the parameters, and the target against the `local` limits
    /// when given.
    pub(crate) fn from_params(
        params: &mut Params<'_>,
        local: Option<&Local>,
    ) -> Result<Self, BuildError> {
        let udp = match params.get("protocol") {
            None | Some("tcp") => false,
            Some("udp") if local.is_some() => {
                return Err(params.refused("UDP checks run from a probe, not an agent (ADR-0148)"))
            }
            Some("udp") => true,
            Some(_) => return Err(params.bad("protocol", "must be `tcp` or `udp`")),
        };
        let query = match params.get("query") {
            None => None,
            Some(_) if !udp => return Err(params.bad("query", "is for `protocol` `udp` only")),
            Some("dns") => Some(UdpQuery::Dns),
            Some("ntp") => Some(UdpQuery::Ntp),
            Some("none") => Some(UdpQuery::None),
            Some(_) => return Err(params.bad("query", "must be `dns`, `ntp` or `none`")),
        };
        let host = match params.get("host") {
            None => Host::Ip(IpAddr::from([127, 0, 0, 1])),
            Some(host) => parse_host(host).map_err(|reason| params.bad("host", reason))?,
        };
        let port = match params.get("port") {
            None => return Err(params.bad("port", "is required")),
            Some(port) => port.parse::<u16>().ok().filter(|p| *p > 0),
        };
        let port = port.ok_or_else(|| params.bad("port", "must be between 1 and 65535"))?;
        let mut sensor = Self::new(host, port);
        if udp {
            sensor.udp = Some(query.unwrap_or_else(|| UdpQuery::default_for(port)));
        }
        if let Some(targets) = local.and_then(|l| l.port_targets.as_ref()) {
            sensor.allowed = sensor
                .check(targets)
                .map_err(|reason| params.refused(reason))?;
        }
        Ok(sensor)
    }

    /// Whether `targets` allows this sensor's target: `Err` with the reason
    /// when not, otherwise the list to check resolved addresses against,
    /// if any.
    fn check(&self, targets: &PortTargets) -> Result<Option<PortTargets>, String> {
        if targets.is_empty() {
            return Err("this agent checks no ports: its sensors.port_targets is empty".into());
        }
        let allowed = match &self.host {
            Host::Ip(ip) if targets.allows_ip(*ip, self.port) => return Ok(None),
            Host::Name(name) if targets.allows_name(name, self.port) => return Ok(None),
            Host::Name(_) => targets.has_range_for(self.port),
            Host::Ip(_) => false,
        };
        if !allowed {
            return Err(format!(
                "`{}` is not in its sensors.port_targets",
                self.target
            ));
        }
        Ok(Some(targets.clone()))
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
        let mut addrs = self.resolve(cutoff).await;
        if let Some(allowed) = &self.allowed {
            let resolved = addrs.len();
            addrs.retain(|a| allowed.allows_ip(a.ip(), a.port()));
            if resolved > 0 && addrs.is_empty() {
                tracing::warn!(
                    endpoint = %self.target,
                    "the host name resolves only to addresses outside sensors.port_targets; reporting down"
                );
            }
        }
        if let Some(query) = self.udp {
            let Some(addr) = addrs.first() else {
                return Ok(vec![sample("up", 0.0), sample("answered", 0.0)]);
            };
            let (up, answered) = udp_check(*addr, query, cutoff).await;
            let mut samples = vec![
                sample("up", f64::from(u8::from(up))),
                sample("answered", f64::from(u8::from(answered.is_some()))),
            ];
            if let Some(took) = answered {
                samples.push(sample("response_seconds", took.as_secs_f64()));
            }
            return Ok(samples);
        }
        for addr in addrs {
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

/// Sends `query` to `addr` until `cutoff`: whether the port is up, and
/// how long the answer took when there was one. A refusal (ICMP port
/// unreachable, which Linux reports on a connected socket) is down at
/// once.
async fn udp_check(addr: SocketAddr, query: UdpQuery, cutoff: Instant) -> (bool, Option<Duration>) {
    let bind: SocketAddr = if addr.is_ipv4() {
        (IpAddr::from([0, 0, 0, 0]), 0).into()
    } else {
        (IpAddr::from([0u16; 8]), 0).into()
    };
    let socket = match UdpSocket::bind(bind).await {
        Ok(socket) => socket,
        Err(error) => {
            tracing::debug!(%addr, %error, "cannot open a UDP socket");
            return (false, None);
        }
    };
    if let Err(error) = socket.connect(addr).await {
        tracing::debug!(%addr, %error, "UDP connect failed");
        return (false, None);
    }
    let id = std::process::id() as u16 ^ (Instant::now().elapsed().subsec_nanos() as u16);
    let datagram = query.datagram(id);
    let started = Instant::now();
    let mut buf = vec![0u8; 1500];
    loop {
        if socket.send(&datagram).await.is_err() {
            return (false, None);
        }
        let wait = (Instant::now() + UDP_RESEND).min(cutoff);
        loop {
            match timeout_at(wait, socket.recv(&mut buf)).await {
                Ok(Ok(n)) if query.answers(id, &buf[..n]) => {
                    return (true, Some(started.elapsed()));
                }
                // Something else: keep waiting.
                Ok(Ok(_)) => continue,
                Ok(Err(error)) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
                    return (false, None);
                }
                Ok(Err(error)) => {
                    tracing::debug!(%addr, %error, "UDP receive failed");
                    return (false, None);
                }
                Err(_) => break,
            }
        }
        if Instant::now() >= cutoff {
            // No answer: only a check that expects none is up.
            return (query == UdpQuery::None, None);
        }
    }
}

#[cfg(test)]
mod tests;
