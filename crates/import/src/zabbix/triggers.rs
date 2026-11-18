//! Triggers: what a trigger expression becomes in MonitorFlow, a
//! threshold on a measure every server has, the alert of a check, or a
//! reason it cannot be moved.
//!
//! What is read: one comparison of `last`, `min`, `max` or `avg` of an
//! item with a value (a number or a user macro), for example
//! `min(/host/system.cpu.util,5m)>{$CPU.UTIL.CRIT}`; the same with guards
//! such as `and last(/host/vm.memory.size[total])>0`, which are dropped
//! with a note; and `or` of comparisons that give the same threshold
//! (reads or writes of a disk waiting too long).

use crate::args;
use crate::model::{Direction, Measure, Watch};
use crate::notes::Note;
use crate::text;

use super::config::{Item, ItemKind, Trigger};
use super::expression::{self, Call, Expr, Op};
use super::items::{self, param, split_key, Mapped};
use super::macros::{Lookup, Macros};

/// One threshold a trigger gives.
#[derive(Debug, Clone, PartialEq)]
pub struct Level {
    pub measure: Measure,
    pub direction: Direction,
    /// In MonitorFlow's unit: a ratio from 0 to 1, a load, seconds.
    pub value: f64,
    pub for_s: u32,
    pub recovery: Option<f64>,
    /// How the threshold differs from the trigger, when it does.
    pub note: Option<Note>,
}

/// What a trigger becomes.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Thresholds: one, and one more per file system or disk a host
    /// overrides the value for with a macro context.
    Thresholds(Vec<Level>),
    /// The alert of the check of an item: `key` is the item's key, or
    /// `web:<scenario>` for a web scenario.
    Alert {
        key: String,
        for_s: u32,
        note: Option<Note>,
    },
    Unmovable(Note),
}

/// What the mapping needs to know of the host.
pub trait Host {
    fn macros(&self) -> &Macros;
    /// The item of `key` on the host, if it has one.
    fn item(&self, key: &str) -> Option<&Item>;
    /// The name of the first step of the web scenario `name`, and the URL
    /// its website check requests, if the host has that scenario.
    fn scenario(&self, _name: &str) -> Option<(String, String)> {
        None
    }
    /// The URL of the host's certificate check (`web.certificate.get`),
    /// if it has one.
    fn certificate_url(&self) -> Option<String> {
        None
    }
}

/// What `trigger` becomes on `host`.
pub fn map(trigger: &Trigger, host: &impl Host) -> Outcome {
    if !trigger.enabled {
        return Outcome::Unmovable(Note::plain("disabled"));
    }
    let Ok(expr) = expression::parse(&trigger.expression) else {
        return Outcome::Unmovable(Note::plain("expression"));
    };
    let mut outcome = combine(&expr, host);
    if let (Outcome::Thresholds(levels), Some(recovery)) = (&mut outcome, &trigger.recovery) {
        if let Some(value) = recovery_value(recovery, &levels[0], host) {
            levels[0].recovery = Some(value);
        }
    }
    outcome
}

/// One part of an expression, mapped.
enum Part {
    Levels(Vec<Level>),
    Alert {
        key: String,
        for_s: u32,
    },
    /// A guard such as `last(...)>0`, which only checks there is data.
    Guard,
    Fails(Note),
}

fn combine(expr: &Expr, host: &impl Host) -> Outcome {
    match expr {
        Expr::Binary(Op::And, ..) => {
            let mut parts = Vec::new();
            flatten(expr, Op::And, &mut parts);
            let mapped: Vec<(&Expr, Part)> = parts.iter().map(|p| (*p, part(p, host))).collect();
            let useful: Vec<&(&Expr, Part)> = mapped
                .iter()
                .filter(|(_, p)| matches!(p, Part::Levels(_) | Part::Alert { .. }))
                .collect();
            match useful.as_slice() {
                [(kept, part)] => {
                    let note = Note::new("zabbix_partial", args! {"kept" => kept});
                    outcome(part, Some(note))
                }
                [] => Outcome::Unmovable(first_reason(&mapped)),
                _ => Outcome::Unmovable(Note::plain("expression")),
            }
        }
        Expr::Binary(Op::Or, ..) => {
            let mut parts = Vec::new();
            flatten(expr, Op::Or, &mut parts);
            let mapped: Vec<(&Expr, Part)> = parts.iter().map(|p| (*p, part(p, host))).collect();
            let first = &mapped[0].1;
            let same = mapped.iter().all(|(_, p)| match (first, p) {
                (Part::Levels(a), Part::Levels(b)) => {
                    a[0].measure == b[0].measure
                        && a[0].direction == b[0].direction
                        && a[0].value == b[0].value
                }
                (Part::Alert { key: a, .. }, Part::Alert { key: b, .. }) => a == b,
                _ => false,
            });
            if same {
                outcome(first, None)
            } else {
                Outcome::Unmovable(first_reason(&mapped))
            }
        }
        other => match part(other, host) {
            Part::Guard => Outcome::Unmovable(Note::plain("expression")),
            p => outcome(&p, None),
        },
    }
}

