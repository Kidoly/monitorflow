//! Alert grouping (ADR-0080): incidents of one customer that the
//! dispatcher claims together become a few messages per channel instead of
//! one each. When they are claimed is the dispatcher's window; this module
//! decides what goes together and what the message says.
//!
//! Per channel, in this order, each incident event joins the first group
//! it fits, and a group needs two members:
//!
//! 1. [`Kind::Silence`]: what a silence held back (ADR-0079), all of the
//!    customer in one list, every member named.
//! 2. [`Kind::Check`]: the same check, like `offline` or `disk_used_percent
//!    /var`, on several servers or relays.
//! 3. [`Kind::Site`]: the rest of one site.
//!
//! Problems and resolutions never share a group, and nothing mixes
//! customers. Other events, and incidents left alone, are sent as before.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};
use uuid::Uuid;

use super::chat::Service;
use super::message::{
    capitalized, email_body, event_text, not_acknowledged, sensor, site_place, str_at, Event,
    Message,
};
use super::{follow_up, silence};

/// The `event` of a group message's payload. Webhooks never get one: they
/// keep one request per event (ADR-0080).
pub const GROUP: &str = "incident.group";

/// What the members of a group share.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// Held back by a silence, released together.
    Silence,
    /// One check on several servers or relays.
    Check,
    /// One site.
    Site,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Silence => "silence",
            Self::Check => "check",
            Self::Site => "site",
        }
    }
}

/// Incident events of one customer sent as one message.
#[derive(Debug, Clone, PartialEq)]
pub struct Group<'a> {
    pub kind: Kind,
    /// Oldest first.
    pub members: Vec<&'a Event>,
}

/// What a channel is sent for the events of a pass.
#[derive(Debug, Clone, PartialEq)]
pub enum Outgoing<'a> {
    One(&'a Event),
    Group(Group<'a>),
}

impl Outgoing<'_> {
    fn first(&self) -> i64 {
        match self {
            Self::One(event) => event.id,
            Self::Group(group) => group.members.first().map_or(0, |e| e.id),
        }
    }
}

fn resolved(event: &Event) -> bool {
    event.event == "incident.resolved"
}

fn incident<'a>(event: &'a Event, key: &str) -> &'a str {
    str_at(&event.details, &["incident", key]).unwrap_or_default()
}

/// Splits `events` by `key` into groups of two or more, and the rest.
fn split<'a, K: Ord>(
    events: Vec<&'a Event>,
    kind: Kind,
    key: impl Fn(&Event) -> K,
) -> (Vec<Group<'a>>, Vec<&'a Event>) {
    let mut by: BTreeMap<K, Vec<&Event>> = BTreeMap::new();
    for event in events {
        by.entry(key(event)).or_default().push(event);
    }
    let mut groups = Vec::new();
    let mut alone = Vec::new();
    for (_, mut members) in by {
        members.sort_by_key(|e| e.id);
        if members.len() > 1 {
            groups.push(Group { kind, members });
        } else {
            alone.extend(members);
        }
    }
    alone.sort_by_key(|e| e.id);
    (groups, alone)
}

/// What to send a channel for `events`, the ones it covers, oldest first.
pub fn group<'a>(events: &[&'a Event]) -> Vec<Outgoing<'a>> {
    let mut out = Vec::new();
    let mut customers: BTreeMap<Uuid, Vec<&Event>> = BTreeMap::new();
    for &event in events {
        match event.customer_id {
            Some(customer) if event.event.starts_with("incident.") => {
                customers.entry(customer).or_default().push(event)
            }
            _ => out.push(Outgoing::One(event)),
        }
    }
    for events in customers.into_values() {
        let (owed, rest): (Vec<&Event>, Vec<&Event>) =
            events.into_iter().partition(|e| silence::owed(&e.details));
        // Follow-up steps (ADR-0082) go together, apart from fresh news.
        let (steps, mut fresh): (Vec<&Event>, Vec<&Event>) = rest
            .into_iter()
            .partition(|e| follow_up::is_step(&e.details));
        // An incident that opened and got worse within the window is told
        // once, as it is now.
        let latest: BTreeMap<(&str, bool), i64> = fresh
            .iter()
            .map(|e| ((incident(e, "id"), resolved(e)), e.id))
            .collect();
        fresh.retain(|e| latest.get(&(incident(e, "id"), resolved(e))) == Some(&e.id));
        let (silenced, alone) = split(owed, Kind::Silence, |_| ());
        out.extend(silenced.into_iter().map(Outgoing::Group));
        out.extend(alone.into_iter().map(Outgoing::One));
        for pool in [steps, fresh] {
            let (checks, rest) = split(pool, Kind::Check, |e| {
                (
                    resolved(e),
                    incident(e, "metric").to_owned(),
                    incident(e, "instance").to_owned(),
                )
            });
            let (sites, rest) = split(rest, Kind::Site, |e| (resolved(e), e.site_id));
            out.extend(checks.into_iter().chain(sites).map(Outgoing::Group));
            out.extend(rest.into_iter().map(Outgoing::One));
        }
    }
    out.sort_by_key(Outgoing::first);
    out
}

