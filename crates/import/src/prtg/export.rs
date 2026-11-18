//! Reads PRTG's configuration file (`PRTG Configuration.dat`, XML) into
//! a [`Tree`]. Probes, groups, devices and sensors are `probenode`,
//! `group`, `device` and `sensor` elements with an `id`, nested in
//! `nodes`; each keeps its settings as the children of its `data`, by the
//! same names the API's `getobjectproperty` uses. Channels are `channel`
//! elements of a sensor, notification templates `notification` elements.

use std::collections::BTreeMap;

use chrono::Duration;
use quick_xml::events::Event;
use quick_xml::Reader;

use crate::source::ReadError;

use super::api::{deliveries, number, setting_date, unescape};
use super::sensors::wanted;
use super::tree::{Channel, Kind, Limits, Maintenance, Notification, Object, Paused, Tree};

/// Deepest nesting read, far beyond any real tree.
const MAX_DEPTH: usize = 256;

/// An XML element, with only what the import reads.
#[derive(Debug, Default)]
struct Element {
    name: String,
    id: Option<i64>,
    text: String,
    children: Vec<Element>,
}

impl Element {
    fn child(&self, name: &str) -> Option<&Element> {
        self.children.iter().find(|c| c.name == name)
    }

    /// Its text, or the text of its first `cell` for the settings PRTG
    /// writes in parts.
    fn value(&self) -> String {
        match self.child("cell") {
            Some(cell) => cell.text.trim().to_owned(),
            None => self.text.trim().to_owned(),
        }
    }

