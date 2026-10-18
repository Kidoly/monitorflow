//! Turns the samples of an authenticated peer into series for
//! VictoriaMetrics (ADR-0013): checks names, values, timestamps and sizes,
//! and imposes the identity labels.
//!
//! The identity labels come from the server, never from the sample: `peer`
//! is the agent the stream speaks for and `site` its inventory row's site,
//! so a peer cannot write series under another peer or site.

use std::time::Duration;

use monitorflow_common::proto::v1::{Sample, SampleBatch};

/// Prefix of every metric name written by the server.
pub const METRIC_PREFIX: &str = "monitorflow_";
/// Samples in one `SampleBatch`; a larger batch is dropped whole.
pub const MAX_SAMPLES_PER_BATCH: usize = 10_000;
/// Labels a sample may carry in its `labels` map.
pub const MAX_LABELS: usize = 16;
/// Bytes in a sensor or label name.
pub const MAX_NAME_LEN: usize = 64;
/// Bytes in a label value, `instance` included. Control characters are
/// refused.
pub const MAX_VALUE_LEN: usize = 256;
/// How far ahead of the server clock a timestamp may be.
pub const MAX_FUTURE: Duration = Duration::from_secs(10 * 60);
/// How old a timestamp may be, to accept backlogs sent after a reconnect.
pub const MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Label names the server sets or keeps for itself; the first two are
/// already reserved by the agent (ADR-0012). Names starting with `__` or
/// `mf_` are reserved as well.
pub const RESERVED_LABELS: [&str; 5] = ["sensor", "instance", "peer", "site", "job"];

/// Who a stream speaks for, as known by the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// The agent ID, or the relay's own ID on its own stream.
    pub peer: String,
    /// Name of the peer's site; empty when no inventory is configured.
    pub site: String,
}

/// One sample of one series, ready for remote write.
#[derive(Debug, Clone, PartialEq)]
pub struct Series {
    /// Sorted by name, `__name__` included, no empty values.
    pub labels: Vec<(String, String)>,
    pub timestamp_ms: i64,
    pub value: f64,
}

/// Why a sample was dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Invalid {
    #[error("sensor name must be 1 to {MAX_NAME_LEN} of [a-z0-9_], starting with a letter")]
    SensorName,
    #[error("value is not finite")]
    NotFinite,
    #[error("timestamp is more than {MAX_FUTURE:?} ahead of the server clock")]
    InFuture,
    #[error("timestamp is older than {MAX_AGE:?}")]
    TooOld,
    #[error("more than {MAX_LABELS} labels")]
    TooManyLabels,
    #[error("label name must be 1 to {MAX_NAME_LEN} of [a-zA-Z0-9_], not starting with a digit")]
    LabelName,
    #[error("label name is reserved for the server")]
    ReservedLabel,
    #[error(
        "label value or instance is longer than {MAX_VALUE_LEN} bytes or has control characters"
    )]
    LabelValue,
    #[error("batch has more than {MAX_SAMPLES_PER_BATCH} samples")]
    BatchTooLarge,
}

/// `monitorflow_<sensor>`, for sensor names of ADR-0012
/// (`[a-z][a-z0-9_]*`): `disk_free_bytes` becomes
/// `monitorflow_disk_free_bytes`. The sensor picks its own unit suffix
/// (`_bytes`, `_ratio`, `_total`).
pub fn metric_name(sensor: &str) -> Result<String, Invalid> {
    let mut chars = sensor.chars();
    let valid = sensor.len() <= MAX_NAME_LEN
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !valid {
        return Err(Invalid::SensorName);
    }
    Ok(format!("{METRIC_PREFIX}{sensor}"))
}

/// Checks an agent-supplied label name: Prometheus charset, not reserved.
pub fn check_label_name(name: &str) -> Result<(), Invalid> {
    let mut chars = name.chars();
    let valid = name.len() <= MAX_NAME_LEN
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !valid {
        return Err(Invalid::LabelName);
    }
    if name.starts_with("__") || name.starts_with("mf_") || RESERVED_LABELS.contains(&name) {
        return Err(Invalid::ReservedLabel);
    }
    Ok(())
}

fn check_label_value(value: &str) -> Result<(), Invalid> {
    if value.len() > MAX_VALUE_LEN || value.chars().any(char::is_control) {
        return Err(Invalid::LabelValue);
    }
    Ok(())
}

