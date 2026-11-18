//! From alerting rules to alerts, thresholds and service checks.
//!
//! Each rule's expression is read by [`super::promql::read`]: a target
//! down (`up == 0`) becomes the offline alert of the node exporter's hosts
//! or the alert of an exporter's port check; a failed probe
//! (`probe_success == 0`) the alert of each probe check; a systemd unit not
//! active a service check; a condition on processor, load, memory, file
//! system or disk wait a threshold. Everything else cannot be moved, with
//! the expression shown. A rule applies to the hosts of the jobs its `job`
//! matcher names, or to every node host when it names none, and only to
//! the instances its `instance` matcher names.

use crate::args;
use crate::model::{
    Alert, Check, Condition, Direction, Fidelity, Judged, Measure, Origin, Severity, Threshold,
    Unmovable, Watch, WindowFunction,
};
use crate::notes::Note;

use super::hosts::{Fleet, Member};
use super::promql::{self, Cmp, LeafKind, Matcher, Reading, Window};

/// An alerting rule, from a rule file or from Prometheus's API.
#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    pub name: String,
    pub expr: String,
    /// The `for:` duration, in seconds.
    pub for_s: u32,
    /// The `severity` label, when it has one.
    pub severity: Option<String>,
}

/// MonitorFlow's severity for a `severity` label, with a note when the
/// value is not a usual one.
pub fn severity(label: Option<&str>) -> (Severity, Option<Note>) {
    let Some(label) = label.map(str::trim).filter(|l| !l.is_empty()) else {
        return (Severity::Warning, None);
    };
    match label.to_ascii_lowercase().as_str() {
        "critical" | "page" | "high" => (Severity::Critical, None),
        "warning" | "info" | "low" => (Severity::Warning, None),
        _ => (
            Severity::Warning,
            Some(Note::new(
                "severity_mapped",
                args! {"severity" => label, "mapped" => "warning"},
            )),
        ),
    }
}

/// Adds what `rules` say to `fleet`.
pub fn apply(fleet: &mut Fleet, rules: &[Rule]) {
    for rule in rules {
        apply_one(fleet, rule);
    }
}

/// The members of `members` a rule's `job` and `instance` matchers keep.
fn select<'a>(members: &'a [Member], reading: &Reading) -> Vec<&'a Member> {
    let jobs: Vec<&Matcher> = reading.matchers("job");
    let instances: Vec<&Matcher> = reading.matchers("instance");
    members
        .iter()
        .filter(|m| jobs.iter().all(|j| j.matches(&m.job)))
        .filter(|m| instances.iter().all(|i| i.matches(&m.instance)))
        .collect()
}

/// The host keys of `members`, each once, in order.
fn hosts_of(members: &[&Member]) -> Vec<String> {
    let mut hosts: Vec<String> = Vec::new();
    for member in members {
        if !hosts.contains(&member.host) {
            hosts.push(member.host.clone());
        }
    }
    hosts
}

