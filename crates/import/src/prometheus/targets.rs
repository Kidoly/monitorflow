//! Scrape targets as Prometheus sees them after relabeling: the address
//! it scrapes, the labels the series get, and for a blackbox exporter the
//! address it probes. From files, a small relabeling engine applies the
//! job's `relabel_configs`; connected, Prometheus has done it already.

use std::collections::BTreeMap;

use regex::Regex;

use super::config::{Relabel, ScrapeConfig, TargetGroup};

/// A target, after relabeling.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Target {
    /// The job's name, as in the `job` label.
    pub job: String,
    /// The `host:port` Prometheus scrapes.
    pub address: String,
    /// Labels the series get, `instance` and `job` among them.
    pub labels: BTreeMap<String, String>,
    /// What a blackbox exporter probes, when it is one.
    pub probe: Option<Probe>,
    /// From `node_uname_info`, when connected: the host's own name and its
    /// system.
    pub nodename: Option<String>,
    pub sysname: Option<String>,
}

impl Target {
    /// The `instance` label, which defaults to the address.
    pub fn instance(&self) -> &str {
        self.labels
            .get("instance")
            .map_or(self.address.as_str(), String::as_str)
    }
}

/// What a blackbox exporter is asked to probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    /// The module, which names the prober: `http_2xx`, `tcp_connect`.
    pub module: String,
    /// The address or URL probed.
    pub target: String,
}

/// Applies `rules` to `labels` as Prometheus does. `false` when the target
/// is dropped.
pub fn relabel(labels: &mut BTreeMap<String, String>, rules: &[Relabel]) -> bool {
    for rule in rules {
        let pattern = rule.regex.as_deref().unwrap_or("(.*)");
        // An invalid expression makes Prometheus refuse the file; here the
        // step is skipped.
        let Ok(regex) = Regex::new(&format!("^(?:{pattern})$")) else {
            continue;
        };
        let separator = rule.separator.as_deref().unwrap_or(";");
        let value = rule
            .source_labels
            .iter()
            .map(|l| labels.get(l).map_or("", String::as_str))
            .collect::<Vec<_>>()
            .join(separator);
        let replacement = rule.replacement.as_deref().unwrap_or("$1");
        let action = rule
            .action
            .as_deref()
            .unwrap_or("replace")
            .to_ascii_lowercase();
        match action.as_str() {
            "replace" => {
                let (Some(target), Some(captures)) =
                    (rule.target_label.as_deref(), regex.captures(&value))
                else {
                    continue;
                };
                let mut name = String::new();
                captures.expand(target, &mut name);
                let mut out = String::new();
                captures.expand(replacement, &mut out);
                if out.is_empty() {
                    labels.remove(&name);
                } else {
                    labels.insert(name, out);
                }
            }
            "lowercase" | "uppercase" => {
                if let Some(target) = &rule.target_label {
                    let out = if action == "lowercase" {
                        value.to_lowercase()
                    } else {
                        value.to_uppercase()
                    };
                    labels.insert(target.clone(), out);
                }
            }
            "keep" if !regex.is_match(&value) => return false,
            "drop" if regex.is_match(&value) => return false,
            "keepequal" | "dropequal" => {
                let other = rule
                    .target_label
                    .as_ref()
                    .and_then(|t| labels.get(t))
                    .map_or("", String::as_str);
                if (action == "keepequal") != (value == other) {
                    return false;
                }
            }
            "labelmap" => {
                let mapped: Vec<(String, String)> = labels
                    .iter()
                    .filter_map(|(name, v)| {
                        let captures = regex.captures(name)?;
                        let mut out = String::new();
                        captures.expand(replacement, &mut out);
                        Some((out, v.clone()))
                    })
                    .collect();
                labels.extend(mapped);
            }
            "labeldrop" => labels.retain(|name, _| !regex.is_match(name)),
            "labelkeep" => labels.retain(|name, _| regex.is_match(name)),
            // hashmod shards targets between servers: every shard is kept.
            _ => {}
        }
    }
    true
}