/// The most telling reason among parts that could not be read: a
/// specific one before the generic `expression`.
fn first_reason(parts: &[(&Expr, Part)]) -> Note {
    parts
        .iter()
        .filter_map(|(_, p)| match p {
            Part::Fails(note) => Some(note.clone()),
            _ => None,
        })
        .find(|n| n.code != "expression")
        .unwrap_or_else(|| Note::plain("expression"))
}

fn outcome(part: &Part, partial: Option<Note>) -> Outcome {
    match part {
        Part::Levels(levels) => Outcome::Thresholds(
            levels
                .iter()
                .cloned()
                .map(|mut l| {
                    if partial.is_some() {
                        l.note.clone_from(&partial);
                    }
                    l
                })
                .collect(),
        ),
        Part::Alert { key, for_s } => Outcome::Alert {
            key: key.clone(),
            for_s: *for_s,
            note: partial,
        },
        Part::Guard => Outcome::Unmovable(Note::plain("expression")),
        Part::Fails(note) => Outcome::Unmovable(note.clone()),
    }
}

fn flatten<'a>(expr: &'a Expr, op: Op, out: &mut Vec<&'a Expr>) {
    match expr {
        Expr::Binary(o, a, b) if *o == op => {
            flatten(a, op, out);
            flatten(b, op, out);
        }
        other => out.push(other),
    }
}

/// A value compared with.
#[derive(Debug, Clone, PartialEq)]
enum Constant {
    Number { value: f64, written: String },
    Text(String),
}

/// `function(item) op constant`, the function on the left.
struct Comparison<'a> {
    call: &'a Call,
    op: Op,
    constant: Constant,
    /// The macro the constant came from, for its contexts.
    from_macro: Option<String>,
    /// Divided by `last(system.cpu.num)`: a load per processor.
    per_cpu: bool,
}

fn constant(expr: &Expr, macros: &Macros) -> Result<(Constant, Option<String>), Note> {
    match expr {
        Expr::Number(n) => text::number(n)
            .map(|value| {
                (
                    Constant::Number {
                        value,
                        written: n.clone(),
                    },
                    None,
                )
            })
            .ok_or_else(|| Note::plain("expression")),
        Expr::Negative(inner) => match constant(inner, macros)? {
            (Constant::Number { value, written }, m) => Ok((
                Constant::Number {
                    value: -value,
                    written: format!("-{written}"),
                },
                m,
            )),
            _ => Err(Note::plain("expression")),
        },
        Expr::Text(t) => Ok((Constant::Text(t.clone()), None)),
        Expr::Macro(name) if name.starts_with("{$") => match macros.get(name) {
            Lookup::Found(value) => {
                let constant = match text::number(&value) {
                    Some(n) => Constant::Number {
                        value: n,
                        written: value.clone(),
                    },
                    None => Constant::Text(value),
                };
                Ok((constant, Some(name.clone())))
            }
            _ => Err(Note::new("unresolved_macro", args! {"macro" => name})),
        },
        _ => Err(Note::plain("expression")),
    }
}

/// An item function, possibly divided by the number of processors.
fn function(expr: &Expr) -> Option<(&Call, bool)> {
    match expr {
        Expr::Call(call) if call.item.is_some() => Some((call, false)),
        Expr::Binary(Op::Div, a, b) => match (&**a, &**b) {
            (Expr::Call(call), Expr::Call(cpus))
                if call.item.is_some()
                    && cpus
                        .item
                        .as_ref()
                        .is_some_and(|i| split_key(&i.key).0 == "system.cpu.num") =>
            {
                Some((call, true))
            }
            _ => None,
        },
        _ => None,
    }
}

