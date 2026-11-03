//! What dashboard panels show (ADR-0095), fetched for a whole dashboard
//! in one request, and the metric lists the panel editor picks from.
//!
//! Tenancy does not rely on what is stored: the servers of each panel are
//! resolved for the viewer, from the sites it can read now, within the
//! dashboard's sharing. Queries are built here from those servers only:
//! their site names (and peer names when the panel names servers), with
//! the same site filter passed as VictoriaMetrics `extra_filters[]`, and
//! series of any other peer dropped from the answer.
//!
//! One request reads the servers once, runs at most [`PARALLEL`] queries
//! at a time, and keeps answers [`CACHE_TTL`] in [`Cache`], keyed by the
//! exact query, filters included.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Query, State};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use uuid::Uuid;

use super::auth::CurrentUser;
use super::dashboards::{check_panels, Combine, Order, Panel, PanelKind, Servers, TimeRange};
use super::metrics::{check_metric, quote, series, SeriesView, METRIC_PREFIX};
use super::sensors::body;
use super::{ApiError, ApiState};
use crate::vm_query::{QueryError, VmQuery};

/// Queries of one request running at once.
pub const PARALLEL: usize = 6;
/// How long an answer is reused.
pub const CACHE_TTL: Duration = Duration::from_secs(15);
/// Most answers kept.
const CACHE_ENTRIES: usize = 2_000;
/// Longest query plus filter sent, well under VictoriaMetrics' default
/// `-search.maxQueryLen` of 16 KiB.
pub const MAX_QUERY_BYTES: usize = 12_000;
/// Lines of a chart showing each server.
pub const EACH_LINES: usize = 8;
/// Figures and top lists show now, to this many seconds, so refreshes
/// within it share one answer.
const NOW_STEP: i64 = 30;
/// How far back the metric lists look.
const CATALOG_WINDOW: i64 = 86_400;
const MAX_CATALOG: usize = 1_000;

/// Answers of VictoriaMetrics by query, shared by every request of this
/// server instance. Two users share an entry only when they would have
/// sent the same query, filters included.
#[derive(Debug, Default)]
pub struct Cache {
    entries: Mutex<HashMap<String, (Instant, Value)>>,
}

impl Cache {
    fn get(&self, key: &str) -> Option<Value> {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries
            .get(key)
            .filter(|(at, _)| at.elapsed() < CACHE_TTL)
            .map(|(_, value)| value.clone())
    }

    fn put(&self, key: String, value: Value) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if entries.len() >= CACHE_ENTRIES {
            entries.retain(|_, (at, _)| at.elapsed() < CACHE_TTL);
            if entries.len() >= CACHE_ENTRIES {
                entries.clear();
            }
        }
        entries.insert(key, (Instant::now(), value));
    }
}

/// The servers a dashboard can show at most: everything the viewer reads,
/// or one site or customer of it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Within {
    #[default]
    All,
    Site {
        id: Uuid,
    },
    Customer {
        id: Uuid,
    },
}

impl Within {
    fn bind(self) -> (Option<Uuid>, Option<Uuid>) {
        match self {
            Self::All => (None, None),
            Self::Site { id } => (Some(id), None),
            Self::Customer { id } => (None, Some(id)),
        }
    }
}

/// Body of `POST /api/dashboard-data`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DataRequest {
    pub range: TimeRange,
    /// The dashboard's sharing: a site's or customer's dashboard shows
    /// only its servers.
    #[serde(default)]
    pub within: Within,
    pub panels: Vec<Panel>,
}

/// One line of a chart.
#[derive(Debug, Serialize)]
pub struct ChartSeries {
    pub label: String,
    /// The server of a line per server; absent when combined.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_id: Option<Uuid>,
    pub points: Vec<(f64, Option<f64>)>,
}

/// One row of a top list.
#[derive(Debug, Serialize)]
pub struct TopRow {
    pub server_id: Uuid,
    pub hostname: String,
    pub instance: Option<String>,
    pub value: Option<f64>,
}