    /// The settings in its `data` child, by lowercase name.
    fn settings(&self) -> BTreeMap<String, String> {
        self.child("data")
            .map(|d| {
                d.children
                    .iter()
                    .map(|c| (c.name.to_ascii_lowercase(), c.value()))
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn unreadable(why: impl std::fmt::Display) -> String {
    why.to_string()
}

fn parse(xml: &str) -> Result<Element, String> {
    let mut reader = Reader::from_str(xml);
    let mut stack = vec![Element::default()];
    loop {
        let event = reader.read_event().map_err(unreadable)?;
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let id = e
                    .try_get_attribute("id")
                    .ok()
                    .flatten()
                    .or_else(|| e.try_get_attribute("ID").ok().flatten())
                    .and_then(|a| a.value.trim().parse().ok());
                let element = Element {
                    name: e.name().as_ref().to_ascii_lowercase(),
                    id,
                    ..Element::default()
                };
                if matches!(event, Event::Empty(_)) {
                    stack
                        .last_mut()
                        .expect("the root stays")
                        .children
                        .push(element);
                } else {
                    if stack.len() > MAX_DEPTH {
                        return Err(unreadable("it is nested too deeply"));
                    }
                    stack.push(element);
                }
            }
            Event::Text(ref t) => stack.last_mut().expect("the root stays").text.push_str(t),
            Event::CData(ref t) => stack.last_mut().expect("the root stays").text.push_str(t),
            Event::GeneralRef(ref r) => {
                let reference: &str = r;
                stack
                    .last_mut()
                    .expect("the root stays")
                    .text
                    .push_str(&unescape(&format!("&{reference};")));
            }
            Event::End(_) => {
                let done = stack
                    .pop()
                    .filter(|_| !stack.is_empty())
                    .ok_or_else(|| unreadable("an element closes twice"))?;
                stack.last_mut().expect("checked above").children.push(done);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if stack.len() != 1 {
        return Err(unreadable("it ends in the middle of an element"));
    }
    Ok(stack.pop().expect("one left"))
}

fn kind(name: &str) -> Option<Kind> {
    match name {
        "probenode" => Some(Kind::Probe),
        "group" => Some(Kind::Group),
        "device" => Some(Kind::Device),
        "sensor" => Some(Kind::Sensor),
        _ => None,
    }
}

/// Reads one channel of a sensor.
fn channel(element: &Element) -> Option<Channel> {
    let id = element.id?;
    let settings = element.settings();
    let get = |name: &str| settings.get(name).and_then(|v| number(v));
    let limits = (settings.get("limitmode").map(String::as_str) == Some("1")).then(|| Limits {
        max_error: get("limitmaxerror"),
        max_warning: get("limitmaxwarning"),
        min_error: get("limitminerror"),
        min_warning: get("limitminwarning"),
    });
    Some(Channel {
        id,
        name: settings.get("name").cloned().unwrap_or_default(),
        unit: settings
            .get("unit")
            .or_else(|| settings.get("customunit"))
            .cloned()
            .unwrap_or_default(),
        limits,
    })
}

/// The channels of a sensor: `channel` elements below it, outside any
/// nested object.
fn channels(element: &Element, out: &mut Vec<Channel>) {
    for child in &element.children {
        if child.name == "channel" {
            out.extend(channel(child));
        } else if kind(&child.name).is_none() {
            channels(child, out);
        }
    }
}

fn object(element: &Element, kind: Kind, parent: Option<i64>, offset: Duration) -> Option<Object> {
    let id = element.id?;
    let mut settings = element.settings();
    let name = settings
        .get("name")
        .cloned()
        .unwrap_or_else(|| format!("#{id}"));
    let mut object = Object::new(id, kind, name, parent);
    if settings.get("active").map(String::as_str) == Some("0") {
        object.paused = Some(Paused::Indefinitely);
    }
    if settings.get("maintenable").map(String::as_str) == Some("1") {
        let utc = |name: &str| {
            settings
                .get(name)
                .and_then(|v| setting_date(v))
                .map(|d| (d - offset).and_utc())
        };
        if let (Some(start), Some(end)) = (utc("maintstart"), utc("maintend")) {
            object.maintenance = Some(Maintenance { start, end });
        }
    }
    object.schedule = settings.get("schedule").and_then(|v| schedule_name(v));
    match kind {
        Kind::Device => object.host = settings.get("host").filter(|h| !h.is_empty()).cloned(),
        Kind::Sensor => {
            object.sensor_type = ["sensortype", "sensorkind", "typename"]
                .iter()
                .find_map(|k| settings.get(*k))
                .map(|t| t.to_ascii_lowercase());
            object.interval_s = settings.get("interval").and_then(|v| {
                let digits: String = v.chars().take_while(char::is_ascii_digit).collect();
                digits.parse().ok()
            });
            let (keep, with_channels) = wanted(object.sensor_type.as_deref().unwrap_or(""));
            settings.retain(|k, _| keep.contains(&k.as_str()));
            object.settings = settings;
            if with_channels {
                channels(element, &mut object.channels);
            }
        }
        _ => {}
    }
    Some(object)
}

fn walk(element: &Element, parent: Option<i64>, offset: Duration, tree: &mut Tree) {
    for child in &element.children {
        match kind(&child.name) {
            Some(k) => {
                // The root group is the whole installation, not a group.
                let k = if k == Kind::Group && child.id == Some(0) {
                    None
                } else {
                    Some(k)
                };
                let parent_here = match k.and_then(|k| object(child, k, parent, offset)) {
                    Some(o) => {
                        let id = o.id;
                        tree.objects.push(o);
                        Some(id)
                    }
                    None => parent,
                };
                walk(child, parent_here, offset, tree);
            }
            // A schedule's definition; an object's `schedule` setting has
            // no ID.
            None if child.name == "schedule" && child.id.is_some() => {
                let settings = child.settings();
                if let Some(name) = settings.get("name").filter(|n| !n.is_empty()) {
                    let periods = settings
                        .get("periodlist")
                        .or_else(|| settings.get("periods"))
                        .filter(|p| !p.trim().is_empty())
                        .cloned();
                    tree.schedules.insert(name.clone(), periods);
                }
            }
            None if child.name == "notification" => {
                if let Some(id) = child.id {
                    let mut fields = BTreeMap::new();
                    flatten(child, "", &mut fields);
                    let settings = child.settings();
                    tree.notifications.push(Notification {
                        id,
                        name: settings
                            .get("name")
                            .cloned()
                            .unwrap_or_else(|| format!("#{id}")),
                        active: settings.get("active").map(String::as_str) != Some("0"),
                        deliveries: deliveries(&fields),
                    });
                }
            }
            None => walk(child, parent, offset, tree),
        }
    }
}

/// The schedule an object setting names: `Weekends`, or PRTG's
/// `651|Weekends|`; `None` for none.
pub fn schedule_name(value: &str) -> Option<String> {
    let parts: Vec<&str> = value.split('|').map(str::trim).collect();
    let name = match parts.as_slice() {
        [id, name, ..] if id.chars().all(|c| c.is_ascii_digit()) => *name,
        [name, ..] => *name,
        [] => "",
    };
    (!name.is_empty() && !name.eq_ignore_ascii_case("none")).then(|| name.to_owned())
}

/// Every leaf below `element` by its path of names, for [`deliveries`].
fn flatten(element: &Element, path: &str, out: &mut BTreeMap<String, String>) {
    for child in &element.children {
        let path = if path.is_empty() {
            child.name.clone()
        } else {
            format!("{path}_{}", child.name)
        };
        if child.children.is_empty() || child.child("cell").is_some() {
            out.insert(path, child.value());
        } else {
            flatten(child, &path, out);
        }
    }
}

/// Reads the configuration file `file`. `offset` is how far PRTG's local
/// time, in which it writes maintenance windows, is ahead of UTC.
pub fn read(file: &str, xml: &str, offset: Duration) -> Result<Tree, ReadError> {
    let root = parse(xml).map_err(|why| ReadError::file(file, why))?;
    let mut tree = Tree::default();
    walk(&root, None, offset, &mut tree);
    if !tree.objects.iter().any(|o| o.kind == Kind::Probe) {
        return Err(ReadError::file(
            file,
            "it holds no probe; is it PRTG's configuration file?",
        ));
    }
    Ok(tree)
}
