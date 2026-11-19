//! The files of a Prometheus setup, as serde types: `prometheus.yml`, the
//! file service discovery files it names, rule files, `alertmanager.yml`
//! and the blackbox exporter's `blackbox.yml`. Only the settings the
//! reader uses are typed; everything else is ignored. Files are told apart
//! by what they hold, not by their name.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

/// `prometheus.yml`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PromConfig {
    pub global: Global,
    pub scrape_configs: Vec<ScrapeConfig>,
    pub rule_files: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Global {
    pub scrape_interval: Option<String>,
}

/// One job of `scrape_configs`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ScrapeConfig {
    pub job_name: String,
    pub scrape_interval: Option<String>,
    pub scheme: Option<String>,
    pub metrics_path: Option<String>,
    /// URL parameters of the scrape, `module: [http_2xx]` for a blackbox
    /// exporter.
    pub params: BTreeMap<String, Vec<String>>,
    pub static_configs: Vec<TargetGroup>,
    pub file_sd_configs: Vec<FileSd>,
    pub relabel_configs: Vec<Relabel>,
    /// Every other setting, among them other service discoveries
    /// (`consul_sd_configs`, `kubernetes_sd_configs`...).
    #[serde(flatten)]
    pub other: BTreeMap<String, Value>,
}

impl ScrapeConfig {
    /// The kinds of service discovery only a running Prometheus resolves:
    /// `consul_sd_configs`, `dns_sd_configs`...
    pub fn other_discoveries(&self) -> Vec<&str> {
        self.other
            .iter()
            .filter(|(key, value)| {
                key.ends_with("_sd_configs") && value.as_array().is_some_and(|a| !a.is_empty())
            })
            .map(|(key, _)| key.as_str())
            .collect()
    }
}

/// Targets with common labels: a static config, or an entry of a file
/// service discovery file.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct TargetGroup {
    pub targets: Vec<String>,
    pub labels: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FileSd {
    pub files: Vec<String>,
}

/// One step of `relabel_configs`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Relabel {
    pub source_labels: Vec<String>,
    pub separator: Option<String>,
    pub regex: Option<String>,
    pub target_label: Option<String>,
    pub replacement: Option<String>,
    pub action: Option<String>,
}

/// A rule file: groups of rules.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RuleFile {
    pub groups: Vec<RuleGroup>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RuleGroup {
    pub name: String,
    pub rules: Vec<RuleEntry>,
}

/// An alerting rule (`alert`) or a recording rule (`record`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RuleEntry {
    pub alert: Option<String>,
    pub record: Option<String>,
    /// A string, or a number YAML read as one.
    pub expr: Value,
    #[serde(rename = "for")]
    pub for_: Option<String>,
    pub labels: BTreeMap<String, Value>,
}

/// `alertmanager.yml`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AmConfig {
    pub global: AmGlobal,
    pub route: Option<Route>,
    pub receivers: Vec<Receiver>,
    /// Named times routes mute or send in (ADR-0151).
    pub time_intervals: Vec<NamedInterval>,
    /// The same, under the older name.
    pub mute_time_intervals: Vec<NamedInterval>,
}

/// `time_intervals` entry: a name and its times.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct NamedInterval {
    pub name: String,
    pub time_intervals: Vec<IntervalSpec>,
}

/// One time interval: every field narrows it, an absent one is every.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct IntervalSpec {
    pub times: Vec<TimeRange>,
    pub weekdays: Vec<String>,
    pub days_of_month: Vec<String>,
    pub months: Vec<String>,
    pub years: Vec<String>,
    pub location: Option<String>,
}

/// `HH:MM` to `HH:MM`, the end excluded; `24:00` ends the day.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct TimeRange {
    pub start_time: String,
    pub end_time: String,
}

/// Defaults of `alertmanager.yml` the receivers use.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AmGlobal {
    pub smtp_smarthost: Option<String>,
    pub smtp_from: Option<String>,
    pub smtp_auth_username: Option<String>,
    pub smtp_auth_password: Option<String>,
    pub smtp_auth_password_file: Option<String>,
    pub slack_api_url: Option<String>,
    pub slack_api_url_file: Option<String>,
}

