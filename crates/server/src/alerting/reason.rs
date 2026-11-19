//! Why a sensor is not OK, in one plain sentence: "9% available is below
//! the warning limit of 12%", "No data for 5 minutes". The API returns it
//! with incidents and sensor values, so every screen says the same thing.
//!
//! Values read as the web interface shows them: a ratio as a percentage, a
//! size in binary units, a duration in its largest unit, and the states of
//! services and ports in words.

use std::str::FromStr;

use serde_json::Value;

use super::offline::OFFLINE_METRIC;
use super::rules::Comparison;

const PREFIX: &str = "monitorflow_";

/// The sentence for an incident on `metric` from the `threshold` stored
/// with it ([`super::Rule::snapshot`]): the last value while open, the
/// peak once resolved.
pub fn incident(metric: &str, severity: &str, open: bool, value: f64, threshold: &Value) -> String {
    if metric == OFFLINE_METRIC {
        let minutes = threshold["after_s"].as_u64().unwrap_or(300).div_ceil(60);
        return format!("No contact for over {}", plural(minutes, "minute"));
    }
    let short = metric.strip_prefix(PREFIX).unwrap_or(metric);
    let sentence = match threshold["kind"].as_str() {
        Some("nodata") => nodata(value, open),
        Some("forecast") => forecast(metric, value, open, threshold),
        Some("window") => window(metric, severity, open, value, threshold),
        Some("compare") => compare(metric, severity, open, value, threshold),
        _ => None,
    };
    if let Some(sentence) = sentence {
        return with_conditions(sentence, threshold);
    }
    if short.ends_with("_status") {
        return script_status(value, open);
    }
    if let Some((now, expected)) = state_words(short, value) {
        return if open {
            format!("{}, expected {expected}", capitalized(now))
        } else {
            format!("Was {now}, expected {expected}")
        };
    }
    let comparison = threshold["comparison"]
        .as_str()
        .and_then(|c| Comparison::from_str(c).ok());
    let limit = threshold[severity].as_f64();
    let shown = with_noun(short, &format_value(metric, value));
    match (comparison, limit) {
        (Some(comparison), Some(limit)) => {
            let side = match comparison {
                Comparison::Gt => "above",
                Comparison::Ge => "at or above",
                Comparison::Lt => "below",
                Comparison::Le => "at or below",
            };
            let limit = format_value(metric, limit);
            if open {
                format!("{shown} is {side} the {severity} limit of {limit}")
            } else {
                format!("Peaked at {shown}, {side} the {severity} limit of {limit}")
            }
        }
        _ if open => format!("{} past the {severity} limit", capitalized(&shown)),
        _ => format!("Peaked at {shown}"),
    }
}

/// ", while 1 more condition holds" for a rule joined with others.
fn with_conditions(sentence: String, threshold: &Value) -> String {
    match threshold["conditions"].as_array().map_or(0, Vec::len) {
        0 => sentence,
        1 => format!("{sentence}, while 1 more condition holds"),
        n => format!("{sentence}, while {n} more conditions hold"),
    }
}

/// "No data for 12 minutes" (ADR-0153).
fn nodata(silence_s: f64, open: bool) -> Option<String> {
    let silence = span(silence_s as i64);
    Some(if open {
        format!("No data for {silence}")
    } else {
        format!("Was silent for {silence}")
    })
}

/// "Expected to reach 100% in about 2 days": the time left until the
/// forecast's target.
fn forecast(metric: &str, left_s: f64, open: bool, threshold: &Value) -> Option<String> {
    let target = format_value(metric, threshold["target"].as_f64()?);
    Some(match (open, left_s) {
        (true, s) if s < 1.0 => format!("Has reached {target}"),
        (true, s) => format!("Expected to reach {target} in about {}", span(s as i64)),
        (false, s) if s < 1.0 => format!("Reached {target}"),
        (false, s) => format!("Was expected to reach {target} within {}", span(s as i64)),
    })
}

/// "Average over 15 minutes", "Rate over 5 minutes".
fn function_over(function: &str, window_s: u64) -> String {
    let name = match function {
        "avg" => "Average",
        "min" => "Lowest value",
        "max" => "Highest value",
        "change" => "Change",
        "rate" => "Rate",
        other => other,
    };
    format!("{name} over {}", span(window_s as i64))
}

/// The limit sentence for a value already formatted: "is 6.2, above the
/// warning limit of 4", or "peaked at 6.2, above ...".
fn against_limit(
    shown: &str,
    severity: &str,
    open: bool,
    threshold: &Value,
    limit: impl Fn(f64) -> String,
) -> String {
    let side = threshold["comparison"]
        .as_str()
        .and_then(|c| Comparison::from_str(c).ok())
        .map_or("past", |c| match c {
            Comparison::Gt => "above",
            Comparison::Ge => "at or above",
            Comparison::Lt => "below",
            Comparison::Le => "at or below",
        });
    let verb = if open { "is" } else { "peaked at" };
    match threshold[severity].as_f64() {
        Some(l) => format!(
            "{verb} {shown}, {side} the {severity} limit of {}",
            limit(l)
        ),
        None => format!("{verb} {shown}"),
    }
}

