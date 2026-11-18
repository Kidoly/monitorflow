//! Website checks (ADR-0150): a page requested over HTTP or HTTPS the way
//! a visitor would, its status, text, time and redirects, and the
//! certificate it came with.
//!
//! The check is a [`Sensor`] of kind [`KIND`], built by [`build`]. It is
//! its own crate rather than part of `monitorflow-sensors`, which keeps no
//! HTTP or TLS client (ADR-0021), and it is run by an agent (the site seen
//! from that server) or by a probe, the central server or a relay.
//!
//! Metrics, prefixed with the configured name (`http` by default):
//!
//! | Metric | Value |
//! | --- | --- |
//! | `up` | 1 when the answer is as expected; 0 with label `reason` otherwise |
//! | `status_code` | Status of the last answer |
//! | `response_seconds` | From the start to the end of the page, redirects included |
//! | `redirects` | Redirects followed, with label `final_url` |
//! | `content_ok` | 1 when the page holds the text expected and not the text refused |
//! | `cert_ok` | 1 when the certificate is trusted, valid now and for the name; 0 with `reason` |
//! | `cert_name_ok` | 1 when the certificate is for the host asked |
//! | `cert_days_left` | Days until it expires, negative after; labels `issuer` and `subject` |
//! | `cert_not_after` | When it expires, Unix seconds |
//!
//! The configured URL is label `url` of `up`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use monitorflow_sensors::port::PortTargets;
use monitorflow_sensors::{BuildError, Sample, Sensor, SensorError};
use tokio::time::Instant;

pub mod certificate;
pub mod fetch;
pub mod spec;

pub use fetch::{Failure, Limits, Outcome};
pub use spec::{Spec, PARAMS, SECRET_PARAM};

/// The sensor kind of a website check.
pub const KIND: &str = "http";

/// Time kept before the deadline to report `up 0` rather than be
/// cancelled with nothing, which could not alert.
const MARGIN: Duration = Duration::from_millis(200);
/// Longest label value the server keeps.
const MAX_LABEL: usize = 256;

/// Checks `params` and builds the check; `targets` is the agent's
/// `sensors.port_targets`, when it sets one (ADR-0111). The server calls
/// it without targets to check a row before saving it.
pub fn build(
    params: &BTreeMap<String, String>,
    targets: Option<&PortTargets>,
) -> Result<Arc<dyn Sensor>, BuildError> {
    let spec = Spec::parse(params)?;
    let limits = Limits {
        targets: targets.cloned(),
        roots: None,
    };
    limits
        .check(&spec.url)
        .map_err(|reason| BuildError::Refused { kind: KIND, reason })?;
    Ok(Arc::new(WebCheck { spec, limits }))
}

/// A website check, see the [crate] documentation.
#[derive(Debug)]
pub struct WebCheck {
    spec: Spec,
    limits: Limits,
}

impl WebCheck {
    /// A check of `spec` within `limits`, for callers that set their own
    /// certificate authorities.
    pub fn new(spec: Spec, limits: Limits) -> Self {
        Self { spec, limits }
    }
}

fn label(text: &str) -> String {
    let mut text: String = text.chars().filter(|c| !c.is_control()).collect();
    while text.len() > MAX_LABEL {
        text.pop();
    }
    text
}

fn bool(value: bool) -> f64 {
    if value {
        1.0
    } else {
        0.0
    }
}