/// One open alert.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct AlertRow {
    pub id: Uuid,
    pub server_id: Uuid,
    pub hostname: String,
    pub metric: String,
    pub instance: String,
    pub severity: String,
    pub opened_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum PanelResult {
    Chart {
        series: Vec<ChartSeries>,
    },
    Figure {
        value: Option<f64>,
    },
    Top {
        rows: Vec<TopRow>,
    },
    Alerts {
        alerts: Vec<AlertRow>,
    },
    /// A sentence for the panel; other panels still show.
    Error {
        error: String,
    },
}

#[derive(Debug, Serialize)]
pub struct PanelData {
    pub id: String,
    /// Servers the panel covers for this viewer.
    pub servers: usize,
    /// Servers the panel names that are removed or not visible to the
    /// viewer, without saying which.
    pub unavailable: usize,
    #[serde(flatten)]
    pub result: PanelResult,
}

/// Body of `POST /api/dashboard-data`.
#[derive(Debug, Serialize)]
pub struct DataResponse {
    /// The charts' range and step, unix seconds.
    pub start: i64,
    pub end: i64,
    pub step: i64,
    /// When figures and top lists were read.
    pub at: i64,
    pub panels: Vec<PanelData>,
}

/// A server the viewer reads, within the dashboard.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Visible {
    pub id: Uuid,
    pub peer_id: String,
    pub hostname: String,
    pub site_id: Uuid,
    pub site_name: String,
    pub customer_id: Uuid,
    pub removed: bool,
}

/// The servers of `target` among `visible`, removed ones left out, and
/// how many named servers are not there.
pub fn resolve<'a>(visible: &'a [Visible], target: &Servers) -> (Vec<&'a Visible>, usize) {
    let live = visible.iter().filter(|s| !s.removed);
    let found: Vec<&Visible> = match target {
        Servers::All => live.collect(),
        Servers::Site { id } => live.filter(|s| s.site_id == *id).collect(),
        Servers::Customer { id } => live.filter(|s| s.customer_id == *id).collect(),
        Servers::List { ids } => live.filter(|s| ids.contains(&s.id)).collect(),
    };
    let missing = match target {
        Servers::List { ids } => ids.iter().collect::<BTreeSet<_>>().len() - found.len(),
        _ => 0,
    };
    (found, missing)
}

/// `value` as a literal inside a RE2 regular expression.
pub fn regex_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        if "\\.+*?()|[]{}^$".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// A quoted regex matching exactly one of `names`.
fn one_of<'a>(names: impl IntoIterator<Item = &'a str>) -> String {
    let set: BTreeSet<&str> = names.into_iter().collect();
    quote(
        &set.into_iter()
            .map(regex_escape)
            .collect::<Vec<_>>()
            .join("|"),
    )
}

/// What a panel's query selects: its servers' sites, their peers when the
/// panel names servers, and the removed servers of those sites otherwise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub sites: String,
    pub peers: Option<String>,
    pub excluded: Option<String>,
}

impl Selection {
    pub fn new(servers: &[&Visible], visible: &[Visible], named: bool) -> Self {
        let sites: BTreeSet<&str> = servers.iter().map(|s| s.site_name.as_str()).collect();
        let excluded: Vec<&str> = visible
            .iter()
            .filter(|s| s.removed && sites.contains(s.site_name.as_str()))
            .map(|s| s.peer_id.as_str())
            .collect();
        Self {
            sites: one_of(sites.iter().copied()),
            peers: named.then(|| one_of(servers.iter().map(|s| s.peer_id.as_str()))),
            excluded: (!named && !excluded.is_empty()).then(|| one_of(excluded)),
        }
    }

    /// The selector of `metric`, optionally of one instance.
    pub fn selector(&self, metric: &str, instance: Option<&str>) -> String {
        let mut out = format!("{metric}{{site=~{}", self.sites);
        if let Some(peers) = &self.peers {
            out.push_str(&format!(",peer=~{peers}"));
        }
        if let Some(excluded) = &self.excluded {
            out.push_str(&format!(",peer!~{excluded}"));
        }
        if let Some(instance) = instance {
            out.push_str(&format!(",instance={}", quote(instance)));
        }
        out.push('}');
        out
    }

