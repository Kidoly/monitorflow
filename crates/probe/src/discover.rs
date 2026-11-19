//! One chunk of a network scan (ADR-0154): what answers among a few
//! addresses. The server runs a scan as a series of one-off `discover`
//! tests on its probe, never as a check that repeats.
//!
//! Per address: one ICMP echo, a TCP connect to each port, an SNMP
//! detection with each set of credentials until one answers, then for
//! what answered anything its MAC address from the ARP table (Linux, own
//! link only) and its name from reverse DNS. Every echo, connection and
//! SNMP attempt takes a slot of a pace of `rate` a second, and waits at
//! most [`WAIT`] for its answer.
//!
//! Parameters:
//! - `targets`: IP addresses, comma-separated, at most [`MAX_TARGETS`];
//!   never names, so the scan stays inside what the person gave.
//! - `ports`: TCP ports, comma-separated, at most [`MAX_PORTS`]; may be
//!   empty.
//! - `rate`: probes a second, 1 to [`MAX_RATE`], [`DEFAULT_RATE`] by
//!   default.
//! - `snmp1_version`, `snmp1_community`, `snmp1_port`, `snmp1_user`...:
//!   up to [`MAX_SNMP`] sets of SNMP credentials, as an `snmp` check takes
//!   them with the `snmpN_` prefix.
//!
//! Samples: one `host` per address that answered, instance the address,
//! value 1, labels `ping` (`1` or `0`), `rtt_seconds` (when it answered
//! ping), `ports` (open ones, ascending, comma-separated), `mac`, `name`,
//! and when SNMP answered `snmp` (the set's number), `descr`, `sysname`,
//! `object_id` and `groups` (those that answer, comma-separated). Then one
//! `scanned` with the number of addresses, and the label `ping_error` when
//! the probe cannot send ICMP.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use monitorflow_sensors::ping::PingSensor;
use monitorflow_sensors::port::Host;
use monitorflow_sensors::{Sample, Sensor, SensorError};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::task::JoinSet;
use tokio::time::{timeout, Instant};
use tracing::instrument::WithSubscriber;

/// The kind of these checks.
pub const KIND: &str = "discover";
/// Most addresses in one test.
pub const MAX_TARGETS: usize = 16;
/// Most ports tried on each address.
pub const MAX_PORTS: usize = 32;
/// Most sets of SNMP credentials.
pub const MAX_SNMP: usize = 4;
/// Probes a second by default.
pub const DEFAULT_RATE: u32 = 50;
/// Most probes a second.
pub const MAX_RATE: u32 = 200;
/// Longest wait for one answer.
pub const WAIT: Duration = Duration::from_secs(1);
/// Longest wait for a name.
const NAME_WAIT: Duration = Duration::from_secs(2);
/// Longest SNMP detection once a device answered.
const SNMP_WAIT: Duration = Duration::from_secs(4);

/// What the parameters ask.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub targets: Vec<IpAddr>,
    pub ports: Vec<u16>,
    pub rate: u32,
    /// Each set as the parameters of an `snmp` check, without `host`.
    pub snmp: Vec<BTreeMap<String, String>>,
}

fn list<T: std::str::FromStr>(value: &str) -> Option<Vec<T>> {
    value
        .split(',')
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| v.parse().ok())
        .collect()
}