/// The targets of a group of `job`, relabeled.
pub fn from_group(job: &ScrapeConfig, group: &TargetGroup) -> Vec<Target> {
    let mut out = Vec::new();
    for address in &group.targets {
        let mut labels = group.labels.clone();
        labels.insert("__address__".into(), address.clone());
        labels
            .entry("job".into())
            .or_insert_with(|| job.job_name.clone());
        labels.insert(
            "__scheme__".into(),
            job.scheme.clone().unwrap_or_else(|| "http".into()),
        );
        labels.insert(
            "__metrics_path__".into(),
            job.metrics_path
                .clone()
                .unwrap_or_else(|| "/metrics".into()),
        );
        for (name, values) in &job.params {
            if let Some(first) = values.first() {
                labels.insert(format!("__param_{name}"), first.clone());
            }
        }
        if !relabel(&mut labels, &job.relabel_configs) {
            continue;
        }
        let Some(address) = labels.get("__address__").filter(|a| !a.is_empty()).cloned() else {
            continue;
        };
        labels
            .entry("instance".into())
            .or_insert_with(|| address.clone());
        let probe = labels.get("__param_target").map(|target| Probe {
            module: labels.get("__param_module").cloned().unwrap_or_default(),
            target: target.clone(),
        });
        labels.retain(|name, _| !name.starts_with("__"));
        out.push(Target {
            job: labels
                .get("job")
                .cloned()
                .unwrap_or_else(|| job.job_name.clone()),
            address,
            labels,
            probe,
            nodename: None,
            sysname: None,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blackbox_job() -> ScrapeConfig {
        serde_norway::from_str(
            r#"
job_name: blackbox-http
metrics_path: /probe
params:
  module: [http_2xx]
static_configs:
  - targets: ["https://shop.example.com/health"]
relabel_configs:
  - source_labels: [__address__]
    target_label: __param_target
  - source_labels: [__param_target]
    target_label: instance
  - target_label: __address__
    replacement: monitor-01:9115
"#,
        )
        .unwrap()
    }

    #[test]
    fn blackbox_relabeling_gives_the_probe_and_the_exporter() {
        let job = blackbox_job();
        let targets = from_group(&job, &job.static_configs[0]);
        assert_eq!(targets.len(), 1);
        let t = &targets[0];
        assert_eq!(t.address, "monitor-01:9115");
        assert_eq!(t.instance(), "https://shop.example.com/health");
        assert_eq!(t.job, "blackbox-http");
        assert_eq!(
            t.probe,
            Some(Probe {
                module: "http_2xx".into(),
                target: "https://shop.example.com/health".into()
            })
        );
        assert!(t.labels.keys().all(|k| !k.starts_with("__")));
    }

    #[test]
    fn relabel_actions() {
        let rule = |action: &str, source: &[&str], regex: &str, target: &str, rep: &str| Relabel {
            source_labels: source.iter().map(|s| (*s).to_owned()).collect(),
            separator: None,
            regex: Some(regex.into()),
            target_label: (!target.is_empty()).then(|| target.into()),
            replacement: (!rep.is_empty()).then(|| rep.into()),
            action: Some(action.into()),
        };
        let mut labels = BTreeMap::from([
            ("__address__".to_owned(), "10.0.0.5:9100".to_owned()),
            ("__meta_host".to_owned(), "web-01".to_owned()),
        ]);
        assert!(relabel(
            &mut labels,
            &[
                rule("replace", &["__address__"], "([^:]+):.*", "ip", "${1}"),
                rule("labelmap", &[], "__meta_(.+)", "", "$1"),
                rule("keep", &["host"], "web-.*", "", ""),
            ]
        ));
        assert_eq!(labels["ip"], "10.0.0.5");
        assert_eq!(labels["host"], "web-01");
        assert!(!relabel(
            &mut labels,
            &[rule("drop", &["host"], "web-01", "", "")]
        ));
        let mut labels = BTreeMap::from([("a".to_owned(), "x".to_owned())]);
        relabel(&mut labels, &[rule("labeldrop", &[], "a", "", "")]);
        assert!(labels.is_empty());
    }

    #[test]
    fn group_labels_and_default_instance() {
        let job: ScrapeConfig = serde_norway::from_str(
            "job_name: node\nstatic_configs:\n- targets: [web-01:9100]\n  labels: {env: production}",
        )
        .unwrap();
        let t = &from_group(&job, &job.static_configs[0])[0];
        assert_eq!(t.instance(), "web-01:9100");
        assert_eq!(t.labels["env"], "production");
        assert_eq!(t.labels["job"], "node");
        assert_eq!(t.probe, None);
    }
}