    /// The `extra_filters[]` VictoriaMetrics adds to every selector.
    pub fn filter(&self) -> String {
        format!("{{site=~{}}}", self.sites)
    }
}

/// Whether a low value of `metric` is the bad one (available, free, up),
/// as in the web interface's rollups (ADR-0020).
pub fn low_is_bad(metric: &str) -> bool {
    let short = metric.strip_prefix(METRIC_PREFIX).unwrap_or(metric);
    short.contains("available")
        || short.contains("free")
        || ["_up", "_active", "_loaded", "_installed"]
            .iter()
            .any(|end| short.ends_with(end))
}

/// The rollup of each step on long ranges: the worst value of the step,
/// or the average for sizes.
fn rollup(metric: &str) -> &'static str {
    let short = metric.strip_prefix(METRIC_PREFIX).unwrap_or(metric);
    let high_is_bad = [
        "_ratio",
        "_used_bytes",
        "_seconds",
        "_status",
        "_failed",
        "_matches",
        "_lag_bytes",
        "_lag_events",
        "_restarts",
    ]
    .iter()
    .any(|end| short.ends_with(end))
        || short.starts_with("cpu_load");
    if low_is_bad(metric) {
        "min_over_time"
    } else if high_is_bad {
        "max_over_time"
    } else {
        "avg_over_time"
    }
}

fn aggregation(combine: Combine) -> &'static str {
    match combine {
        Combine::Avg => "avg",
        Combine::Max => "max",
        Combine::Min => "min",
        Combine::Sum => "sum",
        Combine::Count | Combine::Each => "count",
    }
}

/// A query of a range or of now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    Range(String),
    Instant(String),
}

/// The MetricsQL of a chart, figure or top list over `selector`.
pub fn build(panel: &Panel, selector: &str, range: TimeRange) -> Option<Call> {
    let metric = panel.metric.as_deref()?;
    let combine = panel.combine.unwrap_or(Combine::Avg);
    Some(match panel.kind {
        PanelKind::Chart => {
            let rolled = if range.rolls_up() {
                format!("{}({selector}[{}s])", rollup(metric), range.step())
            } else {
                selector.to_owned()
            };
            Call::Range(match combine {
                Combine::Each if low_is_bad(metric) => {
                    format!("bottomk_min({EACH_LINES}, {rolled})")
                }
                Combine::Each => format!("topk_max({EACH_LINES}, {rolled})"),
                other => format!("{}({rolled})", aggregation(other)),
            })
        }
        PanelKind::Figure => Call::Instant(match combine {
            Combine::Count => format!("count(count by (peer) ({selector}))"),
            other => format!("{}({selector})", aggregation(other)),
        }),
        PanelKind::Top => {
            let function = match panel.order {
                Some(Order::Lowest) => "bottomk",
                _ => "topk",
            };
            Call::Instant(format!(
                "{function}({}, {selector})",
                panel.count.unwrap_or(5)
            ))
        }
        PanelKind::Alerts => return None,
    })
}

fn combined_label(combine: Combine) -> &'static str {
    match combine {
        Combine::Avg => "Average",
        Combine::Max => "Highest",
        Combine::Min => "Lowest",
        Combine::Sum => "Total",
        Combine::Count | Combine::Each => "Servers",
    }
}

/// A sentence for a panel whose query failed.
fn failure(error: &QueryError) -> String {
    match error {
        QueryError::Timeout => "The metrics backend timed out. Try a shorter range.".into(),
        QueryError::Rejected(message) => {
            tracing::warn!(%message, "VictoriaMetrics rejected a dashboard query");
            "The metrics backend refused this panel's query. Choose fewer servers or one disk."
                .into()
        }
        other => {
            tracing::warn!(error = %other, "dashboard query failed");
            "The metrics backend is unavailable.".into()
        }
    }
}