fn apply_one(fleet: &mut Fleet, rule: &Rule) {
    let origin = Origin::new("alerting_rule", &rule.name).with_detail(&rule.expr);
    let unmovable = |fleet: &mut Fleet, hosts: Vec<String>, why: Note| {
        fleet.inventory.unmovable.push(Unmovable {
            hosts,
            origin: origin.clone(),
            why,
        });
    };
    // A status code outside a range: two comparisons joined by `or`.
    let alternatives = || {
        promql::read_alternatives(&rule.expr)
            .filter(|parts| {
                parts.len() > 1
                    && parts
                        .iter()
                        .all(|p| p.condition.is(&[(LeafKind::ProbeStatusCode, 1)]))
            })
            .and_then(|parts| parts.into_iter().next())
    };
    let (level, severity_note) = severity(rule.severity.as_deref());
    // Richer forms first (ADR-0153): a series gone, a comparison with the
    // past.
    if let Some((reading, range)) = promql::read_absent(&rule.expr) {
        return absent(fleet, rule, &origin, &reading, range, level, severity_note);
    }
    if let Some((reading, ratio, offset)) = promql::read_compare(&rule.expr) {
        return compare(
            fleet,
            rule,
            &origin,
            &reading,
            ratio,
            offset,
            level,
            severity_note,
        );
    }
    let Some(reading) = promql::read(&rule.expr).or_else(alternatives) else {
        unmovable(fleet, vec![], Note::plain("expression"));
        return;
    };
    let alert = Alert {
        severity: level,
        for_s: rule.for_s,
        no_data: false,
    };
    let extra = reading
        .extra_condition
        .then(|| Note::plain("extra_condition"));
    let condition = &reading.condition;
    let nodes = select(&fleet.nodes, &reading);
    let node_hosts = hosts_of(&nodes);
    use LeafKind::*;

    if condition.is(&[(Up, 1)]) {
        if !condition.is_false_check() {
            unmovable(fleet, node_hosts, Note::plain("expression"));
            return;
        }
        let exporters: Vec<Option<usize>> = select(&fleet.exporters, &reading)
            .iter()
            .map(|m| m.check)
            .collect();
        let blackbox = hosts_of(&select(&fleet.blackbox_hosts, &reading));
        let agents: Vec<String> = node_hosts.into_iter().chain(blackbox).collect();
        if agents.is_empty() && exporters.is_empty() {
            unmovable(fleet, vec![], Note::plain("no_hosts"));
            return;
        }
        let note = extra.or(severity_note);
        for host in agents {
            let check = Check {
                host,
                watch: Watch::Agent,
                interval_s: None,
                alert: Some(alert.clone()),
                origin: origin.clone(),
                fidelity: note.clone().map_or(Fidelity::Exact, Fidelity::Approximated),
            };
            add_or_alert(fleet, check);
        }
        for index in exporters.into_iter().flatten() {
            set_alert(fleet, index, &alert, note.as_ref());
        }
        return;
    }

    if condition.is(&[(ProbeSuccess, 1)]) {
        let probes: Vec<usize> = select(&fleet.probes, &reading)
            .iter()
            .filter_map(|m| m.check)
            .collect();
        if !condition.is_false_check() {
            unmovable(fleet, vec![], Note::plain("expression"));
            return;
        }
        if probes.is_empty() {
            unmovable(fleet, vec![], Note::plain("no_hosts"));
            return;
        }
        let note = extra.or(severity_note);
        for index in probes {
            set_alert(fleet, index, &alert, note.as_ref());
        }
        return;
    }

    // A website's answer, response time or certificate.
    let website = condition.is(&[(ProbeStatusCode, 1)])
        || condition.is(&[(ProbeDuration { whole: true }, 1)])
        || condition.is(&[(ProbeDuration { whole: false }, 1)])
        || condition.is(&[(CertSecondsLeft, 1)]);
    if website {
        let probes = web_probes(fleet, &reading);
        if probes.is_empty() {
            unmovable(fleet, vec![], Note::plain("no_hosts"));
            return;
        }
        if condition.is(&[(ProbeStatusCode, 1)]) {
            let note = extra.or(severity_note);
            for (index, _, _) in probes {
                set_alert(fleet, index, &alert, note.as_ref());
            }
            return;
        }
        let whole = !condition.is(&[(ProbeDuration { whole: false }, 1)]);
        let (measure, direction, value, measure_note): (fn(String) -> Measure, _, _, _) =
            match condition.cmp {
                Cmp::Gt | Cmp::Ge if !condition.is(&[(CertSecondsLeft, 1)]) => (
                    |url| Measure::WebResponseTime { url },
                    Direction::Above,
                    condition.value,
                    (!whole).then(|| Note::plain("web_response_phase")),
                ),
                Cmp::Lt | Cmp::Le if condition.is(&[(CertSecondsLeft, 1)]) => (
                    |url| Measure::CertificateDays { url },
                    Direction::Below,
                    promql::round(condition.value / 86_400.0),
                    None,
                ),
                _ => {
                    unmovable(fleet, vec![], Note::plain("expression"));
                    return;
                }
            };
        let note = extra.or(measure_note).or(severity_note);
        let (warning, critical) = match level {
            Severity::Warning => (Some(value), None),
            Severity::Critical => (None, Some(value)),
        };
        for (_, host, url) in probes {
            fleet.inventory.thresholds.push(Threshold {
                judged: Default::default(),
                conditions: Vec::new(),
                hosts: vec![host],
                measure: measure(url),
                direction,
                warning,
                critical,
                for_s: rule.for_s,
                recovery: None,
                origin: origin.clone(),
                fidelity: note.clone().map_or(Fidelity::Exact, Fidelity::Approximated),
            });
        }
        return;
    }

    if condition.is(&[(SystemdUnit, 1)]) {
        let state = reading.exact("state");
        let fidelity = match state.as_deref() {
            Some("active") if condition.is_false_check() => None,
            Some("failed") if condition.is_true_check() => Some(Note::plain("service_failed")),
            _ => {
                unmovable(fleet, node_hosts, Note::plain("expression"));
                return;
            }
        };
        let names = reading
            .matchers("name")
            .into_iter()
            .find_map(Matcher::literals);
        let Some(names) = names else {
            unmovable(fleet, node_hosts, Note::plain("expression"));
            return;
        };
        if node_hosts.is_empty() {
            unmovable(fleet, vec![], Note::plain("no_hosts"));
            return;
        }
        let note = extra.or(fidelity).or(severity_note);
        for host in &node_hosts {
            for name in &names {
                let check = Check {
                    host: host.clone(),
                    watch: Watch::Service { name: name.clone() },
                    interval_s: None,
                    alert: Some(alert.clone()),
                    origin: origin.clone(),
                    fidelity: note.clone().map_or(Fidelity::Exact, Fidelity::Approximated),
                };
                add_or_alert(fleet, check);
            }
        }
        return;
    }

    if condition.is(&[(Load(1), 1), (CpuCount, -1)])
        || condition.is(&[(Load(5), 1), (CpuCount, -1)])
        || condition.is(&[(Load(15), 1), (CpuCount, -1)])
    {
        unmovable(fleet, node_hosts, Note::plain("per_cpu_load"));
        return;
    }

    let Some(mut primary) = judge(&reading) else {
        unmovable(fleet, node_hosts, Note::plain("expression"));
        return;
    };
    if node_hosts.is_empty() {
        unmovable(fleet, vec![], Note::plain("no_hosts"));
        return;
    }
    // Conditions joined with `and` (ADR-0153); a forecast among them leads.
    let mut others: Vec<Judgement> = Vec::new();
    let mut lost = false;
    for also in &reading.also {
        match judge(also) {
            Some(j) => others.push(j),
            None => lost = true,
        }
    }
    if !matches!(primary.judged, Judged::Forecast { .. }) {
        if let Some(i) = others
            .iter()
            .position(|j| matches!(j.judged, Judged::Forecast { .. }))
        {
            std::mem::swap(&mut primary, &mut others[i]);
        }
    }
    let mut conditions = Vec::new();
    for other in &others {
        let window = match other.judged {
            Judged::Value => None,
            Judged::Window { function, window_s } => Some((function, window_s)),
            _ => {
                lost = true;
                continue;
            }
        };
        conditions.push(Condition {
            measure: other.measure.clone(),
            direction: other.direction,
            value: other.value,
            window,
        });
    }
    if conditions.len() > 2 {
        conditions.truncate(2);
        lost = true;
    }
    let extra = extra.or_else(|| lost.then(|| Note::plain("extra_condition")));
    let note = extra
        .or(primary.note.clone())
        .or_else(|| others.iter().find_map(|o| o.note.clone()))
        .or(severity_note);
    let (warning, critical) = match level {
        Severity::Warning => (Some(primary.value), None),
        Severity::Critical => (None, Some(primary.value)),
    };
    fleet.inventory.thresholds.push(Threshold {
        judged: primary.judged,
        conditions,
        hosts: node_hosts,
        measure: primary.measure,
        direction: primary.direction,
        warning,
        critical,
        for_s: rule.for_s,
        recovery: None,
        origin,
        fidelity: note.map_or(Fidelity::Exact, Fidelity::Approximated),
    });
}