/// A node of the routing tree.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Route {
    pub receiver: Option<String>,
    pub routes: Vec<Route>,
    pub matchers: Vec<String>,
    #[serde(rename = "match")]
    pub match_: BTreeMap<String, String>,
    pub match_re: BTreeMap<String, String>,
    pub mute_time_intervals: Vec<String>,
    pub active_time_intervals: Vec<String>,
}

impl Route {
    /// Whether a sub-route picks alerts by their labels.
    pub fn routes_by_labels(&self) -> bool {
        self.routes.iter().any(|r| {
            !r.matchers.is_empty()
                || !r.match_.is_empty()
                || !r.match_re.is_empty()
                || r.routes_by_labels()
        })
    }
}

/// A receiver: a name and its notification configs, each a list under a
/// key ending in `_configs`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Receiver {
    pub name: String,
    #[serde(flatten)]
    pub configs: BTreeMap<String, Value>,
}

/// `blackbox.yml`: each module's prober and the settings of its HTTP and
/// TCP probes that a website check can take (ADR-0150).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct BlackboxConfig {
    pub modules: BTreeMap<String, BlackboxModule>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct BlackboxModule {
    pub prober: String,
    /// `5s`, as written.
    pub timeout: Option<String>,
    pub http: HttpProbe,
    pub tcp: TcpProbe,
}

/// The blackbox exporter's `http` prober settings. Settings it has and
/// the reader does not name are kept as values, to say they are not
/// moved.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct HttpProbe {
    /// Empty: any 2xx.
    pub valid_status_codes: Vec<u16>,
    pub method: String,
    pub headers: BTreeMap<String, String>,
    pub body: String,
    pub body_file: String,
    /// Before blackbox exporter 0.22.
    pub no_follow_redirects: Option<bool>,
    pub follow_redirects: Option<bool>,
    pub fail_if_ssl: bool,
    pub fail_if_not_ssl: bool,
    pub fail_if_body_matches_regexp: Vec<String>,
    pub fail_if_body_not_matches_regexp: Vec<String>,
    pub fail_if_header_matches: Vec<Value>,
    pub fail_if_header_not_matches: Vec<Value>,
    /// Only whether it is there: a password is never read.
    pub basic_auth: Option<BasicAuth>,
    pub bearer_token: Option<Value>,
    pub bearer_token_file: Option<Value>,
    pub authorization: Option<Value>,
    pub oauth2: Option<Value>,
    pub preferred_ip_protocol: String,
    pub ip_protocol_fallback: Option<bool>,
}

/// A basic authentication: its user name, and whether it has a password.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct BasicAuth {
    pub username: String,
    pub password: Option<Value>,
    pub password_file: Option<Value>,
}

/// The `tcp` prober settings: whether it speaks TLS.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct TcpProbe {
    pub tls: bool,
    pub query_response: Vec<Value>,
    pub preferred_ip_protocol: String,
    pub ip_protocol_fallback: Option<bool>,
}

/// What a file is, by its content.
#[derive(Debug, Clone)]
pub enum Kind {
    Prometheus(Box<PromConfig>),
    Rules(RuleFile),
    Alertmanager(Box<AmConfig>),
    Blackbox(BlackboxConfig),
    /// snmp_exporter's modules (ADR-0157).
    Snmp(BTreeMap<String, super::snmp::SnmpModule>),
    FileSd(Vec<TargetGroup>),
}

/// Reads JSON or YAML into a value.
pub fn parse_value(text: &str) -> Result<Value, String> {
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        if let Ok(value) = serde_json::from_str(trimmed) {
            return Ok(value);
        }
    }
    serde_norway::from_str(trimmed).map_err(|e| format!("not JSON or YAML ({e})"))
}