/// GETs `path` from `vm` through `cache`.
async fn cached(
    vm: &VmQuery,
    cache: &Cache,
    path: &str,
    params: &[(&str, String)],
) -> Result<Value, QueryError> {
    let key = std::iter::once(path.to_owned())
        .chain(params.iter().map(|(k, v)| format!("{k}={v}")))
        .collect::<Vec<_>>()
        .join("\n");
    if let Some(value) = cache.get(&key) {
        return Ok(value);
    }
    let value = vm.get(path, params).await?;
    cache.put(key, value.clone());
    Ok(value)
}

/// The readable servers of `within`, removed ones included so queries
/// can leave them out.
async fn visible_servers(
    state: &ApiState,
    user: &CurrentUser,
    within: Within,
) -> Result<Vec<Visible>, ApiError> {
    let (all, sites) = user.readable_sites().bind();
    let (site, customer) = within.bind();
    Ok(sqlx::query_as(
        "SELECT s.id, s.peer_id, s.hostname, s.site_id, si.name AS site_name, si.customer_id,
                s.revoked_at IS NOT NULL AS removed
         FROM servers s JOIN sites si ON si.id = s.site_id
         WHERE ($1 OR s.site_id = ANY($2))
           AND ($3::uuid IS NULL OR s.site_id = $3)
           AND ($4::uuid IS NULL OR si.customer_id = $4)",
    )
    .bind(all)
    .bind(&sites)
    .bind(site)
    .bind(customer)
    .fetch_all(&state.pool)
    .await?)
}

/// What one metric panel's answer becomes.
fn convert(panel: &Panel, servers: &[&Visible], data: Value) -> Result<PanelResult, ApiError> {
    let by_peer: HashMap<&str, &Visible> =
        servers.iter().map(|s| (s.peer_id.as_str(), *s)).collect();
    let combine = panel.combine.unwrap_or(Combine::Avg);
    let per_server = matches!(panel.kind, PanelKind::Top)
        || (panel.kind == PanelKind::Chart && combine == Combine::Each);
    // Defense in depth: a series of a peer outside the panel is dropped.
    let found: Vec<(Option<&Visible>, SeriesView)> = series(data)?
        .into_iter()
        .filter_map(|s| match s.labels.get("peer") {
            Some(peer) => by_peer.get(peer.as_str()).map(|v| (Some(*v), s)),
            None if !per_server => Some((None, s)),
            None => None,
        })
        .collect();
    Ok(match panel.kind {
        PanelKind::Chart => {
            let mut lines: Vec<ChartSeries> = found
                .into_iter()
                .map(|(server, s)| {
                    let label = match server {
                        None => combined_label(combine).to_owned(),
                        Some(server) => match (&panel.instance, s.labels.get("instance")) {
                            (None, Some(instance)) if !instance.is_empty() => {
                                format!("{} {instance}", server.hostname)
                            }
                            _ => server.hostname.clone(),
                        },
                    };
                    ChartSeries {
                        label,
                        server_id: server.map(|v| v.id),
                        points: s.points,
                    }
                })
                .collect();
            lines.sort_by(|a, b| a.label.cmp(&b.label));
            PanelResult::Chart { series: lines }
        }
        PanelKind::Figure => PanelResult::Figure {
            value: found
                .first()
                .and_then(|(_, s)| s.points.last())
                .and_then(|p| p.1),
        },
        PanelKind::Top => {
            let mut rows: Vec<TopRow> = found
                .into_iter()
                .filter_map(|(server, s)| {
                    let server = server?;
                    Some(TopRow {
                        server_id: server.id,
                        hostname: server.hostname.clone(),
                        instance: s.labels.get("instance").filter(|i| !i.is_empty()).cloned(),
                        value: s.points.last().and_then(|p| p.1),
                    })
                })
                .collect();
            let lowest = panel.order == Some(Order::Lowest);
            rows.sort_by(|a, b| {
                let (x, y) = (a.value.unwrap_or(f64::NAN), b.value.unwrap_or(f64::NAN));
                let order = x.total_cmp(&y);
                if lowest {
                    order
                } else {
                    order.reverse()
                }
            });
            rows.truncate(panel.count.unwrap_or(5) as usize);
            PanelResult::Top { rows }
        }
        PanelKind::Alerts => PanelResult::Alerts { alerts: Vec::new() },
    })
}

