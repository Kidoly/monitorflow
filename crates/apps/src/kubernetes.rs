//! `kubernetes`: a cluster read through its API with a read-only service
//! account token (ADR-0183): its nodes and their conditions, the pods of
//! each namespace that do not run or restart, the deployments, stateful
//! sets and daemon sets short of ready replicas, and the persistent volume
//! claims left pending.
//!
//! Parameters: `host` (the device's address), `port` (6443), `token` and
//! `ca_certificate` (sealed together by the server, never stored with the
//! other parameters nor returned), and `namespaces` (a regular expression
//! the namespaces must match, all by default).
//!
//! A probe runs it, the central server or a relay (ADR-0148); agents never
//! do. The certificate of the API is always verified: against the
//! cluster's CA when one is given (the `certificate-authority-data` of a
//! kubeconfig, or the CA in PEM), else the machine's roots. Every request
//! is a `GET`: the check changes nothing in the cluster.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use base64::Engine as _;
use monitorflow_sensors::port::{parse_host, Host};
use monitorflow_sensors::{BuildError, Sample, Sensor, SensorError};
use regex::Regex;
use rustls::pki_types::CertificateDer;
use serde_json::Value;

use crate::rates::{Rates, Round};
use crate::{check_place, down, label, ratio, within, Failure, Params, Place, Reason};

/// The service account's token: a sealed parameter.
pub const TOKEN_PARAM: &str = "token";
/// The cluster's CA: sealed with the token.
pub const CA_PARAM: &str = "ca_certificate";
/// Port of the API server by default.
pub const DEFAULT_PORT: u16 = 6443;

/// Longest token: service account tokens are JSON Web Tokens of 1 to 2 KiB.
const MAX_TOKEN: usize = 16 * 1024;
/// Longest `ca_certificate`, as given.
const MAX_CA: usize = 64 * 1024;
/// Longest `namespaces` expression.
const MAX_PATTERN: usize = 256;
/// Objects asked for per page of a list.
const PAGE: usize = 500;
/// Objects read of one list, at most.
const MAX_OBJECTS: usize = 10_000;
/// Bytes read in one collection, at most.
const MAX_BYTES: usize = 64 * 1024 * 1024;
/// Longest label naming pods or claims.
const MAX_NAMES: usize = 200;

/// The lists a collection reads, with what the guide's ClusterRole grants.
const NODES: &str = "/api/v1/nodes";
const PODS: &str = "/api/v1/pods";
const CLAIMS: &str = "/api/v1/persistentvolumeclaims";
const DEPLOYMENTS: &str = "/apis/apps/v1/deployments";
const STATEFUL_SETS: &str = "/apis/apps/v1/statefulsets";
const DAEMON_SETS: &str = "/apis/apps/v1/daemonsets";

pub struct Kubernetes {
    base: String,
    token: Option<String>,
    filter: Option<Regex>,
    http: reqwest::Client,
    rates: Rates,
}

impl std::fmt::Debug for Kubernetes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Kubernetes")
            .field("base", &self.base)
            .field("token", &self.token.as_ref().map(|_| "set"))
            .field("filter", &self.filter)
            .finish()
    }
}

