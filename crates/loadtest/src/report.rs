//! The Markdown report of a run: one section per phase with the resource
//! use sampled during it and what the phase itself measured.

use std::fmt::Write;

use crate::measure::{Snapshot, Spread, Statement};

/// One phase of the run.
#[derive(Debug, Default)]
pub struct Phase {
    pub name: String,
    /// Sampler seconds the phase covered.
    pub from: f64,
    pub to: f64,
    /// What the phase measured, as label and value.
    pub results: Vec<(String, String)>,
    pub statements: Vec<Statement>,
}

impl Phase {
    pub fn result(&mut self, label: &str, value: impl ToString) {
        self.results.push((label.to_owned(), value.to_string()));
    }

    /// Records a spread of seconds.
    pub fn seconds(&mut self, label: &str, spread: Spread) {
        self.result(
            label,
            format!(
                "p50 {:.1} s, p95 {:.1} s, p99 {:.1} s, max {:.1} s (n = {})",
                spread.p50, spread.p95, spread.p99, spread.max, spread.count
            ),
        );
    }

    /// Records a spread of milliseconds.
    pub fn millis(&mut self, label: &str, spread: Spread) {
        self.result(
            label,
            format!(
                "p50 {:.0} ms, p95 {:.0} ms, p99 {:.0} ms, max {:.0} ms (n = {})",
                spread.p50, spread.p95, spread.p99, spread.max, spread.count
            ),
        );
    }
}

/// Mean and maximum of one measure over the snapshots of a phase.
fn mean_max(snapshots: &[&Snapshot], value: fn(&Snapshot) -> f64) -> (f64, f64) {
    if snapshots.is_empty() {
        return (0.0, 0.0);
    }
    let values: Vec<f64> = snapshots.iter().map(|s| value(s)).collect();
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    (mean, values.iter().copied().fold(f64::MIN, f64::max))
}

const MIB: f64 = 1024.0 * 1024.0;