/// `POST /api/dashboard-data`: every panel of a dashboard at once.
pub async fn data(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<DataRequest>, JsonRejection>,
) -> Result<Json<DataResponse>, ApiError> {
    let request = body(request)?;
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    check_panels(&request.panels).map_err(ApiError::Invalid)?;
    let now = Utc::now().timestamp();
    let (range, step) = (request.range, request.range.step());
    let end = now / step * step;
    let start = end - range.seconds();
    let at = now / NOW_STEP * NOW_STEP;

    let visible = visible_servers(&state, &user, request.within).await?;
    let resolved: Vec<(Vec<&Visible>, usize)> = request
        .panels
        .iter()
        .map(|p| resolve(&visible, &p.servers))
        .collect();

    // Open alerts of every alerts panel, in one statement.
    let alert_servers: BTreeSet<Uuid> = request
        .panels
        .iter()
        .zip(&resolved)
        .filter(|(p, _)| p.kind == PanelKind::Alerts)
        .flat_map(|(_, (servers, _))| servers.iter().map(|s| s.id))
        .collect();
    let alerts: Vec<AlertRow> = if alert_servers.is_empty() {
        Vec::new()
    } else {
        sqlx::query_as(
            "SELECT i.id, i.server_id, s.hostname, i.metric, i.instance, i.severity, i.opened_at
             FROM incidents i JOIN servers s ON s.id = i.server_id
             WHERE i.state = 'open' AND i.server_id = ANY($1)
             ORDER BY i.severity = 'critical' DESC, i.opened_at DESC, i.id",
        )
        .bind(alert_servers.into_iter().collect::<Vec<_>>())
        .fetch_all(&state.pool)
        .await?
    };

    // Metric queries, at most PARALLEL at a time.
    let semaphore = Arc::new(Semaphore::new(PARALLEL));
    let mut tasks = JoinSet::new();
    let mut results: Vec<Option<PanelResult>> = Vec::new();
    for (index, (panel, (servers, _))) in request.panels.iter().zip(&resolved).enumerate() {
        let selection = Selection::new(
            servers,
            &visible,
            matches!(panel.servers, Servers::List { .. }),
        );
        let selector = selection.selector(
            panel.metric.as_deref().unwrap_or_default(),
            panel.instance.as_deref(),
        );
        let call = build(panel, &selector, range);
        let empty = || match panel.kind {
            PanelKind::Chart => PanelResult::Chart { series: Vec::new() },
            PanelKind::Figure => PanelResult::Figure { value: None },
            PanelKind::Top => PanelResult::Top { rows: Vec::new() },
            PanelKind::Alerts => PanelResult::Alerts { alerts: Vec::new() },
        };
        let (Some(call), false) = (call, servers.is_empty()) else {
            results.push(Some(empty()));
            continue;
        };
        let filter = selection.filter();
        let (path, mut params) = match call {
            Call::Range(query) => (
                "/api/v1/query_range",
                vec![
                    ("query", query),
                    ("start", start.to_string()),
                    ("end", end.to_string()),
                    ("step", step.to_string()),
                ],
            ),
            Call::Instant(query) => (
                "/api/v1/query",
                vec![("query", query), ("time", at.to_string())],
            ),
        };
        if params[0].1.len() + filter.len() > MAX_QUERY_BYTES {
            results.push(Some(PanelResult::Error {
                error: "Too many sites or servers for one panel. Choose a site or a customer."
                    .into(),
            }));
            continue;
        }
        let Some(vm) = state.metrics.clone() else {
            results.push(Some(PanelResult::Error {
                error: "No metrics backend is configured.".into(),
            }));
            continue;
        };
        params.push(("extra_filters[]", filter));
        results.push(None);
        let (cache, semaphore) = (state.dashboard_cache.clone(), semaphore.clone());
        tasks.spawn(async move {
            let _permit = semaphore.acquire_owned().await;
            (index, cached(&vm, &cache, path, &params).await)
        });
    }
    while let Some(joined) = tasks.join_next().await {
        let (index, answer) = joined.map_err(|e| ApiError::Internal(e.to_string()))?;
        let panel = &request.panels[index];
        results[index] = Some(match answer {
            Ok(data) => {
                convert(panel, &resolved[index].0, data).unwrap_or_else(|e| PanelResult::Error {
                    error: e.to_string(),
                })
            }
            Err(error) => PanelResult::Error {
                error: failure(&error),
            },
        });
    }

    let panels = request
        .panels
        .iter()
        .zip(resolved.iter())
        .zip(results)
        .map(|((panel, (servers, unavailable)), result)| {
            let result = match (panel.kind, result) {
                (PanelKind::Alerts, _) => {
                    let ids: BTreeSet<Uuid> = servers.iter().map(|s| s.id).collect();
                    PanelResult::Alerts {
                        alerts: alerts
                            .iter()
                            .filter(|a| ids.contains(&a.server_id))
                            .take(panel.count.unwrap_or(10) as usize)
                            .map(|a| AlertRow {
                                id: a.id,
                                server_id: a.server_id,
                                hostname: a.hostname.clone(),
                                metric: a.metric.clone(),
                                instance: a.instance.clone(),
                                severity: a.severity.clone(),
                                opened_at: a.opened_at,
                            })
                            .collect(),
                    }
                }
                (_, Some(result)) => result,
                (_, None) => PanelResult::Error {
                    error: "internal error".into(),
                },
            };
            PanelData {
                id: panel.id.clone(),
                servers: servers.len(),
                unavailable: *unavailable,
                result,
            }
        })
        .collect();
    Ok(Json(DataResponse {
        start,
        end,
        step,
        at,
        panels,
    }))
}

