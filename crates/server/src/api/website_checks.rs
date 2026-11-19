//! `GET /api/servers/{id}/website-checks`: for the server page, each
//! website check the server has (ADR-0150) with what its last run found:
//! up or why not, the status, time and final URL, and the certificate.
//!
//! Two VictoriaMetrics queries whatever the number of checks: the last
//! value of every series of the checks' metrics, and its time. A label
//! that changed (a new failure reason, a renewed certificate) leaves an
//! older series in the window; the newest one wins.

use std::collections::BTreeMap;

use axum::extract::{Path, State};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use super::access::authorize;
use super::auth::CurrentUser;
use super::metrics::{self, backend_error, quote, Target, MAX_LOOKBACK, METRIC_PREFIX};
use super::sensors::{parse_id, server_effective, EffectiveSensor, Origin};
use super::{ApiError, ApiState};

/// The metric suffixes of a website check, see `monitorflow_webcheck`.
const SUFFIXES: &[&str] = &[
    "up",
    "status_code",
    "response_seconds",
    "redirects",
    "content_ok",
    "cert_ok",
    "cert_name_ok",
    "cert_days_left",
    "cert_not_after",
];
/// How far back the last run is looked for, at least: VictoriaMetrics'
/// own staleness.
const MIN_LOOKBACK: i64 = 300;

/// Body of `GET /api/servers/{id}/website-checks`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct WebsiteChecks {
    pub checks: Vec<WebsiteCheck>,
}

/// Who runs a website check (ADR-0148).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum RunBy {
    /// The server's own agent.
    Agent,
    /// The central server.
    Central,
    /// A relay of the site, named by `relay`.
    Relay,
}

/// Who runs each probe-run check of a server, by `(name, instance)`.
type Probes = BTreeMap<(String, String), (RunBy, Option<String>)>;

/// One website check and its last run.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct WebsiteCheck {
    pub name: String,
    pub instance: String,
    /// The row it comes from.
    pub entry_id: Option<Uuid>,
    pub origin: Origin,
    pub url: String,
    pub run_by: RunBy,
    /// The relay that runs it, by its ID, when `run_by` is `relay`.
    pub relay: Option<String>,
    pub interval_ms: u64,
    /// When it last ran; `null` before its first run.
    pub last_at: Option<DateTime<Utc>>,
    /// Whether the answer was as expected.
    pub up: Option<bool>,
    /// Why not: `dns`, `connect`, `timeout`, `tls`, `certificate`,
    /// `status`, `content`, `redirects`, `refused` or `protocol`.
    pub reason: Option<String>,
    pub status_code: Option<u16>,
    pub response_seconds: Option<f64>,
    pub redirects: Option<u32>,
    /// Where the redirects ended, without its query.
    pub final_url: Option<String>,
    /// Whether the text conditions passed, when there are some.
    pub content_ok: Option<bool>,
    /// For https sites.
    pub certificate: Option<Certificate>,
}

/// The certificate of a website check's last https connection.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct Certificate {
    /// Trusted, valid now and for the name.
    pub ok: bool,
    /// What is wrong: `expired`, `not_yet_valid`, `self_signed`,
    /// `untrusted`, `name`, `revoked` or `other`.
    pub reason: Option<String>,
    pub name_ok: Option<bool>,
    pub days_left: Option<f64>,
    pub not_after: Option<DateTime<Utc>>,
    pub issuer: Option<String>,
    pub subject: Option<String>,
}

/// The newest series of one metric of one check.
#[derive(Debug, Clone, Default)]
struct Last {
    value: f64,
    at: f64,
    labels: BTreeMap<String, String>,
}

/// By `(metric, instance)`, the newest series.
type Newest = BTreeMap<(String, String), Last>;