/// What `text` is.
pub fn classify(text: &str) -> Result<Kind, String> {
    let value = parse_value(text)?;
    match &value {
        Value::Object(map) => {
            let has = |key: &str| map.contains_key(key);
            if has("scrape_configs") || has("rule_files") || has("alerting") {
                let config: PromConfig =
                    serde_json::from_value(value).map_err(|e| e.to_string())?;
                return Ok(Kind::Prometheus(Box::new(config)));
            }
            if has("groups") {
                let rules: RuleFile = serde_json::from_value(value).map_err(|e| e.to_string())?;
                return Ok(Kind::Rules(rules));
            }
            if has("receivers") || has("route") {
                let config: AmConfig = serde_json::from_value(value).map_err(|e| e.to_string())?;
                return Ok(Kind::Alertmanager(Box::new(config)));
            }
            if map
                .get("modules")
                .and_then(Value::as_object)
                .is_some_and(super::snmp::is_snmp)
            {
                let modules = map.get("modules").cloned().unwrap_or_default();
                let modules: BTreeMap<String, super::snmp::SnmpModule> =
                    serde_json::from_value(modules).map_err(|e| e.to_string())?;
                return Ok(Kind::Snmp(modules));
            }
            if has("modules") {
                let config: BlackboxConfig =
                    serde_json::from_value(value).map_err(|e| e.to_string())?;
                return Ok(Kind::Blackbox(config));
            }
            Err("not a Prometheus, Alertmanager, rule, blackbox or file discovery file".into())
        }
        Value::Array(items)
            if items
                .iter()
                .all(|i| i.get("targets").is_some_and(Value::is_array)) =>
        {
            let groups: Vec<TargetGroup> =
                serde_json::from_value(value).map_err(|e| e.to_string())?;
            Ok(Kind::FileSd(groups))
        }
        _ => Err("not a Prometheus, Alertmanager, rule, blackbox or file discovery file".into()),
    }
}

/// Whether the file name `name` matches the glob `pattern`, both compared
/// by their last path component, as an uploaded file has no directory.
pub fn glob_matches(pattern: &str, name: &str) -> bool {
    let base = |s: &str| s.rsplit(['/', '\\']).next().unwrap_or_default().to_owned();
    let pattern: Vec<char> = base(pattern).chars().collect();
    let name: Vec<char> = base(name).chars().collect();
    fn walk(p: &[char], n: &[char]) -> bool {
        match p.first() {
            None => n.is_empty(),
            Some('*') => (0..=n.len()).any(|i| walk(&p[1..], &n[i..])),
            Some('?') => !n.is_empty() && walk(&p[1..], &n[1..]),
            Some(c) => n.first() == Some(c) && walk(&p[1..], &n[1..]),
        }
    }
    walk(&pattern, &name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_are_told_apart_by_content() {
        assert!(matches!(
            classify("global: {scrape_interval: 15s}\nscrape_configs: []"),
            Ok(Kind::Prometheus(_))
        ));
        assert!(matches!(
            classify("groups:\n- name: a\n  rules: []"),
            Ok(Kind::Rules(_))
        ));
        assert!(matches!(
            classify("route: {receiver: x}\nreceivers: [{name: x}]"),
            Ok(Kind::Alertmanager(_))
        ));
        assert!(matches!(
            classify("modules: {http_2xx: {prober: http}}"),
            Ok(Kind::Blackbox(_))
        ));
        let Ok(Kind::FileSd(groups)) =
            classify(r#"[{"targets": ["db-01:9100"], "labels": {"env": "prod"}}]"#)
        else {
            panic!("file discovery");
        };
        assert_eq!(groups[0].targets, vec!["db-01:9100"]);
        assert_eq!(groups[0].labels["env"], "prod");
        assert!(matches!(
            classify("- targets: [a:1]\n  labels: {team: x}"),
            Ok(Kind::FileSd(_))
        ));
        assert!(classify("hello: world").is_err());
        assert!(classify("[1, 2]").is_err());
        assert!(classify(": : :").is_err());
    }

    #[test]
    fn globs() {
        assert!(glob_matches("/etc/prometheus/rules/*.yml", "node.yml"));
        assert!(glob_matches("targets/db-?.json", "db-1.json"));
        assert!(!glob_matches("rules/*.yml", "node.yaml"));
        assert!(glob_matches("alerts.rules", "alerts.rules"));
        assert!(!glob_matches("alerts.rules", "other.rules"));
    }

    #[test]
    fn routes_by_labels() {
        let route: Route = serde_norway::from_str(
            "receiver: a\nroutes:\n- receiver: b\n  matchers: ['severity=\"critical\"']",
        )
        .unwrap();
        assert!(route.routes_by_labels());
        let route: Route = serde_norway::from_str("receiver: a").unwrap();
        assert!(!route.routes_by_labels());
    }
}
