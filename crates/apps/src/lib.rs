//! Ready-made application checks (ADR-0183): PostgreSQL, MySQL and
//! MariaDB, Redis, Java virtual machines through Jolokia, and Docker, as
//! kinds of check with their metrics and default alerts ([`templates`])
//! instead of scripts.
//!
//! `postgresql`, `mysql`, `redis` and `jmx` connect over the network, so the
//! server's agent runs them against `127.0.0.1` (within its
//! `sensors.port_targets`, ADR-0111) and a probe, the central server or a
//! relay (ADR-0148), against the server's or device's address. `docker`
//! reads the Docker Engine API on the local socket: agents only, on Linux.
//!
//! Every kind reports `up`: 1 when the application answered and the login
//! worked, 0 with labels `reason` (`connect`, `timeout`, `tls`, `login`,
//! `permission`, `protocol`) and `error`, so a stopped database alerts
//! like a failed check. The password ([`SECRET_PARAM`]) is kept sealed by
//! the server and given only to whoever runs the check; it is never in a
//! sample, a label or a log line.

pub mod docker;
pub mod jmx;
pub mod mysql;
mod params;
pub mod postgresql;
mod rates;
pub mod redis;
mod resp;
pub mod templates;
mod unix_http;

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use monitorflow_sensors::port::{parse_host, Host, PortTargets};
use monitorflow_sensors::{BuildError, Sample, Sensor};

pub use params::Params;

/// PostgreSQL, from 10 on.
pub const POSTGRESQL: &str = "postgresql";
/// MySQL from 5.7 and MariaDB from 10.3.
pub const MYSQL: &str = "mysql";
/// Redis from 5, and servers that speak its protocol (Valkey, KeyDB).
pub const REDIS: &str = "redis";
/// A Java virtual machine, read through Jolokia's HTTP bridge to JMX.
pub const JMX: &str = "jmx";
/// The containers of the local Docker or Podman engine.
pub const DOCKER: &str = "docker";
/// Every kind of this crate.
pub const KINDS: [&str; 5] = [POSTGRESQL, MYSQL, REDIS, JMX, DOCKER];
/// The kinds a probe runs as well as an agent: those over the network.
pub const NETWORK_KINDS: [&str; 4] = [POSTGRESQL, MYSQL, REDIS, JMX];
/// The parameter holding the password: sealed by the server, never stored
/// with the check's other parameters nor returned.
pub const SECRET_PARAM: &str = "password";

/// Time kept before the deadline to report `up 0` rather than be
/// cancelled with nothing, which could not alert.
const MARGIN: Duration = Duration::from_millis(300);
/// Longest `error` label.
const MAX_ERROR: usize = 200;

/// Whether `kind` is one of this crate's.
pub fn is_kind(kind: &str) -> bool {
    KINDS.contains(&kind)
}

/// Whether checks of `kind` take a password.
pub fn takes_secret(kind: &str) -> bool {
    NETWORK_KINDS.contains(&kind)
}

/// Who builds the check, and within which limits.
#[derive(Debug, Clone, Copy)]
pub enum Place<'a> {
    /// A server's agent, with its `sensors.port_targets` when set.
    Agent(Option<&'a PortTargets>),
    /// A relay.
    Relay,
    /// The central server, which serves every customer: it never connects
    /// to a link-local address, as website checks do not (ADR-0150).
    Central,
    /// The server checking a row before it stores it.
    Check,
}

/// Checks `params` and builds the check of `kind`.
pub fn build(
    kind: &str,
    params: &BTreeMap<String, String>,
    place: Place<'_>,
) -> Result<Arc<dyn Sensor>, BuildError> {
    let Some(kind) = KINDS.iter().copied().find(|k| *k == kind) else {
        return Err(BuildError::UnknownKind(kind.to_owned()));
    };
    let mut p = Params::new(kind, params);
    let sensor: Arc<dyn Sensor> = match kind {
        POSTGRESQL => Arc::new(postgresql::Postgresql::from_params(&mut p, place)?),
        MYSQL => Arc::new(mysql::Mysql::from_params(&mut p, place)?),
        REDIS => Arc::new(redis::Redis::from_params(&mut p, place)?),
        JMX => Arc::new(jmx::Jmx::from_params(&mut p, place)?),
        _ => Arc::new(docker::Docker::from_params(&mut p, place)?),
    };
    p.finish()?;
    Ok(sensor)
}

/// How a connection is encrypted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tls {
    /// Plain text.
    Off,
    /// Encrypted when the server offers it, its certificate not verified:
    /// what PostgreSQL's and MySQL's own clients do by default.
    Prefer,
    /// Encrypted, the certificate verified against the machine's roots and
    /// the host name.
    Verify,
}

impl Tls {
    fn parse(p: &mut Params<'_>, default: Self) -> Result<Self, BuildError> {
        match p.get("tls") {
            None => Ok(default),
            Some("off") => Ok(Self::Off),
            Some("prefer") => Ok(Self::Prefer),
            Some("verify") => Ok(Self::Verify),
            Some(_) => Err(p.bad("tls", "must be `off`, `prefer` or `verify`")),
        }
    }
}

/// Where and as whom a network check connects.
#[derive(Clone, PartialEq, Eq)]
pub struct Target {
    /// As given, without brackets around an IPv6 address.
    pub host: String,
    pub port: u16,
    pub user: Option<String>,
    pub password: Option<String>,
    pub tls: Tls,
}

impl std::fmt::Debug for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Target")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("user", &self.user)
            .field("password", &self.password.as_ref().map(|_| "set"))
            .field("tls", &self.tls)
            .finish()
    }
}

/// Longest user name or password.
const MAX_CREDENTIAL: usize = 256;