/// Reads and checks the parameters. Errors never quote a secret.
pub fn parse(params: &BTreeMap<String, String>) -> Result<Plan, String> {
    let mut snmp: BTreeMap<usize, BTreeMap<String, String>> = BTreeMap::new();
    for (key, value) in params {
        match key.as_str() {
            "targets" | "ports" | "rate" => {}
            other => {
                let (set, name) = other
                    .strip_prefix("snmp")
                    .and_then(|rest| rest.split_once('_'))
                    .and_then(|(n, name)| Some((n.parse::<usize>().ok()?, name)))
                    .filter(|(n, name)| {
                        (1..=MAX_SNMP).contains(n)
                            && (*name == "port"
                                || monitorflow_snmp::CREDENTIAL_PARAMS.contains(name))
                    })
                    .ok_or_else(|| format!("unknown parameter `{}`", other.escape_debug()))?;
                snmp.entry(set)
                    .or_default()
                    .insert(name.to_owned(), value.clone());
            }
        }
    }
    let targets: Vec<IpAddr> = list(params.get("targets").map_or("", String::as_str))
        .ok_or("parameter `targets` must be IP addresses separated by commas")?;
    if targets.is_empty() || targets.len() > MAX_TARGETS {
        return Err(format!(
            "parameter `targets` must hold 1 to {MAX_TARGETS} addresses"
        ));
    }
    let ports: Vec<u16> = list(params.get("ports").map_or("", String::as_str))
        .filter(|p: &Vec<u16>| p.iter().all(|p| *p > 0))
        .ok_or("parameter `ports` must be ports between 1 and 65535 separated by commas")?;
    if ports.len() > MAX_PORTS {
        return Err(format!(
            "parameter `ports` must hold at most {MAX_PORTS} ports"
        ));
    }
    let rate = match params.get("rate") {
        None => DEFAULT_RATE,
        Some(rate) => rate
            .parse()
            .ok()
            .filter(|r| (1..=MAX_RATE).contains(r))
            .ok_or_else(|| format!("parameter `rate` must be between 1 and {MAX_RATE}"))?,
    };
    let snmp: Vec<BTreeMap<String, String>> = snmp.into_values().collect();
    for (n, set) in snmp.iter().enumerate() {
        let mut check = set.clone();
        check.insert("host".into(), "192.0.2.1".into());
        monitorflow_snmp::parse(&check).map_err(|e| format!("SNMP credentials {}: {e}", n + 1))?;
    }
    Ok(Plan {
        targets: dedup(targets),
        ports: dedup(ports),
        rate,
        snmp,
    })
}

fn dedup<T: Ord + Clone>(values: Vec<T>) -> Vec<T> {
    let mut seen = BTreeSet::new();
    values
        .into_iter()
        .filter(|v| seen.insert(v.clone()))
        .collect()
}

/// Hands out the start times of probes, `rate` a second.
#[derive(Debug)]
pub struct Pace {
    gap: Duration,
    next: Mutex<Instant>,
}

impl Pace {
    pub fn new(rate: u32) -> Self {
        Self {
            gap: Duration::from_secs(1) / rate.max(1),
            next: Mutex::new(Instant::now()),
        }
    }

    /// Waits for the next slot; `false` when it would start after
    /// `cutoff`.
    pub async fn slot(&self, cutoff: Instant) -> bool {
        let at = {
            let mut next = self.next.lock().await;
            let at = (*next).max(Instant::now());
            if at >= cutoff {
                return false;
            }
            *next = at + self.gap;
            at
        };
        tokio::time::sleep_until(at).await;
        true
    }
}

/// What one address answered.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Found {
    pub ping: bool,
    pub rtt: Option<f64>,
    pub ports: Vec<u16>,
    /// The set that answered, from 1, and what detection said.
    pub snmp: Option<(usize, Vec<Sample>)>,
    pub mac: Option<String>,
    pub name: Option<String>,
}

impl Found {
    fn answered(&self) -> bool {
        self.ping || !self.ports.is_empty() || self.snmp.is_some()
    }

    /// The `host` sample of `ip`.
    pub fn sample(&self, ip: IpAddr) -> Sample {
        let mut sample = Sample::metric("host", 1.0)
            .with_instance(ip.to_string())
            .with_label("ping", if self.ping { "1" } else { "0" });
        if let Some(rtt) = self.rtt {
            sample = sample.with_label("rtt_seconds", format!("{rtt:.6}"));
        }
        if !self.ports.is_empty() {
            let ports: Vec<String> = self.ports.iter().map(u16::to_string).collect();
            sample = sample.with_label("ports", ports.join(","));
        }
        if let Some(mac) = &self.mac {
            sample = sample.with_label("mac", mac.clone());
        }
        if let Some(name) = &self.name {
            sample = sample.with_label("name", name.clone());
        }
        if let Some((set, detected)) = &self.snmp {
            sample = sample.with_label("snmp", set.to_string());
            if let Some(answers) = detected.iter().find(|s| s.sensor == "answers") {
                for (from, to) in [
                    ("descr", "descr"),
                    ("name", "sysname"),
                    ("object_id", "object_id"),
                ] {
                    if let Some(value) = answers.labels.get(from).filter(|v| !v.is_empty()) {
                        sample = sample.with_label(to, value.chars().take(512).collect::<String>());
                    }
                }
            }
            let groups: Vec<&str> = detected
                .iter()
                .filter(|s| s.sensor == "has" && s.value == 1.0)
                .map(|s| s.instance.as_str())
                .collect();
            sample = sample.with_label("groups", groups.join(","));
        }
        sample
    }
}

