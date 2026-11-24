//! A report as CSV (ADR-0155): one row per service, server, device and
//! check with its numbers, then one row per outage and per planned
//! maintenance, with a `kind` column to filter on. Numbers are plain
//! (dot decimals, seconds); words are in the report's language; times are
//! RFC 3339 in the report's time zone.

use std::fmt::Write as _;

use chrono::{DateTime, SecondsFormat, Utc};
use chrono_tz::Tz;

use super::report::{Numbers, OutageView, Report};
use super::words::{self, Language};

const HEADER: [&str; 17] = [
    "kind",
    "name",
    "server",
    "site",
    "check",
    "instance",
    "availability_percent",
    "target_percent",
    "target_met",
    "counted_seconds",
    "down_seconds",
    "maintenance_seconds",
    "outages",
    "longest_seconds",
    "start",
    "end",
    "details",
];

/// A field quoted when it needs to be, with a leading `'` before what a
/// spreadsheet would read as a formula.
pub fn field(value: &str) -> String {
    let value = match value.chars().next() {
        Some('=' | '+' | '-' | '@' | '\t' | '\r') if value.parse::<f64>().is_err() => {
            format!("'{value}")
        }
        _ => value.to_owned(),
    };
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value
    }
}

fn number(value: f64) -> String {
    // Plus zero: -0.0 would be written "-0".
    let rounded = (value * 1000.0).round() / 1000.0 + 0.0;
    format!("{rounded}")
}

struct Row([String; 17]);

impl Row {
    fn new(kind: &str, name: &str) -> Self {
        let mut row = Row(Default::default());
        row.0[0] = kind.to_owned();
        row.0[1] = name.to_owned();
        row
    }

    fn numbers(mut self, n: &Numbers) -> Self {
        self.0[6] = n.availability.map(number).unwrap_or_default();
        self.0[9] = number(n.counted_seconds);
        self.0[10] = number(n.down_seconds);
        self.0[11] = number(n.maintenance_seconds);
        self.0[12] = n.outages.to_string();
        self.0[13] = number(n.longest_seconds);
        self
    }

    fn set(mut self, column: usize, value: impl Into<String>) -> Self {
        self.0[column] = value.into();
        self
    }

    fn write(&self, out: &mut String) {
        let fields: Vec<String> = self.0.iter().map(|f| field(f)).collect();
        let _ = writeln!(out, "{}", fields.join(","));
    }
}

fn at(time: DateTime<Utc>, zone: Tz) -> String {
    time.with_timezone(&zone)
        .to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// The rows of `report`, header first, lines ending with `\n`.
pub fn render(report: &Report, zone: Tz, language: Language) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{}", HEADER.join(","));
    let outage = |out: &mut String, name: &str, o: &OutageView| {
        Row::new("outage", name)
            .set(10, number(o.seconds))
            .set(14, at(o.start, zone))
            .set(15, at(o.end, zone))
            .set(16, words::causes(&o.causes, language))
            .write(out);
    };
    for s in &report.services {
        Row::new("service", &s.name)
            .numbers(&s.numbers)
            .set(7, s.target.map(number).unwrap_or_default())
            .set(8, s.met.map(|m| m.to_string()).unwrap_or_default())
            .write(&mut out);
    }
    for m in &report.machines {
        Row::new(if m.device { "device" } else { "server" }, &m.name)
            .numbers(&m.numbers)
            .set(2, m.name.clone())
            .set(3, m.site_name.clone())
            .write(&mut out);
    }
    for c in &report.checks {
        Row::new("check", &format!("{} {}", c.server_name, c.name))
            .numbers(&c.numbers)
            .set(2, c.server_name.clone())
            .set(4, c.name.clone())
            .set(5, c.instance.clone())
            .write(&mut out);
    }
    for s in &report.services {
        for o in &s.outage_list {
            outage(&mut out, &s.name, o);
        }
    }
    for m in &report.machines {
        for o in &m.outage_list {
            outage(&mut out, &m.name, o);
        }
    }
    for m in &report.maintenance {
        Row::new("maintenance", &m.servers.join(" "))
            .set(
                4,
                m.checks
                    .iter()
                    .map(|c| c.name.clone())
                    .collect::<Vec<_>>()
                    .join(" "),
            )
            .set(11, number((m.end - m.start).num_seconds() as f64))
            .set(14, at(m.start, zone))
            .set(15, at(m.end, zone))
            .set(16, m.reason.clone())
            .write(&mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_are_quoted_and_formulas_defused() {
        assert_eq!(field("web-1"), "web-1");
        assert_eq!(field("a,b"), "\"a,b\"");
        assert_eq!(field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(field("=HYPERLINK(1)"), "'=HYPERLINK(1)");
        assert_eq!(field("-12.5"), "-12.5");
        assert_eq!(number(99.12345), "99.123");
        assert_eq!(number(3600.0), "3600");
        assert_eq!(number(-0.0), "0");
    }
}