pub fn render(
    title: &str,
    setup: &[(String, String)],
    phases: &[Phase],
    snapshots: &[Snapshot],
    log: &[String],
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# {title}\n");
    for (label, value) in setup {
        let _ = writeln!(out, "- {label}: {value}");
    }
    for phase in phases {
        let _ = writeln!(out, "\n## {}\n", phase.name);
        let during: Vec<&Snapshot> = snapshots
            .iter()
            .filter(|s| s.at >= phase.from && s.at <= phase.to)
            .collect();
        let _ = writeln!(
            out,
            "{:.0} s, {} snapshots.\n",
            phase.to - phase.from,
            during.len()
        );
        if !during.is_empty() {
            let _ = writeln!(out, "| Measure | Mean | Max |\n| --- | --- | --- |");
            // Label, value, decimals.
            type Row = (&'static str, fn(&Snapshot) -> f64, usize);
            let rows: [Row; 9] = [
                ("Server CPU (% of a core)", |s| s.server_cpu, 0),
                ("Server memory (MiB)", |s| s.server_rss as f64 / MIB, 0),
                ("Relay CPU (% of a core)", |s| s.relay_cpu, 0),
                ("Relay memory (MiB)", |s| s.relay_rss as f64 / MIB, 0),
                ("Load generator CPU (% of a core)", |s| s.loadtest_cpu, 0),
                ("PostgreSQL connections", |s| s.pg_connections as f64, 0),
                ("PostgreSQL active", |s| s.pg_active as f64, 1),
                ("VictoriaMetrics rows/s", |s| s.vm_rows_per_s, 0),
                (
                    "gRPC connections to the server",
                    |s| s.grpc_connections as f64,
                    0,
                ),
            ];
            for (label, value, precision) in rows {
                let (mean, max) = mean_max(&during, value);
                let _ = writeln!(out, "| {label} | {mean:.precision$} | {max:.precision$} |");
            }
            for (label, mean, max) in cgroup_rows(&during) {
                let _ = writeln!(out, "| {label} | {mean:.0} | {max:.0} |");
            }
            out.push('\n');
        }
        for (label, value) in &phase.results {
            let _ = writeln!(out, "- {label}: {value}");
        }
        if !phase.statements.is_empty() {
            let _ = writeln!(
                out,
                "\nStatements by total time:\n\n| Calls | Total ms | Mean ms | Max ms | Statement |\n| --- | --- | --- | --- | --- |"
            );
            for s in &phase.statements {
                let _ = writeln!(
                    out,
                    "| {} | {:.0} | {:.2} | {:.1} | `{}` |",
                    s.calls,
                    s.total_ms,
                    s.mean_ms,
                    s.max_ms,
                    one_line(&s.query, 140)
                );
            }
        }
    }
    if !log.is_empty() {
        let _ = writeln!(out, "\n## Server log\n");
        for line in log {
            let _ = writeln!(out, "- {}", one_line(line, 300));
        }
    }
    out
}

/// CPU and memory of each cgroup of a limited server side, in the order
/// they were first seen: label, mean and maximum.
fn cgroup_rows(during: &[&Snapshot]) -> Vec<(String, f64, f64)> {
    let mut names: Vec<&str> = Vec::new();
    for snapshot in during {
        for (name, _, _) in &snapshot.cgroups {
            if !names.contains(&name.as_str()) {
                names.push(name);
            }
        }
    }
    let spread = |values: Vec<f64>| {
        let mean = values.iter().sum::<f64>() / values.len().max(1) as f64;
        (mean, values.iter().copied().fold(0.0, f64::max))
    };
    let mut rows = Vec::new();
    for name in names {
        let of = |pick: fn(&(String, f64, f64)) -> f64| -> Vec<f64> {
            during
                .iter()
                .filter_map(|s| s.cgroups.iter().find(|c| c.0 == name).map(pick))
                .collect()
        };
        let (mean, max) = spread(of(|c| c.1));
        rows.push((format!("{name} CPU (% of a core)"), mean, max));
        let (mean, max) = spread(of(|c| c.2));
        rows.push((format!("{name} memory (MiB)"), mean, max));
    }
    rows
}

/// What a server log says beyond its routine: warnings and errors by
/// target and message, most frequent first, without the incidents the
/// threshold engine logs as warnings by design; then the last counters of
/// the writers and the threshold engine, which count drops.
pub fn log_summary(log: &str) -> Vec<String> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    let mut counters: Vec<(String, String)> = Vec::new();
    for line in log.lines() {
        let Some(rest) = line
            .split_once(" WARN ")
            .or_else(|| line.split_once(" ERROR "))
        else {
            for key in [
                "writer counters sink=\"victoriametrics\"",
                "writer counters sink=\"victorialogs\"",
                "threshold engine stats=",
            ] {
                if line.contains(key) {
                    let text = line
                        .split_once("INFO ")
                        .map_or(line, |(_, t)| t)
                        .trim()
                        .to_owned();
                    counters.retain(|(k, _)| k != key);
                    counters.push((key.to_owned(), text));
                }
            }
            continue;
        };
        let message = rest.1.trim();
        if message.contains("incident changed") {
            continue;
        }
        // The target and the message, without the fields that vary.
        let head: String = message
            .split_whitespace()
            .take_while(|word| !word.contains('='))
            .collect::<Vec<_>>()
            .join(" ");
        let level = if line.contains(" ERROR ") {
            "ERROR"
        } else {
            "WARN"
        };
        let key = format!("{level} {head}");
        match counts.iter_mut().find(|(k, _)| *k == key) {
            Some((_, n)) => *n += 1,
            None => counts.push((key, 1)),
        }
    }
    counts.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    let mut out: Vec<String> = counts
        .into_iter()
        .take(12)
        .map(|(key, n)| format!("{n} x {key}"))
        .collect();
    if out.is_empty() {
        out.push("No warning or error besides incidents".to_owned());
    }
    out.extend(
        counters
            .into_iter()
            .map(|(_, text)| format!("Last: {text}")),
    );
    out
}

