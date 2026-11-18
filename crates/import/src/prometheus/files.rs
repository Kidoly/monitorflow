//! Reading uploaded files: `prometheus.yml`, the file service discovery
//! files and rule files it names, `alertmanager.yml` and `blackbox.yml`,
//! in any order and under any name. What `prometheus.yml` names and was
//! not uploaded is reported, not refused.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::args;
use crate::model::{Origin, Unmovable};
use crate::notes::Note;
use crate::source::{File, ReadError};
use crate::text::seconds;

use super::config::{classify, glob_matches, Kind, PromConfig, RuleFile, ScrapeConfig};
use super::rules::Rule;
use super::targets::from_group;
use super::{Collected, DEFAULT_INTERVAL_S};

/// Everything the uploaded `files` describe.
pub fn collect(files: &[File]) -> Result<Collected, ReadError> {
    let mut configs: Vec<PromConfig> = Vec::new();
    let mut rule_files: Vec<(String, RuleFile)> = Vec::new();
    let mut discovery: Vec<(String, Vec<super::config::TargetGroup>)> = Vec::new();
    let mut collected = Collected::default();
    for file in files {
        let text = std::str::from_utf8(&file.content)
            .map_err(|_| ReadError::file(&file.name, "not a text file"))?;
        match classify(text).map_err(|reason| ReadError::file(&file.name, reason))? {
            Kind::Prometheus(config) => configs.push(*config),
            Kind::Rules(rules) => rule_files.push((file.name.clone(), rules)),
            Kind::Alertmanager(config) => {
                let merged = collected.alertmanager.get_or_insert_with(Default::default);
                if merged.receivers.is_empty() {
                    merged.global = config.global.clone();
                    merged.route = config.route.clone();
                }
                merged.receivers.extend(config.receivers);
                merged.time_intervals.extend(config.time_intervals);
                merged
                    .mute_time_intervals
                    .extend(config.mute_time_intervals);
            }
            Kind::Blackbox(config) => collected.modules.extend(config.modules),
            Kind::FileSd(groups) => discovery.push((file.name.clone(), groups)),
        }
    }

    let mut used: BTreeSet<String> = BTreeSet::new();
    for config in &configs {
        let default_interval = config
            .global
            .scrape_interval
            .as_deref()
            .and_then(seconds)
            .map_or(DEFAULT_INTERVAL_S, |s| s as u32);
        for job in &config.scrape_configs {
            let interval = job
                .scrape_interval
                .as_deref()
                .and_then(seconds)
                .map_or(default_interval, |s| s as u32);
            collected.intervals.insert(job.job_name.clone(), interval);
            for group in &job.static_configs {
                collected.targets.extend(from_group(job, group));
            }
            for pattern in job.file_sd_configs.iter().flat_map(|f| &f.files) {
                let matched: Vec<_> = discovery
                    .iter()
                    .filter(|(name, _)| glob_matches(pattern, name))
                    .collect();
                if matched.is_empty() {
                    collected
                        .unmovable
                        .push(missing("scrape_target", &job.job_name, pattern));
                }
                for (name, groups) in matched {
                    used.insert(name.clone());
                    for group in groups {
                        collected.targets.extend(from_group(job, group));
                    }
                }
            }
            for kind in job.other_discoveries() {
                collected.unmovable.push(Unmovable {
                    hosts: vec![],
                    origin: Origin::new("scrape_target", &job.job_name).with_detail(kind),
                    why: Note::new("service_discovery", args! {"kind" => kind}),
                });
            }
        }
        for pattern in &config.rule_files {
            if !rule_files
                .iter()
                .any(|(name, _)| glob_matches(pattern, name))
            {
                collected
                    .unmovable
                    .push(missing("alerting_rule", pattern, pattern));
            }
        }
    }

    // Discovery files no job names: their targets, under the `job` label
    // they carry or the file's name.
    for (name, groups) in discovery.iter().filter(|(n, _)| !used.contains(n)) {
        let stem = name
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(name)
            .split('.')
            .next()
            .unwrap_or(name);
        for group in groups {
            let job = ScrapeConfig {
                job_name: group
                    .labels
                    .get("job")
                    .cloned()
                    .unwrap_or_else(|| stem.to_owned()),
                ..ScrapeConfig::default()
            };
            collected.targets.extend(from_group(&job, group));
        }
    }

    for (_, file) in &rule_files {
        collected.rules.extend(rules_of(file));
    }
    Ok(collected)
}