/// The MAC address of `ip` in a `/proc/net/arp` table, complete entries
/// only.
pub fn mac_in_arp(table: &str, ip: IpAddr) -> Option<String> {
    table.lines().skip(1).find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let (address, flags, mac) = (fields.first()?, fields.get(2)?, fields.get(3)?);
        let complete = u32::from_str_radix(flags.trim_start_matches("0x"), 16).ok()? & 0x2 != 0;
        (address.parse::<IpAddr>().ok()? == ip && complete && *mac != "00:00:00:00:00:00")
            .then(|| mac.to_ascii_lowercase())
    })
}

fn arp_table() -> Option<String> {
    std::fs::read_to_string("/proc/net/arp").ok()
}

/// One echo; `Err` when the probe cannot send ICMP at all.
async fn ping(ip: IpAddr, pace: &Pace, cutoff: Instant) -> Result<Option<f64>, String> {
    if !pace.slot(cutoff).await {
        return Ok(None);
    }
    let deadline = (Instant::now() + WAIT + Duration::from_millis(200)).min(cutoff);
    match PingSensor::new(Host::Ip(ip), 1).collect(deadline).await {
        Ok(samples) => {
            let up = samples.iter().any(|s| s.sensor == "up" && s.value == 1.0);
            Ok(up.then(|| {
                samples
                    .iter()
                    .find(|s| s.sensor == "rtt_seconds")
                    .map_or(0.0, |s| s.value)
            }))
        }
        Err(SensorError::Permanent(error)) => Err(error),
        Err(_) => Ok(None),
    }
}

async fn open(ip: IpAddr, port: u16, pace: Arc<Pace>, cutoff: Instant) -> Option<u16> {
    if !pace.slot(cutoff).await {
        return None;
    }
    let wait = WAIT.min(cutoff.saturating_duration_since(Instant::now()));
    match timeout(wait, TcpStream::connect(SocketAddr::new(ip, port))).await {
        Ok(Ok(_)) => Some(port),
        _ => None,
    }
}

async fn snmp(
    ip: IpAddr,
    sets: &[BTreeMap<String, String>],
    pace: &Pace,
    cutoff: Instant,
) -> Option<(usize, Vec<Sample>)> {
    for (n, set) in sets.iter().enumerate() {
        if !pace.slot(cutoff).await {
            return None;
        }
        let mut params = set.clone();
        params.insert("host".into(), ip.to_string());
        params.insert("what".into(), "detect".into());
        let Ok(sensor) = monitorflow_snmp::build(&params) else {
            continue;
        };
        // A silent address costs one request's wait; one that answers
        // gets the time to say which groups it has.
        let deadline = (Instant::now() + SNMP_WAIT).min(cutoff);
        // Most addresses of a scan never answer SNMP: that is what a scan
        // finds out, not an error worth the library's error line each.
        let collected = sensor
            .collect(deadline)
            .with_subscriber(tracing::subscriber::NoSubscriber::default())
            .await;
        if let Ok(samples) = collected {
            if samples.iter().any(|s| s.sensor == "answers") {
                return Some((n + 1, samples));
            }
        }
    }
    None
}

async fn one(
    ip: IpAddr,
    plan: Arc<Plan>,
    pace: Arc<Pace>,
    cutoff: Instant,
) -> (IpAddr, Found, Option<String>) {
    let mut ports = JoinSet::new();
    for port in plan.ports.iter().copied() {
        ports.spawn(open(ip, port, pace.clone(), cutoff));
    }
    let (echo, snmp) = tokio::join!(ping(ip, &pace, cutoff), snmp(ip, &plan.snmp, &pace, cutoff));
    let mut found = Found::default();
    let mut ping_error = None;
    match echo {
        Ok(rtt) => {
            found.ping = rtt.is_some();
            found.rtt = rtt;
        }
        Err(error) => ping_error = Some(error),
    }
    while let Some(port) = ports.join_next().await {
        if let Ok(Some(port)) = port {
            found.ports.push(port);
        }
    }
    found.ports.sort_unstable();
    found.snmp = snmp;
    if found.answered() {
        found.mac = arp_table().and_then(|t| mac_in_arp(&t, ip));
        let wait = NAME_WAIT.min(cutoff.saturating_duration_since(Instant::now()));
        found.name = crate::rdns::name_of(ip, wait).await;
    }
    (ip, found, ping_error)
}