impl Kubernetes {
    pub(crate) fn from_params(p: &mut Params<'_>, place: Place<'_>) -> Result<Self, BuildError> {
        if matches!(place, Place::Agent(_)) {
            return Err(p.refused(
                "a Kubernetes cluster is checked through its API from the central server or a relay, not by an agent",
            ));
        }
        let raw = p.get("host").unwrap_or("127.0.0.1");
        let host = parse_host(raw).map_err(|reason| p.bad("host", reason))?;
        let port = match p.get("port") {
            None => DEFAULT_PORT,
            Some(port) => port
                .parse::<u16>()
                .ok()
                .filter(|p| *p > 0)
                .ok_or_else(|| p.bad("port", "must be between 1 and 65535"))?,
        };
        let token = p.get(TOKEN_PARAM).filter(|t| !t.is_empty());
        if token.is_some_and(|t| t.len() > MAX_TOKEN || !t.bytes().all(|b| b.is_ascii_graphic())) {
            return Err(p.bad(
                TOKEN_PARAM,
                "must be at most 16 KiB of visible characters, without spaces",
            ));
        }
        let roots = match p.get(CA_PARAM).filter(|c| !c.is_empty()) {
            None => None,
            Some(ca) if ca.len() > MAX_CA => return Err(p.bad(CA_PARAM, "must be at most 64 KiB")),
            Some(ca) => Some(parse_ca(ca).map_err(|reason| p.bad(CA_PARAM, reason))?),
        };
        let filter = match p.get("namespaces").filter(|s| !s.is_empty()) {
            None => None,
            Some(pattern) if pattern.len() > MAX_PATTERN => {
                return Err(p.bad("namespaces", "must be at most 256 bytes"))
            }
            Some(pattern) => Some(
                regex::RegexBuilder::new(pattern)
                    .size_limit(1 << 20)
                    .build()
                    .map_err(|e| {
                        p.bad("namespaces", format!("is not a regular expression: {e}"))
                    })?,
            ),
        };
        check_place(p, place, &host, port)?;
        let base = match &host {
            Host::Ip(std::net::IpAddr::V6(ip)) => format!("https://[{ip}]:{port}"),
            Host::Ip(ip) => format!("https://{ip}:{port}"),
            Host::Name(name) => format!("https://{name}:{port}"),
        };
        let http = client(roots).map_err(|reason| p.bad(CA_PARAM, reason))?;
        Ok(Self {
            base,
            token: token.map(str::to_owned),
            filter,
            http,
            rates: Rates::default(),
        })
    }

    /// `text` without the token, should an error ever quote it.
    fn scrub(&self, text: &str) -> String {
        match &self.token {
            Some(token) if token.len() >= 8 => text.replace(token.as_str(), "***"),
            _ => text.to_owned(),
        }
    }

    fn watched(&self, namespace: &str) -> bool {
        self.filter.as_ref().is_none_or(|f| f.is_match(namespace))
    }