/// "1 server", "3 servers".
fn count(n: usize, what: &str) -> String {
    if n == 1 {
        format!("1 {what}")
    } else {
        format!("{n} {what}s")
    }
}

/// The servers and relays of `members`, like "relay gw-1 and 12 servers",
/// and how many they are.
fn hosts(members: &[&Event]) -> (String, usize) {
    let ids = |key: &str| -> BTreeSet<String> {
        members
            .iter()
            .filter_map(|e| str_at(&e.details, &[key, "id"]).map(str::to_owned))
            .collect()
    };
    let (servers, relays) = (ids("server").len(), ids("relay"));
    let relay_name = || {
        members
            .iter()
            .find_map(|e| str_at(&e.details, &["relay", "name"]))
            .unwrap_or("relay")
            .to_owned()
    };
    let text = match (relays.len(), servers) {
        (0, n) => count(n, "server"),
        (1, 0) => format!("relay {}", relay_name()),
        (1, n) => format!("relay {} and {}", relay_name(), count(n, "server")),
        (r, 0) => count(r, "relay"),
        (r, n) => format!("{r} relays and {}", count(n, "server")),
    };
    (text, servers + relays.len())
}

/// " at acme / paris", or " in 3 sites" (" of acme"), or nothing.
fn place(members: &[&Event]) -> String {
    let sites: BTreeSet<Uuid> = members.iter().filter_map(|e| e.site_id).collect();
    let first = members.first().map(|e| &e.details);
    match (sites.len(), first) {
        (1, Some(details)) => site_place(details)
            .map(|p| format!(" at {p}"))
            .unwrap_or_default(),
        (n, Some(details)) if n > 1 => {
            match str_at(details, &["customer", "name"]).filter(|&c| c != "default") {
                Some(customer) => format!(" in {n} sites of {customer}"),
                None => format!(" in {n} sites"),
            }
        }
        _ => String::new(),
    }
}

/// The worst severity of what is still a problem, or of everything when
/// all is resolved.
fn worst<'a>(members: &[&'a Event]) -> &'a str {
    let open: Vec<&&Event> = members.iter().filter(|e| !resolved(e)).collect();
    let pool: Vec<&&Event> = if open.is_empty() {
        members.iter().collect()
    } else {
        open
    };
    if pool.iter().any(|e| incident(e, "severity") == "critical") {
        "critical"
    } else {
        "warning"
    }
}

/// The group in one line: what happened, to how many, where, how bad.
/// Follow-up steps say first how long nobody acknowledged them.
pub fn title(group: &Group) -> String {
    let title = plain_title(group);
    let waited = group
        .members
        .iter()
        .filter_map(|e| e.details["follow_up"]["minutes"].as_i64())
        .min();
    match waited {
        Some(minutes) => format!("{}{title}", not_acknowledged(minutes)),
        None => title,
    }
}

