//! Network maps (ADR-0185): what a map stores, checked without the
//! database, and the rules that colour its nodes and links.
//!
//! A map draws the servers, devices and relays of a site or customer, and
//! links between them from relays, dependencies (ADR-0152) and the LLDP
//! and CDP neighbours of SNMP devices ([`neighbors`]); a network scan's
//! unmonitored addresses (ADR-0154) can be shown too. What a person adds
//! by hand (positions, zones, links, labels, a background) is the map's
//! [`MapDocument`].

pub mod neighbors;

use std::fmt;
use std::net::IpAddr;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Format of the document: 1.
pub const DOCUMENT_VERSION: u32 = 1;
/// Most nodes a map places by hand.
pub const MAX_PLACED: usize = 2_000;
/// Most zones of a map.
pub const MAX_ZONES: usize = 100;
/// Most links drawn by hand.
pub const MAX_LINKS: usize = 500;
/// Most labels.
pub const MAX_LABELS: usize = 200;
/// Coordinates lie within this distance of the origin.
pub const MAX_COORDINATE: f64 = 100_000.0;
/// Longest name or label.
pub const MAX_NAME: usize = 80;
/// Longest label text.
pub const MAX_TEXT: usize = 200;

/// What draws a map besides what was added by hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = MapSources)]
#[serde(deny_unknown_fields)]
pub struct Sources {
    /// The central server, its relays, and what each relay carries or
    /// checks. `true` when absent.
    #[serde(default = "yes")]
    pub relays: bool,
    /// Dependencies set by people or imported (ADR-0152). `true` when
    /// absent.
    #[serde(default = "yes")]
    pub dependencies: bool,
    /// LLDP and CDP neighbours that SNMP devices last reported. `true`
    /// when absent.
    #[serde(default = "yes")]
    pub neighbors: bool,
    /// Addresses the latest run of the site's network scans found and
    /// nothing watches yet (ADR-0154). `false` when absent.
    #[serde(default)]
    pub discovered: bool,
}

fn yes() -> bool {
    true
}

impl Default for Sources {
    fn default() -> Self {
        Self {
            relays: true,
            dependencies: true,
            neighbors: true,
            discovered: false,
        }
    }
}

/// Where a node is, and whether it is left off the map.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = MapPlacement)]
#[serde(deny_unknown_fields)]
pub struct Placement {
    /// The node: `central`, `server:<id>` (a server or device),
    /// `relay:<id>`, `host:<address>` (found by a scan) or
    /// `neighbor:<name>` (reported by a device, not monitored).
    pub key: String,
    pub x: f64,
    pub y: f64,
    /// Left off the map; it comes back when shown again.
    #[serde(default, skip_serializing_if = "is_false")]
    pub hidden: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// A rectangle drawn under nodes, with a name: a floor, a rack, a room.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = MapZone)]
#[serde(deny_unknown_fields)]
pub struct Zone {
    /// Unique within the map: 1 to 36 letters, digits, `-` and `_`.
    pub id: String,
    /// Up to 80 characters; may be empty.
    #[serde(default)]
    pub label: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    /// A colour of the palette, 1 to 8; 1 when absent.
    #[serde(default = "first_color")]
    pub color: u8,
}

fn first_color() -> u8 {
    1
}

/// One interface of a server or device whose traffic a link shows.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = MapInterface)]
#[serde(deny_unknown_fields)]
pub struct InterfaceRef {
    pub server_id: Uuid,
    /// The interface's instance, as its metrics name it: `Gi0/1`, `eth0`.
    pub instance: String,
}

/// A link drawn by hand.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = MapLink)]
#[serde(deny_unknown_fields)]
pub struct ManualLink {
    /// Unique within the map, as a zone's.
    pub id: String,
    /// The key of one end.
    pub from: String,
    /// The key of the other end.
    pub to: String,
    /// Up to 80 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The interface whose traffic and state it shows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface: Option<InterfaceRef>,
}

/// How large a label is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = MapLabelSize)]
#[serde(rename_all = "lowercase")]
pub enum LabelSize {
    Small,
    #[default]
    Medium,
    Large,
}

/// Text placed on the map.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = MapLabel)]
#[serde(deny_unknown_fields)]
pub struct Label {
    /// Unique within the map, as a zone's.
    pub id: String,
    /// 1 to 200 characters.
    pub text: String,
    pub x: f64,
    pub y: f64,
    #[serde(default)]
    pub size: LabelSize,
}

/// Where the background image lies, in the map's coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = MapBackgroundPlacement)]
#[serde(deny_unknown_fields)]
pub struct BackgroundPlacement {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    /// 0.1 to 1; 1 when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f64>,
}

/// What a person set on a map. Everything not placed here is laid out by
/// the client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MapDocument {
    /// Format of the document: 1.
    pub version: u32,
    #[serde(default)]
    pub sources: Sources,
    /// Whether a customer's map draws a zone around each site's nodes.
    /// `true` when absent.
    #[serde(default = "yes")]
    pub group_by_site: bool,
    /// At most 2,000.
    #[serde(default)]
    pub nodes: Vec<Placement>,
    /// At most 100.
    #[serde(default)]
    pub zones: Vec<Zone>,
    /// At most 500.
    #[serde(default)]
    pub links: Vec<ManualLink>,
    /// At most 200.
    #[serde(default)]
    pub labels: Vec<Label>,
    /// Where the background image lies; `null` lays it from the origin
    /// at its own size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<BackgroundPlacement>,
}