    /// One `GET`, its body bounded by what `budget` has left.
    async fn get(
        &self,
        path: &str,
        query: &[(&str, &str)],
        budget: &mut usize,
    ) -> Result<Value, Failure> {
        let token = self.token.as_deref().unwrap_or_default();
        let mut response = self
            .http
            .get(format!("{}{path}", self.base))
            .query(query)
            .bearer_auth(token)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|e| self.failure(&e))?;
        match response.status().as_u16() {
            200 => {}
            401 => {
                return Err(Failure::new(
                    Reason::Login,
                    "the API refused the token (401): it expired, was revoked, or is for another cluster",
                ))
            }
            403 => {
                return Err(Failure::new(
                    Reason::Permission,
                    format!(
                        "the service account may not read {path} (403): bind it to the monitorflow-reader ClusterRole of the guide"
                    ),
                ))
            }
            status => {
                return Err(Failure::new(
                    Reason::Protocol,
                    format!("the API answered {status} to {path}"),
                ))
            }
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|e| self.failure(&e))? {
            if chunk.len() > *budget {
                return Err(Failure::new(
                    Reason::Protocol,
                    "the cluster's answers exceed 64 MiB in one collection",
                ));
            }
            *budget -= chunk.len();
            body.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&body)
            .map_err(|e| Failure::new(Reason::Protocol, format!("{path}: {e}")))
    }

    /// Every item of a list, page after page.
    async fn list(&self, path: &str, budget: &mut usize) -> Result<Vec<Value>, Failure> {
        let limit = PAGE.to_string();
        let mut items = Vec::new();
        let mut next = String::new();
        loop {
            let mut query = vec![("limit", limit.as_str())];
            if !next.is_empty() {
                query.push(("continue", next.as_str()));
            }
            let mut page = self.get(path, &query, budget).await?;
            if let Value::Array(found) = page["items"].take() {
                items.extend(found);
            }
            next = page["metadata"]["continue"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            if next.is_empty() {
                return Ok(items);
            }
            if items.len() >= MAX_OBJECTS {
                return Err(Failure::new(
                    Reason::Protocol,
                    format!("{path} lists more than {MAX_OBJECTS} objects; narrow `namespaces`"),
                ));
            }
        }
    }

    fn failure(&self, e: &reqwest::Error) -> Failure {
        if e.is_timeout() {
            return Failure::new(Reason::Timeout, "no answer before the check's timeout");
        }
        let mut chain = String::new();
        let mut source: Option<&dyn std::error::Error> = Some(e);
        while let Some(error) = source {
            chain.push_str(&error.to_string());
            chain.push_str(": ");
            source = error.source();
        }
        let chain = self.scrub(chain.trim_end_matches(": "));
        let tls = [
            "certificate",
            "tls",
            "handshake",
            "UnknownIssuer",
            "NotValidForName",
        ];
        if tls.iter().any(|w| chain.contains(w)) {
            Failure::new(
                Reason::Tls,
                format!("the API's certificate is not trusted: {chain}; give the cluster's CA on the check"),
            )
        } else if e.is_connect() {
            Failure::new(Reason::Connect, chain)
        } else {
            Failure::new(Reason::Protocol, chain)
        }
    }

    async fn read(&self, round: &mut Round) -> Result<Vec<Sample>, Failure> {
        if self.token.is_none() {
            return Err(Failure::new(
                Reason::Login,
                "no token: enter the service account's token on the check",
            ));
        }
        let started = Instant::now();
        let mut budget = MAX_BYTES;
        let version = self.get("/version", &[], &mut budget).await?;
        let nodes = self.list(NODES, &mut budget).await?;
        let response = started.elapsed().as_secs_f64();
        let pods = self.list(PODS, &mut budget).await?;
        let mut workloads = Vec::new();
        for (path, kind) in [
            (DEPLOYMENTS, Workload::Deployment),
            (STATEFUL_SETS, Workload::StatefulSet),
            (DAEMON_SETS, Workload::DaemonSet),
        ] {
            workloads.push((kind, self.list(path, &mut budget).await?));
        }
        let claims = self.list(CLAIMS, &mut budget).await?;
        let mut out = vec![
            Sample::metric("up", 1.0).with_label(
                "version",
                label(version["gitVersion"].as_str().unwrap_or(""), 64),
            ),
            Sample::metric("response_seconds", response),
        ];
        out.extend(node_samples(&nodes));
        let watched =
            |item: &Value| self.watched(item["metadata"]["namespace"].as_str().unwrap_or(""));
        let pods: Vec<&Value> = pods.iter().filter(|p| watched(p)).collect();
        let claims: Vec<&Value> = claims.iter().filter(|c| watched(c)).collect();
        out.extend(namespace_samples(&pods, &claims, round));
        let workloads: Vec<(Workload, &Value)> = workloads
            .iter()
            .flat_map(|(kind, items)| items.iter().map(|i| (*kind, i)))
            .filter(|(_, i)| watched(i))
            .collect();
        out.extend(workload_samples(&workloads));
        Ok(out)
    }
}