fn plain_title(group: &Group) -> String {
    let members = &group.members[..];
    let (hosts, many) = hosts(members);
    let place = place(members);
    let severity = capitalized(worst(members));
    let first = members[0];
    let verb = if many == 1 { "is" } else { "are" };
    match group.kind {
        Kind::Silence => {
            let open = members.iter().filter(|e| !resolved(e)).count();
            let parts = [
                (open > 0).then(|| format!("{} still open", count(open, "incident"))),
                (members.len() > open).then(|| format!("{} resolved", members.len() - open)),
            ];
            let parts: Vec<String> = parts.into_iter().flatten().collect();
            let lead = if open > 0 {
                format!("{severity} after a silence")
            } else {
                "After a silence".into()
            };
            format!("{lead}{place}: {}", parts.join(", "))
        }
        Kind::Check if incident(first, "metric") == "offline" => {
            if resolved(first) {
                let back = members
                    .iter()
                    .all(|e| matches!(incident(e, "reason"), "" | "back_online"));
                if back {
                    format!("Resolved: {hosts} {verb} back online{place}")
                } else {
                    format!("Resolved: {hosts} no longer offline{place}")
                }
            } else {
                format!("{severity}: {hosts} {verb} offline{place}")
            }
        }
        Kind::Check => {
            let sensor = sensor(&first.details["incident"]);
            if resolved(first) {
                format!("Resolved: {sensor} on {hosts}{place}")
            } else {
                format!("{severity}: {sensor} on {hosts}{place}")
            }
        }
        Kind::Site => {
            let alerts = count(members.len(), "alert");
            if resolved(first) {
                format!("Resolved: {alerts} on {hosts}{place}")
            } else {
                format!("{severity}: {alerts} on {hosts}{place}")
            }
        }
    }
}

/// Renders `group`, whose list in the interface is `id`. `service` is the
/// chat service of the channel, which limits the lines of a message, each
/// cut to [`super::chat::GROUP_LINE`] on its card; email
/// lists every member in one. What a silence held back is owed: every
/// member is named, in as many messages as needed. Otherwise the last line
/// says how many more the link shows.
pub fn render_group(
    group: &Group,
    id: Uuid,
    web_url: &str,
    service: Option<Service>,
) -> Vec<Message> {
    let members = &group.members;
    let title = title(group);
    let url = format!("{web_url}/incidents/?group={id}");
    let lines: Vec<(String, Vec<i64>)> = members
        .iter()
        .map(|e| (event_text(e), vec![e.id]))
        .collect();
    let chunks: Vec<Vec<(String, Vec<i64>)>> = match service.map(Service::group_lines) {
        Some(max) if group.kind == Kind::Silence => lines.chunks(max).map(<[_]>::to_vec).collect(),
        Some(max) if lines.len() > max => {
            let mut kept = lines[..max - 1].to_vec();
            let more = lines.len() + 1 - max;
            let rows = lines[max - 1..]
                .iter()
                .flat_map(|(_, ids)| ids.clone())
                .collect();
            kept.push((format!("and {more} more"), rows));
            vec![kept]
        }
        _ => vec![lines],
    };
    let first = &members[0].details;
    let sites: BTreeSet<Option<Uuid>> = members.iter().map(|e| e.site_id).collect();
    let (hosts, _) = hosts(members);
    let mut details = json!({
        "customer": first.get("customer").cloned().unwrap_or(Value::Null),
        "site": if sites.len() == 1 { first.get("site").cloned().unwrap_or(Value::Null) } else { Value::Null },
        "incident": { "severity": worst(members) },
    });
    if group.kind == Kind::Silence {
        details["silence"] = first.get("silence").cloned().unwrap_or(json!({}));
    }
    let parts = chunks.len();
    chunks
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            let text = if parts > 1 {
                format!("{title} ({} of {parts})", index + 1)
            } else {
                title.clone()
            };
            let rows: Vec<i64> = chunk.iter().flat_map(|(_, ids)| ids.clone()).collect();
            let lines: Vec<String> = chunk.into_iter().map(|(line, _)| line).collect();
            let mut payload = details.clone();
            payload["event"] = GROUP.into();
            payload["text"] = text.clone().into();
            payload["url"] = url.clone().into();
            payload["group"] = json!({
                "id": id,
                "kind": group.kind.name(),
                "resolved": members.iter().all(|e| resolved(e)),
                "size": members.len(),
                "hosts": hosts,
                "rows": rows,
            });
            Message {
                subject: format!("[MonitorFlow] {text}"),
                body: email_body(&text, &url, &lines, &details, web_url),
                text,
                url: url.clone(),
                payload,
                lines,
            }
        })
        .collect()
}

/// The outbox rows a rendered message tells: its `id`, or a group's
/// `rows`.
pub fn rows(payload: &Value) -> Vec<i64> {
    match payload["group"]["rows"].as_array() {
        Some(rows) => rows.iter().filter_map(Value::as_i64).collect(),
        None => payload
            .get("id")
            .and_then(Value::as_str)
            .and_then(|id| id.parse().ok())
            .into_iter()
            .collect(),
    }
}

#[cfg(test)]
mod tests;