impl Default for MapDocument {
    fn default() -> Self {
        Self {
            version: DOCUMENT_VERSION,
            sources: Sources::default(),
            group_by_site: true,
            nodes: Vec::new(),
            zones: Vec::new(),
            links: Vec::new(),
            labels: Vec::new(),
            background: None,
        }
    }
}

/// A node of a map, by its key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NodeKey {
    /// The central server.
    Central,
    /// A server or a device.
    Server(Uuid),
    Relay(Uuid),
    /// An address a network scan found.
    Host(IpAddr),
    /// A neighbour a device reported that nothing on the map is.
    Neighbor(String),
}

impl fmt::Display for NodeKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Central => f.write_str("central"),
            Self::Server(id) => write!(f, "server:{id}"),
            Self::Relay(id) => write!(f, "relay:{id}"),
            Self::Host(address) => write!(f, "host:{address}"),
            Self::Neighbor(name) => write!(f, "neighbor:{name}"),
        }
    }
}

impl FromStr for NodeKey {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let bad = || format!("“{}” is not a node of a map.", clean(value, 60));
        if value == "central" {
            return Ok(Self::Central);
        }
        let (kind, rest) = value.split_once(':').ok_or_else(bad)?;
        match kind {
            "server" => rest.parse().map(Self::Server).map_err(|_| bad()),
            "relay" => rest.parse().map(Self::Relay).map_err(|_| bad()),
            "host" => rest.parse().map(Self::Host).map_err(|_| bad()),
            "neighbor" if is_text(rest, 1, 256) => Ok(Self::Neighbor(rest.to_owned())),
            _ => Err(bad()),
        }
    }
}

/// `text` without control characters, cut to `max` characters, for a
/// message.
fn clean(text: &str, max: usize) -> String {
    text.chars().filter(|c| !c.is_control()).take(max).collect()
}

/// Whether `text` has `min` to `max` characters and none is a control.
fn is_text(text: &str, min: usize, max: usize) -> bool {
    let count = text.chars().count();
    (min..=max).contains(&count) && !text.chars().any(char::is_control)
}

