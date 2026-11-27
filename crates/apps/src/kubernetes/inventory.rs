//! What the Kubernetes check adds to the inventory of ADR-0156: an `_info`
//! sample per node, namespace, workload and failed job, the nodes' sizes
//! and pods, failed jobs per namespace, and what nodes, namespaces and
//! workloads use as metrics-server measures it.
//!
//! Namespaces, jobs and metrics-server are optional reads: a cluster whose
//! ClusterRole predates them, or without metrics-server, is still read,
//! and the source's `up` says what was not (label `unread`).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use monitorflow_sensors::Sample;
use serde_json::Value;

use super::{name_of, names};
use crate::{label, ratio};

pub(crate) const NAMESPACES: &str = "/api/v1/namespaces";
pub(crate) const JOBS: &str = "/apis/batch/v1/jobs";
pub(crate) const NODE_METRICS: &str = "/apis/metrics.k8s.io/v1beta1/nodes";
pub(crate) const POD_METRICS: &str = "/apis/metrics.k8s.io/v1beta1/pods";

/// The label naming an object's kind (ADR-0156).
const VIRT: &str = "virt";

/// A Kubernetes quantity as a number: `250m` is 0.25, `1Gi` 1073741824,
/// `1500000n` 0.0015, `129e6` 129000000.
pub(crate) fn quantity(text: &str) -> Option<f64> {
    const SUFFIXES: [(&str, f64); 15] = [
        ("Ki", 1024.0),
        ("Mi", 1_048_576.0),
        ("Gi", 1_073_741_824.0),
        ("Ti", 1_099_511_627_776.0),
        ("Pi", 1_125_899_906_842_624.0),
        ("Ei", 1_152_921_504_606_846_976.0),
        ("n", 1e-9),
        ("u", 1e-6),
        ("m", 1e-3),
        ("k", 1e3),
        ("M", 1e6),
        ("G", 1e9),
        ("T", 1e12),
        ("P", 1e15),
        ("E", 1e18),
    ];
    let text = text.trim();
    let value = match text.parse::<f64>() {
        Ok(v) => v,
        Err(_) => SUFFIXES.iter().find_map(|(suffix, factor)| {
            text.strip_suffix(suffix)
                .and_then(|n| n.parse::<f64>().ok())
                .map(|n| n * factor)
        })?,
    };
    value.is_finite().then_some(value)
}

/// Processor in cores and memory in bytes of a `usage` or `allocatable`.
fn cpu_memory(resources: &Value) -> (Option<f64>, Option<f64>) {
    let get = |k: &str| resources[k].as_str().and_then(quantity);
    (get("cpu"), get("memory"))
}

/// What one collection read beyond the check's own lists.
#[derive(Default)]
pub(crate) struct Extra<'a> {
    /// `/api/v1/namespaces`, when readable.
    pub namespaces: Option<&'a [Value]>,
    /// `/apis/batch/v1/jobs`, when readable.
    pub jobs: Option<&'a [Value]>,
    pub node_metrics: Option<&'a [Value]>,
    pub pod_metrics: Option<&'a [Value]>,
}

/// Whether a pod still holds its node's resources.
fn active(pod: &Value) -> bool {
    !matches!(
        pod["status"]["phase"].as_str(),
        Some("Succeeded" | "Failed")
    )
}