/// Query string of `GET /api/dashboard-metrics`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogParams {
    pub site_id: Option<Uuid>,
    pub customer_id: Option<Uuid>,
    /// Also list what this metric watches.
    pub metric: Option<String>,
}

/// Body of `GET /api/dashboard-metrics`.
#[derive(Debug, Serialize)]
pub struct Catalog {
    /// `monitorflow_*` metrics reported in the last day, sorted.
    pub metrics: Vec<String>,
    /// The instances of `metric`, sorted; empty without it.
    pub instances: Vec<String>,
}

/// `GET /api/dashboard-metrics`: what panels can show, among the servers
/// the user reads, optionally of one site or customer.
pub async fn catalog(
    State(state): State<ApiState>,
    user: CurrentUser,
    params: Result<Query<CatalogParams>, QueryRejection>,
) -> Result<Json<Catalog>, ApiError> {
    let Query(params) = params.map_err(|r| ApiError::Invalid(r.body_text()))?;
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    if let Some(metric) = &params.metric {
        check_metric(metric)?;
    }
    if params
        .site_id
        .is_some_and(|s| !user.readable_sites().contains(s))
    {
        return Err(ApiError::NotFound);
    }
    let (all, sites) = user.readable_sites().bind();
    let names: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sites
         WHERE ($1 OR id = ANY($2)) AND ($3::uuid IS NULL OR id = $3)
           AND ($4::uuid IS NULL OR customer_id = $4)",
    )
    .bind(all)
    .bind(&sites)
    .bind(params.site_id)
    .bind(params.customer_id)
    .fetch_all(&state.pool)
    .await?;
    let mut catalog = Catalog {
        metrics: Vec::new(),
        instances: Vec::new(),
    };
    if names.is_empty() {
        return Ok(Json(catalog));
    }
    let vm = state
        .metrics
        .clone()
        .ok_or_else(|| ApiError::MetricsUnavailable("no VictoriaMetrics configured".into()))?;
    let sites = one_of(names.iter().map(String::as_str));
    let filter = format!("{{site=~{sites}}}");
    if filter.len() * 2 > MAX_QUERY_BYTES {
        return Err(ApiError::Invalid(
            "Too many sites at once. Choose a customer or a site.".into(),
        ));
    }
    // Aligned to the minute, so the editor's requests share answers.
    let now = Utc::now().timestamp() / 60 * 60;
    let window = |matcher: String| {
        vec![
            ("match[]", matcher),
            ("start", (now - CATALOG_WINDOW).to_string()),
            ("end", now.to_string()),
            ("extra_filters[]", filter.clone()),
        ]
    };
    let strings = |data: Value| -> Vec<String> {
        match data {
            Value::Array(values) => values
                .into_iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .take(MAX_CATALOG)
                .collect(),
            _ => Vec::new(),
        }
    };
    let cache = &state.dashboard_cache;
    let matcher = format!(
        "{{__name__=~{},site=~{sites}}}",
        quote(&format!("{METRIC_PREFIX}.+"))
    );
    let names = cached(
        &vm,
        cache,
        "/api/v1/label/__name__/values",
        &window(matcher),
    )
    .await
    .map_err(backend)?;
    catalog.metrics = strings(names)
        .into_iter()
        .filter(|m| check_metric(m).is_ok())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if let Some(metric) = &params.metric {
        let matcher = format!("{metric}{{site=~{sites}}}");
        let values = cached(
            &vm,
            cache,
            "/api/v1/label/instance/values",
            &window(matcher),
        )
        .await
        .map_err(backend)?;
        catalog.instances = strings(values)
            .into_iter()
            .filter(|i| !i.is_empty())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
    }
    Ok(Json(catalog))
}