/// What one condition becomes: a measure, the side, the limit, what is
/// judged and how it differs (ADR-0153).
#[derive(Debug, Clone)]
struct Judgement {
    measure: Measure,
    direction: Direction,
    /// The limit; seconds left for a forecast.
    value: f64,
    judged: Judged,
    note: Option<Note>,
}

/// A window in MonitorFlow's bounds, with a note when it had to move.
fn bounded(seconds: u32, low: u32, high: u32) -> (u32, Option<Note>) {
    let b = seconds.clamp(low, high);
    (
        b,
        (b != seconds).then(|| Note::new("window_rounded", args! {"seconds" => seconds})),
    )
}

fn judge(reading: &Reading) -> Option<Judgement> {
    use LeafKind::*;
    let window = reading.window();
    let condition = &reading.condition;
    // Free space predicted to reach nothing: the disk full.
    if let Some(Window::Predict { over, within }) = window {
        if condition.is(&[(FsAvail, 1)])
            && matches!(condition.cmp, Cmp::Lt | Cmp::Le)
            && condition.value == 0.0
        {
            let (window_s, note) = bounded(over, 3_600, 604_800);
            return Some(Judgement {
                measure: Measure::DiskUsed {
                    mount: reading.exact("mountpoint"),
                },
                direction: Direction::Above,
                value: f64::from(within),
                judged: Judged::Forecast {
                    target: 1.0,
                    window_s,
                },
                note,
            });
        }
    }
    let (measure, direction, value, note) = threshold_of(reading)?;
    let raw = match condition.cmp {
        Cmp::Gt | Cmp::Ge => Direction::Above,
        _ => Direction::Below,
    };
    // A complement (free as used, idle as busy) turns the lowest into the
    // highest and a change into its opposite.
    let flipped = raw != direction;
    let (judged, value, extra) = match window {
        None => (Judged::Value, value, None),
        Some(Window::Avg(w) | Window::Min(w) | Window::Max(w)) => {
            let function = match (window, flipped) {
                (Some(Window::Min(_)), false) | (Some(Window::Max(_)), true) => WindowFunction::Min,
                (Some(Window::Max(_)), false) | (Some(Window::Min(_)), true) => WindowFunction::Max,
                _ => WindowFunction::Avg,
            };
            let (window_s, note) = bounded(w, 60, 86_400);
            (Judged::Window { function, window_s }, value, note)
        }
        Some(Window::Change(w) | Window::Slope(w)) => {
            let slope = matches!(window, Some(Window::Slope(_)));
            let (window_s, note) = bounded(w, 60, 86_400);
            let function = if slope {
                WindowFunction::Rate
            } else {
                WindowFunction::Change
            };
            let t = if flipped {
                -condition.value
            } else {
                condition.value
            };
            let note = note.or_else(|| slope.then(|| Note::plain("prometheus_deriv")));
            (Judged::Window { function, window_s }, t, note)
        }
        Some(Window::Predict { over, within }) => {
            let (window_s, note) = bounded(over, 3_600, 604_800);
            let forecast = Judged::Forecast {
                target: value,
                window_s,
            };
            (forecast, f64::from(within), note)
        }
    };
    Some(Judgement {
        measure,
        direction,
        value,
        judged,
        note: extra.or(note),
    })
}