/// Each node's `_info`, size, pods and use. `pods` are every pod of the
/// cluster, whatever the namespaces watched.
pub(crate) fn node_inventory(nodes: &[Value], pods: &[Value], extra: &Extra<'_>) -> Vec<Sample> {
    let mut per_node: HashMap<&str, u32> = HashMap::new();
    for pod in pods.iter().filter(|p| active(p)) {
        if let Some(node) = pod["spec"]["nodeName"].as_str() {
            *per_node.entry(node).or_default() += 1;
        }
    }
    let usage: HashMap<&str, (Option<f64>, Option<f64>)> = extra
        .node_metrics
        .into_iter()
        .flatten()
        .map(|m| (name_of(m), cpu_memory(&m["usage"])))
        .collect();
    let mut out = Vec::new();
    for node in nodes {
        let name = label(name_of(node), 128);
        if name.is_empty() {
            continue;
        }
        let status = &node["status"];
        let info = &status["nodeInfo"];
        let text = |v: &Value| label(v.as_str().unwrap_or(""), 128);
        let address = |kinds: &[&str]| -> Vec<String> {
            status["addresses"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|a| kinds.iter().any(|k| a["type"] == *k))
                .filter_map(|a| a["address"].as_str().map(str::to_owned))
                .collect()
        };
        let mut roles: Vec<&str> = node["metadata"]["labels"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(k, _)| k.strip_prefix("node-role.kubernetes.io/"))
            .filter(|r| !r.is_empty())
            .collect();
        roles.sort_unstable();
        let labels = [
            ("uuid", text(&info["systemUUID"]).to_ascii_lowercase()),
            (
                "hostname",
                address(&["Hostname"])
                    .into_iter()
                    .next()
                    .unwrap_or_default(),
            ),
            (
                "addresses",
                monitorflow_sensors::virt::list(&address(&["InternalIP", "ExternalIP"])),
            ),
            ("roles", roles.join(",")),
            ("kubelet_version", text(&info["kubeletVersion"])),
            ("os_image", text(&info["osImage"])),
            ("kernel", text(&info["kernelVersion"])),
            ("runtime", text(&info["containerRuntimeVersion"])),
            ("architecture", text(&info["architecture"])),
        ];
        out.push(info_sample("node_info", &name, uid(node), &labels));
        let (cores, memory) = cpu_memory(&status["allocatable"]);
        let mut put = |metric: &str, value: Option<f64>| {
            if let Some(value) = value {
                out.push(Sample::metric(metric, value).with_instance(&name));
            }
        };
        put("node_cpu_cores", cores);
        put("node_memory_bytes", memory);
        put(
            "node_pods_capacity",
            status["allocatable"]["pods"].as_str().and_then(quantity),
        );
        put(
            "node_pods",
            Some(f64::from(
                per_node.get(name_of(node)).copied().unwrap_or_default(),
            )),
        );
        if let Some((cpu, mem)) = usage.get(name_of(node)) {
            put("node_cpu_usage_cores", *cpu);
            put("node_memory_usage_bytes", *mem);
            put(
                "node_cpu_usage_ratio",
                cpu.zip(cores).and_then(|(u, c)| ratio(u, c)),
            );
            put(
                "node_memory_usage_ratio",
                mem.zip(memory).and_then(|(u, m)| ratio(u, m)),
            );
        }
    }
    out
}

fn uid(item: &Value) -> &str {
    item["metadata"]["uid"]
        .as_str()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| name_of(item))
}

fn info_sample(metric: &str, instance: &str, key: &str, labels: &[(&str, String)]) -> Sample {
    let mut sample = Sample::metric(metric, 1.0)
        .with_instance(instance)
        .with_label("key", label(key, 128));
    for (name, value) in labels {
        let value = label(value, 256);
        if !value.is_empty() {
            sample.labels.insert((*name).to_owned(), value);
        }
    }
    sample
}

/// The workload a pod belongs to, as `namespace/kind/name`: its stateful
/// set or daemon set, or the deployment of its replica set, whose name is
/// the deployment's and the pod template's hash.
pub(crate) fn owner(pod: &Value) -> Option<String> {
    let ns = pod["metadata"]["namespace"].as_str().unwrap_or("");
    let owner = pod["metadata"]["ownerReferences"]
        .as_array()?
        .iter()
        .find(|o| o["controller"].as_bool().unwrap_or(true))?;
    let name = owner["name"].as_str()?;
    let (kind, name) = match owner["kind"].as_str()? {
        "StatefulSet" => ("statefulset", name),
        "DaemonSet" => ("daemonset", name),
        "ReplicaSet" => {
            let hash = pod["metadata"]["labels"]["pod-template-hash"].as_str()?;
            ("deployment", name.strip_suffix(&format!("-{hash}"))?)
        }
        _ => return None,
    };
    Some(format!("{ns}/{kind}/{name}"))
}

