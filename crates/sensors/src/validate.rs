//! Checks on what sensors produce. Sensor names, label names and values
//! end up as VictoriaMetrics label names and values on the server, and a
//! broken or hostile sensor (a custom script) must not be able to inject
//! odd names, huge series or non-numbers.

use std::time::Duration;

use crate::Sample;

/// Longest sensor or label name, in bytes.
pub const MAX_NAME_LEN: usize = 64;
/// Longest instance or label value, in bytes.
pub const MAX_VALUE_LEN: usize = 256;
/// Most labels on one sample.
pub const MAX_LABELS: usize = 16;
/// Label names the server sets itself. Names starting with `__` (reserved
/// by Prometheus) or `mf_` (reserved for the server) are refused too.
pub const RESERVED_LABELS: [&str; 2] = ["sensor", "instance"];

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ValidationError {
    #[error("sensor name `{0}` must match [a-z][a-z0-9_]* and be at most {MAX_NAME_LEN} bytes")]
    SensorName(String),
    #[error("label name `{0}` must match [a-zA-Z_][a-zA-Z0-9_]*, be at most {MAX_NAME_LEN} bytes and not be reserved")]
    LabelName(String),
    #[error("{what} must be at most {MAX_VALUE_LEN} bytes without control characters")]
    Value { what: &'static str },
    #[error("{0} labels, at most {MAX_LABELS} allowed")]
    TooManyLabels(usize),
    #[error("value {0} is not finite")]
    NotFinite(f64),
    #[error("timestamp {0} is not after 1970")]
    Timestamp(i64),
    #[error("interval {0:?} must be between 1 s and 24 h")]
    Interval(Duration),
    #[error("timeout {0:?} must be positive and not longer than the interval")]
    Timeout(Duration),
    #[error("sensor `{name}` instance `{instance}` is configured twice")]
    Duplicate { name: String, instance: String },
}

/// Lowercase ASCII letter, then lowercase letters, digits and `_`.
pub fn sensor_name(name: &str) -> Result<(), ValidationError> {
    let mut chars = name.chars();
    let ok = name.len() <= MAX_NAME_LEN
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    ok.then_some(())
        .ok_or_else(|| ValidationError::SensorName(truncate(name)))
}

/// A Prometheus label name that is not reserved.
pub fn label_name(name: &str) -> Result<(), ValidationError> {
    let mut chars = name.chars();
    let ok = name.len() <= MAX_NAME_LEN
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !name.starts_with("__")
        && !name.starts_with("mf_")
        && !RESERVED_LABELS.contains(&name);
    ok.then_some(())
        .ok_or_else(|| ValidationError::LabelName(truncate(name)))
}

/// An instance, which may be empty.
pub fn instance(value: &str) -> Result<(), ValidationError> {
    text_value(value, "instance")
}

fn text_value(value: &str, what: &'static str) -> Result<(), ValidationError> {
    let ok = value.len() <= MAX_VALUE_LEN && !value.chars().any(char::is_control);
    ok.then_some(()).ok_or(ValidationError::Value { what })
}

/// Everything about a sample except its sensor name, which the scheduler
/// sets from a validated configuration.
pub fn sample(sample: &Sample) -> Result<(), ValidationError> {
    if !sample.value.is_finite() {
        return Err(ValidationError::NotFinite(sample.value));
    }
    if sample.timestamp_unix_ms <= 0 {
        return Err(ValidationError::Timestamp(sample.timestamp_unix_ms));
    }
    instance(&sample.instance)?;
    if sample.labels.len() > MAX_LABELS {
        return Err(ValidationError::TooManyLabels(sample.labels.len()));
    }
    for (name, value) in &sample.labels {
        label_name(name)?;
        text_value(value, "label value")?;
    }
    Ok(())
}

/// Names in errors are logged; keep a hostile one short.
fn truncate(name: &str) -> String {
    let end = (0..=MAX_NAME_LEN.min(name.len()))
        .rev()
        .find(|&i| name.is_char_boundary(i))
        .unwrap_or(0);
    let mut out = name[..end].escape_debug().to_string();
    if end < name.len() {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensor_names() {
        for ok in ["cpu", "disk_io", "a1", &"x".repeat(MAX_NAME_LEN)] {
            assert_eq!(sensor_name(ok), Ok(()), "{ok}");
        }
        for bad in [
            "",
            "Cpu",
            "1cpu",
            "_cpu",
            "cpu-load",
            "cpu.load",
            "cpü",
            &"x".repeat(MAX_NAME_LEN + 1),
        ] {
            assert!(sensor_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn label_names() {
        for ok in ["fs", "Mount_Point", "_x", "core2"] {
            assert_eq!(label_name(ok), Ok(()), "{ok}");
        }
        for bad in [
            "", "2x", "a-b", "a b", "__name__", "__x", "mf_peer", "sensor", "instance", "é",
        ] {
            assert!(label_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn samples() {
        let good = Sample::now(1.0)
            .with_instance("/var/log")
            .with_label("fs", "ext4 (rw)");
        assert_eq!(sample(&good), Ok(()));

        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(matches!(
                sample(&Sample::now(value)),
                Err(ValidationError::NotFinite(_))
            ));
        }
        let mut old = Sample::now(1.0);
        old.timestamp_unix_ms = 0;
        assert_eq!(sample(&old), Err(ValidationError::Timestamp(0)));

        let long = Sample::now(1.0).with_instance("x".repeat(MAX_VALUE_LEN + 1));
        assert_eq!(
            sample(&long),
            Err(ValidationError::Value { what: "instance" })
        );
        let newline = Sample::now(1.0).with_label("a", "b\nc");
        assert_eq!(
            sample(&newline),
            Err(ValidationError::Value {
                what: "label value"
            })
        );

        let many =
            (0..=MAX_LABELS).fold(Sample::now(1.0), |s, i| s.with_label(format!("l{i}"), "v"));
        assert_eq!(
            sample(&many),
            Err(ValidationError::TooManyLabels(MAX_LABELS + 1))
        );
        let reserved = Sample::now(1.0).with_label("sensor", "x");
        assert!(matches!(
            sample(&reserved),
            Err(ValidationError::LabelName(_))
        ));
    }

    #[test]
    fn hostile_names_are_truncated_in_errors() {
        let name = format!("{}\u{1b}[31m", "é".repeat(100));
        let ValidationError::LabelName(shown) = label_name(&name).unwrap_err() else {
            panic!("wrong error");
        };
        assert!(shown.ends_with('…') && shown.len() <= MAX_NAME_LEN + '…'.len_utf8());
        assert!(!truncate("a\u{1b}b").contains('\u{1b}'));
    }
}