fn selector(sensors: &[&EffectiveSensor], target: &Target) -> String {
    let mut names: Vec<&str> = sensors.iter().map(|s| s.name.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    // Names are `[a-z0-9_]`, checked when saved: no escaping needed.
    let pattern = format!(
        "{METRIC_PREFIX}({})_({})",
        names.join("|"),
        SUFFIXES.join("|")
    );
    format!("{{__name__=~{},{}}}", quote(&pattern), target.matchers())
}

/// Joins the values and times of the same series, keeping the newest per
/// `(metric, instance)`.
fn newest(values: Vec<metrics::SeriesView>, times: Vec<metrics::SeriesView>) -> Newest {
    let last = |s: &metrics::SeriesView| s.points.last().and_then(|p| p.1);
    let times: BTreeMap<BTreeMap<String, String>, f64> = times
        .iter()
        .filter_map(|s| Some((s.labels.clone(), last(s)?)))
        .collect();
    let mut newest = Newest::new();
    for series in values {
        let (Some(value), Some(&at)) = (last(&series), times.get(&series.labels)) else {
            continue;
        };
        let key = (
            series.labels.get("__name__").cloned().unwrap_or_default(),
            series.labels.get("instance").cloned().unwrap_or_default(),
        );
        let entry = newest.entry(key).or_default();
        if entry.labels.is_empty() || at > entry.at {
            *entry = Last {
                value,
                at,
                labels: series.labels,
            };
        }
    }
    newest
}

fn check(sensor: &EffectiveSensor, newest: &Newest, probes: &Probes) -> WebsiteCheck {
    let get = |suffix: &str| {
        let metric = format!("{METRIC_PREFIX}{}_{suffix}", sensor.name);
        newest.get(&(metric, sensor.instance.clone()))
    };
    let value = |suffix: &str| get(suffix).map(|l| l.value);
    let label = |suffix: &str, name: &str| get(suffix).and_then(|l| l.labels.get(name).cloned());
    let flag = |suffix: &str| value(suffix).map(|v| v >= 1.0);
    let up = get("up");
    let certificate = get("cert_ok").map(|ok| Certificate {
        ok: ok.value >= 1.0,
        reason: ok.labels.get("reason").cloned(),
        name_ok: flag("cert_name_ok"),
        days_left: value("cert_days_left"),
        not_after: value("cert_not_after").and_then(|t| DateTime::from_timestamp(t as i64, 0)),
        issuer: label("cert_days_left", "issuer"),
        subject: label("cert_days_left", "subject"),
    });
    let (run_by, relay) = probes
        .get(&(sensor.name.clone(), sensor.instance.clone()))
        .cloned()
        .unwrap_or((RunBy::Agent, None));
    WebsiteCheck {
        name: sensor.name.clone(),
        instance: sensor.instance.clone(),
        entry_id: sensor.entry_id,
        origin: sensor.origin,
        url: sensor.params.get("url").cloned().unwrap_or_default(),
        run_by,
        relay,
        interval_ms: sensor.interval_ms,
        last_at: up.and_then(|l| DateTime::from_timestamp(l.at as i64, 0)),
        up: up.map(|l| l.value >= 1.0),
        reason: up.and_then(|l| l.labels.get("reason").cloned()),
        status_code: value("status_code").map(|v| v as u16),
        response_seconds: value("response_seconds"),
        redirects: value("redirects").map(|v| v as u32),
        final_url: label("redirects", "final_url"),
        content_ok: flag("content_ok"),
        certificate,
    }
}

/// The website checks of `server` a probe runs, and which one.
async fn probes(pool: &sqlx::PgPool, server: Uuid) -> Result<Probes, sqlx::Error> {
    let rows: Vec<(String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT c.name, c.instance, c.probe, r.peer_id
         FROM sensor_configs c LEFT JOIN relays r ON r.id = c.probe_relay_id
         WHERE c.server_id = $1 AND c.kind = $2 AND c.probe IS NOT NULL AND c.enabled",
    )
    .bind(server)
    .bind(monitorflow_webcheck::KIND)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(name, instance, probe, relay)| {
            let run_by = if probe == "relay" {
                RunBy::Relay
            } else {
                RunBy::Central
            };
            ((name, instance), (run_by, relay))
        })
        .collect())
}

