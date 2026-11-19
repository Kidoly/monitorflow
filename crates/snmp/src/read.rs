//! `snmp_read`: the one-off test behind "Test" and "Browse" (ADR-0157).
//! Never a check that repeats.
//!
//! Parameters, besides the device: `oids`, up to [`MAX_READ_OIDS`] numeric
//! OIDs separated by commas, read with one GET; or `walk`, an OID whose
//! subtree is walked, with `max` objects at most ([`DEFAULT_WALK`] by
//! default, [`MAX_WALK`] at most).
//!
//! Each object answered is one sample `object` whose instance is its OID,
//! with its value as a number when it is one (0 otherwise) and the labels
//! `type` (`Integer`, `OctetString`, `Counter64`, `noSuchObject`...) and
//! `value` (the value as text, at most [`MAX_VALUE`] characters). A walk
//! that stopped at `max` adds a sample `cut` of 1.

use std::collections::BTreeMap;
use std::sync::Arc;

use monitorflow_sensors::{Sample, Sensor, SensorError};
use tokio::time::{timeout_at, Instant};

use crate::custom::{display, label_text, number, oid_text, parse_oid, reading, type_name};
use crate::session::Session;
use crate::CREDENTIAL_PARAMS;

/// The kind of these tests.
pub const READ_KIND: &str = "snmp_read";
/// Most OIDs of one read.
pub const MAX_READ_OIDS: usize = 20;
/// Objects a walk lists unless told.
pub const DEFAULT_WALK: usize = 500;
/// Most objects a walk lists.
pub const MAX_WALK: usize = 2000;
/// Longest value shown.
pub const MAX_VALUE: usize = 256;

/// What to read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Read {
    Get(Vec<Vec<u32>>),
    Walk { root: Vec<u32>, max: usize },
}

/// Reads and checks what to read, apart from the device.
pub fn parse_read(params: &BTreeMap<String, String>) -> Result<Read, String> {
    if let Some(unknown) = params.keys().find(|k| {
        !["host", "port", "oids", "walk", "max"].contains(&k.as_str())
            && !CREDENTIAL_PARAMS.contains(&k.as_str())
    }) {
        return Err(format!("unknown parameter `{}`", unknown.escape_debug()));
    }
    let get = |name: &str| params.get(name).map(|v| v.trim()).filter(|v| !v.is_empty());
    match (get("oids"), get("walk")) {
        (Some(oids), None) => {
            if params.contains_key("max") {
                return Err("`max` is for a walk".into());
            }
            let list: Vec<Vec<u32>> = oids
                .split(',')
                .map(|o| parse_oid(o).ok_or("`oids` must be numeric OIDs separated by commas"))
                .collect::<Result<_, _>>()?;
            if list.len() > MAX_READ_OIDS {
                return Err(format!("read at most {MAX_READ_OIDS} OIDs at once"));
            }
            Ok(Read::Get(list))
        }
        (None, Some(walk)) => {
            let root = parse_oid(walk).ok_or("`walk` must be a numeric OID")?;
            let max = match get("max") {
                None => DEFAULT_WALK,
                Some(max) => max
                    .parse::<usize>()
                    .ok()
                    .filter(|m| (1..=MAX_WALK).contains(m))
                    .ok_or_else(|| format!("`max` must be between 1 and {MAX_WALK}"))?,
            };
            Ok(Read::Walk { root, max })
        }
        _ => Err("set either `oids` or `walk`".into()),
    }
}

/// The sample of one object.
fn object(oid: &[u32], value: &async_snmp::Value) -> Sample {
    let read = reading(value);
    let shown = read.as_ref().map(display).unwrap_or_default();
    let numeric = match &read {
        Some(crate::mibs::Val::Text(_)) | None => None,
        Some(v) => number(v),
    };
    let mut sample = Sample::metric("object", numeric.unwrap_or(0.0))
        .with_label("type", type_name(value))
        .with_label("value", label_text(&shown, MAX_VALUE));
    sample.instance = oid_text(oid);
    sample
}

pub struct ReadSensor {
    session: Session,
    read: Read,
}

impl std::fmt::Debug for ReadSensor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadSensor")
            .field("session", &self.session)
            .field("read", &self.read)
            .finish()
    }
}