/// Validates `sample` and builds its series with the identity labels.
pub fn series(identity: &Identity, sample: Sample, now_ms: i64) -> Result<Series, Invalid> {
    let name = metric_name(&sample.sensor)?;
    if !sample.value.is_finite() {
        return Err(Invalid::NotFinite);
    }
    let ts = sample.timestamp_unix_ms;
    if ts > now_ms.saturating_add(MAX_FUTURE.as_millis() as i64) {
        return Err(Invalid::InFuture);
    }
    if ts < now_ms.saturating_sub(MAX_AGE.as_millis() as i64) {
        return Err(Invalid::TooOld);
    }
    if sample.labels.len() > MAX_LABELS {
        return Err(Invalid::TooManyLabels);
    }
    let mut labels = Vec::with_capacity(sample.labels.len() + 4);
    for (key, value) in sample.labels {
        check_label_name(&key)?;
        check_label_value(&value)?;
        labels.push((key, value));
    }
    check_label_value(&sample.instance)?;
    labels.push(("__name__".into(), name));
    labels.push(("peer".into(), identity.peer.clone()));
    labels.push(("site".into(), identity.site.clone()));
    labels.push(("instance".into(), sample.instance));
    // Prometheus treats an empty value as an absent label.
    labels.retain(|(_, value)| !value.is_empty());
    labels.sort_unstable();
    Ok(Series {
        labels,
        timestamp_ms: ts,
        value: sample.value,
    })
}

/// The valid series of a batch and what was dropped.
#[derive(Debug, Default, PartialEq)]
pub struct Checked {
    pub series: Vec<Series>,
    pub invalid: u64,
    /// The first reason a sample was dropped, for a log line.
    pub first_error: Option<Invalid>,
}