fn is_id(id: &str) -> bool {
    (1..=36).contains(&id.len())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn coordinate(value: f64) -> bool {
    value.is_finite() && value.abs() <= MAX_COORDINATE
}

fn size(value: f64) -> bool {
    value.is_finite() && value > 0.0 && value <= 2.0 * MAX_COORDINATE
}

/// Checks a document on its own: version, counts, keys, ids, lengths and
/// numbers. The error is a sentence for the form.
pub fn check(document: &MapDocument) -> Result<(), String> {
    if document.version != DOCUMENT_VERSION {
        return Err(format!("`version` must be {DOCUMENT_VERSION}."));
    }
    if document.nodes.len() > MAX_PLACED {
        return Err(format!("A map places at most {MAX_PLACED} nodes."));
    }
    if document.zones.len() > MAX_ZONES {
        return Err(format!("A map has at most {MAX_ZONES} zones."));
    }
    if document.links.len() > MAX_LINKS {
        return Err(format!(
            "A map has at most {MAX_LINKS} links drawn by hand."
        ));
    }
    if document.labels.len() > MAX_LABELS {
        return Err(format!("A map has at most {MAX_LABELS} labels."));
    }
    let mut keys = std::collections::BTreeSet::new();
    for node in &document.nodes {
        let key: NodeKey = node.key.parse()?;
        if !keys.insert(key) {
            return Err(format!("“{}” is placed twice.", clean(&node.key, 60)));
        }
        if !coordinate(node.x) || !coordinate(node.y) {
            return Err("Positions are numbers within 100,000 of the origin.".into());
        }
    }
    let mut ids = std::collections::BTreeSet::new();
    let mut unique = |id: &str| {
        if is_id(id) && ids.insert(id.to_owned()) {
            Ok(())
        } else {
            Err("Each zone, link and label needs its own id of up to 36 letters and digits.")
        }
    };
    for zone in &document.zones {
        unique(&zone.id)?;
        if !is_text(&zone.label, 0, MAX_NAME) {
            return Err(format!("A zone's name has up to {MAX_NAME} characters."));
        }
        if !coordinate(zone.x) || !coordinate(zone.y) || !size(zone.width) || !size(zone.height) {
            return Err("A zone has a position and a size above zero.".into());
        }
        if !(1..=8).contains(&zone.color) {
            return Err("A zone's colour is 1 to 8.".into());
        }
    }
    for link in &document.links {
        unique(&link.id)?;
        let from: NodeKey = link.from.parse()?;
        let to: NodeKey = link.to.parse()?;
        if from == to {
            return Err("A link joins two different nodes.".into());
        }
        if link
            .label
            .as_deref()
            .is_some_and(|l| !is_text(l, 1, MAX_NAME))
        {
            return Err(format!("A link's label has 1 to {MAX_NAME} characters."));
        }
        if let Some(interface) = &link.interface {
            if !is_text(&interface.instance, 1, 256) {
                return Err("Name the interface whose traffic the link shows.".into());
            }
            let end = NodeKey::Server(interface.server_id);
            if end != from && end != to {
                return Err("A link shows the traffic of an interface of one of its ends.".into());
            }
        }
    }
    for label in &document.labels {
        unique(&label.id)?;
        if !is_text(label.text.trim(), 1, MAX_TEXT) {
            return Err(format!("A label has 1 to {MAX_TEXT} characters."));
        }
        if !coordinate(label.x) || !coordinate(label.y) {
            return Err("Positions are numbers within 100,000 of the origin.".into());
        }
    }
    if let Some(background) = &document.background {
        if !coordinate(background.x)
            || !coordinate(background.y)
            || !size(background.width)
            || !size(background.height)
        {
            return Err("The background has a position and a size above zero.".into());
        }
        if background
            .opacity
            .is_some_and(|o| !(o.is_finite() && (0.1..=1.0).contains(&o)))
        {
            return Err("The background's opacity is 0.1 to 1.".into());
        }
    }
    Ok(())
}

/// A node's state, as the fleet's pages say it (ADR-0130), and `unknown`
/// for what nothing watches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[schema(as = MapNodeState)]
#[serde(rename_all = "lowercase")]
pub enum NodeState {
    Critical,
    Warning,
    Acknowledged,
    Silenced,
    Offline,
    Unreachable,
    Waiting,
    Ok,
    Unknown,
}

/// A link's colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, utoipa::ToSchema)]
#[schema(as = MapLinkState)]
#[serde(rename_all = "lowercase")]
pub enum LinkState {
    Unknown,
    Ok,
    Warning,
    Critical,
}

/// What a link's interface reads now.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, utoipa::ToSchema)]
#[schema(as = MapTraffic)]
pub struct Traffic {
    /// Received by the interface.
    pub in_bytes_per_second: Option<f64>,
    /// Sent by the interface.
    pub out_bytes_per_second: Option<f64>,
    /// The higher of in and out over the link's speed, 0 to 1.
    pub utilization: Option<f64>,
    /// Whether the interface is up; `null` when it does not say.
    pub up: Option<bool>,
}

/// Use of a link from which it turns amber, as the switch template's
/// alert (ADR-0149).
pub const BUSY: f64 = 0.9;

fn end(state: NodeState) -> Option<LinkState> {
    match state {
        NodeState::Critical | NodeState::Offline | NodeState::Unreachable => {
            Some(LinkState::Critical)
        }
        NodeState::Warning => Some(LinkState::Warning),
        NodeState::Acknowledged | NodeState::Silenced | NodeState::Waiting | NodeState::Ok => {
            Some(LinkState::Ok)
        }
        NodeState::Unknown => None,
    }
}

/// A link is as bad as the worse of its two ends and of its interface:
/// red when an end is critical, offline or unreachable, or the interface
/// is down; amber when an end has a warning or the interface is busy;
/// `unknown` when nothing watches either end or the interface.
pub fn link_state(a: NodeState, b: NodeState, traffic: Option<&Traffic>) -> LinkState {
    let wire = traffic.and_then(|t| match (t.up, t.utilization) {
        (Some(false), _) => Some(LinkState::Critical),
        (_, Some(u)) if u >= BUSY => Some(LinkState::Warning),
        (Some(true), _) | (_, Some(_)) => Some(LinkState::Ok),
        (None, None) => None,
    });
    [end(a), end(b), wire]
        .into_iter()
        .flatten()
        .max()
        .unwrap_or(LinkState::Unknown)
}

/// A host name as neighbours and scans compare it: lower case, without a
/// CDP serial in brackets nor the domain. An address stays whole.
pub fn short_name(name: &str) -> String {
    let name = name.trim();
    let name = match name.find('(') {
        Some(i) if i > 0 => &name[..i],
        _ => name,
    }
    .trim();
    if name.parse::<IpAddr>().is_ok() {
        return name.to_owned();
    }
    name.split('.').next().unwrap_or(name).to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn document(value: serde_json::Value) -> Result<MapDocument, String> {
        let document: MapDocument = serde_json::from_value(value).map_err(|e| e.to_string())?;
        check(&document)?;
        Ok(document)
    }

    #[test]
    fn an_empty_document_takes_the_defaults() {
        let d = document(json!({"version": 1})).unwrap();
        assert_eq!(d, MapDocument::default());
        assert!(d.sources.relays && d.sources.dependencies && d.sources.neighbors);
        assert!(!d.sources.discovered);
        assert!(d.group_by_site);
        // What it writes back reads the same.
        let again = document(serde_json::to_value(&d).unwrap()).unwrap();
        assert_eq!(again, d);
    }

    #[test]
    fn keys_name_what_a_map_can_show() {
        let id = Uuid::from_u128(7);
        for key in [
            "central".to_owned(),
            format!("server:{id}"),
            format!("relay:{id}"),
            "host:192.0.2.1".to_owned(),
            "host:2001:db8::1".to_owned(),
            "neighbor:sw-floor-2".to_owned(),
        ] {
            let parsed: NodeKey = key.parse().unwrap();
            assert_eq!(parsed.to_string(), key);
        }
        for bad in [
            "",
            "server",
            "server:42",
            "host:example.com",
            "neighbor:",
            "neighbor:a\u{7}b",
            "box:1",
        ] {
            assert!(bad.parse::<NodeKey>().is_err(), "{bad}");
        }
    }

    #[test]
    fn documents_are_bounded_and_consistent() {
        let id = Uuid::from_u128(1);
        let server = format!("server:{id}");
        let ok = json!({"version": 1,
            "nodes": [{"key": server, "x": 10.0, "y": -20.5}, {"key": "central", "x": 0, "y": 0, "hidden": true}],
            "zones": [{"id": "z1", "label": "Floor 2", "x": 0, "y": 0, "width": 300, "height": 200, "color": 3}],
            "links": [{"id": "l1", "from": "central", "to": server, "label": "uplink",
                       "interface": {"server_id": id, "instance": "Gi0/1"}}],
            "labels": [{"id": "t1", "text": "Paris", "x": 5, "y": 5, "size": "large"}],
            "background": {"x": 0, "y": 0, "width": 1600, "height": 900, "opacity": 0.5}});
        let d = document(ok.clone()).unwrap();
        assert!(d.nodes[1].hidden);
        assert_eq!(d.labels[0].size, LabelSize::Large);

        let refused = |patch: &dyn Fn(&mut serde_json::Value)| {
            let mut value = ok.clone();
            patch(&mut value);
            document(value).unwrap_err()
        };
        assert!(refused(&|v| v["version"] = json!(2)).contains("version"));
        assert!(refused(&|v| v["nodes"][1]["key"] = json!(server)).contains("twice"));
        assert!(refused(&|v| v["nodes"][0]["x"] = json!(1e9)).contains("within"));
        assert!(refused(&|v| v["zones"][0]["width"] = json!(0)).contains("size"));
        assert!(refused(&|v| v["zones"][0]["color"] = json!(9)).contains("colour"));
        assert!(refused(&|v| v["labels"][0]["id"] = json!("z1")).contains("own id"));
        assert!(refused(&|v| v["links"][0]["to"] = json!("central")).contains("different"));
        assert!(
            refused(&|v| v["links"][0]["interface"]["server_id"] = json!(Uuid::from_u128(2)))
                .contains("one of its ends")
        );
        assert!(refused(&|v| v["labels"][0]["text"] = json!("  ")).contains("characters"));
        assert!(refused(&|v| v["background"]["opacity"] = json!(0)).contains("opacity"));
        assert!(refused(&|v| v["zones"][0]["extra"] = json!(1)).contains("unknown field"));
        let many: Vec<_> = (0..=MAX_LABELS)
            .map(|i| json!({"id": format!("t{i}"), "text": "x", "x": 0, "y": 0}))
            .collect();
        assert!(refused(&|v| v["labels"] = json!(many)).contains("at most"));
    }

    #[test]
    fn a_link_is_as_bad_as_its_worse_end_or_interface() {
        use NodeState::*;
        assert_eq!(link_state(Ok, Ok, None), LinkState::Ok);
        assert_eq!(link_state(Ok, Warning, None), LinkState::Warning);
        assert_eq!(link_state(Unreachable, Ok, None), LinkState::Critical);
        assert_eq!(link_state(Offline, Warning, None), LinkState::Critical);
        assert_eq!(link_state(Silenced, Acknowledged, None), LinkState::Ok);
        assert_eq!(link_state(Unknown, Unknown, None), LinkState::Unknown);
        assert_eq!(link_state(Unknown, Warning, None), LinkState::Warning);
        let down = Traffic {
            up: Some(false),
            ..Traffic::default()
        };
        assert_eq!(link_state(Ok, Ok, Some(&down)), LinkState::Critical);
        let busy = Traffic {
            up: Some(true),
            utilization: Some(0.95),
            ..Traffic::default()
        };
        assert_eq!(
            link_state(Unknown, Unknown, Some(&busy)),
            LinkState::Warning
        );
        let quiet = Traffic {
            utilization: Some(0.1),
            ..Traffic::default()
        };
        assert_eq!(link_state(Unknown, Unknown, Some(&quiet)), LinkState::Ok);
        assert_eq!(
            link_state(Unknown, Unknown, Some(&Traffic::default())),
            LinkState::Unknown
        );
    }

    #[test]
    fn names_compare_without_domain_case_or_serial() {
        assert_eq!(short_name("SW-Core.example.net"), "sw-core");
        assert_eq!(short_name("sw-core(FOC1234X0AB)"), "sw-core");
        assert_eq!(short_name(" 192.0.2.7 "), "192.0.2.7");
        assert_eq!(short_name("web-1"), "web-1");
    }
}