/// A client trusting `roots`, or the machine's roots without them.
fn client(roots: Option<Vec<CertificateDer<'static>>>) -> Result<reqwest::Client, String> {
    let mut store = rustls::RootCertStore::empty();
    match roots {
        Some(roots) => {
            for root in roots {
                store
                    .add(root)
                    .map_err(|e| format!("is not a usable CA certificate: {e}"))?;
            }
        }
        None => {
            let (added, _) =
                store.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
            if added == 0 {
                store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            }
        }
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_root_certificates(store)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    reqwest::Client::builder()
        .use_preconfigured_tls(config)
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .map_err(|e| e.to_string())
}

/// The certificates of `text`: PEM (its line breaks possibly lost when
/// pasted in one line), or the base64 of PEM or DER, as a kubeconfig's
/// `certificate-authority-data` is.
pub(crate) fn parse_ca(text: &str) -> Result<Vec<CertificateDer<'static>>, &'static str> {
    const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
    const END: &str = "-----END CERTIFICATE-----";
    const BAD: &str =
        "must be the cluster's CA in PEM, or the certificate-authority-data of a kubeconfig";
    let decode = |s: &str| {
        let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        base64::engine::general_purpose::STANDARD
            .decode(s)
            .map_err(|_| BAD)
    };
    if !text.contains(BEGIN) {
        let bytes = decode(text)?;
        return match std::str::from_utf8(&bytes) {
            Ok(pem) if pem.contains(BEGIN) => parse_ca(pem),
            _ if bytes.first() == Some(&0x30) => Ok(vec![CertificateDer::from(bytes)]),
            _ => Err(BAD),
        };
    }
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(BEGIN) {
        let after = &rest[start + BEGIN.len()..];
        let end = after.find(END).ok_or(BAD)?;
        out.push(CertificateDer::from(decode(&after[..end])?));
        rest = &after[end + END.len()..];
    }
    if out.is_empty() || out.len() > 16 {
        return Err(BAD);
    }
    for der in &out {
        if x509_parser::parse_x509_certificate(der).is_err() {
            return Err(BAD);
        }
    }
    Ok(out)
}

/// The status of `condition` in `conditions`: `Some(true)` for `"True"`.
fn condition(conditions: &Value, kind: &str) -> Option<bool> {
    conditions
        .as_array()?
        .iter()
        .find(|c| c["type"] == kind)
        .map(|c| c["status"] == "True")
}

fn name_of(item: &Value) -> &str {
    item["metadata"]["name"].as_str().unwrap_or("")
}

/// The samples of every node, and how many are ready.
pub(crate) fn node_samples(nodes: &[Value]) -> Vec<Sample> {
    let mut out = Vec::new();
    let mut ready_count = 0u32;
    for node in nodes {
        let name = label(name_of(node), 128);
        if name.is_empty() {
            continue;
        }
        let conditions = &node["status"]["conditions"];
        let ready = condition(conditions, "Ready") == Some(true);
        ready_count += u32::from(ready);
        let mut roles: Vec<&str> = node["metadata"]["labels"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(k, _)| k.strip_prefix("node-role.kubernetes.io/"))
            .filter(|r| !r.is_empty())
            .collect();
        roles.sort_unstable();
        out.push(
            Sample::metric("node_ready", f64::from(u8::from(ready)))
                .with_instance(&name)
                .with_label("roles", label(&roles.join(","), 64))
                .with_label(
                    "kubelet_version",
                    label(
                        node["status"]["nodeInfo"]["kubeletVersion"]
                            .as_str()
                            .unwrap_or(""),
                        64,
                    ),
                ),
        );
        for (kind, metric) in [
            ("MemoryPressure", "node_memory_pressure"),
            ("DiskPressure", "node_disk_pressure"),
            ("PIDPressure", "node_pid_pressure"),
            ("NetworkUnavailable", "node_network_unavailable"),
        ] {
            if let Some(on) = condition(conditions, kind) {
                out.push(Sample::metric(metric, f64::from(u8::from(on))).with_instance(&name));
            }
        }
        let cordoned = node["spec"]["unschedulable"].as_bool().unwrap_or(false);
        out.push(
            Sample::metric("node_unschedulable", f64::from(u8::from(cordoned)))
                .with_instance(&name),
        );
    }
    let total = out.iter().filter(|s| s.sensor == "node_ready").count();
    out.push(Sample::metric("nodes", total as f64));
    out.push(Sample::metric(
        "nodes_not_ready",
        (total as f64 - f64::from(ready_count)).max(0.0),
    ));
    out
}

/// Why a pod counts as not running, or `None` when it runs, finished, or
/// is going away.
pub(crate) fn pod_problem(pod: &Value) -> Option<String> {
    if !pod["metadata"]["deletionTimestamp"].is_null() {
        return None;
    }
    let status = &pod["status"];
    // A container waiting says more than the phase: CrashLoopBackOff,
    // ImagePullBackOff, CreateContainerConfigError.
    let waiting = status["containerStatuses"]
        .as_array()
        .into_iter()
        .flatten()
        .find_map(|c| c["state"]["waiting"]["reason"].as_str())
        .map(str::to_owned);
    match status["phase"].as_str().unwrap_or("Unknown") {
        "Succeeded" => None,
        // A failed job's pod is the job's failure, not a pod to bring back.
        "Failed" => {
            let job = pod["metadata"]["ownerReferences"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|o| o["kind"] == "Job");
            (!job).then(|| status["reason"].as_str().unwrap_or("Failed").to_owned())
        }
        "Running" => (condition(&status["conditions"], "Ready") != Some(true))
            .then(|| waiting.unwrap_or_else(|| "NotReady".into())),
        phase => Some(waiting.unwrap_or_else(|| phase.to_owned())),
    }
}

/// Restarts of every container of `pod`, init containers included.
fn restarts(pod: &Value) -> f64 {
    ["containerStatuses", "initContainerStatuses"]
        .iter()
        .flat_map(|k| pod["status"][*k].as_array().into_iter().flatten())
        .filter_map(|c| c["restartCount"].as_f64())
        .sum()
}

/// Names joined within [`MAX_NAMES`] bytes, `…` when some are left out.
fn names(list: &[String]) -> String {
    let mut out = String::new();
    for (i, name) in list.iter().enumerate() {
        let sep = if out.is_empty() { "" } else { ", " };
        if out.len() + sep.len() + name.len() > MAX_NAMES - 4 {
            out.push_str(if i == 0 { "…" } else { " …" });
            break;
        }
        out.push_str(sep);
        out.push_str(name);
    }
    label(&out, MAX_NAMES)
}

#[derive(Default)]
struct Namespace {
    pods: u32,
    problems: Vec<String>,
    restarts: f64,
    pending: Vec<String>,
    lost: Vec<String>,
}

/// The samples of each namespace with pods or claims, and the cluster's
/// totals.
pub(crate) fn namespace_samples(
    pods: &[&Value],
    claims: &[&Value],
    round: &mut Round,
) -> Vec<Sample> {
    let first = round.first();
    let mut spaces: BTreeMap<String, Namespace> = BTreeMap::new();
    for pod in pods {
        let ns = pod["metadata"]["namespace"].as_str().unwrap_or("");
        let space = spaces.entry(label(ns, 128)).or_default();
        space.pods += 1;
        if let Some(problem) = pod_problem(pod) {
            space
                .problems
                .push(format!("{} {}", name_of(pod), label(&problem, 48)));
        }
        let uid = pod["metadata"]["uid"]
            .as_str()
            .unwrap_or_else(|| name_of(pod));
        let count = restarts(pod);
        // A pod first seen after the first collection was created since:
        // all its restarts are new.
        space.restarts += match round.delta(&format!("restarts:{ns}/{uid}"), count) {
            Some(d) => d,
            None if !first && !round.seen(&format!("restarts:{ns}/{uid}")) => count,
            None => 0.0,
        };
    }
    for claim in claims {
        let ns = claim["metadata"]["namespace"].as_str().unwrap_or("");
        let space = spaces.entry(label(ns, 128)).or_default();
        match claim["status"]["phase"].as_str() {
            Some("Pending") => space.pending.push(name_of(claim).to_owned()),
            Some("Lost") => space.lost.push(name_of(claim).to_owned()),
            _ => {}
        }
    }
    let mut out = Vec::new();
    let (mut pods_total, mut not_running, mut pending) = (0u32, 0usize, 0usize);
    for (ns, space) in &spaces {
        pods_total += space.pods;
        not_running += space.problems.len();
        pending += space.pending.len();
        out.push(Sample::metric("namespace_pods", f64::from(space.pods)).with_instance(ns));
        out.push(
            Sample::metric("namespace_pods_not_running", space.problems.len() as f64)
                .with_instance(ns)
                .with_label("pods", names(&space.problems)),
        );
        if !first {
            out.push(Sample::metric("namespace_pod_restarts", space.restarts).with_instance(ns));
        }
        out.push(
            Sample::metric("namespace_claims_pending", space.pending.len() as f64)
                .with_instance(ns)
                .with_label("claims", names(&space.pending)),
        );
        out.push(
            Sample::metric("namespace_claims_lost", space.lost.len() as f64)
                .with_instance(ns)
                .with_label("claims", names(&space.lost)),
        );
    }
    out.push(Sample::metric("pods", f64::from(pods_total)));
    out.push(Sample::metric("pods_not_running", not_running as f64));
    out.push(Sample::metric("claims_pending", pending as f64));
    out
}

/// What kind of workload a list holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Workload {
    Deployment,
    StatefulSet,
    DaemonSet,
}