/// Validates every sample of `batch`; invalid ones are dropped and counted.
pub fn check_batch(identity: &Identity, batch: SampleBatch, now_ms: i64) -> Checked {
    let mut checked = Checked::default();
    if batch.samples.len() > MAX_SAMPLES_PER_BATCH {
        checked.invalid = batch.samples.len() as u64;
        checked.first_error = Some(Invalid::BatchTooLarge);
        return checked;
    }
    checked.series.reserve(batch.samples.len());
    for sample in batch.samples {
        match series(identity, sample, now_ms) {
            Ok(series) => checked.series.push(series),
            Err(error) => {
                checked.invalid += 1;
                checked.first_error.get_or_insert(error);
            }
        }
    }
    checked
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000_000;

    fn identity() -> Identity {
        Identity {
            peer: "web-01".into(),
            site: "paris".into(),
        }
    }

    fn sample(sensor: &str) -> Sample {
        Sample {
            sensor: sensor.into(),
            instance: "/var".into(),
            timestamp_unix_ms: NOW - 1_000,
            value: 0.25,
            labels: Default::default(),
        }
    }

    fn labels(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn metric_names_get_the_prefix() {
        assert_eq!(metric_name("cpu").unwrap(), "monitorflow_cpu");
        assert_eq!(
            metric_name("disk_free_bytes").unwrap(),
            "monitorflow_disk_free_bytes"
        );
        let long = "a".repeat(MAX_NAME_LEN + 1);
        for bad in [
            "",
            "Cpu",
            "1cpu",
            "_cpu",
            "cpu-usage",
            "cpu.load",
            "cpu:rate",
            "cpü",
            &long,
        ] {
            assert_eq!(metric_name(bad), Err(Invalid::SensorName), "{bad}");
        }
    }

    #[test]
    fn identity_labels_are_imposed_and_sorted() {
        let mut s = sample("disk_free_bytes");
        s.labels.insert("device".into(), "sda1".into());
        s.labels.insert("fs".into(), String::new());
        let series = series(&identity(), s, NOW).unwrap();
        assert_eq!(
            series.labels,
            labels(&[
                ("__name__", "monitorflow_disk_free_bytes"),
                ("device", "sda1"),
                ("instance", "/var"),
                ("peer", "web-01"),
                ("site", "paris"),
            ])
        );
        assert_eq!((series.timestamp_ms, series.value), (NOW - 1_000, 0.25));
    }

    #[test]
    fn empty_instance_and_site_are_omitted() {
        let mut s = sample("cpu");
        s.instance.clear();
        let identity = Identity {
            site: String::new(),
            ..identity()
        };
        let series = series(&identity, s, NOW).unwrap();
        assert_eq!(
            series.labels,
            labels(&[("__name__", "monitorflow_cpu"), ("peer", "web-01")])
        );
    }

    #[test]
    fn reserved_or_malformed_labels_drop_the_sample() {
        for (name, error) in [
            ("peer", Invalid::ReservedLabel),
            ("site", Invalid::ReservedLabel),
            ("instance", Invalid::ReservedLabel),
            ("job", Invalid::ReservedLabel),
            ("__name__", Invalid::ReservedLabel),
            ("__peer", Invalid::ReservedLabel),
            ("sensor", Invalid::ReservedLabel),
            ("mf_peer", Invalid::ReservedLabel),
            ("", Invalid::LabelName),
            ("0core", Invalid::LabelName),
            ("dev-name", Invalid::LabelName),
            ("a.b", Invalid::LabelName),
        ] {
            let mut s = sample("cpu");
            s.labels.insert(name.into(), "other".into());
            assert_eq!(series(&identity(), s, NOW), Err(error), "{name}");
        }
        assert!(check_label_name("core_0").is_ok());
        assert!(check_label_name("_x").is_ok());
    }

    #[test]
    fn label_counts_and_sizes_are_bounded() {
        let mut s = sample("cpu");
        for i in 0..=MAX_LABELS {
            s.labels.insert(format!("l{i}"), "v".into());
        }
        assert_eq!(series(&identity(), s, NOW), Err(Invalid::TooManyLabels));
        let mut s = sample("cpu");
        s.labels.insert("l".into(), "v".repeat(MAX_VALUE_LEN + 1));
        assert_eq!(series(&identity(), s, NOW), Err(Invalid::LabelValue));
        let mut s = sample("cpu");
        s.instance = "i".repeat(MAX_VALUE_LEN + 1);
        assert_eq!(series(&identity(), s, NOW), Err(Invalid::LabelValue));
        let mut s = sample("cpu");
        s.labels.insert("l".into(), "a\nb".into());
        assert_eq!(series(&identity(), s, NOW), Err(Invalid::LabelValue));
    }

    #[test]
    fn values_must_be_finite_and_timestamps_in_the_window() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let s = Sample {
                value,
                ..sample("cpu")
            };
            assert_eq!(series(&identity(), s, NOW), Err(Invalid::NotFinite));
        }
        let at = |ts: i64| Sample {
            timestamp_unix_ms: ts,
            ..sample("cpu")
        };
        let future = MAX_FUTURE.as_millis() as i64;
        let age = MAX_AGE.as_millis() as i64;
        assert!(series(&identity(), at(NOW + future), NOW).is_ok());
        assert!(series(&identity(), at(NOW - age), NOW).is_ok());
        assert_eq!(
            series(&identity(), at(NOW + future + 1), NOW),
            Err(Invalid::InFuture)
        );
        assert_eq!(
            series(&identity(), at(NOW - age - 1), NOW),
            Err(Invalid::TooOld)
        );
        assert_eq!(series(&identity(), at(0), NOW), Err(Invalid::TooOld));
        // The window arithmetic saturates instead of overflowing.
        assert!(series(&identity(), at(i64::MAX), i64::MAX).is_ok());
        assert!(series(&identity(), at(i64::MIN), i64::MIN).is_ok());
    }

    #[test]
    fn batches_keep_valid_samples_and_count_the_rest() {
        let batch = SampleBatch {
            samples: vec![sample("cpu"), sample("Bad"), sample("memory"), sample("")],
        };
        let checked = check_batch(&identity(), batch, NOW);
        assert_eq!(checked.series.len(), 2);
        assert_eq!(checked.invalid, 2);
        assert_eq!(checked.first_error, Some(Invalid::SensorName));

        let batch = SampleBatch {
            samples: vec![sample("cpu"); MAX_SAMPLES_PER_BATCH + 1],
        };
        let checked = check_batch(&identity(), batch, NOW);
        assert!(checked.series.is_empty());
        assert_eq!(checked.invalid, MAX_SAMPLES_PER_BATCH as u64 + 1);
        assert_eq!(checked.first_error, Some(Invalid::BatchTooLarge));
    }
}