fn backend(error: QueryError) -> ApiError {
    match error {
        QueryError::Timeout => ApiError::MetricsTimeout,
        other => ApiError::MetricsUnavailable(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn server(n: u128, site: &str, customer: u128, removed: bool) -> Visible {
        Visible {
            id: Uuid::from_u128(n),
            peer_id: format!("agent-{n}"),
            hostname: format!("web-{n}"),
            site_id: Uuid::from_u128(1000 + site.len() as u128),
            site_name: site.to_owned(),
            customer_id: Uuid::from_u128(customer),
            removed,
        }
    }

    fn panel(value: Value) -> Panel {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn regex_characters_in_site_names_are_literal() {
        assert_eq!(regex_escape("a.b|c(d)*"), r"a\.b\|c\(d\)\*");
        assert_eq!(one_of(["b", "a.x", "b"]), r#""a\\.x|b""#);
        assert_eq!(one_of(["o\"b"]), r#""o\"b""#);
    }

    #[test]
    fn selections_name_sites_peers_and_leave_out_removed_servers() {
        let visible = vec![
            server(1, "paris", 1, false),
            server(2, "paris", 1, true),
            server(3, "lyon.x", 1, false),
        ];
        let (all, missing) = resolve(&visible, &Servers::All);
        assert_eq!((all.len(), missing), (2, 0));
        let selection = Selection::new(&all, &visible, false);
        assert_eq!(
            selection.selector("monitorflow_cpu_usage_ratio", Some("total")),
            r#"monitorflow_cpu_usage_ratio{site=~"lyon\\.x|paris",peer!~"agent-2",instance="total"}"#
        );
        assert_eq!(selection.filter(), r#"{site=~"lyon\\.x|paris"}"#);

        let ids = vec![Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(9)];
        let (named, missing) = resolve(&visible, &Servers::List { ids });
        assert_eq!((named.len(), missing), (1, 2));
        let selection = Selection::new(&named, &visible, true);
        assert_eq!(
            selection.selector("monitorflow_disk_used_ratio", None),
            r#"monitorflow_disk_used_ratio{site=~"paris",peer=~"agent-1"}"#
        );
        let site = Servers::Site {
            id: visible[2].site_id,
        };
        assert_eq!(resolve(&visible, &site).0.len(), 1);
        let other = Servers::Customer {
            id: Uuid::from_u128(7),
        };
        assert!(resolve(&visible, &other).0.is_empty());
    }

    #[test]
    fn panels_become_aggregations_in_the_backend() {
        let s = "m{site=~\"p\"}";
        let chart = |combine: &str, metric: &str| {
            panel(
                json!({"id": "a", "title": "t", "kind": "chart", "metric": metric, "combine": combine}),
            )
        };
        let cpu = "monitorflow_cpu_usage_ratio";
        let free = "monitorflow_memory_available_bytes";
        assert_eq!(
            build(&chart("each", cpu), s, TimeRange::Hour),
            Some(Call::Range(format!("topk_max(8, {s})")))
        );
        assert_eq!(
            build(&chart("each", free), s, TimeRange::Week),
            Some(Call::Range(format!(
                "bottomk_min(8, min_over_time({s}[1800s]))"
            )))
        );
        assert_eq!(
            build(&chart("avg", cpu), s, TimeRange::Day),
            Some(Call::Range(format!("avg(max_over_time({s}[300s]))")))
        );
        assert_eq!(
            build(
                &chart("sum", "monitorflow_disk_total_bytes"),
                s,
                TimeRange::Month
            ),
            Some(Call::Range(format!("sum(avg_over_time({s}[7200s]))")))
        );
        let figure = panel(
            json!({"id": "a", "title": "t", "kind": "figure", "metric": cpu, "combine": "count"}),
        );
        assert_eq!(
            build(&figure, s, TimeRange::Hour),
            Some(Call::Instant(format!("count(count by (peer) ({s}))")))
        );
        let top = panel(
            json!({"id": "a", "title": "t", "kind": "top", "metric": free, "order": "lowest", "count": 3}),
        );
        assert_eq!(
            build(&top, s, TimeRange::Hour),
            Some(Call::Instant(format!("bottomk(3, {s})")))
        );
        let alerts = panel(json!({"id": "a", "title": "t", "kind": "alerts", "count": 3}));
        assert_eq!(build(&alerts, s, TimeRange::Hour), None);
    }

    #[test]
    fn answers_keep_only_the_panels_servers() {
        let visible = [server(1, "paris", 1, false), server(2, "paris", 1, false)];
        let servers: Vec<&Visible> = visible.iter().collect();
        let matrix = json!({"result": [
            {"metric": {"peer": "agent-1", "instance": "/"}, "values": [[1, "0.5"]]},
            {"metric": {"peer": "agent-2", "instance": "/data"}, "values": [[1, "0.9"]]},
            {"metric": {"peer": "intruder", "instance": "/"}, "values": [[1, "0.99"]]},
            {"metric": {}, "values": [[1, "0.1"]]}
        ]});
        let chart = panel(json!({"id": "a", "title": "t", "kind": "chart",
            "metric": "monitorflow_disk_used_ratio", "combine": "each"}));
        let PanelResult::Chart { series } = convert(&chart, &servers, matrix.clone()).unwrap()
        else {
            panic!()
        };
        let labels: Vec<&str> = series.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels, ["web-1 /", "web-2 /data"]);

        let top = panel(json!({"id": "a", "title": "t", "kind": "top",
            "metric": "monitorflow_disk_used_ratio", "order": "highest", "count": 1}));
        let PanelResult::Top { rows } = convert(&top, &servers, matrix.clone()).unwrap() else {
            panic!()
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(
            (rows[0].hostname.as_str(), rows[0].value),
            ("web-2", Some(0.9))
        );

        let avg = panel(json!({"id": "a", "title": "t", "kind": "chart",
            "metric": "monitorflow_disk_used_ratio", "combine": "avg"}));
        let combined = json!({"result": [{"metric": {}, "values": [[1, "0.1"]]}]});
        let PanelResult::Chart { series } = convert(&avg, &servers, combined).unwrap() else {
            panic!()
        };
        assert_eq!(
            (series[0].label.as_str(), series[0].server_id),
            ("Average", None)
        );
    }

    #[test]
    fn the_cache_keeps_answers_for_a_while() {
        let cache = Cache::default();
        cache.put("q".into(), json!(1));
        assert_eq!(cache.get("q"), Some(json!(1)));
        assert_eq!(cache.get("other"), None);
        for i in 0..CACHE_ENTRIES + 1 {
            cache.put(i.to_string(), json!(i));
        }
        assert!(cache.entries.lock().unwrap().len() <= CACHE_ENTRIES);
    }
}