/// "Average over 15 minutes is 6.2, above the warning limit of 4".
fn window(
    metric: &str,
    severity: &str,
    open: bool,
    value: f64,
    threshold: &Value,
) -> Option<String> {
    let function = threshold["function"].as_str()?;
    let over = function_over(function, threshold["window_s"].as_u64()?);
    let format = |v: f64| match function {
        "rate" => format!("{} per second", format_value(metric, v)),
        _ => format_value(metric, v),
    };
    let rest = against_limit(&format(value), severity, open, threshold, format);
    Some(format!("{over} {rest}"))
}

/// "2.5 times last week's, above the warning limit of 2 times".
fn compare(
    metric: &str,
    severity: &str,
    open: bool,
    value: f64,
    threshold: &Value,
) -> Option<String> {
    let when = match threshold["offset_s"].as_u64()? {
        86_400 => "yesterday",
        _ => "last week",
    };
    let over = function_over("avg", threshold["window_s"].as_u64()?);
    Some(match threshold["function"].as_str()? {
        "ratio" => {
            let times = |v: f64| format!("{} times {when}'s", number(v, 2));
            format!(
                "{over} {}",
                against_limit(&times(value), severity, open, threshold, times)
            )
        }
        _ => {
            let diff = |v: f64| {
                let sign = if v > 0.0 { "+" } else { "" };
                format!("{sign}{} on {when}", format_value(metric, v))
            };
            format!(
                "{over} {}",
                against_limit(&diff(value), severity, open, threshold, diff)
            )
        }
    })
}

/// The sentence for a sensor whose last sample is `age_s` seconds old,
/// or that sent none in the window looked at (`None`).
pub fn no_data(age_s: Option<i64>) -> String {
    match age_s {
        None => "No data yet".into(),
        Some(s) => format!("No data for {}", span(s)),
    }
}

/// The sentence for a sensor that sent nothing in the last `window_s`
/// seconds.
pub fn no_data_within(window_s: i64) -> String {
    let s = window_s.max(1) as u64;
    let window = match s {
        s if s % 86_400 == 0 => plural(s / 86_400, "day"),
        s if s % 3_600 == 0 => plural(s / 3_600, "hour"),
        s => span(s as i64),
    };
    format!("No data for over {window}")
}

/// "95 seconds", "5 minutes", "3 hours", "3 days": rounded down.
fn span(s: i64) -> String {
    let s = s.max(1) as u64;
    match s {
        s if s < 120 => plural(s, "second"),
        s if s < 2 * 3_600 => plural(s / 60, "minute"),
        s if s < 2 * 86_400 => plural(s / 3_600, "hour"),
        s => plural(s / 86_400, "day"),
    }
}

/// Words for a 0 or 1 state metric: what `value` is, and what it should be.
fn state_words(short: &str, value: f64) -> Option<(&'static str, &'static str)> {
    let (yes, no) = if short.ends_with("_active") {
        ("active", "inactive")
    } else if short.ends_with("_up") {
        ("open", "closed")
    } else if short.ends_with("_loaded") {
        ("loaded", "not loaded")
    } else if short.ends_with("_installed") {
        ("installed", "not installed")
    } else {
        return None;
    };
    Some(if value >= 1.0 { (yes, no) } else { (no, yes) })
}

/// A script's Nagios status (0 OK, 1 warning, 2 critical, else unknown).
fn script_status(value: f64, open: bool) -> String {
    let word = match value as i64 {
        0 => "OK",
        1 => "warning",
        2 => "critical",
        _ => "unknown",
    };
    if open {
        format!("The script reports {word}")
    } else {
        format!("The script reported {word}")
    }
}

/// "9%" becomes "9% available" for metrics whose name says what the
/// share is of.
fn with_noun(short: &str, value: &str) -> String {
    let noun = ["available", "free", "used", "busy"]
        .into_iter()
        .find(|word| short.contains(&format!("_{word}_")))
        .or_else(|| short.contains("_usage_").then_some("used"));
    match noun {
        Some(noun) if short.ends_with("_ratio") => format!("{value} {noun}"),
        _ => value.to_owned(),
    }
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |c| {
        c.to_uppercase().chain(chars).collect::<String>()
    })
}

fn plural(n: u64, unit: &str) -> String {
    if n == 1 {
        format!("1 {unit}")
    } else {
        format!("{n} {unit}s")
    }
}

