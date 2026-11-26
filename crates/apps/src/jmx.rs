//! `jmx`: a Java virtual machine read through Jolokia, the HTTP and JSON
//! bridge to its MBeans (ADR-0183): heap and non-heap memory, garbage
//! collection, threads and deadlocks, processor and file descriptors,
//! then Tomcat's connectors and sessions and a Kafka broker's partitions
//! and traffic where those MBeans exist.
//!
//! Parameters: `host` (`127.0.0.1`), `port` (8778, the Jolokia agent's),
//! `path` (`/jolokia/`), `user` and `password` (Jolokia's HTTP Basic
//! authentication, none by default), `tls` (`off` for HTTP, or `verify`
//! for HTTPS with the certificate verified) and `groups` (`tomcat,kafka`
//! by default; `none` reads the JVM alone). A password goes over HTTPS
//! only, or to this machine's loopback, so it never crosses a network in
//! clear.
//!
//! One `POST` per collection carries every read, as a Jolokia bulk
//! request; each read answers on its own, so a missing MBean (no Tomcat
//! here) or one a Jolokia policy refuses (the deadlock search is an
//! operation, `exec`) leaves its metrics out without failing the rest.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

use monitorflow_sensors::{BuildError, Sample, Sensor, SensorError};
use serde_json::{json, Value};

use crate::rates::{Rates, Round};
use crate::{down, gauge, label, ratio, within, Failure, Params, Place, Reason, Target, Tls};

/// Jolokia's JVM agent listens there by default.
pub const DEFAULT_PORT: u16 = 8778;
const DEFAULT_PATH: &str = "/jolokia/";
/// Largest answer read: a bulk answer is a few kilobytes, a few hundred
/// with many Tomcat contexts.
const MAX_BODY: usize = 4 << 20;
/// Collectors, connectors or contexts reported per pattern, at most.
const MAX_INSTANCES: usize = 50;
const MAX_PATH: usize = 256;

/// The optional groups of MBeans read with the JVM's own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Groups {
    pub tomcat: bool,
    pub kafka: bool,
}

impl Groups {
    fn parse(p: &mut Params<'_>) -> Result<Self, BuildError> {
        let mut groups = Self {
            tomcat: false,
            kafka: false,
        };
        let Some(value) = p.get("groups") else {
            return Ok(Self {
                tomcat: true,
                kafka: true,
            });
        };
        if value.is_empty() || value == "none" {
            return Ok(groups);
        }
        for name in value.split(',').map(str::trim) {
            match name {
                "tomcat" => groups.tomcat = true,
                "kafka" => groups.kafka = true,
                _ => {
                    return Err(p.bad(
                        "groups",
                        "must list `tomcat` and `kafka`, separated by commas, or be `none`",
                    ))
                }
            }
        }
        Ok(groups)
    }
}

#[derive(Debug)]
pub struct Jmx {
    target: Target,
    url: String,
    groups: Groups,
    client: reqwest::Client,
    rates: Rates,
}