fn comparison<'a>(expr: &'a Expr, macros: &Macros) -> Result<Comparison<'a>, Note> {
    match expr {
        Expr::Binary(op, a, b) if op.is_comparison() => {
            let (call, per_cpu, op, other) = match (function(a), function(b)) {
                (Some((call, per_cpu)), _) => (call, per_cpu, *op, &**b),
                (None, Some((call, per_cpu))) => (call, per_cpu, op.flipped(), &**a),
                _ => return Err(Note::plain("expression")),
            };
            let (constant, from_macro) = constant(other, macros)?;
            Ok(Comparison {
                call,
                op,
                constant,
                from_macro,
                per_cpu,
            })
        }
        // A function alone is true when it is not zero: `nodata(...)`.
        Expr::Call(call) if call.item.is_some() => Ok(Comparison {
            call,
            op: Op::Ne,
            constant: Constant::Number {
                value: 0.0,
                written: "0".into(),
            },
            from_macro: None,
            per_cpu: false,
        }),
        _ => Err(Note::plain("expression")),
    }
}

/// The period argument of a function: seconds, or a count of values.
enum Period {
    Latest,
    Seconds(u32),
    Values(u32),
}

fn period(call: &Call, macros: &Macros) -> Result<Period, Note> {
    let raw = call.args.first().map_or("", |a| a.trim());
    if raw.is_empty() || raw == "#1" {
        return Ok(Period::Latest);
    }
    let raw = macros
        .expand(raw)
        .map_err(|m| Note::new("unresolved_macro", args! {"macro" => m}))?;
    // A time shift (`5m:now-1h`) compares the past: not a threshold.
    if raw.contains(':') {
        return Err(Note::plain("expression"));
    }
    if let Some(count) = raw.strip_prefix('#') {
        return count
            .parse()
            .map(Period::Values)
            .map_err(|_| Note::plain("expression"));
    }
    text::seconds(&raw)
        .and_then(|s| u32::try_from(s).ok())
        .map(Period::Seconds)
        .ok_or_else(|| Note::plain("expression"))
}

fn part(expr: &Expr, host: &impl Host) -> Part {
    if let Some(part) = certificate(expr, host) {
        return part;
    }
    let macros = host.macros();
    let comparison = match comparison(expr, macros) {
        Ok(c) => c,
        Err(note) => return Part::Fails(note),
    };
    let item_ref = comparison.call.item.as_ref().expect("an item function");
    let key = item_ref.key.as_str();
    let (name, _) = split_key(key);
    // Web scenarios' own items.
    if let Some(part) = web_test(&comparison, &name, key, host) {
        return part;
    }
    let mapped = match host.item(key) {
        Some(item) => items::map(item, macros),
        None => items::map(
            &Item {
                name: key.to_owned(),
                key: key.to_owned(),
                kind: ItemKind::Agent,
                delay: None,
                enabled: true,
                prototype: false,
                http: None,
                template: None,
            },
            macros,
        ),
    };
    let interval = host
        .item(key)
        .and_then(|i| items::interval(i.delay.as_deref(), macros));
    let period = match period(comparison.call, macros) {
        Ok(p) => p,
        Err(note) => return Part::Fails(note),
    };
    let seconds = |p: &Period| match p {
        Period::Latest => 0,
        Period::Seconds(s) => *s,
        Period::Values(n) => interval.map_or(0, |i| i.saturating_mul(*n)),
    };
    match mapped {
        // The response time of a page.
        Mapped::Check(Watch::Web(site), _)
            if name == "web.page.perf" && matches!(comparison.op, Op::Gt | Op::Ge) =>
        {
            response_level(
                &comparison,
                Measure::WebResponseTime { url: site.url },
                &period,
                seconds(&period),
            )
        }
        // A port, a service or a page doing more than a value is no
        // failure: a response time too long, too many processes.
        Mapped::Check(Watch::Port { .. } | Watch::Service { .. } | Watch::Web(_), _)
            if matches!(comparison.op, Op::Gt | Op::Ge)
                && matches!(comparison.constant, Constant::Number { value, .. } if value > 0.0) =>
        {
            Part::Fails(Note::plain("expression"))
        }
        Mapped::Check(..) | Mapped::Agent(_) => {
            // A check failing, or the agent gone: the check's alert.
            let for_s = match comparison.call.name.as_str() {
                "max" | "nodata" => seconds(&period),
                _ => 0,
            };
            Part::Alert {
                key: key.to_owned(),
                for_s,
            }
        }
        // An item that cannot be moved: its trigger neither.
        Mapped::Unmovable(note)
            if matches!(
                note.code.as_str(),
                "disabled" | "udp" | "script" | "snmp" | "unresolved_macro"
            ) =>
        {
            Part::Fails(note)
        }
        _ => level(&comparison, &name, key, &period, seconds(&period), macros),
    }
}