impl Target {
    /// Reads `host`, `port`, `user`, `password` and `tls`, and checks the
    /// address against `place`'s limits.
    fn parse(
        p: &mut Params<'_>,
        place: Place<'_>,
        default_port: u16,
        default_user: Option<&str>,
        default_tls: Tls,
    ) -> Result<Self, BuildError> {
        let raw = p.get("host").unwrap_or("127.0.0.1");
        let host = parse_host(raw).map_err(|reason| p.bad("host", reason))?;
        let port = match p.get("port") {
            None => default_port,
            Some(port) => port
                .parse::<u16>()
                .ok()
                .filter(|p| *p > 0)
                .ok_or_else(|| p.bad("port", "must be between 1 and 65535"))?,
        };
        let user = match p.get("user") {
            Some("") | None => default_user.map(str::to_owned),
            Some(user) => Some(user.to_owned()),
        };
        if user
            .as_deref()
            .is_some_and(|u| u.len() > MAX_CREDENTIAL || u.chars().any(char::is_control))
        {
            return Err(p.bad(
                "user",
                "must be at most 256 bytes without control characters",
            ));
        }
        let password = p.get_raw(SECRET_PARAM).filter(|s| !s.is_empty());
        if password.is_some_and(|s| s.len() > MAX_CREDENTIAL || s.contains(['\r', '\n', '\0'])) {
            return Err(p.bad(
                SECRET_PARAM,
                "must be at most 256 bytes without line breaks",
            ));
        }
        let tls = Tls::parse(p, default_tls)?;
        check_place(p, place, &host, port)?;
        let host = match host {
            Host::Ip(ip) => ip.to_string(),
            Host::Name(name) => name,
        };
        Ok(Self {
            host,
            port,
            user,
            password: password.map(str::to_owned),
            tls,
        })
    }

    /// `text` without the password, should an error ever quote it.
    fn scrub(&self, text: &str) -> String {
        match &self.password {
            Some(password) if password.len() >= 3 => text.replace(password.as_str(), "***"),
            _ => text.to_owned(),
        }
    }
}

/// Refuses an address `place` may not reach: outside an agent's
/// `sensors.port_targets`, or link-local from the central server.
fn check_place(p: &Params<'_>, place: Place<'_>, host: &Host, port: u16) -> Result<(), BuildError> {
    match place {
        Place::Agent(Some(targets)) => {
            if targets.is_empty() {
                return Err(
                    p.refused("this agent connects nowhere: its sensors.port_targets is empty")
                );
            }
            let allowed = match host {
                Host::Ip(ip) => targets.allows_ip(*ip, port),
                Host::Name(name) => targets.allows_name(name, port),
            };
            if !allowed {
                return Err(p.refused(format!(
                    "{} is not in this agent's sensors.port_targets",
                    monitorflow_sensors::port::target(host, port)
                )));
            }
            Ok(())
        }
        Place::Central => match host {
            Host::Ip(ip) if link_local(*ip) => {
                Err(p.refused("the central server does not connect to link-local addresses"))
            }
            _ => Ok(()),
        },
        Place::Agent(None) | Place::Relay | Place::Check => Ok(()),
    }
}

fn link_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_link_local(),
        IpAddr::V6(ip) => {
            ip.to_ipv4_mapped().is_some_and(|v4| v4.is_link_local())
                || (ip.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

/// Why a collection did not reach the application, as `up`'s `reason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    Connect,
    Timeout,
    Tls,
    Login,
    Permission,
    Protocol,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Connect => "connect",
            Self::Timeout => "timeout",
            Self::Tls => "tls",
            Self::Login => "login",
            Self::Permission => "permission",
            Self::Protocol => "protocol",
        }
    }
}

/// A failure to collect, with what to say about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub reason: Reason,
    pub error: String,
}

impl Failure {
    pub fn new(reason: Reason, error: impl Into<String>) -> Self {
        Self {
            reason,
            error: error.into(),
        }
    }
}

/// `up 0` with the failure's reason and error, the password scrubbed.
fn down(target: Option<&Target>, failure: &Failure) -> Sample {
    let error = target.map_or_else(|| failure.error.clone(), |t| t.scrub(&failure.error));
    Sample::metric("up", 0.0)
        .with_label("reason", failure.reason.as_str())
        .with_label("error", label(&error, MAX_ERROR))
}

/// `text` without control characters, cut to `max` bytes on a character
/// boundary.
pub(crate) fn label(text: &str, max: usize) -> String {
    let mut out = String::new();
    for c in text.chars().filter(|c| !c.is_control()) {
        if out.len() + c.len_utf8() > max {
            break;
        }
        out.push(c);
    }
    out
}

/// Runs `work` until `deadline` less [`MARGIN`]; a timeout is a failure.
async fn within<T>(
    deadline: tokio::time::Instant,
    work: impl std::future::Future<Output = Result<T, Failure>>,
) -> Result<T, Failure> {
    let cutoff = deadline
        .checked_sub(MARGIN)
        .unwrap_or_else(tokio::time::Instant::now);
    match tokio::time::timeout_at(cutoff, work).await {
        Ok(result) => result,
        Err(_) => Err(Failure::new(
            Reason::Timeout,
            "no answer before the check's timeout",
        )),
    }
}

/// A sample of `metric` at `value`, finite values only.
fn gauge(metric: &str, value: f64) -> Option<Sample> {
    value.is_finite().then(|| Sample::metric(metric, value))
}

/// `part / whole`, when `whole` is positive.
fn ratio(part: f64, whole: f64) -> Option<f64> {
    (whole > 0.0 && part.is_finite()).then(|| (part / whole).clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests;