impl Jmx {
    pub(crate) fn from_params(p: &mut Params<'_>, place: Place<'_>) -> Result<Self, BuildError> {
        let target = Target::parse(p, place, DEFAULT_PORT, None, Tls::Off)?;
        if target.tls == Tls::Prefer {
            return Err(p.bad("tls", "must be `off` (HTTP) or `verify` (HTTPS)"));
        }
        let path = p
            .get("path")
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_PATH);
        if !path.starts_with('/')
            || path.len() > MAX_PATH
            || path
                .chars()
                .any(|c| c.is_control() || c.is_whitespace() || matches!(c, '?' | '#' | '\\'))
        {
            return Err(p.bad(
                "path",
                "must start with `/`, at most 256 bytes, without spaces, `?` or `#`",
            ));
        }
        if target.password.is_some() && target.tls == Tls::Off && !loopback(&target.host) {
            return Err(p.bad(
                crate::SECRET_PARAM,
                "is sent over HTTPS only (`tls` `verify`), or to this machine's loopback",
            ));
        }
        let groups = Groups::parse(p)?;
        let scheme = if target.tls == Tls::Off {
            "http"
        } else {
            "https"
        };
        let host = if target.host.contains(':') {
            format!("[{}]", target.host)
        } else {
            target.host.clone()
        };
        let url = format!("{scheme}://{host}:{}{path}", target.port);
        let client = reqwest::Client::builder()
            // A proxy of the environment would see the password, and a
            // redirect would take it elsewhere.
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            // Nothing is kept between collections (ADR-0183).
            .pool_max_idle_per_host(0)
            .connect_timeout(Duration::from_secs(10))
            .https_only(target.tls == Tls::Verify)
            .build()
            .map_err(|e| p.refused(format!("cannot prepare its HTTP client: {e}")))?;
        Ok(Self {
            target,
            url,
            groups,
            client,
            rates: Rates::default(),
        })
    }

    async fn read(&self, round: &mut Round) -> Result<Vec<Sample>, Failure> {
        let started = Instant::now();
        let reads = reads(self.groups);
        let body: Vec<Value> = reads.iter().map(|r| r.request()).collect();
        let mut request = self
            .client
            .post(&self.url)
            .header("content-type", "application/json")
            .body(Value::Array(body).to_string());
        if let Some(password) = &self.target.password {
            request = request.basic_auth(self.target.user.as_deref().unwrap_or(""), Some(password));
        }
        let mut response = request.send().await.map_err(failure)?;
        match response.status().as_u16() {
            200 => {}
            401 => return Err(Failure::new(Reason::Login, "Jolokia refused the login")),
            403 => {
                return Err(Failure::new(
                    Reason::Permission,
                    "Jolokia refused the request",
                ))
            }
            404 => {
                return Err(Failure::new(
                    Reason::Protocol,
                    "no Jolokia at this path (HTTP 404)",
                ))
            }
            code => {
                return Err(Failure::new(
                    Reason::Protocol,
                    format!("Jolokia answered HTTP {code}"),
                ))
            }
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(failure)? {
            if bytes.len() + chunk.len() > MAX_BODY {
                return Err(Failure::new(Reason::Protocol, "the answer is too large"));
            }
            bytes.extend_from_slice(&chunk);
        }
        let elapsed = started.elapsed().as_secs_f64();
        let answers: Value = serde_json::from_slice(&bytes)
            .map_err(|_| Failure::new(Reason::Protocol, "the answer is not Jolokia's JSON"))?;
        samples(&reads, &answers, elapsed, round)
    }
}

