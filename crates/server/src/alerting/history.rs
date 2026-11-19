//! History for richer rules (ADR-0153): the past of a series, read from
//! VictoriaMetrics by a task of its own so the engine never waits on it.
//!
//! The engine sends [`Request`]s without waiting and receives [`Answer`]s
//! in its own loop. At most [`CONCURRENCY`] queries run at a time; a full
//! queue drops the request, which the engine asks again later. Queries
//! carry the server's `peer` and `site` matchers and the same values as
//! `extra_label` filters, as ADR-0018's do.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::{mpsc, Semaphore};
use uuid::Uuid;

use super::rules::Function;
use crate::api::metrics::{quote, Target};
use crate::vm_query::{QueryError, VmQuery};

/// Queries running at once.
pub const CONCURRENCY: usize = 4;
/// Requests waiting; more are dropped.
const QUEUE: usize = 4_096;

/// Points of one instance, `(Unix ms, value)`, oldest first.
pub type Points = Vec<(i64, f64)>;

/// What the engine asks for.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    /// The past of every instance of `metric`, one point per bucket of
    /// `step_ms`, read with `function` (`avg`, `min` or `max`).
    Seed {
        server_id: Uuid,
        target: Target,
        metric: String,
        function: Function,
        from_ms: i64,
        to_ms: i64,
        step_ms: i64,
    },
    /// The instances of `metric` seen since `from_ms`.
    Instances {
        server_id: Uuid,
        target: Target,
        metric: String,
        from_ms: i64,
    },
    /// The average over `window_s` of every instance of `metric`, every
    /// `step_ms` from `from_ms` to `to_ms`: a comparison's past.
    Past {
        key: PastKey,
        target: Target,
        from_ms: i64,
        to_ms: i64,
        step_ms: i64,
    },
}

/// A comparison's past: one per server, metric, window and offset.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PastKey {
    pub server_id: Uuid,
    pub metric: String,
    pub window_s: u32,
    pub offset_s: u32,
}

/// What the task answers; `series` is empty when the query failed.
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    Seed {
        server_id: Uuid,
        metric: String,
        step_ms: i64,
        from_ms: i64,
        series: Vec<(String, Points)>,
    },
    Instances {
        server_id: Uuid,
        metric: String,
        instances: Vec<String>,
    },
    Past {
        key: PastKey,
        from_ms: i64,
        to_ms: i64,
        step_ms: i64,
        series: Vec<(String, Points)>,
        failed: bool,
    },
}

pub type Fetch<'a> = Pin<Box<dyn Future<Output = Result<Value, QueryError>> + Send + 'a>>;

