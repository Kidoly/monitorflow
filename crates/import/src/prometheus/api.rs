//! Reading a running Prometheus, and its Alertmanager when given, through
//! their HTTP APIs. Only GET requests: nothing is ever written.
//!
//! Prometheus: `/api/v1/status/config` (the loaded configuration, for
//! scrape intervals), `/api/v1/targets` (active targets, with service
//! discovery and relabeling done), `/api/v1/rules` (alerting rules) and
//! the query `node_uname_info` (each node's own name). Alertmanager:
//! `/api/v2/status` (its configuration, secrets shown as `<secret>`) and
//! `/api/v2/silences` (active and pending ones are kept).

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use reqwest::{Method, Url};
use serde::de::DeserializeOwned;
use serde::Deserialize;

use crate::http;
use crate::source::{Connection, Context, ReadError};
use crate::text::seconds;

use super::alertmanager::SilenceIn;
use super::config::{AmConfig, PromConfig};
use super::promql::{MatchOp, Matcher};
use super::rules::Rule;
use super::targets::{Probe, Target};
use super::{Collected, DEFAULT_INTERVAL_S};

/// Prometheus's answer envelope.
#[derive(Debug, Deserialize)]
struct Envelope<T> {
    status: String,
    data: Option<T>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ConfigData {
    yaml: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TargetsData {
    #[serde(default)]
    active_targets: Vec<ActiveTarget>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ActiveTarget {
    #[serde(default)]
    labels: BTreeMap<String, String>,
    #[serde(default)]
    scrape_pool: String,
    #[serde(default)]
    scrape_url: String,
    #[serde(default)]
    scrape_interval: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RulesData {
    #[serde(default)]
    groups: Vec<ApiGroup>,
}

#[derive(Debug, Deserialize)]
struct ApiGroup {
    #[serde(default)]
    rules: Vec<ApiRule>,
}

#[derive(Debug, Deserialize)]
struct ApiRule {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    query: String,
    /// Seconds.
    #[serde(default)]
    duration: f64,
    #[serde(default)]
    labels: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct QueryData {
    #[serde(default)]
    result: Vec<Series>,
}

#[derive(Debug, Deserialize)]
struct Series {
    #[serde(default)]
    metric: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct AmStatus {
    config: AmStatusConfig,
}

#[derive(Debug, Deserialize)]
struct AmStatusConfig {
    original: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiSilence {
    id: String,
    status: ApiSilenceStatus,
    matchers: Vec<ApiMatcher>,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    #[serde(default)]
    comment: String,
}

#[derive(Debug, Deserialize)]
struct ApiSilenceStatus {
    state: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiMatcher {
    name: String,
    value: String,
    #[serde(default)]
    is_regex: bool,
    #[serde(default = "yes")]
    is_equal: bool,
}

fn yes() -> bool {
    true
}

/// One API with its client and token.
struct Api {
    client: http::Client,
    base: Url,
    headers: HeaderMap,
}

impl Api {
    async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<T, ReadError> {
        let mut url = http::join(&self.base, path);
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        let shown = http::join(&self.base, path);
        let body = http::send(&self.client, Method::GET, url, self.headers.clone(), None).await?;
        serde_json::from_slice(&body).map_err(|_| {
            ReadError::Connect(format!(
                "{shown} did not answer as Prometheus or Alertmanager do."
            ))
        })
    }

    /// `data` of a Prometheus answer.
    async fn data<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<T, ReadError> {
        let envelope: Envelope<T> = self.get(path, query).await?;
        let shown = http::join(&self.base, path);
        match (envelope.status.as_str(), envelope.data) {
            ("success", Some(data)) => Ok(data),
            _ => Err(ReadError::Connect(format!(
                "{shown} answered an error: {}",
                envelope.error.unwrap_or_else(|| envelope.status.clone())
            ))),
        }
    }
}

/// Headers carrying `token` as a bearer token, marked sensitive so that
/// no log shows it.
fn headers(token: Option<&str>) -> Result<HeaderMap, ReadError> {
    let mut headers = HeaderMap::new();
    if let Some(token) = token {
        let mut value = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| ReadError::Input("token: it holds characters a header cannot.".into()))?;
        value.set_sensitive(true);
        headers.insert(AUTHORIZATION, value);
    }
    Ok(headers)
}

/// Everything a running Prometheus, and its Alertmanager, describe.
pub async fn collect(connection: &Connection, context: &Context) -> Result<Collected, ReadError> {
    let url = connection
        .get("url")
        .ok_or_else(|| ReadError::Input("Fill in url.".into()))?;
    let client = http::client(context)?;
    let headers = headers(connection.get("token"))?;
    let prometheus = Api {
        client: client.clone(),
        base: http::base_url(url, "url")?,
        headers: headers.clone(),
    };
    let mut collected = Collected::default();

    let config: ConfigData = prometheus.data("api/v1/status/config", &[]).await?;
    let config: PromConfig = serde_norway::from_str(&config.yaml).unwrap_or_default();
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
    }

    let unames: QueryData = prometheus
        .data("api/v1/query", &[("query", "node_uname_info")])
        .await?;
    let unames: BTreeMap<(String, String), (Option<String>, Option<String>)> = unames
        .result
        .into_iter()
        .map(|s| {
            let key = (
                s.metric.get("job").cloned().unwrap_or_default(),
                s.metric.get("instance").cloned().unwrap_or_default(),
            );
            (
                key,
                (
                    s.metric.get("nodename").cloned(),
                    s.metric.get("sysname").cloned(),
                ),
            )
        })
        .collect();

    let targets: TargetsData = prometheus
        .data("api/v1/targets", &[("state", "active")])
        .await?;
    for active in targets.active_targets {
        let mut target = target_of(active, &mut collected.intervals);
        if let Some((nodename, sysname)) =
            unames.get(&(target.job.clone(), target.instance().to_owned()))
        {
            target.nodename.clone_from(nodename);
            target.sysname.clone_from(sysname);
        }
        collected.targets.push(target);
    }

    let rules: RulesData = prometheus
        .data("api/v1/rules", &[("type", "alert")])
        .await?;
    for rule in rules.groups.into_iter().flat_map(|g| g.rules) {
        if rule.kind != "alerting" {
            continue;
        }
        collected.rules.push(Rule {
            name: rule.name,
            expr: rule.query,
            for_s: rule.duration.max(0.0).min(f64::from(u32::MAX)) as u32,
            severity: rule.labels.get("severity").cloned(),
        });
    }

    if let Some(am_url) = connection.get("alertmanager_url") {
        let alertmanager = Api {
            client,
            base: http::base_url(am_url, "alertmanager_url")?,
            headers,
        };
        let status: AmStatus = alertmanager.get("api/v2/status", &[]).await?;
        let config: AmConfig = serde_norway::from_str(&status.config.original).map_err(|_| {
            ReadError::Connect("Alertmanager's configuration could not be read.".into())
        })?;
        collected.alertmanager = Some(config);
        let silences: Vec<ApiSilence> = alertmanager.get("api/v2/silences", &[]).await?;
        collected.silences = silences
            .into_iter()
            .filter(|s| matches!(s.status.state.as_str(), "active" | "pending"))
            .map(|s| SilenceIn {
                id: s.id,
                matchers: s
                    .matchers
                    .into_iter()
                    .map(|m| Matcher {
                        label: m.name,
                        op: match (m.is_regex, m.is_equal) {
                            (false, true) => MatchOp::Eq,
                            (false, false) => MatchOp::Ne,
                            (true, true) => MatchOp::Re,
                            (true, false) => MatchOp::Nre,
                        },
                        value: m.value,
                    })
                    .collect(),
                starts_at: s.starts_at,
                ends_at: s.ends_at,
                comment: s.comment,
            })
            .collect();
    }
    Ok(collected)
}

/// A target as Prometheus reports it: the scrape URL gives the address
/// scraped and, for a blackbox exporter, what it probes.
fn target_of(active: ActiveTarget, intervals: &mut BTreeMap<String, u32>) -> Target {
    let url = Url::parse(&active.scrape_url).ok();
    let address = url
        .as_ref()
        .and_then(|u| {
            let host = u.host_str()?;
            let host = if host.contains(':') {
                format!("[{host}]")
            } else {
                host.to_owned()
            };
            Some(format!("{host}:{}", u.port_or_known_default()?))
        })
        .unwrap_or_default();
    let param = |name: &str| {
        url.as_ref().and_then(|u| {
            u.query_pairs()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.into_owned())
        })
    };
    let probe = param("target").map(|target| Probe {
        module: param("module").unwrap_or_default(),
        target,
    });
    let job = active
        .labels
        .get("job")
        .cloned()
        .unwrap_or_else(|| active.scrape_pool.clone());
    if let Some(interval) = active.scrape_interval.as_deref().and_then(seconds) {
        intervals.insert(job.clone(), interval as u32);
    }
    Target {
        job,
        address,
        labels: active.labels,
        probe,
        nodename: None,
        sysname: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_from_the_api() {
        let mut intervals = BTreeMap::new();
        let active: ActiveTarget = serde_json::from_str(
            r#"{"labels": {"instance": "http://shop/", "job": "blackbox-http"},
                "scrapePool": "blackbox-http",
                "scrapeUrl": "http://monitor-01:9115/probe?module=http_2xx&target=http%3A%2F%2Fshop%2F",
                "scrapeInterval": "15s"}"#,
        )
        .unwrap();
        let target = target_of(active, &mut intervals);
        assert_eq!(target.address, "monitor-01:9115");
        assert_eq!(
            target.probe,
            Some(Probe {
                module: "http_2xx".into(),
                target: "http://shop/".into()
            })
        );
        assert_eq!(intervals["blackbox-http"], 15);
        let active: ActiveTarget = serde_json::from_str(
            r#"{"labels": {"instance": "web-01:9100", "job": "node"}, "scrapePool": "node",
                "scrapeUrl": "http://web-01:9100/metrics"}"#,
        )
        .unwrap();
        let target = target_of(active, &mut intervals);
        assert_eq!(
            (target.address.as_str(), target.probe),
            ("web-01:9100", None)
        );
    }

    #[test]
    fn tokens_are_sensitive() {
        let headers = headers(Some("s3cret")).unwrap();
        assert!(headers[AUTHORIZATION].is_sensitive());
        assert!(!format!("{headers:?}").contains("s3cret"));
        assert!(super::headers(Some("a\nb")).is_err());
    }
}