#[async_trait::async_trait]
impl Sensor for ReadSensor {
    async fn collect(&self, deadline: Instant) -> Result<Vec<Sample>, SensorError> {
        let client = self.session.client(deadline).await?;
        let work = async {
            match &self.read {
                Read::Get(oids) => {
                    let values = self.session.get_raw(&client, oids).await?;
                    Ok(oids
                        .iter()
                        .map(|oid| match values.get(oid) {
                            Some(value) => object(oid, value),
                            None => object(oid, &async_snmp::Value::NoSuchObject),
                        })
                        .collect())
                }
                Read::Walk { root, max } => {
                    let (rows, cut) = self.session.walk_raw(&client, root, *max).await?;
                    let mut samples: Vec<Sample> =
                        rows.iter().map(|(oid, value)| object(oid, value)).collect();
                    if cut {
                        samples.push(Sample::metric("cut", 1.0));
                    }
                    Ok(samples)
                }
            }
        };
        match timeout_at(deadline, work).await {
            Ok(result) => result,
            Err(_) => Err(SensorError::Transient(
                "the device did not answer in time".into(),
            )),
        }
    }
}

/// Builds the sensor of a `snmp_read` test.
pub fn build_read(params: &BTreeMap<String, String>) -> Result<Arc<dyn Sensor>, String> {
    let device = crate::device(params)?;
    let read = parse_read(params)?;
    Ok(Arc::new(ReadSensor {
        session: Session::new(device.host, device.port, device.credentials),
        read,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn reads_and_walks_are_bounded() {
        assert_eq!(
            parse_read(&params(&[("oids", "1.3.6.1.2.1.1.3.0, 1.3.6.1.2.1.1.5.0")])),
            Ok(Read::Get(vec![
                vec![1, 3, 6, 1, 2, 1, 1, 3, 0],
                vec![1, 3, 6, 1, 2, 1, 1, 5, 0]
            ]))
        );
        assert_eq!(
            parse_read(&params(&[("walk", "1.3.6.1.4.1")])),
            Ok(Read::Walk {
                root: vec![1, 3, 6, 1, 4, 1],
                max: DEFAULT_WALK
            })
        );
        let err = |pairs: &[(&str, &str)]| parse_read(&params(pairs)).unwrap_err();
        assert!(err(&[]).contains("either"));
        assert!(err(&[("oids", "1.3"), ("walk", "1.3")]).contains("either"));
        assert!(err(&[("oids", "sysName")]).contains("numeric"));
        assert!(err(&[("oids", &vec!["1.3.6"; 21].join(","))]).contains("at most 20"));
        assert!(err(&[("walk", "1.3"), ("max", "2001")]).contains("between"));
        assert!(err(&[("walk", "1.3"), ("max", "0")]).contains("between"));
        assert!(err(&[("oids", "1.3"), ("max", "5")]).contains("for a walk"));
        assert!(err(&[("oids", "1.3"), ("oid1", "1.3")]).contains("unknown"));
    }

    #[test]
    fn objects_carry_their_type_and_value() {
        let s = object(
            &[1, 3, 6, 1, 2, 1, 1, 3, 0],
            &async_snmp::Value::TimeTicks(4200),
        );
        assert_eq!(s.sensor, "object");
        assert_eq!(s.instance, "1.3.6.1.2.1.1.3.0");
        assert_eq!(s.value, 4200.0);
        assert_eq!(s.labels["type"], "TimeTicks");
        assert_eq!(s.labels["value"], "4200");
        let text = object(
            &[1, 3],
            &async_snmp::Value::OctetString(b"42".to_vec().into()),
        );
        assert_eq!(text.value, 0.0, "a text is not a number here");
        assert_eq!(text.labels["value"], "42");
        let missing = object(&[1, 3], &async_snmp::Value::NoSuchObject);
        assert_eq!(missing.labels["type"], "noSuchObject");
        assert_eq!(missing.labels["value"], "");
        assert!(build_read(&params(&[
            ("host", "192.0.2.1"),
            ("community", "c"),
            ("walk", "1.3.6.1")
        ]))
        .is_ok());
    }
}