/// Processor and memory each pod uses, by `namespace/name`.
fn pod_usage(metrics: &[Value]) -> HashMap<String, (f64, f64)> {
    metrics
        .iter()
        .map(|m| {
            let (cpu, memory) = m["containers"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|c| cpu_memory(&c["usage"]))
                .fold((0.0, 0.0), |(a, b), (c, d)| {
                    (a + c.unwrap_or(0.0), b + d.unwrap_or(0.0))
                });
            let ns = m["metadata"]["namespace"].as_str().unwrap_or("");
            (format!("{ns}/{}", name_of(m)), (cpu, memory))
        })
        .collect()
}

/// The jobs that failed: a job of no cron job that failed, or the latest
/// run of a cron job when it failed, as an earlier failure a later run
/// made good is history.
pub(crate) fn failed_jobs<'a>(jobs: &[&'a Value]) -> Vec<&'a Value> {
    let cron = |job: &Value| -> Option<String> {
        job["metadata"]["ownerReferences"]
            .as_array()?
            .iter()
            .find(|o| o["kind"] == "CronJob")
            .and_then(|o| o["name"].as_str())
            .map(str::to_owned)
    };
    let created = |job: &Value| {
        job["metadata"]["creationTimestamp"]
            .as_str()
            .unwrap_or("")
            .to_owned()
    };
    let mut latest: HashMap<(String, String), &Value> = HashMap::new();
    for job in jobs {
        if let Some(cron) = cron(job) {
            let ns = job["metadata"]["namespace"]
                .as_str()
                .unwrap_or("")
                .to_owned();
            let slot = latest.entry((ns, cron)).or_insert(job);
            if (created(job), name_of(job)) > (created(slot), name_of(slot)) {
                *slot = job;
            }
        }
    }
    jobs.iter()
        .copied()
        .filter(|job| job["metadata"]["deletionTimestamp"].is_null())
        .filter(|job| failure(job).is_some())
        .filter(|job| {
            cron(job).is_none_or(|c| {
                let ns = job["metadata"]["namespace"]
                    .as_str()
                    .unwrap_or("")
                    .to_owned();
                latest.get(&(ns, c)).is_some_and(|l| std::ptr::eq(*l, *job))
            })
        })
        .collect()
}

/// A job's `Failed` condition, when true.
fn failure(job: &Value) -> Option<&Value> {
    job["status"]["conditions"]
        .as_array()?
        .iter()
        .find(|c| c["type"] == "Failed" && c["status"] == "True")
}