/// The sensor of one `discover` test.
#[derive(Debug)]
pub struct DiscoverSensor {
    plan: Arc<Plan>,
}

impl DiscoverSensor {
    pub fn new(plan: Plan) -> Self {
        Self {
            plan: Arc::new(plan),
        }
    }
}

/// Builds the sensor of a `discover` test.
pub fn build(params: &BTreeMap<String, String>) -> Result<Arc<dyn Sensor>, String> {
    Ok(Arc::new(DiscoverSensor::new(parse(params)?)))
}

#[async_trait::async_trait]
impl Sensor for DiscoverSensor {
    async fn collect(&self, deadline: Instant) -> Result<Vec<Sample>, SensorError> {
        // Room to report what was found before the deadline.
        let cutoff = deadline
            - Duration::from_millis(500)
                .min(deadline.saturating_duration_since(Instant::now()) / 4);
        let pace = Arc::new(Pace::new(self.plan.rate));
        let mut addresses = JoinSet::new();
        for ip in self.plan.targets.iter().copied() {
            addresses.spawn(one(ip, self.plan.clone(), pace.clone(), cutoff));
        }
        let mut found = Vec::new();
        let mut ping_error = None;
        while let Some(result) = addresses.join_next().await {
            if let Ok((ip, host, error)) = result {
                ping_error = ping_error.or(error);
                if host.answered() {
                    found.push((ip, host));
                }
            }
        }
        found.sort_by_key(|(ip, _)| *ip);
        let mut samples: Vec<Sample> = found.iter().map(|(ip, f)| f.sample(*ip)).collect();
        let mut scanned = Sample::metric("scanned", self.plan.targets.len() as f64);
        if let Some(error) = ping_error {
            scanned = scanned.with_label("ping_error", error);
        }
        samples.push(scanned);
        Ok(samples)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn parameters_are_checked() {
        let plan = parse(&params(&[
            ("targets", "192.0.2.2, 192.0.2.1,192.0.2.2"),
            ("ports", "22,443"),
            ("snmp1_version", "v2c"),
            ("snmp1_community", "public"),
            ("snmp2_version", "v3"),
            ("snmp2_user", "monitor"),
        ]))
        .unwrap();
        assert_eq!(plan.targets.len(), 2);
        assert_eq!(plan.ports, [22, 443]);
        assert_eq!(plan.rate, DEFAULT_RATE);
        assert_eq!(plan.snmp.len(), 2);
        assert_eq!(plan.snmp[0]["community"], "public");

        let many: Vec<String> = (1..=17).map(|i| format!("192.0.2.{i}")).collect();
        let ports: Vec<String> = (1..=33).map(|i| i.to_string()).collect();
        for bad in [
            params(&[("targets", "")]),
            params(&[("targets", "printer.lan")]),
            params(&[("targets", "192.0.2.0/24")]),
            params(&[("targets", &many.join(","))]),
            params(&[("targets", "192.0.2.1"), ("ports", "0")]),
            params(&[("targets", "192.0.2.1"), ("ports", "http")]),
            params(&[("targets", "192.0.2.1"), ("ports", &ports.join(","))]),
            params(&[("targets", "192.0.2.1"), ("rate", "0")]),
            params(&[("targets", "192.0.2.1"), ("rate", "1000")]),
            params(&[("targets", "192.0.2.1"), ("host", "192.0.2.9")]),
            params(&[("targets", "192.0.2.1"), ("snmp5_version", "v2c")]),
            params(&[("targets", "192.0.2.1"), ("snmp1_what", "host")]),
            params(&[("targets", "192.0.2.1"), ("snmp1_version", "v2c")]),
        ] {
            assert!(parse(&bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn errors_quote_no_secret() {
        let error = parse(&params(&[
            ("targets", "192.0.2.1"),
            ("snmp1_version", "v3"),
            ("snmp1_user", "u"),
            ("snmp1_auth_protocol", "sha"),
            ("snmp1_auth_password", "short"),
        ]))
        .unwrap_err();
        assert!(!error.contains("short"), "{error}");
    }

    #[test]
    fn arp_entries_are_read() {
        let table = "IP address       HW type     Flags       HW address            Mask     Device\n\
                     192.0.2.5        0x1         0x2         02:42:AC:11:00:05     *        eth0\n\
                     192.0.2.6        0x1         0x0         00:00:00:00:00:00     *        eth0\n";
        assert_eq!(
            mac_in_arp(table, "192.0.2.5".parse().unwrap()).as_deref(),
            Some("02:42:ac:11:00:05")
        );
        assert_eq!(mac_in_arp(table, "192.0.2.6".parse().unwrap()), None);
        assert_eq!(mac_in_arp(table, "192.0.2.7".parse().unwrap()), None);
    }

    #[test]
    fn found_becomes_a_sample() {
        let detected = vec![
            Sample::metric("answers", 1.0)
                .with_label("descr", "UPS firmware 2.1")
                .with_label("name", "ups-1")
                .with_label("object_id", "1.3.6.1.4.1.318"),
            Sample::metric("has", 1.0).with_instance("ups"),
            Sample::metric("has", 0.0).with_instance("printer"),
        ];
        let found = Found {
            ping: true,
            rtt: Some(0.0012),
            ports: vec![22, 80],
            snmp: Some((2, detected)),
            mac: Some("02:42:ac:11:00:05".into()),
            name: Some("ups-1.lan".into()),
        };
        let sample = found.sample("192.0.2.5".parse().unwrap());
        assert_eq!(sample.sensor, "host");
        assert_eq!(sample.instance, "192.0.2.5");
        let label = |k: &str| sample.labels.get(k).map(String::as_str);
        assert_eq!(label("ping"), Some("1"));
        assert_eq!(label("rtt_seconds"), Some("0.001200"));
        assert_eq!(label("ports"), Some("22,80"));
        assert_eq!(label("snmp"), Some("2"));
        assert_eq!(label("descr"), Some("UPS firmware 2.1"));
        assert_eq!(label("sysname"), Some("ups-1"));
        assert_eq!(label("object_id"), Some("1.3.6.1.4.1.318"));
        assert_eq!(label("groups"), Some("ups"));
        assert_eq!(label("mac"), Some("02:42:ac:11:00:05"));
        assert_eq!(label("name"), Some("ups-1.lan"));
        assert!(!Found::default().answered());
    }

    #[tokio::test(start_paused = true)]
    async fn the_pace_spaces_probes() {
        let pace = Pace::new(10);
        let start = Instant::now();
        for _ in 0..5 {
            assert!(pace.slot(start + Duration::from_secs(10)).await);
        }
        assert!(Instant::now() - start >= Duration::from_millis(400));
        assert!(!pace.slot(Instant::now()).await);
    }

    #[tokio::test]
    async fn open_ports_on_loopback_are_found() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let open_port = listener.local_addr().unwrap().port();
        let closed = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let sensor = build(&params(&[
            ("targets", "127.0.0.1"),
            ("ports", &format!("{open_port},{closed}")),
        ]))
        .unwrap();
        let samples = sensor
            .collect(Instant::now() + Duration::from_secs(10))
            .await
            .unwrap();
        let host = samples.iter().find(|s| s.sensor == "host").unwrap();
        assert_eq!(host.instance, "127.0.0.1");
        assert_eq!(host.labels["ports"], open_port.to_string());
        let scanned = samples.iter().find(|s| s.sensor == "scanned").unwrap();
        assert_eq!(scanned.value, 1.0);
    }

    #[tokio::test]
    async fn a_silent_address_reports_nothing() {
        // TEST-NET-1 is never routed; the cutoff keeps the test short.
        let sensor = build(&params(&[("targets", "192.0.2.1"), ("ports", "9")])).unwrap();
        let samples = sensor
            .collect(Instant::now() + Duration::from_secs(3))
            .await
            .unwrap();
        assert!(samples.iter().all(|s| s.sensor != "host"), "{samples:?}");
    }
}