impl Workload {
    fn as_str(self) -> &'static str {
        match self {
            Self::Deployment => "deployment",
            Self::StatefulSet => "statefulset",
            Self::DaemonSet => "daemonset",
        }
    }

    /// Replicas wanted and ready.
    fn replicas(self, item: &Value) -> (f64, f64) {
        let status = &item["status"];
        match self {
            Self::DaemonSet => (
                status["desiredNumberScheduled"].as_f64().unwrap_or(0.0),
                status["numberReady"].as_f64().unwrap_or(0.0),
            ),
            // `spec.replicas` is 1 when left out.
            _ => (
                item["spec"]["replicas"].as_f64().unwrap_or(1.0),
                status["readyReplicas"].as_f64().unwrap_or(0.0),
            ),
        }
    }
}

/// The samples of each workload, named `namespace/kind/name`, and how
/// many are short of ready replicas.
pub(crate) fn workload_samples(workloads: &[(Workload, &Value)]) -> Vec<Sample> {
    let mut out = Vec::new();
    let mut short = 0u32;
    for (kind, item) in workloads {
        let instance = label(
            &format!(
                "{}/{}/{}",
                item["metadata"]["namespace"].as_str().unwrap_or(""),
                kind.as_str(),
                name_of(item)
            ),
            200,
        );
        let (desired, ready) = kind.replicas(item);
        short += u32::from(ready < desired);
        out.push(
            Sample::metric("workload_replicas_desired", desired)
                .with_instance(&instance)
                .with_label("workload", kind.as_str()),
        );
        out.push(Sample::metric("workload_replicas_ready", ready).with_instance(&instance));
        // Absent when scaled to zero: nothing is missing.
        if let Some(r) = ratio(ready, desired) {
            out.push(Sample::metric("workload_ready_ratio", r).with_instance(&instance));
        }
    }
    out.push(Sample::metric("workloads_short", f64::from(short)));
    out
}

#[async_trait::async_trait]
impl Sensor for Kubernetes {
    async fn collect(&self, deadline: tokio::time::Instant) -> Result<Vec<Sample>, SensorError> {
        let mut round = self.rates.round();
        match within(deadline, self.read(&mut round)).await {
            Ok(samples) => {
                self.rates.finish(round);
                Ok(samples)
            }
            Err(failure) => {
                self.rates.reset();
                let failure = Failure::new(failure.reason, self.scrub(&failure.error));
                Ok(vec![down(None, &failure)])
            }
        }
    }
}

#[cfg(test)]
mod tests;