/// The `_info` of each namespace and workload and failed job, with the
/// use and failed jobs of each namespace and the use of each workload.
/// `namespaces` are those the check's own samples name; `watched` says
/// whether one is watched; `pods` are the watched ones.
pub(crate) fn object_inventory(
    namespaces: &BTreeSet<String>,
    workloads: &[(super::Workload, &Value)],
    pods: &[&Value],
    extra: &Extra<'_>,
    watched: &dyn Fn(&str) -> bool,
) -> Vec<Sample> {
    let mut out = Vec::new();
    let listed: BTreeMap<&str, &Value> = extra
        .namespaces
        .into_iter()
        .flatten()
        .map(|n| (name_of(n), n))
        .collect();
    let usage = extra.pod_metrics.map(pod_usage);
    // Use by namespace and by workload.
    let mut by_ns: HashMap<String, (f64, f64)> = HashMap::new();
    let mut by_workload: HashMap<String, (f64, f64)> = HashMap::new();
    if let Some(usage) = &usage {
        for pod in pods {
            let ns = pod["metadata"]["namespace"].as_str().unwrap_or("");
            let Some((cpu, memory)) = usage.get(&format!("{ns}/{}", name_of(pod))) else {
                continue;
            };
            let add = |slot: &mut (f64, f64)| {
                slot.0 += cpu;
                slot.1 += memory;
            };
            add(by_ns.entry(label(ns, 128)).or_default());
            if let Some(owner) = owner(pod) {
                add(by_workload.entry(label(&owner, 200)).or_default());
            }
        }
    }
    let jobs: Option<Vec<&Value>> = extra.jobs.map(|jobs| {
        let jobs: Vec<&Value> = jobs
            .iter()
            .filter(|j| watched(j["metadata"]["namespace"].as_str().unwrap_or("")))
            .collect();
        failed_jobs(&jobs)
    });
    let mut failed_by_ns: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for job in jobs.iter().flatten() {
        let ns = label(job["metadata"]["namespace"].as_str().unwrap_or(""), 128);
        failed_by_ns
            .entry(ns.clone())
            .or_default()
            .push(name_of(job).to_owned());
        let cron = job["metadata"]["ownerReferences"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|o| o["kind"] == "CronJob")
            .and_then(|o| o["name"].as_str())
            .unwrap_or("");
        let condition = failure(job).cloned().unwrap_or_default();
        let text = |v: &Value| v.as_str().unwrap_or("").to_owned();
        out.push(info_sample(
            "job_info",
            &label(&format!("{ns}/{}", name_of(job)), 200),
            uid(job),
            &[
                ("namespace", ns.clone()),
                ("cron_job", cron.to_owned()),
                ("reason", text(&condition["reason"])),
                ("message", text(&condition["message"])),
                ("failed_at", text(&condition["lastTransitionTime"])),
            ],
        ));
    }
    let mut all: BTreeSet<String> = namespaces.clone();
    all.extend(failed_by_ns.keys().cloned());
    for ns in &all {
        let item = listed.get(ns.as_str());
        let phase = item
            .and_then(|n| n["status"]["phase"].as_str())
            .unwrap_or("")
            .to_owned();
        out.push(info_sample(
            "namespace_info",
            ns,
            item.map_or(ns.as_str(), |n| uid(n)),
            &[("phase", phase)],
        ));
        if usage.is_some() {
            let (cpu, memory) = by_ns.get(ns).copied().unwrap_or_default();
            out.push(Sample::metric("namespace_cpu_usage_cores", cpu).with_instance(ns));
            out.push(Sample::metric("namespace_memory_usage_bytes", memory).with_instance(ns));
        }
        if jobs.is_some() {
            let failed = failed_by_ns.get(ns).cloned().unwrap_or_default();
            out.push(
                Sample::metric("namespace_jobs_failed", failed.len() as f64)
                    .with_instance(ns)
                    .with_label("jobs", names(&failed)),
            );
        }
    }
    if let Some(jobs) = &jobs {
        out.push(Sample::metric("jobs_failed", jobs.len() as f64));
    }
    for (kind, item) in workloads {
        let ns = item["metadata"]["namespace"].as_str().unwrap_or("");
        let instance = super::workload_instance(*kind, item);
        out.push(info_sample(
            "workload_info",
            &instance,
            uid(item),
            &[
                ("namespace", ns.to_owned()),
                ("workload", kind.as_str().to_owned()),
                ("name", name_of(item).to_owned()),
            ],
        ));
        if usage.is_some() {
            let (cpu, memory) = by_workload.get(&instance).copied().unwrap_or_default();
            out.push(Sample::metric("workload_cpu_usage_cores", cpu).with_instance(&instance));
            out.push(
                Sample::metric("workload_memory_usage_bytes", memory).with_instance(&instance),
            );
        }
    }
    out
}

/// Marks each sample of an object with its kind, and every sample with
/// the collection's time, so the server keeps them as one view of the
/// cluster (ADR-0156).
pub(crate) fn mark(samples: &mut [Sample], now_ms: i64) {
    for sample in samples {
        sample.timestamp_unix_ms = now_ms;
        let kind = ["node", "namespace", "workload", "job"]
            .into_iter()
            .find(|k| {
                sample
                    .sensor
                    .strip_prefix(k)
                    .is_some_and(|rest| rest.starts_with('_'))
            });
        if let Some(kind) = kind {
            sample.labels.insert(VIRT.into(), kind.into());
        } else if sample.sensor == "up" {
            sample.labels.insert(VIRT.into(), "source".into());
            sample.labels.insert("product".into(), "Kubernetes".into());
        }
    }
}