/// `absent(...)` and `absent_over_time(...)`: no data (ADR-0153).
fn absent(
    fleet: &mut Fleet,
    rule: &Rule,
    origin: &Origin,
    reading: &Reading,
    range: Option<u32>,
    level: Severity,
    severity_note: Option<Note>,
) {
    use LeafKind::*;
    let nodes = select(&fleet.nodes, reading);
    let hosts = hosts_of(&nodes);
    let unmovable = |fleet: &mut Fleet, hosts: Vec<String>, why: Note| {
        fleet.inventory.unmovable.push(Unmovable {
            hosts,
            origin: origin.clone(),
            why,
        });
    };
    if hosts.is_empty() {
        unmovable(fleet, vec![], Note::plain("no_hosts"));
        return;
    }
    // `absent` is true when no series at all is left; MonitorFlow watches
    // each series.
    let note = range
        .is_none()
        .then(|| Note::plain("prometheus_absent"))
        .or(severity_note);
    let silence = (rule.for_s + range.unwrap_or(0)).clamp(60, 604_800);
    let kind = &reading.leaves[0].kind;
    let measure = match kind {
        Up => {
            let alert = Alert {
                severity: level,
                for_s: silence,
                no_data: false,
            };
            for host in hosts {
                let check = Check {
                    host,
                    watch: Watch::Agent,
                    interval_s: None,
                    alert: Some(alert.clone()),
                    origin: origin.clone(),
                    fidelity: Fidelity::Approximated(Note::plain("prometheus_absent")),
                };
                add_or_alert(fleet, check);
            }
            return;
        }
        Load(minutes) => Measure::CpuLoad { minutes: *minutes },
        MemAvailable | MemTotal => Measure::MemoryAvailable,
        FsAvail | FsSize => Measure::DiskUsed {
            mount: reading.exact("mountpoint"),
        },
        _ => {
            unmovable(fleet, hosts, Note::plain("expression"));
            return;
        }
    };
    let value = Some(f64::from(silence));
    let (warning, critical) = match level {
        Severity::Warning => (value, None),
        Severity::Critical => (None, value),
    };
    fleet.inventory.thresholds.push(Threshold {
        judged: Judged::NoData,
        conditions: Vec::new(),
        hosts,
        measure,
        direction: Direction::Above,
        warning,
        critical,
        for_s: 0,
        recovery: None,
        origin: origin.clone(),
        fidelity: note.map_or(Fidelity::Exact, Fidelity::Approximated),
    });
}

/// `x / x offset 1w > 2`, `x - x offset 1d > 3`: a comparison with the past
/// (ADR-0153), on a measure read as it is.
#[allow(clippy::too_many_arguments)]
fn compare(
    fleet: &mut Fleet,
    rule: &Rule,
    origin: &Origin,
    reading: &Reading,
    ratio: bool,
    offset: u32,
    level: Severity,
    severity_note: Option<Note>,
) {
    let nodes = select(&fleet.nodes, reading);
    let hosts = hosts_of(&nodes);
    let refuse = |fleet: &mut Fleet, hosts: Vec<String>, why: &str| {
        fleet.inventory.unmovable.push(Unmovable {
            hosts,
            origin: origin.clone(),
            why: Note::plain(why),
        });
    };
    let Some((measure, direction, value, measure_note)) = threshold_of(reading) else {
        return refuse(fleet, hosts, "expression");
    };
    let raw = match reading.condition.cmp {
        Cmp::Gt | Cmp::Ge => Direction::Above,
        _ => Direction::Below,
    };
    // Only a measure read as it is compares the same way: a share free is
    // not a share used once divided.
    if raw != direction || value != reading.condition.value || ![86_400, 604_800].contains(&offset)
    {
        return refuse(fleet, hosts, "expression");
    }
    if hosts.is_empty() {
        return refuse(fleet, vec![], "no_hosts");
    }
    let (window_s, window_note) = match reading.window() {
        Some(Window::Avg(w)) => bounded(w, 300, 86_400),
        _ => (300, Some(Note::plain("prometheus_compare_window"))),
    };
    let note = window_note.or(measure_note).or(severity_note);
    let (warning, critical) = match level {
        Severity::Warning => (Some(value), None),
        Severity::Critical => (None, Some(value)),
    };
    fleet.inventory.thresholds.push(Threshold {
        judged: Judged::Compare {
            ratio,
            offset_s: offset,
            window_s,
        },
        conditions: Vec::new(),
        hosts,
        measure,
        direction,
        warning,
        critical,
        for_s: rule.for_s,
        recovery: None,
        origin: origin.clone(),
        fidelity: note.map_or(Fidelity::Exact, Fidelity::Approximated),
    });
}

