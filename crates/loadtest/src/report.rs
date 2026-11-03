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
                "p50 {:.1} s, p95 {:.1} s, max {:.1} s (n = {})",
                spread.p50, spread.p95, spread.max, spread.count
            ),
        );
    }

    /// Records a spread of milliseconds.
    pub fn millis(&mut self, label: &str, spread: Spread) {
        self.result(
            label,
            format!(
                "p50 {:.0} ms, p95 {:.0} ms, max {:.0} ms (n = {})",
                spread.p50, spread.p95, spread.max, spread.count
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
        );
        assert!(
            text.contains("| Server CPU (% of a core) | 20 | 30 |"),
            "{text}"
        );
        assert!(text.contains("- streams: 3"));
        assert!(text.contains("- agents: 3"));
    }
}