fn missing(kind: &str, name: &str, file: &str) -> Unmovable {
    Unmovable {
        hosts: vec![],
        origin: Origin::new(kind, name).with_detail(file),
        why: Note::new("file_missing", args! {"file" => file}),
    }
}

/// The alerting rules of a rule file; recording rules are skipped.
pub fn rules_of(file: &RuleFile) -> Vec<Rule> {
    let mut out = Vec::new();
    for group in &file.groups {
        for entry in &group.rules {
            let Some(name) = entry.alert.as_ref().filter(|_| entry.record.is_none()) else {
                continue;
            };
            let expr = match &entry.expr {
                Value::String(s) => s.trim().to_owned(),
                Value::Null => continue,
                other => other.to_string(),
            };
            let severity = entry.labels.get("severity").map(|v| match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            });
            out.push(Rule {
                name: name.clone(),
                expr,
                for_s: entry
                    .for_
                    .as_deref()
                    .and_then(seconds)
                    .map_or(0, |s| u32::try_from(s).unwrap_or(u32::MAX)),
                severity,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, text: &str) -> File {
        File {
            name: name.into(),
            content: text.as_bytes().to_vec(),
        }
    }

    #[test]
    fn files_named_by_the_configuration_are_matched() {
        let collected = collect(&[
            file(
                "prometheus.yml",
                r#"
global: {scrape_interval: 30s}
rule_files: [/etc/prometheus/rules/*.yml, /etc/prometheus/extra.rules]
scrape_configs:
  - job_name: node
    scrape_interval: 15s
    static_configs: [{targets: ["web-01:9100"]}]
    file_sd_configs:
      - files: [/etc/prometheus/targets/*.json, /etc/prometheus/targets/missing.yml]
  - job_name: cloud
    consul_sd_configs: [{server: "consul:8500"}]
"#,
            ),
            file("db.json", r#"[{"targets": ["db-01:9100"], "labels": {"env": "prod"}}]"#),
            file("lonely.yml", "- targets: [cache-01:9100]"),
            file(
                "node.yml",
                "groups:\n- name: node\n  rules:\n  - record: x\n    expr: up\n  - alert: Down\n    expr: up == 0\n    for: 2m\n    labels: {severity: critical}",
            ),
        ])
        .unwrap();
        let targets: Vec<_> = collected
            .targets
            .iter()
            .map(|t| (t.job.as_str(), t.address.as_str()))
            .collect();
        assert_eq!(
            targets,
            vec![
                ("node", "web-01:9100"),
                ("node", "db-01:9100"),
                ("lonely", "cache-01:9100")
            ]
        );
        assert_eq!(collected.targets[1].labels["env"], "prod");
        assert_eq!(collected.intervals["node"], 15);
        assert_eq!(collected.intervals["cloud"], 30);
        assert_eq!(
            collected.rules,
            vec![Rule {
                name: "Down".into(),
                expr: "up == 0".into(),
                for_s: 120,
                severity: Some("critical".into()),
            }]
        );
        let notes: Vec<_> = collected.unmovable.iter().map(|u| u.why.text()).collect();
        assert_eq!(
            notes,
            vec![
                "The configuration refers to /etc/prometheus/targets/missing.yml, which is not among the uploaded files.",
                "The targets come from consul_sd_configs, which only a running Prometheus resolves: connect to it to read them.",
                "The configuration refers to /etc/prometheus/extra.rules, which is not among the uploaded files.",
            ]
        );
    }

    #[test]
    fn a_file_of_nothing_known_is_refused() {
        let err = collect(&[file("notes.yml", "hello: world")]).unwrap_err();
        assert!(matches!(err, ReadError::File { ref file, .. } if file == "notes.yml"));
        assert!(collect(&[File {
            name: "x.json".into(),
            content: vec![0xff, 0xfe]
        }])
        .is_err());
    }
}