/// `value` of `metric` in its unit, from the name's last word (ADR-0015).
pub fn format_value(metric: &str, value: f64) -> String {
    if metric.ends_with("_ratio") {
        format!("{}%", number(value * 100.0, 1))
    } else if metric.ends_with("_bytes_per_second") {
        format!("{}/s", bytes(value))
    } else if metric.ends_with("_bytes") {
        bytes(value)
    } else if metric.ends_with("_per_second") {
        format!("{}/s", number(value, if value < 10.0 { 1 } else { 0 }))
    } else if metric.ends_with("_seconds") {
        seconds(value)
    } else {
        number(value, 2)
    }
}

fn bytes(value: f64) -> String {
    const UNITS: [&str; 7] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    let exponent = if value.abs() < 1.0 {
        0
    } else {
        (value.abs().log(1024.0).floor() as usize).min(UNITS.len() - 1)
    };
    let scaled = value / 1024f64.powi(exponent as i32);
    let decimals = if exponent == 0 { 0 } else { 1 };
    format!("{} {}", number(scaled, decimals), UNITS[exponent])
}

fn seconds(value: f64) -> String {
    for (unit, size) in [("d", 86_400.0), ("h", 3_600.0), ("min", 60.0)] {
        if value.abs() >= size {
            return format!("{} {unit}", number(value / size, 1));
        }
    }
    if value.abs() >= 1.0 || value == 0.0 {
        format!("{} s", number(value, 1))
    } else {
        format!("{} ms", number(value * 1000.0, 1))
    }
}