/// The statement on one line, cut at `max` characters, safe in a table.
fn one_line(query: &str, max: usize) -> String {
    let flat = query.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut cut: String = flat.chars().take(max).collect();
    if flat.chars().count() > max {
        cut.push('…');
    }
    cut.replace('|', "\\|").replace('`', "'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_server_log_is_summed_up() {
        let log = "\
2026-11-20T13:00:00Z  WARN monitorflow_server::alerting::engine: incident changed agent_id=a metric=m
2026-11-20T13:00:01Z  WARN monitorflow_server::db: could not acquire a connection agent_id=a
2026-11-20T13:00:02Z  WARN monitorflow_server::db: could not acquire a connection agent_id=b
2026-11-20T13:00:03Z ERROR monitorflow_server::ingest::writer: write failed sink=victoriametrics
2026-11-20T13:00:04Z  INFO monitorflow_server::ingest::writer: writer counters sink=\"victoriametrics\" now=Stats { dropped: 0 }
2026-11-20T13:00:05Z  INFO monitorflow_server::ingest::writer: writer counters sink=\"victoriametrics\" now=Stats { dropped: 7 }
";
        assert_eq!(
            log_summary(log),
            [
                "2 x WARN monitorflow_server::db: could not acquire a connection",
                "1 x ERROR monitorflow_server::ingest::writer: write failed",
                "Last: monitorflow_server::ingest::writer: writer counters sink=\"victoriametrics\" now=Stats { dropped: 7 }",
            ]
        );
        assert_eq!(
            log_summary("x  INFO started\n"),
            ["No warning or error besides incidents"]
        );
    }

    #[test]
    fn spreads_show_the_tail() {
        let mut phase = Phase::default();
        let mut values: Vec<f64> = (1..=100).map(f64::from).collect();
        phase.millis("Calls", Spread::of(values.clone()));
        values.iter_mut().for_each(|v| *v /= 10.0);
        phase.seconds("Waits", Spread::of(values));
        assert_eq!(
            phase.results[0].1,
            "p50 51 ms, p95 95 ms, p99 99 ms, max 100 ms (n = 100)"
        );
        assert_eq!(
            phase.results[1].1,
            "p50 5.1 s, p95 9.5 s, p99 9.9 s, max 10.0 s (n = 100)"
        );
    }

    #[test]
    fn statements_fit_on_one_table_line() {
        assert_eq!(
            one_line("SELECT a\n   FROM b | c", 100),
            "SELECT a FROM b \\| c"
        );
        assert_eq!(one_line("SELECT 1234", 6), "SELECT…");
    }

    #[test]
    fn phases_summarise_their_snapshots_only() {
        let snapshots = [
            Snapshot {
                at: 1.0,
                server_cpu: 10.0,
                ..Snapshot::default()
            },
            Snapshot {
                at: 2.0,
                server_cpu: 30.0,
                ..Snapshot::default()
            },
            Snapshot {
                at: 9.0,
                server_cpu: 500.0,
                ..Snapshot::default()
            },
        ];
        let mut phase = Phase {
            name: "steady".into(),
            from: 0.5,
            to: 2.5,
            ..Phase::default()
        };
        phase.result("streams", 3);
        let text = render(
            "Run",
            &[("agents".into(), "3".into())],
            &[phase],
            &snapshots,
            &["1 x WARN something".to_owned()],
        );
        assert!(
            text.contains("## Server log\n\n- 1 x WARN something"),
            "{text}"
        );
        assert!(
            text.contains("| Server CPU (% of a core) | 20 | 30 |"),
            "{text}"
        );
        assert!(text.contains("- streams: 3"));
        assert!(text.contains("- agents: 3"));
    }
}
