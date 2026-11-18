//! Whether an address answers ICMP echo, how fast and how many packets
//! are lost. A probe kind (ADR-0148): the central server and relays run
//! it, agents refuse it.
//!
//! Metrics (prefixed with the configured name, `ping` by default), with
//! label `target` (the host as configured):
//! - `up`: 1 when at least one echo was answered;
//! - `rtt_seconds`: the average round-trip time of the answered echoes,
//!   omitted when none was;
//! - `loss_ratio`: the share of echoes not answered, 0 to 1.
//!
//! Parameters: `host` (required, an IP address or a host name) and
//! `count` (echoes per collection, 1 to 20, default 5). The echoes leave
//! [`GAP`] apart and each waits at most [`WAIT`], all within the
//! collection's deadline; a host name is resolved once per collection.
//!
//! The socket is an unprivileged ICMP datagram socket, allowed by Linux's
//! `net.ipv4.ping_group_range` (open to every group by systemd and by
//! Docker), or a raw socket when the process has `CAP_NET_RAW`. Without
//! either the check fails with a permanent error that says so.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use tokio::time::{timeout_at, Instant};

use crate::port::{parse_host, Host};
use crate::registry::{BuildError, Params};
use crate::{Sample, Sensor, SensorError};

/// Time between two echoes of one collection.
pub const GAP: Duration = Duration::from_millis(200);
/// Longest wait for one answer.
pub const WAIT: Duration = Duration::from_secs(2);
/// Echoes per collection by default.
pub const DEFAULT_COUNT: u32 = 5;
const MAX_COUNT: u32 = 20;
/// Time kept before the deadline to report what was measured.
const MARGIN: Duration = Duration::from_millis(200);

#[derive(Debug)]
pub struct PingSensor {
    host: Host,
    target: String,
    count: u32,
}

impl PingSensor {
    pub fn new(host: Host, count: u32) -> Self {
        let target = match &host {
            Host::Ip(ip) => ip.to_string(),
            Host::Name(name) => name.clone(),
        };
        Self {
            host,
            target,
            count,
        }
    }

    pub(crate) fn from_params(params: &mut Params<'_>) -> Result<Self, BuildError> {
        let host = match params.get("host") {
            None => return Err(params.bad("host", "is required")),
            Some(host) => parse_host(host).map_err(|reason| params.bad("host", reason))?,
        };
        let count = match params.get("count") {
            None => DEFAULT_COUNT,
            Some(count) => count
                .parse()
                .ok()
                .filter(|c| (1..=MAX_COUNT).contains(c))
                .ok_or_else(|| params.bad("count", format!("must be between 1 and {MAX_COUNT}")))?,
        };
        Ok(Self::new(host, count))
    }

    async fn address(&self, cutoff: Instant) -> Option<IpAddr> {
        let name = match &self.host {
            Host::Ip(ip) => return Some(*ip),
            Host::Name(name) => name.clone(),
        };
        match timeout_at(cutoff, tokio::net::lookup_host((name, 0))).await {
            Ok(Ok(mut addrs)) => addrs.next().map(|a: SocketAddr| a.ip()),
            Ok(Err(error)) => {
                tracing::debug!(target = %self.target, %error, "lookup failed");
                None
            }
            Err(_) => None,
        }
    }
}

/// What one collection measured.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub sent: u32,
    pub answered: Vec<Duration>,
}

impl Outcome {
    /// The samples of `outcome`, labelled with `target`.
    pub fn samples(&self, target: &str) -> Vec<Sample> {
        let sample =
            |metric: &str, value: f64| Sample::metric(metric, value).with_label("target", target);
        let answered = self.answered.len() as f64;
        let loss = if self.sent == 0 {
            1.0
        } else {
            1.0 - answered / f64::from(self.sent)
        };
        let mut samples = vec![
            sample("up", if self.answered.is_empty() { 0.0 } else { 1.0 }),
            sample("loss_ratio", loss),
        ];
        if !self.answered.is_empty() {
            let total: Duration = self.answered.iter().sum();
            samples.push(sample("rtt_seconds", total.as_secs_f64() / answered));
        }
        samples
    }
}