/// The threshold a condition is, if it is one on a measure MonitorFlow
/// has: the measure, the direction, the value, and the note when it is
/// approximated.
fn threshold_of(reading: &Reading) -> Option<(Measure, Direction, f64, Option<Note>)> {
    use LeafKind::*;
    let condition = &reading.condition;
    let direction = |cmp: Cmp| match cmp {
        Cmp::Gt | Cmp::Ge => Some(Direction::Above),
        Cmp::Lt | Cmp::Le => Some(Direction::Below),
        Cmp::Eq | Cmp::Ne => None,
    };
    let opposite = |d: Direction| match d {
        Direction::Above => Direction::Below,
        Direction::Below => Direction::Above,
    };
    let same = direction(condition.cmp)?;
    let t = condition.value;
    let complement = promql::round(1.0 - t);
    let found = if condition.is(&[(CpuIdle, 1)]) {
        (Measure::CpuUsage, opposite(same), complement, None)
    } else if condition.is(&[(CpuBusy { exact: true }, 1)]) {
        (Measure::CpuUsage, same, t, None)
    } else if condition.is(&[(CpuBusy { exact: false }, 1)]) {
        (
            Measure::CpuUsage,
            same,
            t,
            Some(Note::plain("cpu_busy_modes")),
        )
    } else if let Some(minutes) = [1, 5, 15]
        .into_iter()
        .find(|m| condition.is(&[(Load(*m), 1)]))
    {
        (Measure::CpuLoad { minutes }, same, t, None)
    } else if condition.is(&[(MemAvailable, 1), (MemTotal, -1)]) {
        (Measure::MemoryAvailable, same, t, None)
    } else if condition.is(&[(FsAvail, 1), (FsSize, -1)]) {
        (
            Measure::DiskUsed {
                mount: reading.exact("mountpoint"),
            },
            opposite(same),
            complement,
            Some(Note::plain("disk_free_as_used")),
        )
    } else if condition.is(&[(DiskReadTime, 1), (DiskReads, -1)])
        || condition.is(&[(DiskWriteTime, 1), (DiskWrites, -1)])
        || condition.is(&[(DiskIoTimeWeighted, 1), (DiskIoTime, -1)])
    {
        (
            Measure::DiskWait {
                device: reading.exact("device"),
            },
            same,
            t,
            Some(Note::plain("disk_wait")),
        )
    } else {
        return None;
    };
    Some(found)
}

/// The website checks of the probes a rule selects: their index, host
/// and URL.
fn web_probes(fleet: &Fleet, reading: &Reading) -> Vec<(usize, String, String)> {
    select(&fleet.probes, reading)
        .iter()
        .filter_map(|m| {
            let index = m.check?;
            match &fleet.inventory.checks.get(index)?.watch {
                Watch::Web(site) => Some((index, m.host.clone(), site.url.clone())),
                _ => None,
            }
        })
        .collect()
}

/// Adds `check`, or when its host has the same check already, gives it
/// the alert.
fn add_or_alert(fleet: &mut Fleet, check: Check) {
    let existing = fleet
        .inventory
        .checks
        .iter()
        .position(|c| c.host == check.host && c.watch == check.watch);
    match existing {
        Some(index) => {
            // A rule on the host itself says more than a ping of it.
            let existing = &mut fleet.inventory.checks[index];
            if check.fidelity == Fidelity::Exact && existing.fidelity != Fidelity::Exact {
                existing.fidelity = Fidelity::Exact;
                existing.origin = check.origin.clone();
            }
            let note = match &check.fidelity {
                Fidelity::Approximated(n) => Some(n.clone()),
                Fidelity::Exact => None,
            };
            if let Some(alert) = &check.alert {
                set_alert(fleet, index, alert, note.as_ref());
            }
        }
        None => fleet.inventory.checks.push(check),
    }
}