/// What a trigger on a web scenario's own item becomes: the scenario
/// failing is its check's alert, a step's response time a threshold on
/// it. Only the first step has a check.
fn web_test(comparison: &Comparison<'_>, name: &str, key: &str, host: &impl Host) -> Option<Part> {
    if !name.starts_with("web.test.") {
        return None;
    }
    let expanded = host.macros().expand(key).unwrap_or_else(|_| key.to_owned());
    let (_, params) = split_key(&expanded);
    let scenario = param(&params, 0);
    if scenario.is_empty() {
        return Some(Part::Fails(Note::plain("expression")));
    }
    let alert = Part::Alert {
        key: format!("web:{scenario}"),
        for_s: 0,
    };
    match name {
        "web.test.fail" | "web.test.error" => Some(alert),
        "web.test.rspcode" | "web.test.time" => {
            let step = param(&params, 1);
            let first = host.scenario(scenario);
            if let Some((first_step, _)) = &first {
                if !step.is_empty() && step != first_step {
                    return Some(Part::Fails(Note::new(
                        "zabbix_web_step",
                        args! {"step" => step},
                    )));
                }
            }
            if name == "web.test.rspcode" {
                return Some(alert);
            }
            let Some((_, url)) = first else {
                return Some(Part::Fails(Note::plain("web_no_check")));
            };
            let period = match period(comparison.call, host.macros()) {
                Ok(p) => p,
                Err(note) => return Some(Part::Fails(note)),
            };
            let seconds = match period {
                Period::Seconds(s) => s,
                Period::Latest | Period::Values(_) => 0,
            };
            Some(response_level(
                comparison,
                Measure::WebResponseTime { url },
                &period,
                seconds,
            ))
        }
        _ => None,
    }
}

/// A response time above a value, in seconds as Zabbix measures it.
fn response_level(
    comparison: &Comparison<'_>,
    measure: Measure,
    period: &Period,
    period_s: u32,
) -> Part {
    let Constant::Number { value, .. } = &comparison.constant else {
        return Part::Fails(Note::plain("expression"));
    };
    let function = comparison.call.name.as_str();
    let latest = matches!(period, Period::Latest);
    if !matches!(comparison.op, Op::Gt | Op::Ge)
        || !matches!(function, "last" | "min" | "max" | "avg")
        || (function == "last" && !latest)
    {
        return Part::Fails(Note::plain("expression"));
    }
    let sustained = matches!(function, "min" | "avg");
    Part::Levels(vec![Level {
        measure,
        direction: Direction::Above,
        value: round(*value),
        for_s: if sustained { period_s } else { 0 },
        recovery: None,
        note: None,
    }])
}

/// `last(cert.not_after) - now()`, the seconds a certificate has left as
/// Zabbix's certificate template computes them, times the factor it is
/// divided or multiplied by.
fn seconds_left(expr: &Expr) -> Option<f64> {
    match expr {
        Expr::Binary(Op::Sub, a, b) => {
            let after = matches!(&**a, Expr::Call(c) if c.name == "last"
                && c.item.as_ref().is_some_and(|i| split_key(&i.key).0 == "cert.not_after"));
            let now = matches!(&**b, Expr::Call(c) if c.name == "now" && c.item.is_none());
            (after && now).then_some(1.0)
        }
        Expr::Binary(Op::Div, a, b) => {
            let factor = seconds_left(a)?;
            let Expr::Number(n) = &**b else { return None };
            let n = text::number(n).filter(|n| *n != 0.0)?;
            Some(factor / n)
        }
        _ => None,
    }
}

/// The value of a constant expression: numbers and macros with `*` and
/// `/`, such as `{$CERT.EXPIRY.WARN} * 86400`.
fn value(expr: &Expr, macros: &Macros) -> Option<f64> {
    match expr {
        Expr::Binary(op @ (Op::Mul | Op::Div), a, b) => {
            let (a, b) = (value(a, macros)?, value(b, macros)?);
            match op {
                Op::Mul => Some(a * b),
                _ => (b != 0.0).then(|| a / b),
            }
        }
        other => match constant(other, macros).ok()?.0 {
            Constant::Number { value, .. } => Some(value),
            Constant::Text(_) => None,
        },
    }
}