/// Where history is read: VictoriaMetrics, or a fake in tests.
pub trait Backend: Send + Sync + 'static {
    /// GETs a Prometheus API `path` and returns its `data`.
    fn get<'a>(&'a self, path: &'a str, params: &'a [(&'a str, String)]) -> Fetch<'a>;
}

impl Backend for VmQuery {
    fn get<'a>(&'a self, path: &'a str, params: &'a [(&'a str, String)]) -> Fetch<'a> {
        Box::pin(VmQuery::get(self, path, params))
    }
}

/// The selector of every instance of `metric` of `target`.
pub fn selector(metric: &str, target: &Target) -> String {
    format!(
        "{metric}{{peer={},site={}}}",
        quote(&target.peer),
        quote(&target.site)
    )
}

/// The rollup function that reads `function` from history.
fn rollup(function: Function) -> &'static str {
    match function {
        Function::Min => "min_over_time",
        Function::Max => "max_over_time",
        _ => "avg_over_time",
    }
}

fn extra_labels(target: &Target) -> [(&'static str, String); 2] {
    [
        ("extra_label", format!("peer={}", target.peer)),
        ("extra_label", format!("site={}", target.site)),
    ]
}

fn seconds(ms: i64) -> String {
    format!("{:.3}", ms as f64 / 1000.0)
}

/// The query of a request: path and parameters.
pub fn query(request: &Request) -> (&'static str, Vec<(&'static str, String)>) {
    let range = |promql: String, from: i64, to: i64, step: i64, target: &Target| {
        let mut params = vec![
            ("query", promql),
            ("start", seconds(from)),
            ("end", seconds(to)),
            ("step", format!("{}s", (step / 1000).max(1))),
        ];
        params.extend(extra_labels(target));
        ("/api/v1/query_range", params)
    };
    match request {
        Request::Seed {
            target,
            metric,
            function,
            from_ms,
            to_ms,
            step_ms,
            ..
        } => {
            let window = (step_ms / 1000).max(1);
            let promql = format!(
                "{}({}[{window}s])",
                rollup(*function),
                selector(metric, target)
            );
            range(promql, *from_ms, *to_ms, *step_ms, target)
        }
        Request::Past {
            key,
            target,
            from_ms,
            to_ms,
            step_ms,
        } => {
            let promql = format!(
                "avg_over_time({}[{}s])",
                selector(&key.metric, target),
                key.window_s
            );
            range(promql, *from_ms, *to_ms, *step_ms, target)
        }
        Request::Instances {
            target,
            metric,
            from_ms,
            ..
        } => {
            let mut params = vec![
                ("match[]", selector(metric, target)),
                ("start", seconds(*from_ms)),
            ];
            params.extend(extra_labels(target));
            ("/api/v1/series", params)
        }
    }
}

/// The points of a `matrix` answer by instance (`""` without the label),
/// only of the series of `target`.
pub fn matrix(data: &Value, target: &Target) -> Option<Vec<(String, Points)>> {
    let mut out = Vec::new();
    for result in data["result"].as_array()? {
        let labels = &result["metric"];
        if labels["peer"].as_str().is_some_and(|p| p != target.peer) {
            continue;
        }
        let instance = labels["instance"].as_str().unwrap_or_default().to_owned();
        let points = result["values"]
            .as_array()?
            .iter()
            .filter_map(|p| {
                let at = p.get(0)?.as_f64()?;
                let value: f64 = p.get(1)?.as_str()?.parse().ok()?;
                value.is_finite().then_some(((at * 1000.0) as i64, value))
            })
            .collect();
        out.push((instance, points));
    }
    Some(out)
}

/// The instances of a `/api/v1/series` answer, sorted and unique.
fn instances(data: &Value, target: &Target) -> Option<Vec<String>> {
    let mut out: Vec<String> = data
        .as_array()?
        .iter()
        .filter(|labels| labels["peer"].as_str().is_none_or(|p| p == target.peer))
        .map(|labels| labels["instance"].as_str().unwrap_or_default().to_owned())
        .collect();
    out.sort();
    out.dedup();
    Some(out)
}

/// The engine's end of the task.
pub struct History {
    tx: mpsc::Sender<Request>,
}

impl History {
    /// Starts the task; answers go to `answers` until it is closed.
    pub fn start(backend: Arc<dyn Backend>, answers: mpsc::UnboundedSender<Answer>) -> Self {
        let (tx, mut rx) = mpsc::channel::<Request>(QUEUE);
        tokio::spawn(async move {
            let permits = Arc::new(Semaphore::new(CONCURRENCY));
            while let Some(request) = rx.recv().await {
                let Ok(permit) = permits.clone().acquire_owned().await else {
                    return;
                };
                let (backend, answers) = (backend.clone(), answers.clone());
                tokio::spawn(async move {
                    let answer = run(backend.as_ref(), request).await;
                    drop(permit);
                    let _ = answers.send(answer);
                });
            }
        });
        Self { tx }
    }

    /// Queues `request`; `false` when the queue is full.
    pub fn ask(&self, request: Request) -> bool {
        self.tx.try_send(request).is_ok()
    }
}

async fn run(backend: &dyn Backend, request: Request) -> Answer {
    let (path, params) = query(&request);
    let data = backend.get(path, &params).await;
    if let Err(error) = &data {
        tracing::warn!(%error, path, "could not read history for an alert rule");
    }
    match request {
        Request::Seed {
            server_id,
            target,
            metric,
            from_ms,
            step_ms,
            ..
        } => Answer::Seed {
            server_id,
            metric,
            step_ms,
            from_ms,
            series: data
                .ok()
                .and_then(|d| matrix(&d, &target))
                .unwrap_or_default(),
        },
        Request::Instances {
            server_id,
            target,
            metric,
            ..
        } => Answer::Instances {
            server_id,
            metric,
            instances: data
                .ok()
                .and_then(|d| instances(&d, &target))
                .unwrap_or_default(),
        },
        Request::Past {
            key,
            target,
            from_ms,
            to_ms,
            step_ms,
        } => {
            let series = data.ok().and_then(|d| matrix(&d, &target));
            Answer::Past {
                key,
                from_ms,
                to_ms,
                step_ms,
                failed: series.is_none(),
                series: series.unwrap_or_default(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn target() -> Target {
        Target {
            peer: "web-1".into(),
            site: "par\"is".into(),
        }
    }

    #[test]
    fn queries_carry_the_server_matchers_and_filters() {
        let (path, params) = query(&Request::Seed {
            server_id: Uuid::nil(),
            target: target(),
            metric: "monitorflow_disk_used_ratio".into(),
            function: Function::Max,
            from_ms: 1_000_000,
            to_ms: 4_600_000,
            step_ms: 60_000,
        });
        assert_eq!(path, "/api/v1/query_range");
        assert_eq!(
            params[0].1,
            r#"max_over_time(monitorflow_disk_used_ratio{peer="web-1",site="par\"is"}[60s])"#
        );
        assert_eq!(params[1].1, "1000.000");
        assert_eq!(params[3].1, "60s");
        assert!(params.contains(&("extra_label", "peer=web-1".into())));
        assert!(params.contains(&("extra_label", "site=par\"is".into())));
        let (_, params) = query(&Request::Past {
            key: PastKey {
                server_id: Uuid::nil(),
                metric: "monitorflow_cpu_load1".into(),
                window_s: 900,
                offset_s: 604_800,
            },
            target: target(),
            from_ms: 0,
            to_ms: 3_600_000,
            step_ms: 300_000,
        });
        assert!(params[0]
            .1
            .starts_with("avg_over_time(monitorflow_cpu_load1{"));
        assert!(params[0].1.ends_with("[900s])"));
        let (path, params) = query(&Request::Instances {
            server_id: Uuid::nil(),
            target: target(),
            metric: "monitorflow_port_443_up".into(),
            from_ms: 0,
        });
        assert_eq!(path, "/api/v1/series");
        assert_eq!(params[0].0, "match[]");
    }

    #[test]
    fn answers_keep_only_the_server_and_read_instances() {
        let data = json!({"resultType": "matrix", "result": [
            {"metric": {"peer": "web-1", "instance": "/"}, "values": [[60, "0.5"], [120, "NaN"], [180, "0.6"]]},
            {"metric": {"peer": "web-1"}, "values": [[60, "1"]]},
            {"metric": {"peer": "other", "instance": "/"}, "values": [[60, "9"]]}
        ]});
        let series = matrix(&data, &target()).unwrap();
        assert_eq!(
            series,
            [
                ("/".to_owned(), vec![(60_000, 0.5), (180_000, 0.6)]),
                (String::new(), vec![(60_000, 1.0)]),
            ]
        );
        assert_eq!(matrix(&json!({}), &target()), None);
        let found = instances(
            &json!([{"peer": "web-1", "instance": "/b"}, {"peer": "web-1", "instance": "/a"},
                    {"peer": "web-1", "instance": "/a"}, {"peer": "x", "instance": "/c"}]),
            &target(),
        )
        .unwrap();
        assert_eq!(found, ["/a", "/b"]);
    }

    struct Fake(Value);

    impl Backend for Fake {
        fn get<'a>(&'a self, _path: &'a str, _params: &'a [(&'a str, String)]) -> Fetch<'a> {
            let value = self.0.clone();
            Box::pin(async move {
                if value.is_null() {
                    Err(QueryError::Timeout)
                } else {
                    Ok(value)
                }
            })
        }
    }

    #[tokio::test]
    async fn the_task_answers_every_request_even_when_the_query_fails() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let ok = History::start(
            Arc::new(Fake(
                json!({"result": [{"metric": {"peer": "web-1"}, "values": [[1, "2"]]}]}),
            )),
            tx.clone(),
        );
        let key = PastKey {
            server_id: Uuid::nil(),
            metric: "monitorflow_cpu_load1".into(),
            window_s: 900,
            offset_s: 86_400,
        };
        let past = Request::Past {
            key: key.clone(),
            target: target(),
            from_ms: 0,
            to_ms: 1,
            step_ms: 300_000,
        };
        assert!(ok.ask(past.clone()));
        match rx.recv().await.unwrap() {
            Answer::Past { series, failed, .. } => {
                assert!(!failed);
                assert_eq!(series, [(String::new(), vec![(1000, 2.0)])]);
            }
            other => panic!("{other:?}"),
        }
        let failing = History::start(Arc::new(Fake(Value::Null)), tx);
        assert!(failing.ask(past));
        assert!(matches!(
            rx.recv().await.unwrap(),
            Answer::Past { failed: true, .. }
        ));
    }
}