#[cfg(unix)]
async fn echo(ip: IpAddr, count: u32, cutoff: Instant) -> Result<Outcome, SensorError> {
    use surge_ping::{Client, Config, PingIdentifier, PingSequence, ICMP};
    let kind = if ip.is_ipv4() { ICMP::V4 } else { ICMP::V6 };
    let datagram = Config::builder().kind(kind).build();
    let client = match Client::new(&datagram) {
        Ok(client) => client,
        Err(dgram) => {
            let raw = Config::builder()
                .kind(kind)
                .sock_type_hint(socket2::Type::RAW)
                .build();
            Client::new(&raw).map_err(|raw| {
                SensorError::Permanent(format!(
                    "cannot open an ICMP socket: {dgram} (datagram), {raw} (raw); allow this \
                     process's group in net.ipv4.ping_group_range or give it CAP_NET_RAW"
                ))
            })?
        }
    };
    let ident = PingIdentifier(std::process::id() as u16 ^ rand_ident());
    let payload = [0u8; 56];
    let mut outcome = Outcome {
        sent: 0,
        answered: Vec::new(),
    };
    let mut waits = tokio::task::JoinSet::new();
    for seq in 0..count {
        let now = Instant::now();
        if now >= cutoff {
            break;
        }
        let mut one = client.pinger(ip, ident).await;
        one.timeout(WAIT.min(cutoff - now));
        outcome.sent += 1;
        waits.spawn(async move { one.ping(PingSequence(seq as u16), &payload).await });
        if seq + 1 < count {
            tokio::time::sleep(GAP.min(cutoff.saturating_duration_since(Instant::now()))).await;
        }
    }
    while let Some(result) = waits.join_next().await {
        match result {
            Ok(Ok((_, rtt))) => outcome.answered.push(rtt),
            Ok(Err(surge_ping::SurgeError::Timeout { .. })) | Err(_) => {}
            Ok(Err(error)) => tracing::debug!(%ip, %error, "echo failed"),
        }
    }
    Ok(outcome)
}

/// Varies the identifier between collections, so answers to an echo of a
/// cancelled collection are not counted in the next one.
#[cfg(unix)]
fn rand_ident() -> u16 {
    use std::hash::BuildHasher;
    std::collections::hash_map::RandomState::new().hash_one(Instant::now()) as u16
}

#[cfg(not(unix))]
async fn echo(_: IpAddr, _: u32, _: Instant) -> Result<Outcome, SensorError> {
    Err(SensorError::Permanent(
        "ping checks run from a probe on Linux".into(),
    ))
}

#[async_trait::async_trait]
impl Sensor for PingSensor {
    async fn collect(&self, deadline: Instant) -> Result<Vec<Sample>, SensorError> {
        let now = Instant::now();
        let cutoff = deadline - MARGIN.min(deadline.saturating_duration_since(now) / 2);
        let outcome = match self.address(cutoff).await {
            Some(ip) => echo(ip, self.count, cutoff).await?,
            None => Outcome {
                sent: self.count,
                answered: Vec::new(),
            },
        };
        Ok(outcome.samples(&self.target))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn params(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn params_are_checked() {
        let build = |pairs: &[(&str, &str)]| crate::registry::build("ping", &params(pairs));
        assert!(build(&[])
            .err()
            .unwrap()
            .to_string()
            .contains("`host` is required"));
        assert!(build(&[("host", "192.0.2.1")]).is_ok());
        assert!(build(&[("host", "nas.lan"), ("count", "20")]).is_ok());
        for count in ["0", "21", "x", "-1"] {
            assert!(
                build(&[("host", "a"), ("count", count)]).is_err(),
                "{count}"
            );
        }
        assert!(build(&[("host", "a b")]).is_err());
        assert!(build(&[("host", "a"), ("port", "1")]).is_err());
    }

    #[test]
    fn outcomes_become_samples() {
        let values = |o: &Outcome| -> BTreeMap<String, f64> {
            o.samples("nas")
                .into_iter()
                .inspect(|s| assert_eq!(s.labels["target"], "nas"))
                .map(|s| (s.sensor, s.value))
                .collect()
        };
        let none = values(&Outcome {
            sent: 5,
            answered: vec![],
        });
        assert_eq!(none["up"], 0.0);
        assert_eq!(none["loss_ratio"], 1.0);
        assert!(!none.contains_key("rtt_seconds"));
        let some = values(&Outcome {
            sent: 4,
            answered: vec![Duration::from_millis(10), Duration::from_millis(30)],
        });
        assert_eq!(some["up"], 1.0);
        assert_eq!(some["loss_ratio"], 0.5);
        assert!((some["rtt_seconds"] - 0.02).abs() < 1e-9);
        assert_eq!(
            values(&Outcome {
                sent: 0,
                answered: vec![]
            })["loss_ratio"],
            1.0
        );
    }

    /// Loopback answers, when this host lets the process open an ICMP
    /// socket (CI runners and Docker do); otherwise the error says why.
    #[cfg(unix)]
    #[tokio::test]
    async fn loopback_answers() {
        let sensor = PingSensor::new(Host::Ip(IpAddr::from([127, 0, 0, 1])), 3);
        match sensor
            .collect(Instant::now() + Duration::from_secs(5))
            .await
        {
            Ok(samples) => {
                let up = samples.iter().find(|s| s.sensor == "up").unwrap();
                assert_eq!(up.value, 1.0);
            }
            Err(error) => assert!(error.to_string().contains("ping_group_range"), "{error}"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_unresolvable_name_is_down() {
        let sensor = PingSensor::new(Host::Name("nothing.invalid".into()), 2);
        let samples = sensor
            .collect(Instant::now() + Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(samples[0].value, 0.0);
        assert_eq!(samples[1].value, 1.0);
    }
}