/// A certificate expiring in fewer days than a value: a threshold on the
/// days left of the host's certificate check.
fn certificate(expr: &Expr, host: &impl Host) -> Option<Part> {
    let Expr::Binary(op, a, b) = expr else {
        return None;
    };
    if !op.is_comparison() {
        return None;
    }
    let (factor, other, op) = match (seconds_left(a), seconds_left(b)) {
        (Some(factor), _) => (factor, &**b, *op),
        (None, Some(factor)) => (factor, &**a, op.flipped()),
        (None, None) => return None,
    };
    if !matches!(op, Op::Lt | Op::Le) {
        return Some(Part::Fails(Note::plain("expression")));
    }
    let Some(limit) = value(other, host.macros()) else {
        return Some(Part::Fails(Note::plain("expression")));
    };
    let Some(url) = host.certificate_url() else {
        return Some(Part::Fails(Note::plain("web_no_check")));
    };
    Some(Part::Levels(vec![Level {
        measure: Measure::CertificateDays { url },
        direction: Direction::Below,
        value: round(limit / factor / 86_400.0),
        for_s: 0,
        recovery: None,
        note: None,
    }]))
}

/// A threshold from a comparison on a measure every server has.
fn level(
    comparison: &Comparison<'_>,
    name: &str,
    key: &str,
    period: &Period,
    period_s: u32,
    macros: &Macros,
) -> Part {
    let Constant::Number { value, written } = &comparison.constant else {
        return Part::Fails(Note::plain("expression"));
    };
    let direction = match comparison.op {
        Op::Gt | Op::Ge => Direction::Above,
        Op::Lt | Op::Le => Direction::Below,
        _ => return Part::Fails(Note::plain("expression")),
    };
    if direction == Direction::Above && *value == 0.0 && !comparison.per_cpu {
        return Part::Guard;
    }
    let function = comparison.call.name.as_str();
    if !matches!(function, "last" | "min" | "max" | "avg") {
        return Part::Fails(Note::plain("expression"));
    }
    if function == "last" && !matches!(period, Period::Latest) {
        // `last(...,#2)`: an older value.
        return Part::Fails(Note::plain("expression"));
    }
    let no_measure = || Part::Fails(Note::new("zabbix_no_measure", args! {"key" => key}));
    let expanded = macros.expand(key).unwrap_or_else(|_| key.to_owned());
    let (_, params) = split_key(&expanded);
    let p = |n| param(&params, n);
    let named = |text: &str| Some(text.to_owned()).filter(|t| !t.is_empty() && !t.contains("{#"));
    // (measure, transform of the value, note).
    type Transform = fn(f64) -> f64;
    let percent: Transform = |v| v / 100.0;
    let free_percent: Transform = |v| (100.0 - v) / 100.0;
    let (measure, flip, transform, note): (Measure, bool, Transform, Option<Note>) = match name {
        "system.cpu.load" => {
            if comparison.per_cpu || p(0) == "percpu" {
                return Part::Fails(Note::plain("per_cpu_load"));
            }
            if !matches!(p(0), "" | "all") {
                return no_measure();
            }
            let minutes = match p(1) {
                "" | "avg1" => 1,
                "avg5" => 5,
                "avg15" => 15,
                _ => return no_measure(),
            };
            (Measure::CpuLoad { minutes }, false, |v| v, None)
        }
        _ if comparison.per_cpu => return Part::Fails(Note::plain("per_cpu_load")),
        "system.cpu.util" if matches!(p(0), "" | "all") => match p(1) {
            "" => (Measure::CpuUsage, false, percent, None),
            "idle" => (Measure::CpuUsage, true, free_percent, None),
            _ => return no_measure(),
        },
        "vm.memory.util" | "vm.memory.utilization" => {
            (Measure::MemoryAvailable, true, free_percent, None)
        }
        "vm.memory.size" => match p(0) {
            "pused" => (Measure::MemoryAvailable, true, free_percent, None),
            "pavailable" => (Measure::MemoryAvailable, false, percent, None),
            _ => return Part::Fails(Note::new("zabbix_bytes", args! {"value" => written})),
        },
        "vfs.fs.size" | "vfs.fs.dependent.size" => {
            let mount = named(p(0));
            match p(1) {
                "pused" => (Measure::DiskUsed { mount }, false, percent, None),
                "pfree" => (
                    Measure::DiskUsed { mount },
                    true,
                    free_percent,
                    Some(Note::plain("disk_free_as_used")),
                ),
                _ => return Part::Fails(Note::new("zabbix_bytes", args! {"value" => written})),
            }
        }
        "vfs.dev.read.await" | "vfs.dev.write.await" => (
            Measure::DiskWait {
                device: named(p(0)),
            },
            false,
            |v| v / 1000.0,
            Some(Note::plain("disk_wait")),
        ),
        n if n.starts_with("net.if.") => return Part::Fails(Note::plain("network_traffic")),
        n if n.starts_with("system.cpu.")
            || n.starts_with("system.swap.")
            || n.starts_with("vm.memory.")
            || n.starts_with("vfs.fs.")
            || n.starts_with("vfs.dev.") =>
        {
            return no_measure()
        }
        _ => return Part::Fails(Note::plain("expression")),
    };
    let direction = match (direction, flip) {
        (d, false) => d,
        (Direction::Above, true) => Direction::Below,
        (Direction::Below, true) => Direction::Above,
    };
    // How long the value must last: the window of `min` above or `max`
    // below a value, or of an average.
    let sustained = matches!(
        (function, comparison.op),
        ("min", Op::Gt | Op::Ge) | ("max", Op::Lt | Op::Le) | ("avg", _)
    );
    let for_s = if sustained { period_s } else { 0 };
    let base = Level {
        measure: measure.clone(),
        direction,
        value: round(transform(*value)),
        for_s,
        recovery: None,
        note,
    };
    let mut levels = vec![base.clone()];
    // `{$VFS.FS.PUSED.MAX.CRIT:"{#FSNAME}"}` with host overrides for some
    // mounts: a threshold for each of them.
    if let Some(name) = comparison
        .from_macro
        .as_deref()
        .filter(|m| m.contains("{#"))
    {
        for (context, value) in macros.contexts(name) {
            let Some(number) = text::number(&value) else {
                continue;
            };
            let measure = match &measure {
                Measure::DiskUsed { mount: None } => Measure::DiskUsed {
                    mount: Some(context),
                },
                Measure::DiskWait { device: None } => Measure::DiskWait {
                    device: Some(context),
                },
                _ => continue,
            };
            levels.push(Level {
                measure,
                value: round(transform(number)),
                ..base.clone()
            });
        }
    }
    Part::Levels(levels)
}

