//! The PRTG objects an import reads, whatever they came from: its API
//! ([`super::api`]) or its configuration file ([`super::export`]). The
//! mapping ([`super::mapping`]) only sees this tree.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};

/// What an object is in the PRTG tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Probe,
    Group,
    Device,
    Sensor,
}

/// Why PRTG does not monitor an object right now, from its status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Paused {
    /// Paused by a person, with no end.
    Indefinitely,
    /// Paused by a person until this time.
    Until(DateTime<Utc>),
    /// Paused by its schedule: PRTG decides, nobody did. One paused by a
    /// dependency right now is not paused: its dependency is read instead.
    ByRule,
}

/// A scheduled maintenance window of an object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Maintenance {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

/// The limits of one channel of a sensor, in the channel's unit.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Limits {
    pub max_error: Option<f64>,
    pub max_warning: Option<f64>,
    pub min_error: Option<f64>,
    pub min_warning: Option<f64>,
}

impl Limits {
    pub fn is_empty(&self) -> bool {
        self.max_error.is_none()
            && self.max_warning.is_none()
            && self.min_error.is_none()
            && self.min_warning.is_none()
    }
}

/// One channel of a sensor.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Channel {
    pub id: i64,
    pub name: String,
    /// Like `%`, `MByte`, `#`, `msec`; empty when unknown.
    pub unit: String,
    /// `None` when limits are off.
    pub limits: Option<Limits>,
    /// Its lookup, by PRTG's ID, with its values when a lookup file read
    /// had them (ADR-0157).
    pub lookup: Option<crate::model::SnmpLookup>,
    /// PRTG's multiplication and division of its value.
    pub factor_m: Option<f64>,
    pub factor_d: Option<f64>,
}

/// A probe, group, device or sensor.
#[derive(Debug, Clone, PartialEq)]
pub struct Object {
    pub id: i64,
    pub kind: Kind,
    pub name: String,
    /// `None` for a probe, under the root.
    pub parent: Option<i64>,
    /// Of a device: its IPv4 or IPv6 address or DNS name.
    pub host: Option<String>,
    /// Of a sensor: its type, like `ping` or `wmiservice`, lowercase.
    pub sensor_type: Option<String>,
    /// Of a sensor: seconds between scans.
    pub interval_s: Option<u32>,
    pub paused: Option<Paused>,
    pub maintenance: Option<Maintenance>,
    /// The name of the schedule it follows, if any (ADR-0151).
    pub schedule: Option<String>,
    /// The sensor's own settings, like `service` or `httpurl`, by
    /// lowercase name.
    pub settings: BTreeMap<String, String>,
    pub channels: Vec<Channel>,
    /// Its "dependency" setting (ADR-0152).
    pub depends: Option<Depends>,
    /// Of a custom SNMP sensor: the lookups it names, by PRTG's ID, with
    /// their values when a lookup file had them (ADR-0157).
    pub lookups: BTreeMap<String, crate::model::SnmpLookup>,
}

/// What an object's dependency setting says, when it is not "use parent".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depends {
    /// "Select object": it is paused while this object is down.
    Object(i64),
    /// A sensor set as "master object for parent": the other sensors of its
    /// device are paused while it is down.
    Master,
}

impl Depends {
    /// From the `dependencytype` and `dependency` settings: `1` with an
    /// object ID (PRTG may write `2034|name`), or `2`.
    pub fn parse(kind: Option<&str>, object: Option<&str>) -> Option<Self> {
        match kind?.trim() {
            "1" => {
                let digits: String = object?
                    .trim()
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect();
                digits.parse().ok().map(Self::Object)
            }
            "2" => Some(Self::Master),
            _ => None,
        }
    }
}

impl Object {
    pub fn new(id: i64, kind: Kind, name: impl Into<String>, parent: Option<i64>) -> Self {
        Self {
            id,
            kind,
            name: name.into(),
            parent,
            host: None,
            sensor_type: None,
            interval_s: None,
            paused: None,
            maintenance: None,
            schedule: None,
            settings: BTreeMap::new(),
            channels: Vec::new(),
            depends: None,
            lookups: BTreeMap::new(),
        }
    }