/// `GET /api/servers/{id}/website-checks`.
pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<WebsiteChecks>, ApiError> {
    let server_id = parse_id(&id)?;
    type Server = (String, Uuid, String, Option<i32>, bool);
    let row: Option<Server> = sqlx::query_as(
        "SELECT s.peer_id, s.site_id, si.name, si.check_interval_ms, s.kind = 'device'
         FROM servers s JOIN sites si ON si.id = s.site_id WHERE s.id = $1",
    )
    .bind(server_id)
    .fetch_optional(&state.pool)
    .await?;
    authorize(&user, row.as_ref().map(|r| r.1), false)?;
    let (peer, site_id, site, site_interval, device) = row.ok_or(ApiError::NotFound)?;
    let probes = probes(&state.pool, server_id).await?;
    let sensors = server_effective(&state.pool, server_id, site_id, site_interval).await?;
    let sensors: Vec<&EffectiveSensor> = sensors
        .iter()
        .filter(|s| s.kind == monitorflow_webcheck::KIND)
        // A device runs only its own probe checks (ADR-0148).
        .filter(|s| !device || probes.contains_key(&(s.name.clone(), s.instance.clone())))
        .collect();
    if sensors.is_empty() {
        return Ok(Json(WebsiteChecks { checks: Vec::new() }));
    }
    let vm = state
        .metrics
        .clone()
        .ok_or_else(|| ApiError::MetricsUnavailable("no VictoriaMetrics configured".into()))?;
    let target = Target { peer, site };
    let slowest = sensors
        .iter()
        .map(|s| s.interval_ms / 1000)
        .max()
        .unwrap_or(0);
    let lookback =
        (2 * i64::try_from(slowest).unwrap_or(MAX_LOOKBACK)).clamp(MIN_LOOKBACK, MAX_LOOKBACK);
    let selector = selector(&sensors, &target);
    let now = Utc::now().timestamp().to_string();
    let mut results = Vec::new();
    // Sequential, like the sensor values: two small queries.
    for function in ["last_over_time", "tlast_over_time"] {
        let params: Vec<(&str, String)> = [
            (
                "query",
                format!("{function}({selector}[{lookback}s]) keep_metric_names"),
            ),
            ("time", now.clone()),
        ]
        .into_iter()
        .chain(target.extra_labels())
        .collect();
        let data = vm
            .get("/api/v1/query", &params)
            .await
            .map_err(backend_error)?;
        results.push(metrics::series(data)?);
    }
    let times = results.pop().unwrap_or_default();
    let values = results.pop().unwrap_or_default();
    let newest = newest(values, times);
    let checks = sensors.iter().map(|s| check(s, &newest, &probes)).collect();
    Ok(Json(WebsiteChecks { checks }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use metrics::SeriesView;

    fn series(labels: &[(&str, &str)], at: f64, value: f64) -> (SeriesView, SeriesView) {
        let labels: BTreeMap<String, String> = labels
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        (
            SeriesView {
                labels: labels.clone(),
                points: vec![(1.0, Some(value))],
            },
            SeriesView {
                labels,
                points: vec![(1.0, Some(at))],
            },
        )
    }

    fn sensor(params: &[(&str, &str)]) -> EffectiveSensor {
        EffectiveSensor {
            origin: Origin::Server,
            entry_id: None,
            kind: "http".into(),
            name: "http".into(),
            instance: "shop".into(),
            interval_ms: 60_000,
            timeout_ms: None,
            params: params
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            probe: None,
            probe_relay_id: None,
        }
    }

    #[test]
    fn the_selector_covers_every_metric_of_the_checks() {
        let a = sensor(&[]);
        let target = Target {
            peer: "web-1".into(),
            site: "paris".into(),
        };
        let selector = selector(&[&a, &a], &target);
        assert!(selector.starts_with(r#"{__name__=~"monitorflow_(http)_(up|status_code|"#));
        assert!(selector.ends_with(r#"cert_not_after)",peer="web-1",site="paris"}"#));
    }

    #[test]
    fn the_newest_series_wins_and_fills_the_check() {
        let lines = [
            series(
                &[
                    ("__name__", "monitorflow_http_up"),
                    ("instance", "shop"),
                    ("reason", "status"),
                ],
                100.0,
                0.0,
            ),
            series(
                &[("__name__", "monitorflow_http_up"), ("instance", "shop")],
                160.0,
                1.0,
            ),
            series(
                &[("__name__", "monitorflow_http_up"), ("instance", "other")],
                170.0,
                0.0,
            ),
            series(
                &[
                    ("__name__", "monitorflow_http_status_code"),
                    ("instance", "shop"),
                ],
                160.0,
                200.0,
            ),
            series(
                &[
                    ("__name__", "monitorflow_http_response_seconds"),
                    ("instance", "shop"),
                ],
                160.0,
                0.5,
            ),
            series(
                &[
                    ("__name__", "monitorflow_http_redirects"),
                    ("instance", "shop"),
                    ("final_url", "https://www.shop.test/"),
                ],
                160.0,
                1.0,
            ),
            series(
                &[
                    ("__name__", "monitorflow_http_cert_ok"),
                    ("instance", "shop"),
                    ("reason", "expired"),
                ],
                160.0,
                0.0,
            ),
            series(
                &[
                    ("__name__", "monitorflow_http_cert_name_ok"),
                    ("instance", "shop"),
                ],
                160.0,
                1.0,
            ),
            series(
                &[
                    ("__name__", "monitorflow_http_cert_days_left"),
                    ("instance", "shop"),
                    ("issuer", "Test CA"),
                    ("subject", "shop.test"),
                ],
                160.0,
                -2.5,
            ),
            series(
                &[
                    ("__name__", "monitorflow_http_cert_not_after"),
                    ("instance", "shop"),
                ],
                160.0,
                1_763_000_000.0,
            ),
        ];
        let (values, times): (Vec<_>, Vec<_>) = lines.into_iter().unzip();
        let newest = newest(values, times);
        let probes = Probes::from([(
            ("http".to_owned(), "shop".to_owned()),
            (RunBy::Relay, Some("relay-paris".to_owned())),
        )]);
        let check = check(&sensor(&[("url", "https://shop.test/")]), &newest, &probes);
        assert_eq!(check.url, "https://shop.test/");
        assert_eq!(check.run_by, RunBy::Relay);
        assert_eq!(check.relay.as_deref(), Some("relay-paris"));
        assert_eq!(check.up, Some(true));
        assert_eq!(check.reason, None);
        assert_eq!(check.last_at.unwrap().timestamp(), 160);
        assert_eq!(check.status_code, Some(200));
        assert_eq!(check.response_seconds, Some(0.5));
        assert_eq!(check.redirects, Some(1));
        assert_eq!(check.final_url.as_deref(), Some("https://www.shop.test/"));
        assert_eq!(check.content_ok, None);
        let cert = check.certificate.unwrap();
        assert!(!cert.ok);
        assert_eq!(cert.reason.as_deref(), Some("expired"));
        assert_eq!(cert.name_ok, Some(true));
        assert_eq!(cert.days_left, Some(-2.5));
        assert_eq!(cert.not_after.unwrap().timestamp(), 1_763_000_000);
        assert_eq!(cert.issuer.as_deref(), Some("Test CA"));
        assert_eq!(cert.subject.as_deref(), Some("shop.test"));
    }

    #[test]
    fn a_check_that_never_ran_has_nothing_yet() {
        let check = check(
            &sensor(&[("url", "http://x/")]),
            &Newest::new(),
            &Probes::new(),
        );
        assert_eq!(check.run_by, RunBy::Agent);
        assert!(check.up.is_none() && check.last_at.is_none() && check.certificate.is_none());
    }
}