/// Rounded to remove float noise: `(100 - 80) / 100` is 0.2.
fn round(value: f64) -> f64 {
    (value * 1e9).round() / 1e9
}

/// The value a recovery expression ends the problem at, when it compares
/// the same measure.
fn recovery_value(text: &str, level: &Level, host: &impl Host) -> Option<f64> {
    let expr = expression::parse(text).ok()?;
    let mut parts = Vec::new();
    flatten(&expr, Op::And, &mut parts);
    parts.into_iter().find_map(|p| match part(p, host) {
        Part::Levels(levels) if levels[0].measure == level.measure => Some(levels[0].value),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::super::config::{Macro, Priority};
    use super::super::macros::HostValues;
    use super::*;

    struct TestHost {
        macros: Macros,
        items: Vec<Item>,
    }

    impl Host for TestHost {
        fn macros(&self) -> &Macros {
            &self.macros
        }
        fn item(&self, key: &str) -> Option<&Item> {
            self.items.iter().find(|i| i.key == key)
        }
    }

    fn host() -> TestHost {
        let m = |name: &str, value: &str| Macro {
            name: name.into(),
            value: Some(value.into()),
        };
        TestHost {
            macros: Macros::new(
                vec![
                    vec![
                        m("{$CPU.UTIL.CRIT}", "80"),
                        m("{$VFS.FS.PUSED.MAX.CRIT:\"/var/lib/postgresql\"}", "95"),
                    ],
                    vec![
                        m("{$CPU.UTIL.CRIT}", "90"),
                        m("{$VFS.FS.PUSED.MAX.CRIT}", "90"),
                        m("{$MEMORY.UTIL.MAX}", "90"),
                        m("{$MEMORY.AVAILABLE.MIN}", "20M"),
                        m("{$AGENT.TIMEOUT}", "3m"),
                        m("{$LOAD_AVG_PER_CPU.MAX.WARN}", "1.5"),
                        m("{$VFS.DEV.READ.AWAIT.WARN}", "20"),
                        m("{$VFS.DEV.WRITE.AWAIT.WARN}", "20"),
                    ],
                ],
                HostValues::default(),
            ),
            items: vec![
                Item {
                    name: "Port".into(),
                    key: "net.tcp.port[,5432]".into(),
                    kind: ItemKind::Agent,
                    delay: Some("30s".into()),
                    enabled: true,
                    prototype: false,
                    http: None,
                    template: None,
                },
                Item {
                    name: "CPU".into(),
                    key: "system.cpu.util".into(),
                    kind: ItemKind::Dependent,
                    delay: None,
                    enabled: true,
                    prototype: false,
                    http: None,
                    template: None,
                },
            ],
        }
    }

    fn trigger(expression: &str) -> Trigger {
        Trigger {
            name: "t".into(),
            expression: expression.into(),
            recovery: None,
            priority: Priority::Warning,
            enabled: true,
            prototype: false,
            template: None,
        }
    }

    fn level(measure: Measure, direction: Direction, value: f64, for_s: u32) -> Level {
        Level {
            measure,
            direction,
            value,
            for_s,
            recovery: None,
            note: None,
        }
    }

    fn one(expression: &str) -> Outcome {
        map(&trigger(expression), &host())
    }

    fn unmovable(expression: &str) -> String {
        match one(expression) {
            Outcome::Unmovable(note) => note.code,
            other => panic!("{expression}: unmovable expected, got {other:?}"),
        }
    }

    #[test]
    fn processor() {
        assert_eq!(
            one("min(/Linux by Zabbix agent/system.cpu.util,5m)>{$CPU.UTIL.CRIT}"),
            Outcome::Thresholds(vec![level(Measure::CpuUsage, Direction::Above, 0.8, 300)])
        );
        assert_eq!(
            one("last(/h/system.cpu.util)>95"),
            Outcome::Thresholds(vec![level(Measure::CpuUsage, Direction::Above, 0.95, 0)])
        );
        assert_eq!(
            one("avg(/h/system.cpu.util[,idle],10m)<20"),
            Outcome::Thresholds(vec![level(Measure::CpuUsage, Direction::Above, 0.8, 600)])
        );
        assert_eq!(
            one("{Template OS Linux:system.cpu.load[percpu,avg1].avg(5m)}>5"),
            Outcome::Unmovable(Note::plain("per_cpu_load"))
        );
        assert_eq!(
            one("avg(/h/system.cpu.load[all,avg15],15m)>=4"),
            Outcome::Thresholds(vec![level(
                Measure::CpuLoad { minutes: 15 },
                Direction::Above,
                4.0,
                900
            )])
        );
        // The stock trigger: per processor, with guards.
        assert_eq!(
            unmovable(
                "min(/Linux by Zabbix agent/system.cpu.load[all,avg1],5m)/last(/Linux by Zabbix agent/system.cpu.num)>{$LOAD_AVG_PER_CPU.MAX.WARN}\nand last(/Linux by Zabbix agent/system.cpu.load[all,avg5])>0\nand last(/Linux by Zabbix agent/system.cpu.load[all,avg15])>0"
            ),
            "per_cpu_load"
        );
        assert_eq!(
            unmovable("last(/h/system.cpu.util[,iowait])>20"),
            "zabbix_no_measure"
        );
    }

    #[test]
    fn memory() {
        assert_eq!(
            one("min(/L/vm.memory.util,5m)>{$MEMORY.UTIL.MAX}"),
            Outcome::Thresholds(vec![level(
                Measure::MemoryAvailable,
                Direction::Below,
                0.1,
                300
            )])
        );
        assert_eq!(
            one("last(/L/vm.memory.size[pavailable])<15"),
            Outcome::Thresholds(vec![level(
                Measure::MemoryAvailable,
                Direction::Below,
                0.15,
                0
            )])
        );
        assert_eq!(
            one("max(/L/vm.memory.size[available],5m)<{$MEMORY.AVAILABLE.MIN} and last(/L/vm.memory.size[total])>0"),
            Outcome::Unmovable(Note::new("zabbix_bytes", args! {"value" => "20M"}))
        );
        assert_eq!(
            unmovable(
                "max(/L/system.swap.size[,pfree],5m)<50 and last(/L/system.swap.size[,total])>0"
            ),
            "zabbix_no_measure"
        );
    }

    #[test]
    fn disks() {
        assert_eq!(
            one("last(/web-01/vfs.fs.size[/,pused])>85"),
            Outcome::Thresholds(vec![level(
                Measure::DiskUsed {
                    mount: Some("/".into())
                },
                Direction::Above,
                0.85,
                0
            )])
        );
        let mut free = level(
            Measure::DiskUsed {
                mount: Some("/data".into()),
            },
            Direction::Above,
            0.9,
            0,
        );
        free.note = Some(Note::plain("disk_free_as_used"));
        assert_eq!(
            one("last(/h/vfs.fs.size[/data,pfree])<10"),
            Outcome::Thresholds(vec![free])
        );
        // A prototype, and the host's override for one mount.
        let base = level(
            Measure::DiskUsed { mount: None },
            Direction::Above,
            0.9,
            300,
        );
        let mut postgres = base.clone();
        postgres.measure = Measure::DiskUsed {
            mount: Some("/var/lib/postgresql".into()),
        };
        postgres.value = 0.95;
        assert_eq!(
            one("min(/L/vfs.fs.dependent.size[{#FSNAME},pused],5m)>{$VFS.FS.PUSED.MAX.CRIT:\"{#FSNAME}\"}"),
            Outcome::Thresholds(vec![base, postgres])
        );
        let mut wait = level(
            Measure::DiskWait { device: None },
            Direction::Above,
            0.02,
            900,
        );
        wait.note = Some(Note::plain("disk_wait"));
        assert_eq!(
            one("min(/L/vfs.dev.read.await[{#DEVNAME}],15m) > {$VFS.DEV.READ.AWAIT.WARN:\"{#DEVNAME}\"} or min(/L/vfs.dev.write.await[{#DEVNAME}],15m) > {$VFS.DEV.WRITE.AWAIT.WARN:\"{#DEVNAME}\"}"),
            Outcome::Thresholds(vec![wait])
        );
        assert_eq!(
            unmovable("min(/L/vfs.fs.dependent.inode[{#FSNAME},pfree],5m)<10"),
            "zabbix_no_measure"
        );
    }

    #[test]
    fn checks_and_the_agent() {
        assert_eq!(
            one("max(/db-01/net.tcp.port[,5432],3m)=0"),
            Outcome::Alert {
                key: "net.tcp.port[,5432]".into(),
                for_s: 180,
                note: None
            }
        );
        assert_eq!(
            one("max(/L/zabbix[host,agent,available],{$AGENT.TIMEOUT})=0"),
            Outcome::Alert {
                key: "zabbix[host,agent,available]".into(),
                for_s: 180,
                note: None
            }
        );
        assert_eq!(
            one("nodata(/L/agent.ping,5m)=1"),
            Outcome::Alert {
                key: "agent.ping".into(),
                for_s: 300,
                note: None
            }
        );
        assert_eq!(
            one("last(/web-01/web.test.fail[Shop home])<>0"),
            Outcome::Alert {
                key: "web:Shop home".into(),
                for_s: 0,
                note: None
            }
        );
        assert_eq!(
            one(r#"find(/db-01/log[/var/log/syslog,error],,"regexp","error")=1"#),
            Outcome::Alert {
                key: "log[/var/log/syslog,error]".into(),
                for_s: 0,
                note: None
            }
        );
        let kept = r#"last(/N/net.tcp.service[http,"localhost","80"])=0"#;
        assert_eq!(
            one(&format!(
                "{kept} and last(/N/nginx.proc.num[{{#NGINX.NAME}}])>0"
            )),
            Outcome::Alert {
                key: r#"net.tcp.service[http,"localhost","80"]"#.into(),
                for_s: 0,
                note: Some(Note::new("zabbix_partial", args! {"kept" => kept}))
            }
        );
    }

    #[test]
    fn recovery_values() {
        let mut t = trigger("min(/db-01/system.cpu.util,10m)>90");
        t.recovery = Some("max(/db-01/system.cpu.util,5m)<70".into());
        let mut expected = level(Measure::CpuUsage, Direction::Above, 0.9, 600);
        expected.recovery = Some(0.7);
        assert_eq!(map(&t, &host()), Outcome::Thresholds(vec![expected]));
    }

    #[test]
    fn what_cannot_be_read() {
        assert_eq!(unmovable("last(/h/system.uptime)<10m"), "expression");
        assert_eq!(
            unmovable("last(/L/vfs.file.cksum[/etc/passwd,sha256],#1)<>last(/L/vfs.file.cksum[/etc/passwd,sha256],#2)"),
            "expression"
        );
        assert_eq!(
            unmovable("last(/h/system.cpu.util)>{$NOPE}"),
            "unresolved_macro"
        );
        assert_eq!(
            unmovable("avg(/h/net.if.in[eth0],15m)>100M"),
            "network_traffic"
        );
        assert_eq!(unmovable("last(/h/system.cpu.util,#2)>90"), "expression");
        assert_eq!(unmovable("last(/h/system.cpu.util"), "expression");
        let mut off = trigger("last(/h/system.cpu.util)>90");
        off.enabled = false;
        assert_eq!(
            map(&off, &host()),
            Outcome::Unmovable(Note::plain("disabled"))
        );
    }
}