/// At most `decimals` decimals, no trailing zeros, thousands separated by
/// commas, like the web interface's `toLocaleString("en")`.
fn number(value: f64, decimals: usize) -> String {
    // Half away from zero, as the browser rounds, not half to even.
    let scale = 10f64.powi(decimals as i32);
    let text = format!("{:.*}", decimals, (value * scale).round() / scale);
    let (int, frac) = text.split_once('.').unwrap_or((&text, ""));
    let frac = frac.trim_end_matches('0');
    let (sign, digits) = int.strip_prefix('-').map_or(("", int), |d| ("-", d));
    let mut grouped = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }
    // "-0" after rounding reads as "0".
    let sign = if grouped.chars().all(|c| c == '0') && frac.is_empty() {
        ""
    } else {
        sign
    };
    if frac.is_empty() {
        format!("{sign}{grouped}")
    } else {
        format!("{sign}{grouped}.{frac}")
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn rule(comparison: &str, warning: f64, critical: Option<f64>) -> Value {
        json!({"comparison": comparison, "warning": warning, "critical": critical})
    }

    #[test]
    fn a_crossed_threshold_names_the_value_the_side_and_the_limit() {
        let low = rule("<", 0.12, Some(0.05));
        assert_eq!(
            incident(
                "monitorflow_memory_available_ratio",
                "warning",
                true,
                0.09,
                &low
            ),
            "9% available is below the warning limit of 12%"
        );
        let high = rule(">", 0.85, Some(0.95));
        assert_eq!(
            incident(
                "monitorflow_disk_used_ratio",
                "critical",
                true,
                0.9712,
                &high
            ),
            "97.1% used is above the critical limit of 95%"
        );
        assert_eq!(
            incident(
                "monitorflow_disk_used_ratio",
                "critical",
                false,
                0.99,
                &high
            ),
            "Peaked at 99% used, above the critical limit of 95%"
        );
        assert_eq!(
            incident(
                "monitorflow_cpu_usage_ratio",
                "warning",
                true,
                0.93,
                &rule(">=", 0.9, None)
            ),
            "93% used is at or above the warning limit of 90%"
        );
        assert_eq!(
            incident(
                "monitorflow_diskio_wait_seconds",
                "warning",
                true,
                0.35,
                &rule(">", 0.2, None)
            ),
            "350 ms is above the warning limit of 200 ms"
        );
        assert_eq!(
            incident(
                "monitorflow_cpu_load5",
                "warning",
                true,
                12.5,
                &rule(">", 8.0, None)
            ),
            "12.5 is above the warning limit of 8"
        );
    }

    #[test]
    fn states_read_in_words() {
        let down = rule("<", 1.0, Some(1.0));
        assert_eq!(
            incident("monitorflow_nginx_active", "critical", true, 0.0, &down),
            "Inactive, expected active"
        );
        assert_eq!(
            incident("monitorflow_port_https_up", "critical", true, 0.0, &down),
            "Closed, expected open"
        );
        assert_eq!(
            incident("monitorflow_port_https_up", "critical", false, 0.0, &down),
            "Was closed, expected open"
        );
        assert_eq!(
            incident(
                "monitorflow_script_backup_status",
                "critical",
                true,
                2.0,
                &rule(">=", 1.0, Some(2.0))
            ),
            "The script reports critical"
        );
    }

    #[test]
    fn offline_says_how_long_without_contact() {
        let offline = json!({"metric": "offline", "after_s": 300});
        assert_eq!(
            incident("offline", "critical", true, 0.0, &offline),
            "No contact for over 5 minutes"
        );
    }

    #[test]
    fn a_threshold_missing_its_limit_still_reads() {
        assert_eq!(
            incident("monitorflow_x_ratio", "critical", true, 0.5, &json!({})),
            "50% past the critical limit"
        );
    }

    #[test]
    fn no_data_counts_in_the_largest_unit() {
        assert_eq!(no_data(None), "No data yet");
        assert_eq!(no_data(Some(0)), "No data for 1 second");
        assert_eq!(no_data(Some(95)), "No data for 95 seconds");
        assert_eq!(no_data(Some(300)), "No data for 5 minutes");
        assert_eq!(no_data(Some(3 * 3_600 + 5)), "No data for 3 hours");
        assert_eq!(no_data(Some(3 * 86_400)), "No data for 3 days");
        assert_eq!(no_data_within(3_600), "No data for over 1 hour");
        assert_eq!(no_data_within(2 * 86_400), "No data for over 2 days");
    }

    #[test]
    fn values_format_like_the_web_interface() {
        assert_eq!(format_value("m_ratio", 0.853), "85.3%");
        assert_eq!(format_value("m_ratio", 0.12), "12%");
        assert_eq!(format_value("m_bytes", 1536.0), "1.5 KiB");
        assert_eq!(format_value("m_bytes", 512.0), "512 B");
        assert_eq!(
            format_value("m_bytes_per_second", 2.0 * 1024.0 * 1024.0),
            "2 MiB/s"
        );
        assert_eq!(format_value("m_per_second", 4.25), "4.3/s");
        assert_eq!(format_value("m_per_second", 1234.0), "1,234/s");
        assert_eq!(format_value("m_seconds", 90.0), "1.5 min");
        assert_eq!(format_value("m_seconds", 0.0), "0 s");
        assert_eq!(format_value("m_matches", 1_234_567.891), "1,234,567.89");
        assert_eq!(format_value("m_value", -0.001), "0");
        assert_eq!(format_value("m_value", -42.0), "-42");
    }

    #[test]
    fn richer_rules_read_in_plain_words() {
        let disk = "monitorflow_disk_used_ratio";
        let forecast = json!({"kind": "forecast", "comparison": ">=", "target": 1.0,
                              "warning": 259200.0, "critical": 86400.0});
        assert_eq!(
            incident(disk, "warning", true, 2.5 * 86_400.0, &forecast),
            "Expected to reach 100% in about 2 days"
        );
        assert_eq!(
            incident(disk, "critical", true, 0.0, &forecast),
            "Has reached 100%"
        );
        assert_eq!(
            incident(disk, "critical", false, 5_400.0, &forecast),
            "Was expected to reach 100% within 90 minutes"
        );
        let nodata = json!({"kind": "nodata", "comparison": ">", "warning": 300.0});
        assert_eq!(
            incident(disk, "warning", true, 720.0, &nodata),
            "No data for 12 minutes"
        );
        assert_eq!(
            incident(disk, "warning", false, 900.0, &nodata),
            "Was silent for 15 minutes"
        );
        let window = json!({"kind": "window", "function": "avg", "window_s": 900,
                            "comparison": ">", "warning": 4.0});
        assert_eq!(
            incident("monitorflow_cpu_load1", "warning", true, 6.2, &window),
            "Average over 15 minutes is 6.2, above the warning limit of 4"
        );
        let rate = json!({"kind": "window", "function": "rate", "window_s": 300,
                          "comparison": ">", "critical": 1048576.0});
        assert_eq!(
            incident("monitorflow_disk_written_bytes", "critical", false, 2097152.0, &rate),
            "Rate over 5 minutes peaked at 2 MiB per second, above the critical limit of 1 MiB per second"
        );
        let ratio = json!({"kind": "compare", "function": "ratio", "window_s": 3600,
                           "offset_s": 604800, "comparison": ">", "warning": 2.0,
                           "conditions": [{"metric": "m"}]});
        assert_eq!(
            incident("monitorflow_cpu_load1", "warning", true, 2.5, &ratio),
            "Average over 60 minutes is 2.5 times last week's, above the warning limit of 2 times last week's, while 1 more condition holds"
        );
        let difference = json!({"kind": "compare", "function": "difference", "window_s": 86400,
                                "offset_s": 86400, "comparison": ">", "warning": 0.1});
        assert_eq!(
            incident(disk, "warning", true, 0.15, &difference),
            "Average over 24 hours is +15% on yesterday, above the warning limit of +10% on yesterday"
        );
    }
}