/// Whether `host` is this machine's loopback.
fn loopback(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Why a request did not get an answer.
fn failure(error: reqwest::Error) -> Failure {
    if error.is_timeout() {
        return Failure::new(Reason::Timeout, "no answer before the check's timeout");
    }
    let mut text = error.to_string();
    let mut tls = false;
    let mut source: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(&error);
    while let Some(e) = source {
        text = format!("{text}: {e}");
        let inner = e
            .downcast_ref::<std::io::Error>()
            .and_then(|io| io.get_ref())
            .map(|inner| inner as &(dyn std::error::Error + 'static));
        if e.is::<rustls::Error>() || inner.is_some_and(|i| i.is::<rustls::Error>()) {
            tls = true;
        }
        source = e.source();
    }
    let lower = text.to_ascii_lowercase();
    if tls || lower.contains("certificate") || lower.contains("handshake") {
        Failure::new(Reason::Tls, text)
    } else if error.is_connect() {
        Failure::new(Reason::Connect, text)
    } else {
        Failure::new(Reason::Protocol, text)
    }
}

/// How a Kafka MBean's value becomes a metric.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Turn {
    Gauge,
    /// What grew since the last collection.
    Delta,
    /// What grew, per second.
    Rate,
}

/// A Kafka broker's metric: its MBean, the attribute read, the suffix.
pub(crate) struct Kafka {
    mbean: &'static str,
    attribute: &'static str,
    suffix: &'static str,
    turn: Turn,
}

const fn k(
    mbean: &'static str,
    attribute: &'static str,
    suffix: &'static str,
    turn: Turn,
) -> Kafka {
    Kafka {
        mbean,
        attribute,
        suffix,
        turn,
    }
}

const KAFKA: &[Kafka] = &[
    k(
        "kafka.server:type=ReplicaManager,name=UnderReplicatedPartitions",
        "Value",
        "kafka_under_replicated",
        Turn::Gauge,
    ),
    k(
        "kafka.server:type=ReplicaManager,name=UnderMinIsrPartitionCount",
        "Value",
        "kafka_under_min_isr",
        Turn::Gauge,
    ),
    k(
        "kafka.server:type=ReplicaManager,name=PartitionCount",
        "Value",
        "kafka_partitions",
        Turn::Gauge,
    ),
    k(
        "kafka.server:type=ReplicaManager,name=IsrShrinksPerSec",
        "Count",
        "kafka_isr_shrinks",
        Turn::Delta,
    ),
    k(
        "kafka.controller:type=KafkaController,name=OfflinePartitionsCount",
        "Value",
        "kafka_offline_partitions",
        Turn::Gauge,
    ),
    k(
        "kafka.controller:type=KafkaController,name=ActiveControllerCount",
        "Value",
        "kafka_active_controller",
        Turn::Gauge,
    ),
    k(
        "kafka.server:type=BrokerTopicMetrics,name=MessagesInPerSec",
        "Count",
        "kafka_messages_in_per_second",
        Turn::Rate,
    ),
    k(
        "kafka.server:type=BrokerTopicMetrics,name=BytesInPerSec",
        "Count",
        "kafka_bytes_in_per_second",
        Turn::Rate,
    ),
    k(
        "kafka.server:type=BrokerTopicMetrics,name=BytesOutPerSec",
        "Count",
        "kafka_bytes_out_per_second",
        Turn::Rate,
    ),
    k(
        "kafka.server:type=KafkaRequestHandlerPool,name=RequestHandlerAvgIdlePercent",
        "OneMinuteRate",
        "kafka_handler_idle_ratio",
        Turn::Gauge,
    ),
];

/// The domains Tomcat registers its MBeans in: `Catalina` standalone,
/// `Tomcat` embedded (Spring Boot).
const TOMCAT_DOMAINS: [&str; 2] = ["Catalina", "Tomcat"];

/// One read of the bulk request, and what its answer becomes.
pub(crate) enum Read {
    Memory,
    Gc,
    Threads,
    Deadlocks,
    Runtime,
    Os,
    TomcatPool(&'static str),
    TomcatRequests(&'static str),
    TomcatSessions(&'static str),
    Kafka(&'static Kafka),
}

/// The reads of one collection: the JVM's first, the memory first of all,
/// which says whether Jolokia answers at all.
fn reads(groups: Groups) -> Vec<Read> {
    let mut out = vec![
        Read::Memory,
        Read::Gc,
        Read::Threads,
        Read::Deadlocks,
        Read::Runtime,
        Read::Os,
    ];
    if groups.tomcat {
        for domain in TOMCAT_DOMAINS {
            out.push(Read::TomcatPool(domain));
            out.push(Read::TomcatRequests(domain));
            out.push(Read::TomcatSessions(domain));
        }
    }
    if groups.kafka {
        out.extend(KAFKA.iter().map(Read::Kafka));
    }
    out
}

fn read(mbean: &str, attributes: &[&str]) -> Value {
    // `ignoreErrors`: an attribute this JVM lacks (file descriptors on
    // Windows) is left out rather than failing the read.
    json!({"type": "read", "mbean": mbean, "attribute": attributes, "config": {"ignoreErrors": true}})
}

impl Read {
    fn request(&self) -> Value {
        match self {
            Self::Memory => read(
                "java.lang:type=Memory",
                &["HeapMemoryUsage", "NonHeapMemoryUsage"],
            ),
            Self::Gc => read(
                "java.lang:type=GarbageCollector,name=*",
                &["CollectionCount", "CollectionTime"],
            ),
            Self::Threads => read(
                "java.lang:type=Threading",
                &["ThreadCount", "DaemonThreadCount", "PeakThreadCount"],
            ),
            Self::Deadlocks => json!({
                "type": "exec",
                "mbean": "java.lang:type=Threading",
                "operation": "findDeadlockedThreads",
            }),
            Self::Runtime => read("java.lang:type=Runtime", &["VmName", "VmVersion", "Uptime"]),
            Self::Os => read(
                "java.lang:type=OperatingSystem",
                &[
                    "ProcessCpuLoad",
                    "OpenFileDescriptorCount",
                    "MaxFileDescriptorCount",
                ],
            ),
            Self::TomcatPool(domain) => read(
                &format!("{domain}:type=ThreadPool,name=*"),
                &["currentThreadsBusy", "maxThreads"],
            ),
            Self::TomcatRequests(domain) => read(
                &format!("{domain}:type=GlobalRequestProcessor,name=*"),
                &["requestCount", "errorCount", "processingTime"],
            ),
            Self::TomcatSessions(domain) => read(
                &format!("{domain}:type=Manager,host=*,context=*"),
                &["activeSessions"],
            ),
            Self::Kafka(m) => {
                json!({"type": "read", "mbean": m.mbean, "attribute": m.attribute})
            }
        }
    }
}

/// A number of a JSON answer: Jolokia writes numbers, and some MBeans
/// strings of digits.
fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
    .filter(|v: &f64| v.is_finite())
}

/// The value of an answer that succeeded.
fn ok(answer: &Value) -> Option<&Value> {
    (answer.get("status").and_then(Value::as_u64) == Some(200))
        .then(|| answer.get("value"))
        .flatten()
}

/// The keys of an MBean name, `domain:a=1,b="x,y"`, quotes removed.
pub fn mbean_keys(name: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Some((_, keys)) = name.split_once(':') else {
        return out;
    };
    let mut key = String::new();
    let mut value = String::new();
    let (mut in_value, mut quoted, mut escaped) = (false, false, false);
    for c in keys.chars() {
        if !in_value {
            if c == '=' {
                in_value = true;
            } else {
                key.push(c);
            }
            continue;
        }
        if escaped {
            value.push(c);
            escaped = false;
        } else if quoted && c == '\\' {
            escaped = true;
        } else if c == '"' {
            quoted = !quoted;
        } else if c == ',' && !quoted {
            out.insert(std::mem::take(&mut key), std::mem::take(&mut value));
            in_value = false;
        } else {
            value.push(c);
        }
    }
    if in_value {
        out.insert(key, value);
    }
    out
}

/// The MBeans of a pattern's answer, sorted by name and bounded, each
/// with the key naming its instance.
fn instances<'a>(value: &'a Value, keys: &[&str]) -> Vec<(String, &'a Value)> {
    let Some(map) = value.as_object() else {
        return Vec::new();
    };
    let mut out: Vec<(String, &Value)> = map
        .iter()
        .map(|(name, attrs)| {
            let parsed = mbean_keys(name);
            let instance: Vec<&str> = keys
                .iter()
                .filter_map(|k| parsed.get(*k).map(String::as_str))
                .collect();
            (label(&instance.join(""), 128), attrs)
        })
        .filter(|(instance, _)| !instance.is_empty())
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out.truncate(MAX_INSTANCES);
    out
}

/// The samples of one collection: `reads` and Jolokia's answers to them,
/// in the same order.
pub(crate) fn samples(
    reads: &[Read],
    answers: &Value,
    response: f64,
    round: &mut Round,
) -> Result<Vec<Sample>, Failure> {
    let Some(answers) = answers.as_array().filter(|a| a.len() == reads.len()) else {
        return Err(Failure::new(
            Reason::Protocol,
            "the answer is not Jolokia's bulk answer",
        ));
    };
    // The heap answers on every JVM: without it, Jolokia did not let the
    // check read.
    let memory = answers.first().and_then(ok).ok_or_else(|| {
        let first = answers.first().unwrap_or(&Value::Null);
        let error = first
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("java.lang:type=Memory could not be read");
        match first.get("status").and_then(Value::as_u64) {
            Some(401) => Failure::new(Reason::Login, error),
            Some(403) => Failure::new(Reason::Permission, error),
            _ => Failure::new(Reason::Protocol, error),
        }
    })?;
    let mut out = Vec::new();
    let mut up = Sample::metric("up", 1.0);
    out.push(Sample::metric("response_seconds", response));
    for (read, answer) in reads.iter().zip(answers) {
        let Some(value) = ok(answer) else {
            continue;
        };
        let attr = |name: &str| value.get(name).and_then(number);
        match read {
            Read::Memory => memory_samples(memory, &mut out),
            Read::Gc => gc_samples(value, round, &mut out),
            Read::Threads => {
                out.extend(attr("ThreadCount").and_then(|v| gauge("threads", v)));
                out.extend(attr("DaemonThreadCount").and_then(|v| gauge("threads_daemon", v)));
                out.extend(attr("PeakThreadCount").and_then(|v| gauge("threads_peak", v)));
            }
            Read::Deadlocks => {
                let count = match value {
                    Value::Null => Some(0.0),
                    Value::Array(ids) => Some(ids.len() as f64),
                    _ => None,
                };
                out.extend(count.and_then(|v| gauge("threads_deadlocked", v)));
            }
            Read::Runtime => {
                let text = |name: &str| value.get(name).and_then(Value::as_str).unwrap_or("");
                up = up
                    .with_label("version", label(text("VmVersion"), 64))
                    .with_label("vm", label(text("VmName"), 64));
                out.extend(
                    attr("Uptime")
                        .filter(|v| *v >= 0.0)
                        .and_then(|v| gauge("uptime_seconds", v / 1000.0)),
                );
            }
            Read::Os => {
                // Negative while the JVM has no reading yet.
                out.extend(
                    attr("ProcessCpuLoad")
                        .filter(|v| *v >= 0.0)
                        .map(|v| Sample::metric("process_cpu_ratio", v.min(1.0))),
                );
                let open = attr("OpenFileDescriptorCount");
                out.extend(open.and_then(|v| gauge("file_descriptors_open", v)));
                if let (Some(open), Some(max)) = (open, attr("MaxFileDescriptorCount")) {
                    out.extend(
                        ratio(open, max).map(|r| Sample::metric("file_descriptors_ratio", r)),
                    );
                }
            }
            Read::TomcatPool(_) => {
                for (name, attrs) in instances(value, &["name"]) {
                    let get = |a: &str| attrs.get(a).and_then(number);
                    let busy = get("currentThreadsBusy").filter(|v| *v >= 0.0);
                    let max = get("maxThreads").filter(|v| *v > 0.0);
                    let at = |s: Sample| s.with_instance(name.clone());
                    out.extend(busy.and_then(|v| gauge("tomcat_threads_busy", v)).map(at));
                    out.extend(max.and_then(|v| gauge("tomcat_threads_max", v)).map(at));
                    if let (Some(busy), Some(max)) = (busy, max) {
                        out.extend(
                            ratio(busy, max).map(|r| at(Sample::metric("tomcat_threads_ratio", r))),
                        );
                    }
                }
            }
            Read::TomcatRequests(domain) => {
                for (name, attrs) in instances(value, &["name"]) {
                    let get = |a: &str| attrs.get(a).and_then(number);
                    let key = |a: &str| format!("{domain}/{name}/{a}");
                    let at = |s: Sample| s.with_instance(name.clone());
                    let requests = get("requestCount");
                    let count = requests.and_then(|v| round.delta(&key("count"), v));
                    if let Some(v) = requests {
                        out.extend(
                            round
                                .rate(&key("rate"), v)
                                .and_then(|r| gauge("tomcat_requests_per_second", r))
                                .map(at),
                        );
                    }
                    out.extend(
                        get("errorCount")
                            .and_then(|v| round.delta(&key("errors"), v))
                            .and_then(|v| gauge("tomcat_errors", v))
                            .map(at),
                    );
                    let time = get("processingTime").and_then(|v| round.delta(&key("time"), v));
                    if let (Some(count), Some(time)) = (count, time) {
                        if count > 0.0 {
                            out.push(at(Sample::metric(
                                "tomcat_request_seconds",
                                time / count / 1000.0,
                            )));
                        }
                    }
                }
            }
            Read::TomcatSessions(_) => {
                for (name, attrs) in instances(value, &["host", "context"]) {
                    out.extend(
                        attrs
                            .get("activeSessions")
                            .and_then(number)
                            .and_then(|v| gauge("tomcat_sessions", v))
                            .map(|s| s.with_instance(name)),
                    );
                }
            }
            Read::Kafka(m) => {
                let Some(v) = number(value) else {
                    continue;
                };
                let v = match m.turn {
                    Turn::Gauge => Some(v),
                    Turn::Delta => round.delta(m.suffix, v),
                    Turn::Rate => round.rate(m.suffix, v),
                };
                out.extend(v.and_then(|v| gauge(m.suffix, v)));
            }
        }
    }
    out.insert(0, up);
    Ok(out)
}

fn memory_samples(memory: &Value, out: &mut Vec<Sample>) {
    for (attribute, prefix) in [
        ("HeapMemoryUsage", "heap"),
        ("NonHeapMemoryUsage", "non_heap"),
    ] {
        let Some(usage) = memory.get(attribute) else {
            continue;
        };
        let get = |k: &str| usage.get(k).and_then(number);
        let used = get("used");
        // -1 when the JVM sets no maximum, as it usually does not for
        // non-heap memory.
        let max = get("max").filter(|m| *m > 0.0);
        out.extend(used.and_then(|v| gauge(&format!("{prefix}_used_bytes"), v)));
        out.extend(get("committed").and_then(|v| gauge(&format!("{prefix}_committed_bytes"), v)));
        out.extend(max.and_then(|v| gauge(&format!("{prefix}_max_bytes"), v)));
        if let (Some(used), Some(max)) = (used, max) {
            out.extend(
                ratio(used, max).map(|r| Sample::metric(format!("{prefix}_usage_ratio"), r)),
            );
        }
    }
}

/// Per collector, what it collected since the last collection; for the
/// JVM, the share of time spent collecting.
fn gc_samples(value: &Value, round: &mut Round, out: &mut Vec<Sample>) {
    let mut share: Option<f64> = None;
    for (name, attrs) in instances(value, &["name"]) {
        let get = |a: &str| attrs.get(a).and_then(number);
        let at = |s: Sample| s.with_instance(name.clone());
        out.extend(
            get("CollectionCount")
                .and_then(|v| round.delta(&format!("gc/{name}/count"), v))
                .and_then(|v| gauge("gc_collections", v))
                .map(at),
        );
        if let Some(ms) = get("CollectionTime") {
            out.extend(
                round
                    .delta(&format!("gc/{name}/time"), ms)
                    .and_then(|v| gauge("gc_seconds", v / 1000.0))
                    .map(at),
            );
            if let Some(rate) = round.rate(&format!("gc/{name}/rate"), ms) {
                *share.get_or_insert(0.0) += rate / 1000.0;
            }
        }
    }
    out.extend(share.map(|s| Sample::metric("gc_time_ratio", s.clamp(0.0, 1.0))));
}

#[async_trait::async_trait]
impl Sensor for Jmx {
    async fn collect(&self, deadline: tokio::time::Instant) -> Result<Vec<Sample>, SensorError> {
        let mut round = self.rates.round();
        match within(deadline, self.read(&mut round)).await {
            Ok(samples) => {
                self.rates.finish(round);
                Ok(samples)
            }
            Err(failure) => {
                self.rates.reset();
                Ok(vec![down(Some(&self.target), &failure)])
            }
        }
    }
}

#[cfg(test)]
pub(crate) fn all_reads() -> Vec<Read> {
    reads(Groups {
        tomcat: true,
        kafka: true,
    })
}

#[cfg(test)]
pub(crate) fn kafka_index(suffix: &str) -> usize {
    all_reads()
        .iter()
        .position(|r| matches!(r, Read::Kafka(m) if m.suffix == suffix))
        .unwrap()
}