    /// A setting, trimmed; `None` when absent or empty.
    pub fn setting(&self, name: &str) -> Option<&str> {
        self.settings
            .get(name)
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
    }
}

/// How a notification template or contact reaches people.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// Email to these addresses.
    Email(Vec<String>),
    /// An HTTP request to this URL, which may hold a secret.
    Http(String),
    /// Anything else (SMS, push, syslog, SNMP trap, a program, a ticket),
    /// named for the report.
    Other(String),
}

/// A notification template, or a contact of one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub id: i64,
    pub name: String,
    /// `false` for a template paused in PRTG.
    pub active: bool,
    /// The methods it has turned on.
    pub deliveries: Vec<Delivery>,
}

/// Everything an import read.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tree {
    pub objects: Vec<Object>,
    pub notifications: Vec<Notification>,
    /// Maintenance windows are in PRTG's local time, read as UTC: its
    /// time zone was not known.
    pub local_times: bool,
    /// Schedules by name: their period list, the times objects that follow
    /// them are paused, one `ww:hh:mm-ww:hh:mm` per line; `None` for a
    /// time table.
    pub schedules: BTreeMap<String, Option<String>>,
}

impl Tree {
    pub fn get(&self, id: i64) -> Option<&Object> {
        self.objects.iter().find(|o| o.id == id)
    }

    /// The direct children of `id`, in the order read.
    pub fn children(&self, id: i64) -> impl Iterator<Item = &Object> {
        self.objects.iter().filter(move |o| o.parent == Some(id))
    }

    /// The nearest object of `kind` at or above `id`.
    pub fn ancestor(&self, id: i64, kind: Kind) -> Option<&Object> {
        let mut current = self.get(id);
        // Bounded so a parent loop in a damaged file cannot hang the import.
        for _ in 0..=self.objects.len() {
            let object = current?;
            if object.kind == kind {
                return Some(object);
            }
            current = object.parent.and_then(|p| self.get(p));
        }
        None
    }

    /// Names from the probe down to `id`, joined with ` / `, for the
    /// preview to say where each item comes from.
    pub fn path(&self, id: i64) -> String {
        let mut names = Vec::new();
        let mut current = self.get(id);
        for _ in 0..=self.objects.len() {
            let Some(object) = current else { break };
            names.push(object.name.as_str());
            current = object.parent.and_then(|p| self.get(p));
        }
        names.reverse();
        names.join(" / ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> Tree {
        Tree {
            objects: vec![
                Object::new(1, Kind::Probe, "Probe", None),
                Object::new(10, Kind::Group, "Servers", Some(1)),
                Object::new(20, Kind::Device, "web", Some(10)),
                Object::new(30, Kind::Sensor, "Ping", Some(20)),
            ],
            notifications: Vec::new(),
            local_times: false,
            schedules: BTreeMap::new(),
        }
    }

    #[test]
    fn walks_up_and_down() {
        let tree = tree();
        assert_eq!(tree.path(30), "Probe / Servers / web / Ping");
        assert_eq!(tree.ancestor(30, Kind::Probe).unwrap().id, 1);
        assert_eq!(tree.ancestor(30, Kind::Device).unwrap().id, 20);
        assert!(tree.ancestor(10, Kind::Device).is_none());
        assert_eq!(tree.children(10).map(|o| o.id).collect::<Vec<_>>(), [20]);
    }

    #[test]
    fn a_parent_loop_ends() {
        let mut tree = tree();
        tree.objects[0].parent = Some(30);
        assert!(tree.ancestor(30, Kind::Probe).is_some());
        assert!(tree.ancestor(30, Kind::Group).is_some());
        assert_eq!(tree.path(1).split(" / ").count(), 5);
    }

    #[test]
    fn settings_are_trimmed() {
        let mut object = Object::new(1, Kind::Sensor, "s", None);
        object
            .settings
            .insert("service".into(), "  W32Time ".into());
        object.settings.insert("port".into(), " ".into());
        assert_eq!(object.setting("service"), Some("W32Time"));
        assert_eq!(object.setting("port"), None);
    }
}