/// The samples of one run.
pub fn samples(spec: &Spec, outcome: &Outcome, now_unix: i64) -> Vec<Sample> {
    let mut up = Sample::metric("up", bool(outcome.failure.is_none()))
        .with_label("url", label(spec.url.as_str()));
    if let Some(failure) = outcome.failure {
        up = up.with_label("reason", failure.as_str());
    }
    let mut samples = vec![up];
    if let Some(status) = outcome.status {
        samples.push(Sample::metric("status_code", f64::from(status)));
    }
    if let Some(seconds) = outcome.seconds {
        samples.push(Sample::metric("response_seconds", seconds));
    }
    if spec.follow_redirects && outcome.status.is_some() {
        let mut shown = outcome.final_url.clone();
        // A query may carry a session token: not kept in the label.
        shown.set_query(None);
        shown.set_fragment(None);
        samples.push(
            Sample::metric("redirects", f64::from(outcome.redirects))
                .with_label("final_url", label(shown.as_str())),
        );
    }
    if let Some(ok) = outcome.content_ok {
        samples.push(Sample::metric("content_ok", bool(ok)));
    }
    if let Some(cert) = &outcome.certificate {
        let mut ok = Sample::metric("cert_ok", bool(cert.ok()));
        if let Some(problem) = cert.problem {
            ok = ok.with_label("reason", problem.as_str());
        }
        samples.push(ok);
        samples.push(Sample::metric("cert_name_ok", bool(cert.name_ok)));
        let mut days = Sample::metric("cert_days_left", cert.days_left(now_unix));
        if !cert.issuer.is_empty() {
            days = days.with_label("issuer", label(&cert.issuer));
        }
        if !cert.subject.is_empty() {
            days = days.with_label("subject", label(&cert.subject));
        }
        samples.push(days);
        samples.push(Sample::metric("cert_not_after", cert.not_after as f64));
    }
    samples
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

impl WebCheck {
    pub fn spec(&self) -> &Spec {
        &self.spec
    }

    /// Runs once, stopping a little before `deadline`.
    pub async fn run(&self, deadline: Instant) -> Outcome {
        let now = Instant::now();
        let cutoff = deadline - MARGIN.min(deadline.saturating_duration_since(now) / 2);
        fetch::run(&self.spec, &self.limits, cutoff).await
    }
}

#[async_trait::async_trait]
impl Sensor for WebCheck {
    async fn collect(&self, deadline: Instant) -> Result<Vec<Sample>, SensorError> {
        let outcome = self.run(deadline).await;
        Ok(samples(&self.spec, &outcome, now_unix()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::certificate::{Problem, Report};

    fn params(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn the_agent_limits_apply_when_built() {
        let targets = PortTargets::parse(&["10.0.0.0/8"]).unwrap();
        assert!(build(&params(&[("url", "http://10.0.0.1/")]), Some(&targets)).is_ok());
        let error = build(&params(&[("url", "http://192.0.2.1/")]), Some(&targets))
            .err()
            .unwrap();
        assert!(
            matches!(error, BuildError::Refused { kind: "http", .. }),
            "{error}"
        );
        assert!(build(&params(&[("url", "http://192.0.2.1/")]), None).is_ok());
        assert!(build(&params(&[]), None).is_err());
    }

    #[test]
    fn samples_say_what_failed_and_what_the_certificate_says() {
        let spec = Spec::parse(&params(&[
            ("url", "https://shop.test/?s=1"),
            ("contains", "x"),
        ]))
        .unwrap();
        let mut final_url = url::Url::parse("https://www.shop.test/home?session=abc").unwrap();
        final_url.set_fragment(Some("top"));
        let outcome = Outcome {
            failure: Some(Failure::Content),
            status: Some(200),
            seconds: Some(0.25),
            redirects: 1,
            final_url,
            content_ok: Some(false),
            certificate: Some(Report {
                problem: Some(Problem::Expired),
                name_ok: true,
                issuer: "Test CA".into(),
                subject: "shop.test".into(),
                not_after: 1_000_000,
            }),
        };
        let text: Vec<String> = samples(&spec, &outcome, 1_000_000 + 86_400)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            text,
            [
                r#"up{reason="content",url="https://shop.test/?s=1"} 0"#,
                "status_code 200",
                "response_seconds 0.25",
                r#"redirects{final_url="https://www.shop.test/home"} 1"#,
                "content_ok 0",
                r#"cert_ok{reason="expired"} 0"#,
                "cert_name_ok 1",
                r#"cert_days_left{issuer="Test CA",subject="shop.test"} -1"#,
                "cert_not_after 1000000",
            ]
        );
    }

    #[test]
    fn a_site_that_does_not_answer_reports_up_0_only() {
        let spec = Spec::parse(&params(&[
            ("url", "http://x.test/"),
            ("follow_redirects", "false"),
        ]))
        .unwrap();
        let outcome = Outcome {
            failure: Some(Failure::Connect),
            status: None,
            seconds: None,
            redirects: 0,
            final_url: spec.url.clone(),
            content_ok: None,
            certificate: None,
        };
        let text: Vec<String> = samples(&spec, &outcome, 0)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(text, [r#"up{reason="connect",url="http://x.test/"} 0"#]);
    }

    #[test]
    fn labels_are_cut_to_what_the_server_keeps() {
        assert_eq!(label(&"é".repeat(200)).len(), 256);
        assert_eq!(label("a\nb"), "ab");
    }
}

/// The kinds a probe runs (ADR-0148) plus website checks: what the
/// central server and relays build their checks with.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProbeKinds;

impl monitorflow_probe::ProbeFactory for ProbeKinds {
    fn build(
        &self,
        kind: &str,
        params: &BTreeMap<String, String>,
    ) -> Result<Arc<dyn Sensor>, String> {
        if kind == KIND {
            // A probe has no port targets: those are an agent's own limit.
            build(params, None).map_err(|e| e.to_string())
        } else {
            monitorflow_probe::Builtin.build(kind, params)
        }
    }
}

#[cfg(test)]
mod probe_tests {
    use super::*;
    use monitorflow_probe::ProbeFactory;

    #[test]
    fn probes_build_website_checks_and_the_network_kinds() {
        let url = BTreeMap::from([("url".to_owned(), "https://example.com/".to_owned())]);
        assert!(ProbeKinds.build("http", &url).is_ok());
        let error = ProbeKinds.build("http", &BTreeMap::new()).err().unwrap();
        assert!(error.contains("`url` is required"), "{error}");
        let host = BTreeMap::from([("host".to_owned(), "192.0.2.1".to_owned())]);
        assert!(ProbeKinds.build("ping", &host).is_ok());
        assert!(ProbeKinds.build("cpu", &BTreeMap::new()).is_err());
    }
}