/// Gives the check at `index` `alert`, keeping the more severe of two.
fn set_alert(fleet: &mut Fleet, index: usize, alert: &Alert, note: Option<&Note>) {
    let Some(check) = fleet.inventory.checks.get_mut(index) else {
        return;
    };
    let keep = check
        .alert
        .as_ref()
        .is_some_and(|a| a.severity >= alert.severity);
    if !keep {
        check.alert = Some(alert.clone());
    }
    if let (Fidelity::Exact, Some(note)) = (&check.fidelity, note) {
        check.fidelity = Fidelity::Approximated(note.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::super::targets::{Probe, Target};
    use super::super::Collected;
    use super::*;
    use crate::model::SilenceScope;

    fn node(job: &str, name: &str) -> Target {
        Target {
            job: job.into(),
            address: format!("{name}:9100"),
            labels: [("job", job), ("instance", &format!("{name}:9100"))]
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            ..Target::default()
        }
    }

    fn fleet() -> Fleet {
        let mut collected = Collected::default();
        collected.targets.push(node("node", "web-01"));
        collected.targets.push(node("node", "db-01"));
        collected.targets.push(node("node-dmz", "proxy-01"));
        let mut probe = node("blackbox", "monitor-01");
        probe.address = "monitor-01:9115".into();
        probe
            .labels
            .insert("instance".into(), "https://shop.example.com".into());
        probe.probe = Some(Probe {
            module: "http_2xx".into(),
            target: "https://shop.example.com".into(),
        });
        collected.targets.push(probe);
        Fleet::new(&collected)
    }

    fn rule(name: &str, expr: &str, severity: &str, for_s: u32) -> Rule {
        Rule {
            name: name.into(),
            expr: expr.into(),
            for_s,
            severity: (!severity.is_empty()).then(|| severity.into()),
        }
    }

    #[test]
    fn severities() {
        assert_eq!(severity(Some("critical")), (Severity::Critical, None));
        assert_eq!(severity(Some("Page")), (Severity::Critical, None));
        assert_eq!(severity(Some("info")), (Severity::Warning, None));
        assert_eq!(severity(None), (Severity::Warning, None));
        let (level, note) = severity(Some("disaster"));
        assert_eq!(level, Severity::Warning);
        assert_eq!(
            note.unwrap().text(),
            "The source's severity disaster becomes warning."
        );
    }

    #[test]
    fn thresholds_apply_to_the_jobs_named() {
        let mut fleet = fleet();
        apply(
            &mut fleet,
            &[
                rule(
                    "HostHighCpuLoad",
                    r#"100 - (avg by (instance) (rate(node_cpu_seconds_total{job="node",mode="idle"}[5m])) * 100) > 80"#,
                    "warning",
                    300,
                ),
                rule(
                    "HostOutOfDiskSpace",
                    r#"(node_filesystem_avail_bytes{mountpoint="/"} * 100) / node_filesystem_size_bytes{mountpoint="/"} < 10"#,
                    "critical",
                    120,
                ),
                rule(
                    "OneHost",
                    r#"node_load5{instance="db-01:9100"} > 8"#,
                    "warning",
                    0,
                ),
            ],
        );
        let t = &fleet.inventory.thresholds;
        assert_eq!(t.len(), 3);
        assert_eq!(t[0].hosts, vec!["web-01", "db-01"]);
        assert_eq!(t[0].measure, Measure::CpuUsage);
        assert_eq!(t[0].direction, Direction::Above);
        assert_eq!(
            (t[0].warning, t[0].critical, t[0].for_s),
            (Some(0.8), None, 300)
        );
        assert_eq!(t[0].fidelity, Fidelity::Exact);
        assert_eq!(t[1].hosts, vec!["web-01", "db-01", "proxy-01"]);
        assert_eq!(
            t[1].measure,
            Measure::DiskUsed {
                mount: Some("/".into())
            }
        );
        assert_eq!(
            (t[1].direction, t[1].critical),
            (Direction::Above, Some(0.9))
        );
        assert_eq!(
            t[1].fidelity,
            Fidelity::Approximated(Note::plain("disk_free_as_used"))
        );
        assert_eq!(t[2].hosts, vec!["db-01"]);
        assert_eq!(t[2].measure, Measure::CpuLoad { minutes: 5 });
    }

    #[test]
    fn down_rules_give_offline_and_probe_alerts() {
        let mut fleet = fleet();
        apply(
            &mut fleet,
            &[
                rule("NodeDown", r#"up{job=~"node.*"} == 0"#, "critical", 60),
                rule("ProbeFailed", "probe_success == 0", "warning", 120),
                rule("ProbeFailedCritical", "probe_success == 0", "critical", 300),
                rule("NothingThere", r#"up{job="mysql"} == 0"#, "critical", 60),
            ],
        );
        let checks = &fleet.inventory.checks;
        let agents: Vec<_> = checks
            .iter()
            .filter(|c| c.watch == Watch::Agent)
            .map(|c| (c.host.as_str(), c.alert.clone().unwrap()))
            .collect();
        assert_eq!(agents.len(), 3);
        assert_eq!(agents[2].0, "proxy-01");
        assert_eq!(
            agents[0].1,
            Alert {
                severity: Severity::Critical,
                for_s: 60,
                no_data: false
            }
        );
        let web = checks
            .iter()
            .find(|c| matches!(c.watch, Watch::Web { .. }))
            .unwrap();
        assert_eq!(web.host, "monitor-01");
        // The more severe of the two probe rules.
        assert_eq!(
            web.alert,
            Some(Alert {
                severity: Severity::Critical,
                for_s: 300,
                no_data: false
            })
        );
        assert_eq!(fleet.inventory.unmovable.len(), 1);
        assert_eq!(fleet.inventory.unmovable[0].why.code, "no_hosts");
        let _ = SilenceScope::Everything;
    }

    #[test]
    fn services_and_unmovable_rules() {
        let mut fleet = fleet();
        apply(
            &mut fleet,
            &[
                rule(
                    "NginxDown",
                    r#"node_systemd_unit_state{job="node",name=~"nginx.service|php-fpm.service",state="active"} == 0"#,
                    "high",
                    60,
                ),
                rule(
                    "UnitFailed",
                    r#"node_systemd_unit_state{state="failed"} == 1"#,
                    "warning",
                    0,
                ),
                rule(
                    "SystemSaturated",
                    r#"node_load1 / count without (cpu, mode) (node_cpu_seconds_total{mode="idle"}) > 2"#,
                    "warning",
                    900,
                ),
                rule(
                    "TcpProbeSlow",
                    "probe_success < 0 or probe_duration_seconds > 2",
                    "warning",
                    0,
                ),
            ],
        );
        let services: Vec<_> = fleet
            .inventory
            .checks
            .iter()
            .map(|c| (c.host.as_str(), c.watch.clone()))
            .filter(|(_, w)| matches!(w, Watch::Service { .. }))
            .collect();
        assert_eq!(services.len(), 4);
        assert_eq!(
            services[1],
            (
                "web-01",
                Watch::Service {
                    name: "php-fpm.service".into()
                }
            )
        );
        let codes: Vec<_> = fleet
            .inventory
            .unmovable
            .iter()
            .map(|u| (u.origin.name.as_str(), u.why.code.as_str(), u.hosts.len()))
            .collect();
        assert_eq!(
            codes,
            vec![
                ("UnitFailed", "expression", 3),
                ("SystemSaturated", "per_cpu_load", 3),
                ("TcpProbeSlow", "expression", 0),
            ]
        );
        assert_eq!(
            fleet.inventory.unmovable[2].origin.detail.as_deref(),
            Some("probe_success < 0 or probe_duration_seconds > 2")
        );
    }

    #[test]
    fn website_rules_give_alerts_and_thresholds() {
        let mut fleet = fleet();
        apply(
            &mut fleet,
            &[
                rule(
                    "HttpFailure",
                    "probe_http_status_code <= 199 OR probe_http_status_code >= 400",
                    "critical",
                    60,
                ),
                rule(
                    "SlowProbe",
                    "avg_over_time(probe_duration_seconds[1m]) > 1",
                    "warning",
                    60,
                ),
                rule(
                    "SlowPhase",
                    "probe_http_duration_seconds > 2",
                    "critical",
                    0,
                ),
                rule(
                    "CertificateExpiry",
                    "probe_ssl_earliest_cert_expiry - time() < 86400 * 20",
                    "warning",
                    0,
                ),
                rule(
                    "CertificateSoon",
                    "3 <= round((last_over_time(probe_ssl_earliest_cert_expiry[10m]) - time()) / 86400, 0.1) < 20",
                    "critical",
                    0,
                ),
                rule(
                    "CertificateLong",
                    "probe_ssl_earliest_cert_expiry - time() > 86400",
                    "warning",
                    0,
                ),
            ],
        );
        let web = fleet
            .inventory
            .checks
            .iter()
            .find(|c| matches!(c.watch, Watch::Web(_)))
            .unwrap();
        assert_eq!(
            web.alert,
            Some(Alert {
                severity: Severity::Critical,
                for_s: 60,
                no_data: false
            })
        );
        let url = "https://shop.example.com".to_owned();
        let t = &fleet.inventory.thresholds;
        let got: Vec<_> = t
            .iter()
            .map(|t| {
                (
                    t.origin.name.as_str(),
                    t.hosts.clone(),
                    t.measure.clone(),
                    t.direction,
                    t.warning,
                    t.critical,
                    t.for_s,
                )
            })
            .collect();
        let response = Measure::WebResponseTime { url: url.clone() };
        let days = Measure::CertificateDays { url };
        let host = vec!["monitor-01".to_owned()];
        assert_eq!(
            got,
            vec![
                (
                    "SlowProbe",
                    host.clone(),
                    response.clone(),
                    Direction::Above,
                    Some(1.0),
                    None,
                    60
                ),
                (
                    "SlowPhase",
                    host.clone(),
                    response,
                    Direction::Above,
                    None,
                    Some(2.0),
                    0
                ),
                (
                    "CertificateExpiry",
                    host.clone(),
                    days.clone(),
                    Direction::Below,
                    Some(20.0),
                    None,
                    0
                ),
                (
                    "CertificateSoon",
                    host,
                    days,
                    Direction::Below,
                    None,
                    Some(20.0),
                    0
                ),
            ]
        );
        assert_eq!(t[0].fidelity, Fidelity::Exact);
        assert_eq!(
            t[1].fidelity,
            Fidelity::Approximated(Note::plain("web_response_phase"))
        );
        assert_eq!(t[2].fidelity, Fidelity::Exact);
        // The lower bound is a condition MonitorFlow does not check.
        assert_eq!(
            t[3].fidelity,
            Fidelity::Approximated(Note::plain("extra_condition"))
        );
        let codes: Vec<_> = fleet
            .inventory
            .unmovable
            .iter()
            .map(|u| (u.origin.name.as_str(), u.why.code.as_str()))
            .collect();
        assert_eq!(codes, vec![("CertificateLong", "expression")]);
    }

    #[test]
    fn richer_rules_move_as_richer_thresholds() {
        let mut fleet = fleet();
        apply(
            &mut fleet,
            &[
                rule(
                    "Load15m",
                    r#"avg_over_time(node_load1{job="node"}[15m]) > 4"#,
                    "warning",
                    0,
                ),
                rule(
                    "FreeFalling",
                    r#"min_over_time(node_filesystem_avail_bytes{job="node",mountpoint="/"}[10m]) / node_filesystem_size_bytes{job="node",mountpoint="/"} < 0.1"#,
                    "critical",
                    0,
                ),
                rule(
                    "DiskFull",
                    r#"node_filesystem_avail_bytes{job="node"} / node_filesystem_size_bytes{job="node"} < 0.2 and predict_linear(node_filesystem_avail_bytes{job="node"}[6h], 4 * 3600) < 0"#,
                    "critical",
                    60,
                ),
                rule(
                    "LoadAndMemory",
                    r#"node_load1{job="node"} > 8 and node_memory_MemAvailable_bytes{job="node"} / node_memory_MemTotal_bytes{job="node"} < 0.1"#,
                    "warning",
                    0,
                ),
                rule(
                    "LoadVsLastWeek",
                    r#"node_load5{job="node"} / node_load5{job="node"} offset 1w > 2"#,
                    "warning",
                    600,
                ),
                rule(
                    "Silent",
                    r#"absent_over_time(node_load1{job="node"}[10m])"#,
                    "critical",
                    0,
                ),
                rule(
                    "Gone",
                    r#"absent(node_memory_MemAvailable_bytes{job="node"})"#,
                    "warning",
                    300,
                ),
                rule(
                    "FreeVsYesterday",
                    r#"node_filesystem_avail_bytes{job="node"} / node_filesystem_avail_bytes{job="node"} offset 1d < 0.5"#,
                    "warning",
                    0,
                ),
            ],
        );
        let t = &fleet.inventory.thresholds;
        let by = |name: &str| {
            t.iter()
                .find(|t| t.origin.name == name)
                .unwrap_or_else(|| panic!("{name}"))
        };
        let load = by("Load15m");
        assert_eq!(
            (&load.judged, &load.fidelity),
            (
                &Judged::Window {
                    function: WindowFunction::Avg,
                    window_s: 900
                },
                &Fidelity::Exact
            )
        );
        // The lowest free share is the highest share used.
        let free = by("FreeFalling");
        assert_eq!(
            free.judged,
            Judged::Window {
                function: WindowFunction::Max,
                window_s: 600
            }
        );
        assert_eq!(
            (free.direction, free.critical),
            (Direction::Above, Some(0.9))
        );
        // The forecast leads, the share free is its condition.
        let full = by("DiskFull");
        assert_eq!(
            full.judged,
            Judged::Forecast {
                target: 1.0,
                window_s: 21_600
            }
        );
        assert_eq!(full.critical, Some(14_400.0));
        assert_eq!(full.conditions.len(), 1);
        assert_eq!(
            (full.conditions[0].direction, full.conditions[0].value),
            (Direction::Above, 0.8)
        );
        let both = by("LoadAndMemory");
        assert_eq!(both.measure, Measure::CpuLoad { minutes: 1 });
        assert_eq!(both.conditions[0].measure, Measure::MemoryAvailable);
        assert_eq!(both.fidelity, Fidelity::Exact);
        let past = by("LoadVsLastWeek");
        assert_eq!(
            past.judged,
            Judged::Compare {
                ratio: true,
                offset_s: 604_800,
                window_s: 300
            }
        );
        assert_eq!(
            past.fidelity,
            Fidelity::Approximated(Note::plain("prometheus_compare_window"))
        );
        let silent = by("Silent");
        assert_eq!(
            (&silent.judged, silent.critical, &silent.fidelity),
            (&Judged::NoData, Some(600.0), &Fidelity::Exact)
        );
        let gone = by("Gone");
        assert_eq!(
            (gone.warning, &gone.fidelity),
            (
                Some(300.0),
                &Fidelity::Approximated(Note::plain("prometheus_absent"))
            )
        );
        // A share free divided by yesterday's is not a share used.
        assert!(fleet
            .inventory
            .unmovable
            .iter()
            .any(|u| u.origin.name == "FreeVsYesterday"));
    }
}
